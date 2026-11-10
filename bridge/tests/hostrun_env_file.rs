//! A repository's `env_file` reaches the agent and nothing else, end to end.
//!
//! The real `jesse-runner` binary runs one round of a stub agent (a shell script standing in for
//! `claude -p`) for a run the real [`HostRuns`] created, launched, watched and reported. The stub
//! writes its environment to a file outside the run directory. Then:
//!
//!   * the env file's variables are in the stub's environment;
//!   * no value is in `run.json`, `job.plist`, `prompt.md`, the stream, `exit.json`, the
//!     runner's log, the Inbox report, any route body, any `devjob_*` tool output, any push or
//!     turn, or **the bridge's own log**.
//!
//! The bridge's log is its stderr, so this target has `harness = false`: no test harness
//! captures `eprintln!`, and the process points its own file descriptor 2 at a file for the
//! duration and reads it back.

use std::collections::BTreeMap;
use std::future::Future;
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use jesse_bridge::devjob::Notifier;
use jesse_bridge::gateway::InProcessServer as _;
use jesse_bridge::hostrun::sys::{Forge, Launcher, PrState};
use jesse_bridge::hostrun::{
    self, HostRunSettings, HostRunToml, HostRuns, RepoCapToml, RunSink, StartKind, StartRequest,
    TurnSender,
};
use serde_json::json;

const SECRET: &str = "s3cr3t-env-value-9a71";
const PLAIN: &str = "plain-env-value-41be";

/// Runs the round synchronously: `bootstrap` is the runner's whole life.
struct InlineLauncher {
    runner: PathBuf,
}

impl Launcher for InlineLauncher {
    fn bootstrap(&self, plist: &Path, _label: &str) -> Result<(), String> {
        let dir = plist.parent().ok_or("no run directory")?;
        let log =
            std::fs::File::create(dir.join(hostrun::RUNNER_LOG)).map_err(|e| e.to_string())?;
        let status = Command::new(&self.runner)
            .arg(dir)
            .arg("--no-bootout")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("the runner exited {status}"))
        }
    }
    fn bootout(&self, _label: &str) -> Result<(), String> {
        Ok(())
    }
    fn is_running(&self, _label: &str) -> bool {
        false
    }
    fn loaded_labels(&self, _prefix: &str) -> Vec<String> {
        Vec::new()
    }
    fn plist_of(&self, _label: &str) -> Option<PathBuf> {
        None
    }
}

struct NoPr;

