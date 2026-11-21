//! A Studio run cannot push AI attribution, end to end.
//!
//! The real `jesse-runner` runs one round of a stub agent (a shell script standing in for
//! `claude -p`) for a run the real [`HostRuns`] created, launched, watched and reported, with
//! `attribution_dir` set as the bridge sets it from its state dir. The stub:
//!
//!   * records its environment and argv: `GIT_CONFIG_*` points git at the bridge's hooks, after
//!     the entries the repository's env file brings, and `--settings` names the bridge's file;
//!   * commits with a `Co-Authored-By: Claude` trailer (refused by `commit-msg`), commits it
//!     again with `--no-verify`, pushes (refused by `pre-push`) and pushes with `--no-verify`.
//!
//! The post-run check then reads the pushed branch back through the forge and the run ends
//! `needs_you`, its Inbox report and its push leading with `ATTRIBUTION:`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use jesse_bridge::attribution::{self, CommitInfo, GitHubView, PullText};
use jesse_bridge::devjob::Notifier;
use jesse_bridge::hostrun::sys::{Forge, Launcher, PrState};
use jesse_bridge::hostrun::{
    self, HostRunSettings, HostRunToml, HostRuns, RepoCapToml, RunSink, StartKind, StartRequest,
    TurnSender,
};

/// Runs the round synchronously: `bootstrap` is the runner's whole life, with the variables
/// the bridge put in the job's plist.
struct InlineLauncher {
    runner: PathBuf,
    attribution: PathBuf,
}

