//! THE REVIEW LOOP, LIVE: a reviewed Studio run on a scratch local repository, started through
//! `devjob_start` exactly as a turn would, with the real `claude` doing the work and a real
//! reviewer from the live registry reading it through its own harness. Ignored by default: it
//! spends agent and reviewer credits and takes minutes. Run it on the Studio, one reviewer per
//! process:
//!
//!   cargo build --bin jesse-runner
//!   JESSE_XR1_REVIEWER=codex-write cargo test --test hostrun_review_live -- --ignored --nocapture
//!
//! The bridge's own plist environment and `jesse.local.toml` give the registry and the keys
//! (nothing is printed from either). `JESSE_HOSTRUN_LIVE_DIR` names the scratch folder (default
//! `$TMPDIR/jesse-hostrun-live`); the run's root, its `local/xr1-scratch` repository and its
//! notes live under `<dir>/review-<reviewer>`, which the test removes first and leaves for
//! inspection.
//!
//! The prompt plants one real defect (`last_n(xs, 0)` returns every item, since `xs[-0:]` is the
//! whole list) and two constructs that look wrong and are right: `n & (n - 1) == 0`, which in
//! Python groups as `(n & (n - 1)) == 0`, and `fib`'s memo, a mutable default shared across
//! calls on purpose. What each reviewer says is its own; the test prints the verdict table the
//! answering round wrote and checks what the bridge owns: nothing sent and
//! nothing merged before the answering round, one push after it, the note archived, no leak.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jesse_bridge::gateway::InProcessServer;
use jesse_bridge::hostrun::review::ReviewStatus;
use jesse_bridge::hostrun::{
    check_reviewer, read_run, HostRunSettings, HostRunToml, HostRuns, ReviewCall, Reviewer,
    ReviewerInfo, RunSink, RunStatus,
};
use jesse_bridge::{capability_label, Config, HealthStore};
use serde_json::json;

const PLIST: &str = "Library/LaunchAgents/com.jeremy.jesse-bridge.plist";
const REPO: &str = "local/xr1-scratch";

const PROMPT: &str = concat!(
    "# XR1T: two small helpers\n\n",
    "Create `helpers.py` at the repository root with exactly this content, character for ",
    "character, and do not change it in this round, even if you think something in it is ",
    "wrong:\n\n",
    "```python\n",
    "def last_n(xs, n):\n",
    "    \"\"\"The last n items of the list xs: all of them when n >= len(xs), none when n == 0.\"\"\"\n",
    "    return xs[-n:]\n",
    "\n",
    "\n",
    "def is_power_of_two(n):\n",
    "    \"\"\"True when the integer n is a positive power of two.\"\"\"\n",
    "    return n > 0 and n & (n - 1) == 0\n",
    "\n",
    "\n",
    "def fib(n, _memo={0: 0, 1: 1}):\n",
    "    \"\"\"The n-th Fibonacci number, for 0 <= n <= 500.\"\"\"\n",
    "    if n not in _memo:\n",
    "        _memo[n] = fib(n - 1) + fib(n - 2)\n",
    "    return _memo[n]\n",
    "```\n\n",
    "Create `test_helpers.py` with `unittest` tests of `last_n([1, 2, 3], 2) == [2, 3]`, ",
    "`last_n([1, 2], 5) == [1, 2]`, `is_power_of_two(1)`, `is_power_of_two(64)`, ",
    "`not is_power_of_two(0)`, `not is_power_of_two(12)`, `fib(10) == 55` and ",
    "`fib(90) == 2880067194370816120`. Verify with ",
    "`python3 -m unittest -v` from the repository root; it must pass. Commit both files.\n",
);

#[test]
fn the_live_prompt_plants_valid_python() {
    assert!(PROMPT.contains("\n    return xs[-n:]\n"));
    assert!(PROMPT.contains("\n    return n > 0 and n & (n - 1) == 0\n"));
}

