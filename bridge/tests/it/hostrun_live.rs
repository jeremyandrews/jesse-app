//! THE OWNERSHIP TEST for Studio runs (`[hostrun]`): a real run through launchd, with a stub
//! `claude`. Ignored by default: it starts launchd jobs in the user's GUI domain and takes
//! minutes. Run it on the Studio, in three steps, each its own process:
//!
//!   cargo build --bin jesse-runner
//!   cargo test --test hostrun_live -- --ignored --exact ownership_start   # starts, then exits
//!   # ... at least three minutes later ...
//!   cargo test --test hostrun_live -- --ignored --exact ownership_check   # a second process
//!   cargo test --test hostrun_live -- --ignored --exact cancel_escalates_to_sigkill
//!
//! `JESSE_HOSTRUN_LIVE_DIR` names the scratch folder (default `$TMPDIR/jesse-hostrun-live`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jesse_bridge::hostrun::{
    read_exit, read_run, HostRunSettings, HostRunToml, HostRuns, RunSink, RunStatus, StartKind,
    StartRequest,
};

fn live_dir() -> PathBuf {
    std::env::var("JESSE_HOSTRUN_LIVE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("jesse-hostrun-live"))
}

fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn runner_bin() -> PathBuf {
    // `JESSE_RUNNER_BIN` names a runner built elsewhere (a release build, or a `CARGO_TARGET_DIR`
    // outside the crate); `jesse-hook` must sit beside it for the steer hook.
    let p = std::env::var("JESSE_RUNNER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/jesse-runner")
        });
    assert!(
        p.is_file(),
        "build it first: cargo build --bin jesse-runner"
    );
    p
}

// The one way a test writes a script it will spawn (the ETXTBSY race is described in
// `src/testutil/executable.rs`, which `tests/common` loads once for the whole binary).
use crate::common::executable;

fn write_stub(path: &Path, body: &str) {
    executable::write_executable(path, body);
}

/// A stub `claude`: three minutes of stream events, then the report and exit 0. It records its
/// argv and its environment beside the report.
const STUB: &str = r#"#!/bin/sh
dir=$(dirname "$JESSE_RUN_REPORT")
echo "$@" > "$dir/stub-args.txt"
env | sort > "$dir/stub-env.txt"
cat > "$dir/stub-stdin.txt"
i=0
while [ $i -lt 18 ]; do
  echo '{"type":"assistant","message":{"content":[{"type":"text","text":"tick '$i'"}]}}'
  i=$((i+1))
  sleep 10
done
printf 'Stub report line one.\nStub report line two.\n' > "$JESSE_RUN_REPORT"
echo '{"type":"result","subtype":"success","result":"stub done"}'
exit 0
"#;

/// A stub that ignores SIGTERM and never finishes.
const STUBBORN: &str = r#"#!/bin/sh
trap '' TERM
cat > /dev/null
while true; do
  echo '{"type":"assistant","message":{"content":[{"type":"text","text":"still here"}]}}'
  sleep 1
done
"#;

fn setup(base: &Path, stub: &str) -> (HostRunSettings, PathBuf) {
    std::fs::create_dir_all(base.join("notes/Inbox")).unwrap();
    let seed = base.join("seed");
    if !seed.join(".git").exists() {
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "-q", "-b", "main"]);
        std::fs::write(seed.join("README.md"), "# n\n").unwrap();
        git(&seed, &["add", "README.md"]);
        git(&seed, &["commit", "-q", "-m", "first"]);
        std::fs::create_dir_all(base.join("remote/o")).unwrap();
        let ok = std::process::Command::new("git")
            .args(["clone", "-q", "--bare"])
            .arg(&seed)
            .arg(base.join("remote/o/n.git"))
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }
    let stub_path = base.join(stub);
    write_stub(
        &stub_path,
        if stub.contains("stubborn") {
            STUBBORN
        } else {
            STUB
        },
    );
    let mut st = HostRunSettings::from_toml(
        Some(HostRunToml {
            enabled: true,
            root: Some(base.join("root")),
            ..HostRunToml::default()
        }),
        &std::env::var("HOME").unwrap(),
        &stub_path.to_string_lossy(),
    );
    st.runner = runner_bin();
    st.min_free_bytes = 0;
    st.derived_data_root = base.join("derived");
    st.remote_base = format!("{}/", base.join("remote").display());
    (st, base.join("notes"))
}