impl Forge for NoPr {
    fn pr_state(&self, _repo: &str, _branch: &str) -> Option<PrState> {
        None
    }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl Notifier for Recorder {
    fn notify(
        &self,
        title: String,
        body: String,
        job: String,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.0.lock().unwrap().push(format!("{title} {body} {job}"));
        Box::pin(async {})
    }
}

impl TurnSender for Recorder {
    fn send(&self, text: String) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
        self.0.lock().unwrap().push(text);
        Box::pin(async { Ok(()) })
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

/// Everything the test checks, under one temporary folder.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Point fd 2 at `file` until the guard drops.
struct Stderr {
    saved: i32,
}

impl Stderr {
    fn to(file: &std::fs::File) -> Stderr {
        // SAFETY: dup and dup2 on descriptors this process owns.
        let saved = unsafe { libc::dup(2) };
        assert!(saved >= 0);
        assert!(unsafe { libc::dup2(file.as_raw_fd(), 2) } >= 0);
        Stderr { saved }
    }
}

impl Drop for Stderr {
    fn drop(&mut self) {
        // SAFETY: restores the descriptor saved above.
        unsafe {
            libc::dup2(self.saved, 2);
            libc::close(self.saved);
        }
    }
}

fn main() {
    let base = std::env::temp_dir().join(format!("jesse-hostrun-env-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let scratch = Scratch(base.clone());

    // A bare "GitHub" with o/n on main.
    let seed = base.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("README.md"), "# n\n").unwrap();
    git(&seed, &["add", "README.md"]);
    git(&seed, &["commit", "-q", "-m", "first"]);
    std::fs::create_dir_all(base.join("remote/o")).unwrap();
    assert!(Command::new("git")
        .args(["clone", "-q", "--bare"])
        .arg(&seed)
        .arg(base.join("remote/o/n.git"))
        .status()
        .unwrap()
        .success());

    // The vault, the env file and the stub agent.
    let notes = base.join("notes");
    std::fs::create_dir_all(notes.join("Inbox")).unwrap();
    let env_file = base.join("o-n.env");
    std::fs::write(
        &env_file,
        format!("# test\nTEST_TOKEN={SECRET}\nexport OTHER_VALUE=\"{PLAIN}\"\n"),
    )
    .unwrap();
    let dump = base.join("agent-env.txt");
    let stub = base.join("claude-stub");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\n/bin/cat > /dev/null\n/usr/bin/env > '{}'\n\
             printf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}}'\n\
             printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Stub done.\"}}'\n\
             printf 'Stub done.\\n' > \"$JESSE_RUN_REPORT\"\n",
            dump.display()
        ),
    )
    .unwrap();
    assert!(Command::new("/bin/chmod")
        .arg("755")
        .arg(&stub)
        .status()
        .unwrap()
        .success());

    let runner = PathBuf::from(env!("CARGO_BIN_EXE_jesse-runner"));
    let mut repos = BTreeMap::new();
    repos.insert(
        "o/n".to_string(),
        RepoCapToml {
            env_file: Some(env_file.clone()),
            ..RepoCapToml::default()
        },
    );
    let mut st = HostRunSettings::from_toml(
        Some(HostRunToml {
            enabled: true,
            root: Some(base.join("root")),
            repos,
            ..HostRunToml::default()
        }),
        &base.to_string_lossy(),
        &stub.to_string_lossy(),
    );
    st.runner = runner.clone();
    st.min_free_bytes = 0;
    st.derived_data_root = base.join("derived");
    st.remote_base = format!("{}/", base.join("remote").display());
    st.label_prefix = "com.example.jesse-run.".into();

    let log_path = base.join("bridge-stderr.log");
    let log_file = std::fs::File::create(&log_path).unwrap();
    let rec = Arc::new(Recorder::default());
    let mut seen: Vec<(String, String)> = Vec::new();
    {
        let _redirect = Stderr::to(&log_file);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let svc = Arc::new(HostRuns::new(
                st,
                notes.clone(),
                Arc::new(InlineLauncher { runner }),
                Arc::new(NoPr),
            ));
            svc.attach(RunSink {
                broker: Arc::new(jesse_bridge::LockBroker::new()),
                notifier: Some(rec.clone()),
                turns: Some(rec.clone()),
            });
            let started = svc
                .start(StartRequest {
                    kind: StartKind::Free {
                        repo: "o/n".into(),
                        prompt: "Read the README.".into(),
                        slug: "envcheck".into(),
                        model: None,
                    },
                    origin: "turn".into(),
                    again: false,
                })
                .await
                .unwrap();
            assert_eq!(started.run_id, "s-envcheck");
            svc.pass(hostrun::now_ms()).await;
            let run = svc.get("s-envcheck").unwrap();
            assert_eq!(run.status, hostrun::RunStatus::Reported, "{run:?}");

            let kc = base.join("kc.yaml");
            std::fs::write(
                &kc,
                "apiVersion: v1\nkind: Config\nclusters:\n- name: c\n  cluster:\n    server: https://127.0.0.1:1\n\
                 contexts:\n- name: x\n  context:\n    cluster: c\n    user: u\ncurrent-context: x\n\
                 users:\n- name: u\n  user:\n    token: not-a-real-token\n",
            )
            .unwrap();
            let dj = jesse_bridge::devjob::DevJobs::connect(
                jesse_bridge::devjob::DevJobSettings::from_toml(
                    Some(jesse_bridge::devjob::DevJobToml {
                        enabled: true,
                        kubeconfig: Some(kc),
                        ..jesse_bridge::devjob::DevJobToml::default()
                    }),
                    "/home/u",
                ),
            )
            .await
            .unwrap();
            dj.attach_studio(svc.clone());
            for (tool, args) in [
                ("devjob_status", json!({ "job_name": "s-envcheck" })),
                ("devjob_list", json!({})),
                ("devjob_reply", json!({ "run_id": "s-envcheck", "text": "Again." })),
            ] {
                seen.push((tool.into(), dj.call(tool, args).await.unwrap().to_string()));
            }
            // The reply's round ran too (the runner read the file a second time).
            svc.pass(hostrun::now_ms()).await;
            seen.push((
                "devjob_cancel".into(),
                dj.call("devjob_cancel", json!({ "run_id": "s-envcheck" }))
                    .await
                    .unwrap()
                    .to_string(),
            ));
            seen.push((
                "devjob_close".into(),
                dj.call(
                    "devjob_close",
                    json!({ "run_id": "s-envcheck", "reason": "settled" }),
                )
                .await
                .unwrap()
                .to_string(),
            ));
            let run = svc.get("s-envcheck").unwrap();
            seen.push(("run_json".into(), hostrun::run_json(&run).to_string()));
            seen.push(("list_json".into(), svc.list_json().to_string()));
            seen.push((
                "log route".into(),
                svc.log_lines("s-envcheck", hostrun::LOG_MAX)
                    .unwrap()
                    .join("\n"),
            ));
            let dir = svc.settings.run_dir("s-envcheck");
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                if e.path().is_file() {
                    seen.push((
                        e.file_name().to_string_lossy().into_owned(),
                        String::from_utf8_lossy(&std::fs::read(e.path()).unwrap()).into_owned(),
                    ));
                }
            }
            for e in std::fs::read_dir(notes.join("Inbox")).unwrap().flatten() {
                seen.push((
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read_to_string(e.path()).unwrap(),
                ));
            }
        });
    }
    seen.push((
        "the bridge's stderr".into(),
        std::fs::read_to_string(&log_path).unwrap(),
    ));
    seen.push(("pushes and turns".into(), rec.0.lock().unwrap().join("\n")));

    // The agent got both variables.
    let env = std::fs::read_to_string(&dump).expect("the stub agent ran");
    assert!(
        env.lines().any(|l| l == format!("TEST_TOKEN={SECRET}")),
        "{env}"
    );
    assert!(
        env.lines().any(|l| l == format!("OTHER_VALUE={PLAIN}")),
        "{env}"
    );
    assert!(env.lines().any(|l| l == "JESSE_RUN_ID=s-envcheck"), "{env}");

    // And nothing else did.
    let names: Vec<&str> = seen.iter().map(|(n, _)| n.as_str()).collect();
    for must in [
        "run.json",
        "job.plist",
        "prompt.md",
        "stream.jsonl",
        "exit.json",
        "runner.log",
        "devjob_status",
        "devjob_list",
        "devjob_reply",
        "devjob_cancel",
        "devjob_close",
        "run_json",
        "list_json",
        "the bridge's stderr",
    ] {
        assert!(names.contains(&must), "{must} was not read: {names:?}");
    }
    assert!(
        seen.iter()
            .any(|(n, t)| n == "the bridge's stderr" && t.contains("hostrun CREATE s-envcheck")),
        "the bridge's log was captured"
    );
    assert!(
        seen.iter()
            .any(|(n, t)| n == "runner.log" && t.contains("jesse-runner: s-envcheck round")),
        "the runner's log was captured"
    );
    for (what, text) in &seen {
        for v in [SECRET, PLAIN] {
            assert!(!text.contains(v), "{what} carries an env file value");
        }
    }
    drop(scratch);
    println!("hostrun_env_file: ok ({} places checked)", seen.len());
}