fn live_dir() -> PathBuf {
    std::env::var("JESSE_HOSTRUN_LIVE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("jesse-hostrun-live"))
}

fn runner_bin() -> PathBuf {
    let dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
    let p = dir.join("debug/jesse-runner");
    assert!(
        p.is_file(),
        "build it first: cargo build --bin jesse-runner"
    );
    p
}

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

#[derive(Default)]
struct Pushes(Mutex<Vec<(String, String)>>);

impl jesse_bridge::devjob::Notifier for Pushes {
    fn notify(
        &self,
        title: String,
        body: String,
        _job: String,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.0.lock().unwrap().push((title, body));
        Box::pin(async {})
    }
}

/// The bridge's own reviewer, minus the app state: the live registry, its health, its harnesses.
struct LiveReviewer {
    cfg: Arc<Config>,
    health: Arc<HealthStore>,
}

impl Reviewer for LiveReviewer {
    fn check(&self, id: &str) -> Result<ReviewerInfo, String> {
        check_reviewer(&self.cfg, &self.health, id).map(|(i, _)| i)
    }
    fn review(
        &self,
        call: ReviewCall,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
        let (cfg, health) = (self.cfg.clone(), self.health.clone());
        Box::pin(async move {
            let (info, cap) = check_reviewer(&cfg, &health, &call.reviewer)?;
            let m = cfg.model_registry.get(&info.id).unwrap();
            println!(
                "REVIEWER {} via harness {} at {} in {}",
                m.id,
                m.harness,
                capability_label(cap),
                call.cwd.display()
            );
            jesse_bridge::run_review_child(
                &cfg,
                m,
                cap,
                &call.prompt,
                &call.cwd,
                call.timeout.as_secs(),
            )
            .await
            .map_err(|(s, e)| format!("the reviewer failed ({s}): {e}"))
        })
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
    assert!(
        ok,
        "the runner failed; see {}",
        dir.join("runner.log").display()
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn inbox(notes: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(notes.join("Inbox"))
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the real claude CLI and a real reviewer; spends credits; see the module doc"]
async fn a_reviewed_run_live() {
    let reviewer = std::env::var("JESSE_XR1_REVIEWER").expect("set JESSE_XR1_REVIEWER");
    let home = std::env::var("HOME").unwrap();
    jesse_bridge::plistenv::load_plist_env(&format!("{home}/{PLIST}")).expect("the plist");
    let cfg = Arc::new(Config::from_env());
    let health = Arc::new(HealthStore::seeded(&cfg.model_registry));
    match check_reviewer(&cfg, &health, &reviewer) {
        Ok((i, cap)) => println!(
            "CHECK {} ({}) on {} at {}",
            i.id,
            i.label,
            i.harness,
            capability_label(cap)
        ),
        Err(e) => panic!("REVIEWER REFUSED: {e}"),
    }

    let base = live_dir().join(format!("review-{reviewer}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("notes/Inbox")).unwrap();
    let notes = base.join("notes");
    let mut st = HostRunSettings::from_toml(
        Some(HostRunToml {
            enabled: true,
            root: Some(base.join("root")),
            ..HostRunToml::default()
        }),
        &home,
        "claude",
    );
    st.runner = runner_bin();
    st.min_free_bytes = 0;
    st.derived_data_root = base.join("derived");
    // The bridge's attribution hooks and settings, as production points every round at them.
    let attribution = base.join("state/attribution");
    jesse_bridge::attribution::install_root(&attribution).unwrap();
    st.attribution_dir = Some(attribution.clone());
    std::env::set_var(jesse_bridge::hostrun::runner::ATTRIBUTION_ENV, &attribution);
    let svc = Arc::new(HostRuns::new(
        st,
        notes.clone(),
        Arc::new(Manual),
        Arc::new(jesse_bridge::hostrun::sys::NoForge),
    ));
    let pushes = Arc::new(Pushes::default());
    svc.attach(RunSink {
        broker: Arc::new(jesse_bridge::LockBroker::new()),
        notifier: Some(pushes.clone()),
        turns: None,
    });
    svc.attach_reviewer(Arc::new(LiveReviewer {
        cfg: cfg.clone(),
        health,
    }));

    // devjob_start, as a write turn calls it.
    let kc = base.join("kc.yaml");
    std::fs::write(
        &kc,
        "apiVersion: v1\nkind: Config\nclusters:\n- name: c\n  cluster:\n    server: https://127.0.0.1:1\n\
         contexts:\n- name: x\n  context:\n    cluster: c\n    user: u\ncurrent-context: x\n\
         users:\n- name: u\n  user:\n    token: not-a-real-token\n",
    )
    .unwrap();
    let dj =
        jesse_bridge::devjob::DevJobs::connect(jesse_bridge::devjob::DevJobSettings::from_toml(
            Some(jesse_bridge::devjob::DevJobToml {
                enabled: true,
                kubeconfig: Some(kc),
                ..jesse_bridge::devjob::DevJobToml::default()
            }),
            &home,
        ))
        .await
        .unwrap();
    dj.attach_studio(svc.clone());
    let args = json!({
        "host": "studio",
        "repo": REPO,
        "branch_slug": "xr1t-helpers",
        "prompt": PROMPT,
        "reviewer": reviewer,
        "review_focus": "edge cases of last_n and is_power_of_two, and state shared between calls",
    });
    println!(
        "CALL devjob_start {}",
        serde_json::to_string(&args).unwrap()
    );
    let out = dj
        .call(jesse_bridge::devjob::TOOL_START, args)
        .await
        .unwrap();
    assert_eq!(out["isError"], false, "{out}");
    let text = out["content"][0]["text"].as_str().unwrap();
    let run_id = serde_json::from_str::<serde_json::Value>(text).unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let dir = svc.settings.run_dir(&run_id);
    let bare = base.join("root/local/xr1-scratch.git");
    let main_at_start = git_out(&bare, &["rev-parse", "main"]);
    println!("RUN {run_id} in {}", dir.display());

    // Round 1: the implementing round.
    run_runner(&dir);
    svc.pass(jesse_bridge::hostrun::now_ms()).await;
    let mut waited = 0;
    loop {
        let r = read_run(&dir).unwrap();
        if r.status != RunStatus::Reviewing {
            break;
        }
        assert!(waited < 40 * 60, "the review did not settle");
        tokio::time::sleep(Duration::from_secs(5)).await;
        waited += 5;
    }
    let rec = read_run(&dir).unwrap();
    let v = rec.reviews.first().expect("one review").clone();
    println!(
        "AFTER ROUND 1: status {:?}, review {:?}, error {:?}",
        rec.status, v.status, v.error
    );
    println!("  pushes so far: {:?}", pushes.0.lock().unwrap());
    println!("  Inbox: {:?}", inbox(&notes));
    println!(
        "  main: {} (at start {main_at_start})",
        git_out(&bare, &["rev-parse", "main"])
    );
    assert!(
        pushes.0.lock().unwrap().is_empty(),
        "nothing sent before the answering round"
    );
    assert_eq!(
        git_out(&bare, &["rev-parse", "main"]),
        main_at_start,
        "nothing merged before the answering round"
    );
    assert!(
        inbox(&notes).iter().all(|f| f.contains("-review-")),
        "no round report before the answering round"
    );
    for (i, f) in v.findings.iter().enumerate() {
        println!(
            "  FINDING {} {:?} {}:{}: {}",
            f.id, f.severity, f.file, f.line, f.claim
        );
        let _ = i;
    }
    if let Some(p) = &v.path {
        println!("  NOTE {p}");
    }
    assert_eq!(
        rec.status,
        RunStatus::Running,
        "the hand off is the next round"
    );

    // Round 2: the answering round, in the same session.
    let session = rec.session_id.clone();
    run_runner(&dir);
    svc.pass(jesse_bridge::hostrun::now_ms()).await;
    let rec = read_run(&dir).unwrap();
    assert_eq!(rec.session_id, session, "the same session answers");
    let v = &rec.reviews[0];
    println!(
        "AFTER ROUND 2: status {:?}, review {:?}, leaks {:?}",
        rec.status, v.status, v.leaks
    );
    let report_rel = rec.rounds.last().unwrap().report_path.clone().unwrap();
    let report = std::fs::read_to_string(notes.join(&report_rel)).unwrap();
    println!("REPORT {report_rel} <<<\n{report}\n>>>");
    println!("PUSHES {:?}", pushes.0.lock().unwrap());
    println!("Inbox: {:?}", inbox(&notes));
    println!(
        "main: {} log:\n{}",
        git_out(&bare, &["rev-parse", "main"]),
        git_out(&bare, &["log", "--format=%h %s%n%b", "main"])
    );
    println!(
        "helpers.py on main:\n{}",
        git_out(&bare, &["show", "main:helpers.py"])
    );
    assert_eq!(
        pushes.0.lock().unwrap().len(),
        1,
        "one push, at the answering round"
    );
    if v.status == ReviewStatus::Answered {
        assert!(v.leaks.is_empty(), "no leak: {:?}", v.leaks);
        let archived = notes
            .join("Inbox/archive")
            .join(v.path.as_deref().unwrap().rsplit('/').next().unwrap());
        assert!(archived.is_file(), "the note is archived");
    }
}