fn service(st: HostRunSettings, notes: PathBuf) -> Arc<HostRuns> {
    let svc = Arc::new(HostRuns::production(st, notes));
    svc.attach(RunSink {
        broker: Arc::new(jesse_bridge::LockBroker::new()),
        notifier: None,
        turns: None,
    });
    svc
}

fn free(slug: &str) -> StartRequest {
    StartRequest {
        kind: StartKind::Free {
            repo: "o/n".into(),
            prompt: "Read the README. Write a two line report. Make no changes.".into(),
            slug: slug.into(),
            model: None,
        },
        origin: "turn".into(),
        again: false,
        reviewer: None,
        review_focus: None,
        priority: None,
    }
}

#[tokio::test]
#[ignore = "starts a launchd job; run on the Studio, see the module doc"]
async fn ownership_start() {
    let base = live_dir();
    let _ = std::fs::remove_dir_all(&base);
    let (st, notes) = setup(&base, "stub-claude");
    let svc = service(st, notes);
    let started = svc.start(free("ownership")).await.unwrap();
    assert_eq!(started.status, RunStatus::Running, "{started:?}");
    let dir = svc.settings.run_dir(&started.run_id);
    // The runner is up and the agent has started streaming, under launchd.
    for _ in 0..30 {
        if std::fs::metadata(dir.join("stream.jsonl")).is_ok_and(|m| m.len() > 0) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let rec = read_run(&dir).unwrap();
    assert!(rec.session_id.is_some() && rec.pgid.is_some(), "{rec:?}");
    assert!(
        read_exit(&dir).is_none(),
        "still running when this process exits"
    );
    println!(
        "STARTED {} in {} pid group {:?}",
        rec.run_id,
        dir.display(),
        rec.pgid
    );
    // This process now exits; the run must not.
}

#[tokio::test]
#[ignore = "checks the run ownership_start began; run at least three minutes later"]
async fn ownership_check() {
    let base = live_dir();
    let (st, notes) = setup(&base, "stub-claude");
    let svc = service(st, notes.clone());
    let dir = svc.settings.run_dir("s-ownership");
    let exit =
        read_exit(&dir).expect("exit.json: the run finished without the process that started it");
    assert_eq!(exit.code, Some(0), "{exit:?}");
    let report = std::fs::read_to_string(dir.join("report.md")).unwrap();
    assert_eq!(report, "Stub report line one.\nStub report line two.\n");
    svc.pass(jesse_bridge::hostrun::now_ms()).await;
    let rec = read_run(&dir).unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    let inbox = rec.round().unwrap().report_path.clone().unwrap();
    let text = std::fs::read_to_string(notes.join(&inbox)).unwrap();
    assert!(text.ends_with("---\n\nStub report line one.\nStub report line two.\n"));
    // The agent saw the allowlist only.
    let env = std::fs::read_to_string(dir.join("stub-env.txt")).unwrap();
    for line in env.lines() {
        let k = line.split('=').next().unwrap();
        assert!(
            jesse_bridge::hostrun::ENV_ALLOWLIST.contains(&k)
                || [
                    "JESSE_RUN_ID",
                    "JESSE_RUN_REPORT",
                    "CARGO_TARGET_DIR",
                    "PWD",
                    "SHLVL",
                    "_",
                    "OLDPWD"
                ]
                .contains(&k),
            "{k} reached the agent"
        );
    }
    let args = std::fs::read_to_string(dir.join("stub-args.txt")).unwrap();
    println!("ARGS {args}");
    println!(
        "ENV KEYS {:?}",
        env.lines()
            .map(|l| l.split('=').next().unwrap())
            .collect::<Vec<_>>()
    );
    println!("CHECKED {} {:?} report at {inbox}", rec.run_id, exit);
}

#[tokio::test]
#[ignore = "starts a launchd job and waits 40 seconds; run on the Studio"]
async fn cancel_escalates_to_sigkill() {
    let base = live_dir().join("cancel");
    let _ = std::fs::remove_dir_all(&base);
    let marker = format!("stubborn-claude-{}", std::process::id());
    let (st, notes) = setup(&base, &marker);
    let svc = service(st, notes);
    let started = svc.start(free("stubborn")).await.unwrap();
    let dir = svc.settings.run_dir(&started.run_id);
    let mut pgid = None;
    for _ in 0..30 {
        pgid = read_run(&dir).unwrap().pgid;
        if pgid.is_some() && std::fs::metadata(dir.join("stream.jsonl")).is_ok_and(|m| m.len() > 0)
        {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(pgid.is_some());
    svc.cancel(&started.run_id).unwrap();
    // SIGTERM is ignored: still alive after five seconds.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(read_exit(&dir).is_none(), "the stub ignores SIGTERM");
    // The watcher escalates after the grace.
    tokio::time::sleep(Duration::from_secs(27)).await;
    for _ in 0..20 {
        svc.pass(jesse_bridge::hostrun::now_ms()).await;
        if read_run(&dir).unwrap().status == RunStatus::Cancelled {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let rec = read_run(&dir).unwrap();
    assert_eq!(rec.status, RunStatus::Cancelled, "{rec:?}");
    let exit = read_exit(&dir).unwrap();
    assert_eq!(exit.signal, Some(9), "{exit:?}");
    let left = std::process::Command::new("pgrep")
        .args(["-f", &marker])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&left.stdout).trim().is_empty(),
        "no child process is left"
    );
    println!("CANCELLED {} {:?}", rec.run_id, exit);
}

/// A launcher that starts nothing: the smoke test runs the runner itself, outside the bridge.
struct Manual;

impl jesse_bridge::hostrun::sys::Launcher for Manual {
    fn bootstrap(&self, _plist: &Path, _label: &str) -> Result<(), String> {
        Ok(())
    }
    fn bootout(&self, _label: &str) -> Result<(), String> {
        Ok(())
    }
    fn is_running(&self, _label: &str) -> bool {
        true
    }
    fn loaded_labels(&self, _prefix: &str) -> Vec<String> {
        Vec::new()
    }
    fn plist_of(&self, _label: &str) -> Option<PathBuf> {
        None
    }
}

fn run_runner(dir: &Path) {
    let ok = std::process::Command::new(runner_bin())
        .arg(dir)
        .arg("--no-bootout")
        .stderr(std::fs::File::create(dir.join("runner.log")).unwrap())
        .status()
        .unwrap()
        .success();
    assert!(ok);
}

/// THE REAL CLAUDE CODE SMOKE: the runner, outside the bridge and outside launchd, runs the
/// real `claude` on a scratch repository, then one reply round resumes the same session.
#[tokio::test]
#[ignore = "runs the real claude CLI; spends agent credits"]
async fn smoke_real_claude() {
    let base = live_dir().join("smoke");
    let _ = std::fs::remove_dir_all(&base);
    let (mut st, notes) = setup(&base, "unused-stub");
    st.claude = "claude".into();
    let svc = Arc::new(HostRuns::new(
        st,
        notes,
        Arc::new(Manual),
        Arc::new(jesse_bridge::hostrun::sys::NoForge),
    ));
    svc.attach(RunSink {
        broker: Arc::new(jesse_bridge::LockBroker::new()),
        notifier: None,
        turns: None,
    });
    let started = svc.start(free("smoke")).await.unwrap();
    let dir = svc.settings.run_dir(&started.run_id);
    run_runner(&dir);
    let rec = read_run(&dir).unwrap();
    let session = rec.session_id.clone().unwrap();
    let report = std::fs::read_to_string(dir.join("report.md")).unwrap_or_default();
    println!("SESSION {session}");
    println!("REPORT1 <<<{report}>>>");
    println!("EXIT1 {:?}", read_exit(&dir));
    svc.pass(jesse_bridge::hostrun::now_ms()).await;
    let rec = read_run(&dir).unwrap();
    println!("STATUS1 {:?}", rec.status);
    svc.reply_and_pump(
        &rec.run_id,
        "Follow up: in one line, what is the first line of the README? Make no changes.",
    )
    .await
    .unwrap();
    run_runner(&dir);
    let rec = read_run(&dir).unwrap();
    assert_eq!(
        rec.session_id.as_deref(),
        Some(session.as_str()),
        "the same session"
    );
    let stream = std::fs::read_to_string(dir.join("stream.jsonl")).unwrap();
    let sessions: std::collections::BTreeSet<String> = stream
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v["session_id"].as_str().map(str::to_string))
        .collect();
    println!(
        "STREAM SESSIONS {sessions:?} LINES {}",
        stream.lines().count()
    );
    println!(
        "REPORT2 <<<{}>>>",
        std::fs::read_to_string(dir.join("report.md")).unwrap_or_default()
    );
    println!(
        "RESULT2 {:?}",
        jesse_bridge::hostrun::stream::last_result(&stream)
    );
    svc.pass(jesse_bridge::hostrun::now_ms()).await;
    println!("STATUS2 {:?}", read_run(&dir).unwrap().status);
    println!(
        "RUNNER LOG {}",
        std::fs::read_to_string(dir.join("runner.log")).unwrap_or_default()
    );
}

/// THE STEER SMOKE (DJ33): the real runner and the real `claude` on a scratch repository, told
/// to run `sleep 20` three times. During the first sleep a reply is sent; it is queued as a
/// message, and the steer hook hands it over after that tool call. The stream shows what the
/// agent did with it: the `echo` it asks for, between the sleeps. Needs `jesse-hook` beside
/// the runner (`JESSE_RUNNER_BIN`).
///
///   cargo build --release --bin jesse-runner --bin jesse-hook
///   JESSE_RUNNER_BIN=<target>/release/jesse-runner \
///     cargo test --test it hostrun_live::steer_real_claude -- --ignored --nocapture
#[tokio::test]
#[ignore = "runs the real claude CLI; spends agent credits"]
async fn steer_real_claude() {
    let base = live_dir().join("steer");
    let _ = std::fs::remove_dir_all(&base);
    let (mut st, notes) = setup(&base, "unused-stub");
    st.claude = "claude".into();
    let svc = Arc::new(HostRuns::new(
        st,
        notes,
        Arc::new(Manual),
        Arc::new(jesse_bridge::hostrun::sys::NoForge),
    ));
    svc.attach(RunSink {
        broker: Arc::new(jesse_bridge::LockBroker::new()),
        notifier: None,
        turns: None,
    });
    let req = StartRequest {
        kind: StartKind::Free {
            repo: "o/n".into(),
            prompt: "Run the shell command `sleep 20` three times, one Bash tool call each, one \
                     after another. Then write a one line report. Make no changes to the \
                     repository."
                .into(),
            slug: "steer".into(),
            model: Some("haiku".into()),
        },
        ..free("steer")
    };
    let started = svc.start(req).await.unwrap();
    let dir = svc.settings.run_dir(&started.run_id);
    let d2 = dir.clone();
    let runner = std::thread::spawn(move || run_runner(&d2));
    // Wait for the first `sleep 20` to start, then send the message during it.
    let stream_has = |needle: &str| {
        std::fs::read_to_string(dir.join("stream.jsonl"))
            .unwrap_or_default()
            .contains(needle)
    };
    let mut waited = 0;
    while !stream_has("sleep 20") {
        assert!(waited < 240, "the agent never started its first sleep");
        tokio::time::sleep(Duration::from_millis(500)).await;
        waited += 1;
    }
    let out = svc
        .reply_full_and_pump(
            &started.run_id,
            "Before your next sleep, run the shell command `echo steered-DJ33`.",
        )
        .await
        .unwrap();
    println!("SENT {:?}", out.message);
    println!("DELIVERY {:?}", out.delivery());
    runner.join().unwrap();
    let rec = svc.sync_messages(&started.run_id).unwrap();
    println!("MESSAGES {:?}", rec.messages);
    let stream = std::fs::read_to_string(dir.join("stream.jsonl")).unwrap();
    for line in stream.lines() {
        if line.contains("sleep 20") || line.contains("steered-DJ33") {
            let short: String = line.chars().take(400).collect();
            println!("STREAM {short}");
        }
    }
    assert!(
        rec.messages[0].delivered_at.is_some(),
        "the hook delivered it"
    );
    let cmds = bash_commands(&stream);
    assert!(
        acted_between_sleeps(&cmds, "echo steered-DJ33"),
        "the agent ran the message's command between its first and second sleep: {cmds:?}"
    );
}

/// The `command` of every Bash tool call the agent made, in the order it made them. Only the
/// agent's own `tool_use` blocks count: the message's text also reaches the stream as the
/// hook's added context, and that is not the agent acting on it.
fn bash_commands(stream: &str) -> Vec<String> {
    stream
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["type"] == "assistant")
        .flat_map(|v| {
            v["message"]["content"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|b| b["type"] == "tool_use" && b["name"] == "Bash")
        .filter_map(|b| b["input"]["command"].as_str().map(str::to_string))
        .collect()
}

/// Whether `cmd` ran after the first `sleep 20` and before the second: the message arrived
/// during the first, so acting on it at the next tool call puts it right there.
fn acted_between_sleeps(cmds: &[String], cmd: &str) -> bool {
    let sleeps: Vec<usize> = (0..cmds.len()).filter(|&i| cmds[i] == "sleep 20").collect();
    let Some(&first) = sleeps.first() else {
        return false;
    };
    let second = sleeps.get(1).copied().unwrap_or(cmds.len());
    cmds[first + 1..second].iter().any(|c| c == cmd)
}

#[test]
fn the_live_check_counts_only_the_command_the_agent_ran_where_it_was_steered() {
    let call = |c: &str| {
        serde_json::json!({ "type": "assistant", "message": { "content": [
            { "type": "tool_use", "name": "Bash", "input": { "command": c } } ] } })
        .to_string()
    };
    let context = serde_json::json!({ "type": "system", "subtype": "hook_response",
        "output": "Message from Jeremy while you were running: run `echo steered-DJ33`." })
    .to_string();
    let stream = |lines: &[String]| lines.join("\n");
    let steered = stream(&[
        call("sleep 20"),
        context.clone(),
        call("echo steered-DJ33"),
        call("sleep 20"),
        call("sleep 20"),
    ]);
    let cmds = bash_commands(&steered);
    assert!(acted_between_sleeps(&cmds, "echo steered-DJ33"), "{cmds:?}");
    // The text arrived but the agent never ran it: the raw stream still names it.
    let ignored = stream(&[
        call("sleep 20"),
        context.clone(),
        call("sleep 20"),
        call("sleep 20"),
    ]);
    assert!(ignored.contains("echo steered-DJ33"));
    assert!(!acted_between_sleeps(
        &bash_commands(&ignored),
        "echo steered-DJ33"
    ));
    // It ran, but only after every sleep: the next round's text, not the next tool call.
    let late = stream(&[
        call("sleep 20"),
        context,
        call("sleep 20"),
        call("sleep 20"),
        call("echo steered-DJ33"),
    ]);
    assert!(!acted_between_sleeps(
        &bash_commands(&late),
        "echo steered-DJ33"
    ));
}