impl Launcher for InlineLauncher {
    fn bootstrap(&self, plist: &Path, _label: &str) -> Result<(), String> {
        let dir = plist.parent().ok_or("no run directory")?;
        let text = std::fs::read_to_string(plist).map_err(|e| e.to_string())?;
        assert!(
            text.contains(hostrun::runner::ATTRIBUTION_ENV)
                && text.contains(&*self.attribution.to_string_lossy()),
            "the job's plist hands the runner the attribution dir: {text}"
        );
        let log =
            std::fs::File::create(dir.join(hostrun::RUNNER_LOG)).map_err(|e| e.to_string())?;
        let status = Command::new(&self.runner)
            .arg(dir)
            .arg("--no-bootout")
            .env(hostrun::runner::ATTRIBUTION_ENV, &self.attribution)
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

/// "GitHub", answered from the local bare remote the run pushes to.
struct LocalGitHub {
    remote: PathBuf,
}

impl Forge for LocalGitHub {
    fn pr_state(&self, _repo: &str, _branch: &str) -> Option<PrState> {
        None
    }
    fn github(&self) -> Option<&dyn GitHubView> {
        Some(self)
    }
}

impl GitHubView for LocalGitHub {
    fn pulls_for_branch(&self, _repo: &str, _branch: &str) -> Option<Vec<PullText>> {
        Some(Vec::new())
    }
    fn commits(&self, _repo: &str, base: &str, head: &str) -> Option<Vec<CommitInfo>> {
        attribution::local_commits(&self.remote, &format!("{base}..{head}"))
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
        self.0
            .lock()
            .unwrap()
            .push(format!("{title} | {body} | {job}"));
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
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_CONFIG_COUNT")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const TRAILER: &str = "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>";

#[test]
fn a_studio_run_cannot_push_attribution_and_one_that_tries_needs_you() {
    let base = std::env::temp_dir().join(format!("jesse-hostrun-attr-{}", std::process::id()));
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
    let remote = base.join("remote/o/n.git");
    assert!(Command::new("git")
        .args(["clone", "-q", "--bare"])
        .arg(&seed)
        .arg(&remote)
        .status()
        .unwrap()
        .success());

    // The env file: git identity and a hermetic config, through GIT_CONFIG_* of its own.
    let env_file = base.join("o-n.env");
    std::fs::write(
        &env_file,
        "GIT_CONFIG_NOSYSTEM=1\nGIT_CONFIG_GLOBAL=/dev/null\nGIT_CONFIG_COUNT=2\n\
         GIT_CONFIG_KEY_0=user.name\nGIT_CONFIG_VALUE_0=Agent\n\
         GIT_CONFIG_KEY_1=user.email\nGIT_CONFIG_VALUE_1=agent@example.com\n",
    )
    .unwrap();

    let notes = base.join("notes");
    std::fs::create_dir_all(notes.join("Inbox")).unwrap();
    let out = base.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let stub = base.join("claude-stub");
    std::fs::write(
        &stub,
        format!(
            r#"#!/bin/sh
/bin/cat > /dev/null
O='{out}'
/usr/bin/env > "$O/env"
printf '%s\n' "$@" > "$O/argv"
echo change > change.txt
git add change.txt
git commit -q -m 'Agent change' -m '{TRAILER}' 2>> "$O/stderr"; echo "commit=$?" >> "$O/results"
git commit -q --no-verify -m 'Agent change' -m '{TRAILER}'; echo "bypass=$?" >> "$O/results"
git push -q origin HEAD 2>> "$O/stderr"; echo "push=$?" >> "$O/results"
git push -q --no-verify origin HEAD 2>> "$O/stderr"; echo "forced=$?" >> "$O/results"
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"s"}}'
printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"Pushed."}}'
printf 'Pushed the change.\n' > "$JESSE_RUN_REPORT"
"#,
            out = out.display()
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
    let attribution_dir = base.join("state").join(attribution::DIR);
    let mut repos = std::collections::BTreeMap::new();
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
    st.attribution_dir = Some(attribution_dir.clone());

    let rec = Arc::new(Recorder::default());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (run, inbox) = rt.block_on(async {
        let svc = Arc::new(HostRuns::new(
            st,
            notes.clone(),
            Arc::new(InlineLauncher {
                runner,
                attribution: attribution_dir.clone(),
            }),
            Arc::new(LocalGitHub {
                remote: remote.clone(),
            }),
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
                    prompt: "Change something.".into(),
                    slug: "attr".into(),
                    model: None,
                },
                origin: "turn".into(),
                again: false,
                reviewer: None,
                review_focus: None,
            })
            .await
            .unwrap();
        svc.pass(hostrun::now_ms()).await;
        let run = svc.get(&started.run_id).unwrap();
        let mut inbox = String::new();
        for e in std::fs::read_dir(notes.join("Inbox")).unwrap().flatten() {
            inbox.push_str(&std::fs::read_to_string(e.path()).unwrap());
        }
        (run, inbox)
    });

    // The agent's git was pointed at the hooks, after the env file's two entries.
    let env = std::fs::read_to_string(out.join("env")).expect("the stub agent ran");
    let hooks = attribution_dir.join(attribution::HOOKS_DIR);
    for want in [
        "GIT_CONFIG_COUNT=3".to_string(),
        "GIT_CONFIG_KEY_0=user.name".to_string(),
        "GIT_CONFIG_KEY_2=core.hooksPath".to_string(),
        format!("GIT_CONFIG_VALUE_2={}", hooks.display()),
    ] {
        assert!(env.lines().any(|l| l == want), "{want} missing:\n{env}");
    }
    assert!(
        !env.contains(hostrun::runner::ATTRIBUTION_ENV),
        "the runner's own variable stays the runner's"
    );
    // Claude Code was handed the settings file, which the runner wrote.
    let argv = std::fs::read_to_string(out.join("argv")).unwrap();
    let settings = attribution_dir.join(attribution::SETTINGS_FILE);
    assert!(
        argv.contains(&format!("--settings\n{}\n", settings.display())),
        "{argv}"
    );
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert_eq!(v, attribution::settings_value());

    // commit-msg refused the trailer; pre-push refused the bypassed commit.
    let results = std::fs::read_to_string(out.join("results")).unwrap();
    let stderr = std::fs::read_to_string(out.join("stderr")).unwrap();
    assert!(results.contains("commit=1"), "{results}\n{stderr}");
    assert!(results.contains("bypass=0"), "{results}\n{stderr}");
    assert!(!results.contains("push=0"), "{results}\n{stderr}");
    assert!(results.contains("forced=0"), "{results}\n{stderr}");
    assert!(stderr.contains("commit rejected"), "{stderr}");
    assert!(stderr.contains("push rejected"), "{stderr}");

    // And the post-run check caught what --no-verify let through.
    assert_eq!(run.status, hostrun::RunStatus::NeedsYou, "{run:?}");
    let body = inbox.split("\n---\n\n").nth(1).unwrap_or("");
    assert!(body.starts_with("ATTRIBUTION: "), "{inbox}");
    assert!(
        body.contains("Agent change") && body.contains(TRAILER),
        "{inbox}"
    );
    let pushes = rec.0.lock().unwrap().join("\n");
    assert!(pushes.contains("needs you | ATTRIBUTION:"), "{pushes}");
    drop(scratch);
}
