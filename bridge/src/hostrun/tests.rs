use super::*;
use std::collections::HashSet;

use crate::devjob::{DevJobArgs, DevJobSpec};

// ---- FIXTURES ------------------------------------------------------------------------

/// A scratch folder: `root` (the runs), `notes` (the vault), `remote` (the bare "GitHub"),
/// `derived` (a DerivedData folder).
struct Scratch {
    base: PathBuf,
}

impl Scratch {
    fn new() -> Scratch {
        let base = std::env::temp_dir().join(format!("jesse-hostrun-{}", random_hex()));
        std::fs::create_dir_all(base.join("notes/Inbox")).unwrap();
        std::fs::create_dir_all(base.join("derived")).unwrap();
        Scratch { base }
    }
    fn root(&self) -> PathBuf {
        self.base.join("root")
    }
    fn notes(&self) -> PathBuf {
        self.base.join("notes")
    }
    fn derived(&self) -> PathBuf {
        self.base.join("derived")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn sh(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A bare repository `o/n` under `<base>/remote/`, with one commit on `main`.
fn remote_fixture(s: &Scratch) {
    let seed = s.base.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    sh(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("README.md"), "# n\n").unwrap();
    sh(&seed, &["add", "README.md"]);
    sh(&seed, &["commit", "-q", "-m", "first"]);
    let remote = s.base.join("remote/o");
    std::fs::create_dir_all(&remote).unwrap();
    let out = std::process::Command::new("git")
        .args(["clone", "-q", "--bare"])
        .arg(&seed)
        .arg(remote.join("n.git"))
        .output()
        .unwrap();
    assert!(out.status.success());
}

const NOTE: &str = "---\ngroup: tag1\nstate: active\nupdated: 2026-11-09\n---\n# Gd\n\n**Now:** testing\n\n## Drafts\n\
- [ ] **GD5** Run the thing on the Studio. [[todo-list/Projects/drafts/gd5-prompt]]\n\
- [x] **GD6** Checked one. [[todo-list/Projects/drafts/gd6-prompt]]\n\
- [ ] **GD7** Archived one. [[todo-list/Projects/drafts/archive/gd7-prompt]]\n\
- [ ] **GD8** No prompt section. [[todo-list/Projects/drafts/gd8-prompt]]\n\
- [ ] **GD9** No repository. [[todo-list/Projects/drafts/gd9-prompt]]\n\
- [ ] **GD10** No host. [[todo-list/Projects/drafts/gd10-prompt]]\n\
- [ ] **GD11** Wrong id. [[todo-list/Projects/drafts/gd11-prompt]]\n\
- [ ] **GD12** On the cluster. [[todo-list/Projects/drafts/gd12-prompt]]\n\
### Done\n";

const SECTION: &str = "\nDo \"the thing\": carefully.\n\n```sh\ncargo test\n```\n\n";

fn draft(id: &str, extra: &str, body: &str) -> String {
    format!("---\nstrand: Gd\nstrand-id: {id}\n{extra}---\n# {id}\n\n{body}")
}

const STUDIO: &str = "devjob-repo: o/n\ndevjob-host: studio\n";

fn vault_fixture(s: &Scratch) {
    let n = s.notes();
    std::fs::create_dir_all(n.join("Strands")).unwrap();
    std::fs::create_dir_all(n.join("Projects/drafts/archive")).unwrap();
    std::fs::write(n.join("Strands/Gd.md"), NOTE).unwrap();
    let body = format!("Some context.\n\n## PROMPT\n{SECTION}---\n- [ ] archived\n- [ ] done\n");
    let d = n.join("Projects/drafts");
    std::fs::write(d.join("gd5-prompt.md"), draft("GD5", STUDIO, &body)).unwrap();
    std::fs::write(d.join("gd6-prompt.md"), draft("GD6", STUDIO, &body)).unwrap();
    std::fs::write(d.join("archive/gd7-prompt.md"), draft("GD7", STUDIO, &body)).unwrap();
    std::fs::write(
        d.join("gd8-prompt.md"),
        draft("GD8", STUDIO, "No heading here.\n"),
    )
    .unwrap();
    std::fs::write(
        d.join("gd9-prompt.md"),
        draft("GD9", "devjob-host: studio\n", &body),
    )
    .unwrap();
    std::fs::write(
        d.join("gd10-prompt.md"),
        draft("GD10", "devjob-repo: o/n\n", &body),
    )
    .unwrap();
    std::fs::write(d.join("gd11-prompt.md"), draft("GD99", STUDIO, &body)).unwrap();
    std::fs::write(
        d.join("gd12-prompt.md"),
        draft("GD12", "devjob-repo: o/n\ndevjob-host: cluster\n", &body),
    )
    .unwrap();
}

#[derive(Default)]
struct FakeLauncher {
    running: Mutex<HashSet<String>>,
    bootstrapped: Mutex<Vec<String>>,
    booted_out: Mutex<Vec<String>>,
    loaded_extra: Mutex<Vec<String>>,
    plists: Mutex<BTreeMap<String, PathBuf>>,
}

impl Launcher for FakeLauncher {
    fn bootstrap(&self, plist: &Path, label: &str) -> Result<(), String> {
        assert!(plist.is_file());
        lock(&self.plists).insert(label.to_string(), plist.to_path_buf());
        lock(&self.running).insert(label.to_string());
        lock(&self.bootstrapped).push(label.to_string());
        Ok(())
    }
    fn bootout(&self, label: &str) -> Result<(), String> {
        if lock(&self.running).remove(label)
            || lock(&self.loaded_extra).contains(&label.to_string())
        {
            lock(&self.booted_out).push(label.to_string());
        }
        lock(&self.loaded_extra).retain(|l| l != label);
        Ok(())
    }
    fn is_running(&self, label: &str) -> bool {
        lock(&self.running).contains(label)
    }
    fn plist_of(&self, label: &str) -> Option<PathBuf> {
        lock(&self.plists).get(label).cloned()
    }
    fn loaded_labels(&self, prefix: &str) -> Vec<String> {
        let mut v: Vec<String> = lock(&self.running)
            .iter()
            .filter(|l| l.starts_with(prefix))
            .cloned()
            .collect();
        v.extend(lock(&self.loaded_extra).iter().cloned());
        v
    }
}

impl FakeLauncher {
    /// The runner has gone (its job ended).
    fn gone(&self, run_id: &str) {
        lock(&self.running).remove(&format!("{}{run_id}", sys::DEFAULT_LABEL_PREFIX));
    }
}

#[derive(Default)]
struct FakeForge {
    states: Mutex<BTreeMap<String, PrState>>,
}

impl Forge for FakeForge {
    fn pr_state(&self, _repo: &str, branch: &str) -> Option<PrState> {
        lock(&self.states).get(branch).copied()
    }
}

#[derive(Default)]
struct Recorder {
    pushes: Mutex<Vec<(String, String, String)>>,
    turns: Mutex<Vec<String>>,
}

impl Notifier for Recorder {
    fn notify(
        &self,
        title: String,
        body: String,
        job: String,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        lock(&self.pushes).push((title, body, job));
        Box::pin(async {})
    }
}

impl TurnSender for Recorder {
    fn send(&self, text: String) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
        lock(&self.turns).push(text);
        Box::pin(async { Ok(()) })
    }
}

fn settings(s: &Scratch) -> HostRunSettings {
    let mut st = HostRunSettings::from_toml(
        Some(HostRunToml {
            enabled: true,
            root: Some(s.root()),
            ..HostRunToml::default()
        }),
        &s.base.to_string_lossy(),
        "/bin/echo",
    );
    st.runner = PathBuf::from("/bin/sh");
    st.min_free_bytes = 0;
    st.derived_data_root = s.derived();
    st.remote_base = format!("{}/", s.base.join("remote").to_string_lossy());
    st.label_prefix = sys::DEFAULT_LABEL_PREFIX.to_string();
    st
}

fn test_label(run_id: &str) -> String {
    format!("{}{run_id}", sys::DEFAULT_LABEL_PREFIX)
}

struct Rig {
    svc: Arc<HostRuns>,
    launcher: Arc<FakeLauncher>,
    forge: Arc<FakeForge>,
    rec: Arc<Recorder>,
}

fn rig_with(s: &Scratch, st: HostRunSettings) -> Rig {
    let launcher = Arc::new(FakeLauncher::default());
    let forge = Arc::new(FakeForge::default());
    let rec = Arc::new(Recorder::default());
    let svc = Arc::new(HostRuns::new(
        st,
        s.notes(),
        launcher.clone(),
        forge.clone(),
    ));
    svc.attach(RunSink {
        broker: Arc::new(LockBroker::new()),
        notifier: Some(rec.clone()),
        turns: Some(rec.clone()),
    });
    Rig {
        svc,
        launcher,
        forge,
        rec,
    }
}

fn rig(s: &Scratch) -> Rig {
    rig_with(s, settings(s))
}

fn item(id: &str) -> StartRequest {
    StartRequest {
        kind: StartKind::Item {
            strand: "Gd".into(),
            item_id: id.into(),
        },
        origin: "app".into(),
        again: false,
    }
}

fn free(slug: &str) -> StartRequest {
    StartRequest {
        kind: StartKind::Free {
            repo: "o/n".into(),
            prompt: "Read the README.".into(),
            slug: slug.into(),
            model: None,
        },
        origin: "turn".into(),
        again: false,
    }
}

/// The runner's part, faked: the session, the stream, the report and the exit.
fn finish_round(r: &Rig, run_id: &str, code: i32, report: Option<&str>, stream: &str) {
    let dir = r.svc.settings.run_dir(run_id);
    let rec = read_run(&dir).unwrap();
    let n = rec.round().unwrap().n;
    if rec.session_id.is_none() {
        update_run(&dir, |x| {
            x.session_id = Some("11111111-2222-3333-4444-555555555555".into())
        })
        .unwrap();
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(STREAM))
        .unwrap();
    std::io::Write::write_all(&mut f, stream.as_bytes()).unwrap();
    if let Some(t) = report {
        std::fs::write(dir.join(REPORT_MD), t).unwrap();
    }
    write_exit(
        &dir,
        &ExitInfo {
            round: n,
            code: Some(code),
            signal: None,
            ended_at: rfc(now_ms()),
        },
    )
    .unwrap();
    r.launcher.gone(run_id);
}

fn inbox_files(s: &Scratch) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(s.notes().join("Inbox"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

const STREAM_TEXT: &str = concat!(
    r#"{"type":"system","subtype":"init","session_id":"11111111-2222-3333-4444-555555555555"}"#,
    "\n",
    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Reading the README."},{"type":"tool_use","name":"Bash","input":{"command":"cargo test\nmore"}},{"type":"tool_use","name":"Edit","input":{"file_path":"/x/runs/GD5/worktree/bridge/src/x.rs"}}]}}"#,
    "\n",
    r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"SECRET OUTPUT BODY"}]}}"#,
    "\n",
    r#"{"type":"result","subtype":"success","result":"Final answer from the result event.","is_error":false}"#,
    "\n",
);

// ---- 1. IDS, ELIGIBILITY, THE PROMPT, THE ARGUMENTS ------------------------------------

#[test]
fn run_ids_are_the_item_then_b_then_c_and_never_reused() {
    let s = Scratch::new();
    let runs = s.root().join("runs");
    assert_eq!(allocate_run_id(&runs, "GD5").unwrap(), "GD5");
    assert_eq!(allocate_run_id(&runs, "GD5").unwrap(), "GD5b");
    // Both "reported": the directories stay, so the next is c, and GD5 never comes back.
    for id in ["GD5", "GD5b"] {
        std::fs::write(runs.join(id).join(RUN_JSON), "{}").unwrap();
    }
    assert_eq!(allocate_run_id(&runs, "GD5").unwrap(), "GD5c");
    assert_eq!(nth_run_id("GD5", 25), "GD5z");
    assert_eq!(nth_run_id("GD5", 26), "GD5-27");
    assert_eq!(nth_run_id("s-fix-readme", 1), "s-fix-readmeb");
}

#[tokio::test]
async fn a_second_start_of_a_live_item_is_refused_without_again() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    let first = r.svc.start(item("GD5")).await.unwrap();
    assert_eq!(first.run_id, "GD5");
    assert_eq!(first.status, RunStatus::Running);
    let e = r.svc.start(item("GD5")).await.unwrap_err();
    assert!(matches!(e, Refusal::Live(_)), "{e:?}");
    assert_eq!(e.message(), "GD5 is still running. Start GD5b too?");
    assert_eq!(e.status(), StatusCode::CONFLICT);
    let again = r
        .svc
        .start(StartRequest {
            again: true,
            ..item("GD5")
        })
        .await
        .unwrap();
    assert_eq!(again.run_id, "GD5b");
    // The start turn, exactly, once per run.
    assert_eq!(
        *lock(&r.rec.turns),
        vec![
            "Run GD5 started for GD5 in Strands/Gd.".to_string(),
            "Run GD5b started for GD5 in Strands/Gd.".to_string()
        ]
    );
    // After GD5 and GD5b are reported, the next is GD5c.
    finish_round(&r, "GD5", 0, Some("done\n"), "");
    r.svc.pass(now_ms()).await;
    finish_round(&r, "GD5b", 0, Some("done\n"), "");
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Reported);
    assert_eq!(r.svc.get("GD5b").unwrap().status, RunStatus::Reported);
    assert_eq!(r.svc.start(item("GD5")).await.unwrap().run_id, "GD5c");
}

#[test]
fn eligibility_refuses_each_thing_by_name() {
    let s = Scratch::new();
    vault_fixture(&s);
    let n = s.notes();
    let ok = studio_item(&n, "Gd", "GD5").unwrap();
    assert_eq!(ok.repo, "o/n");
    assert_eq!(ok.prompt_path, "Projects/drafts/gd5-prompt.md");
    assert_eq!(ok.section, SECTION);
    let why = |id: &str| studio_item(&n, "Gd", id).unwrap_err();
    assert!(why("GD6").contains("checked"), "{}", why("GD6"));
    assert!(why("GD7").contains("archived"), "{}", why("GD7"));
    assert!(why("GD8").contains("no ## PROMPT"), "{}", why("GD8"));
    assert!(why("GD9").contains("no devjob-repo"), "{}", why("GD9"));
    assert!(
        why("GD10").contains("devjob-host: studio"),
        "{}",
        why("GD10")
    );
    assert!(
        why("GD11").contains("is step GD99, not GD11"),
        "{}",
        why("GD11")
    );
    assert!(
        why("GD12").contains("devjob-host: studio"),
        "{}",
        why("GD12")
    );
    assert!(why("GD404").contains("no line GD404"));
    assert!(studio_item(&n, "../etc", "GD5").is_err());
}

#[test]
fn the_served_prompt_is_the_section_then_the_generated_section_exactly() {
    let expected = format!(
        "{SECTION}### Run ID and report\n\nThis run is GD5b. Start every branch name with gd5b- and put [GD5b] at the start of the pull request title. Other runs and people work on this repository at the same time: before your final verification, and again right before any merge, fetch the latest default branch, merge or rebase it into your branch, resolve any conflicts, and re-run your checks. Keep everything you create inside your working directory or the system temporary directory. When you finish, or stop early, write your complete report back, the same text you would answer with, to the file named in the environment variable JESSE_RUN_REPORT, then answer as usual. If you need a decision from Jeremy to continue, stop, make the first line of that report NEEDS YOU: followed by the one question, and end the run.\n"
    );
    assert_eq!(served_prompt(SECTION, "GD5b"), expected);
    // The section is taken byte for byte, up to the rule before the archive checkboxes.
    let src = format!("---\na: b\n---\n# T\n\n## PROMPT\n{SECTION}---\n- [ ] x\n");
    assert_eq!(prompt_section(&src).as_deref(), Some(SECTION));
    // A rule inside the prompt (not followed only by checkboxes) is part of it.
    let inner = "## PROMPT\nA\n\n---\n\nB\n";
    assert_eq!(prompt_section(inner).as_deref(), Some("A\n\n---\n\nB\n"));
    assert_eq!(prompt_section("# no prompt\n"), None);
}

#[tokio::test]
async fn the_run_gets_the_served_prompt_on_disk() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    let dir = r.svc.settings.run_dir("GD5");
    assert_eq!(
        std::fs::read_to_string(dir.join(PROMPT_MD)).unwrap(),
        served_prompt(SECTION, "GD5")
    );
    let rec = read_run(&dir).unwrap();
    assert_eq!(rec.branch, "gd5-run-the-thing-on-the-studio");
    assert_eq!(rec.origin, "app");
    assert_eq!(
        rec.prompt_path.as_deref(),
        Some("Projects/drafts/gd5-prompt.md")
    );
    assert!(rec.base_sha.as_deref().is_some_and(|s| s.len() == 40));
    assert_eq!(rec.default_branch.as_deref(), Some("main"));
    // The plist: the runner and the run directory, no keep alive, interactive.
    let plist = std::fs::read_to_string(dir.join(JOB_PLIST)).unwrap();
    assert!(plist.contains("<string>com.example.jesse-run.GD5</string>"));
    assert!(plist.contains("<key>KeepAlive</key>\n  <false/>"));
    assert!(plist.contains("<string>Interactive</string>"));
    assert!(plist.contains(&format!("<string>{}</string>", dir.display())));
    assert_eq!(
        *lock(&r.launcher.bootstrapped),
        vec!["com.example.jesse-run.GD5".to_string()]
    );
}

#[test]
fn devjob_args_keep_refusing_unknown_keys_and_without_host_build_the_same_cluster_spec() {
    let v = json!({ "repo": "a/b", "prompt": "p", "branch_slug": "s", "namespace": "x" });
    assert!(serde_json::from_value::<DevJobArgs>(v)
        .unwrap_err()
        .to_string()
        .contains("unknown field"));
    let args = DevJobArgs {
        repo: Some("jeremyandrews/k3s".into()),
        prompt: Some("Read README.md.".into()),
        branch_slug: Some("read-readme".into()),
        timeout_minutes: Some(30),
        ..DevJobArgs::default()
    };
    let before = DevJobSpec::validate(args.clone(), devjob::Agent::Claude).unwrap();
    assert_eq!(
        devjob::plan_start(args.clone(), devjob::Agent::Claude).unwrap(),
        devjob::StartPlan::Cluster(before.clone())
    );
    let cluster = DevJobArgs {
        host: Some("cluster".into()),
        ..args.clone()
    };
    assert_eq!(
        devjob::plan_start(cluster, devjob::Agent::Claude).unwrap(),
        devjob::StartPlan::Cluster(before)
    );
    // The Studio's keys are refused on the cluster, and a Studio run has no clock.
    let e = devjob::plan_start(
        DevJobArgs {
            strand: Some("Gd".into()),
            ..args.clone()
        },
        devjob::Agent::Claude,
    )
    .unwrap_err();
    assert!(e.contains("host \"studio\""), "{e}");
    let e = devjob::plan_start(
        DevJobArgs {
            host: Some("studio".into()),
            ..args
        },
        devjob::Agent::Claude,
    )
    .unwrap_err();
    assert!(e.contains("no clock"), "{e}");
    let studio = serde_json::from_value::<DevJobArgs>(
        json!({ "host": "studio", "strand": "Gd", "item_id": "GD5", "again": true }),
    )
    .unwrap();
    assert_eq!(
        devjob::plan_start(studio, devjob::Agent::Claude).unwrap(),
        devjob::StartPlan::Studio {
            kind: StartKind::Item {
                strand: "Gd".into(),
                item_id: "GD5".into()
            },
            again: true
        }
    );
}

// ---- 2. THE WATCHER --------------------------------------------------------------------

#[tokio::test]
async fn a_report_from_report_md_reaches_the_inbox_byte_for_byte() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    let report = "Done: the \"thing\" works.\n\n```rust\nfn main() {}\n```\n\nOpened https://github.com/o/n/pull/42 for review.\n";
    finish_round(&r, "GD5", 0, Some(report), STREAM_TEXT);
    r.svc.pass(now_ms()).await;
    let rel = format!("Inbox/{}-run-GD5.md", today());
    let text = std::fs::read_to_string(s.notes().join(&rel)).unwrap();
    let expected = format!(
        "---\ndevjob: GD5\nrepo: o/n\noutcome: reported\nstrand: Gd\nstrand-id: GD5\nprompt: Projects/drafts/gd5-prompt.md\nhost: studio\nsession: 11111111-2222-3333-4444-555555555555\nbranch: gd5-run-the-thing-on-the-studio\nround: 1\n---\n\n{report}\nPull request: [https://github.com/o/n/pull/42](https://github.com/o/n/pull/42)\n"
    );
    assert_eq!(text, expected);
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    assert_eq!(
        rec.round().unwrap().report_path.as_deref(),
        Some(rel.as_str())
    );
    assert!(rec.round().unwrap().notified);
    assert_eq!(
        lock(&r.rec.turns).last().unwrap(),
        &format!(
            "Run report for GD5 in Strands/Gd: [[Inbox/{}-run-GD5]]",
            today()
        )
    );
    let pushes = lock(&r.rec.pushes).clone();
    assert_eq!(
        pushes,
        vec![(
            "Run GD5 reported".to_string(),
            "Done: the \"thing\" works.".to_string(),
            "GD5".to_string()
        )]
    );

    // A second pass writes nothing, pushes nothing and sends no turn.
    let turns = lock(&r.rec.turns).len();
    r.svc.pass(now_ms()).await;
    assert_eq!(inbox_files(&s), vec![format!("{}-run-GD5.md", today())]);
    assert_eq!(lock(&r.rec.pushes).len(), 1);
    assert_eq!(lock(&r.rec.turns).len(), turns);

    // Nor does a freshly constructed watcher (a bridge restart).
    let fresh = rig(&s);
    fresh.svc.pass(now_ms()).await;
    assert_eq!(inbox_files(&s), vec![format!("{}-run-GD5.md", today())]);
    assert!(lock(&fresh.rec.pushes).is_empty());
    assert!(lock(&fresh.rec.turns).is_empty());
}

#[tokio::test]
async fn a_restart_between_the_inbox_write_and_the_record_writes_nothing_twice() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    finish_round(&r, "GD5", 0, Some("ok\n"), "");
    // The Inbox file exists, but run.json never learnt of it.
    let rel = format!("Inbox/{}-run-GD5.md", today());
    let rec = r.svc.get("GD5").unwrap();
    std::fs::write(
        s.notes().join(&rel),
        render_inbox(&rec, 1, RunStatus::Reported, "ok\n"),
    )
    .unwrap();
    let fresh = rig(&s);
    fresh.svc.pass(now_ms()).await;
    assert_eq!(inbox_files(&s), vec![format!("{}-run-GD5.md", today())]);
    let rec = fresh.svc.get("GD5").unwrap();
    assert_eq!(
        rec.round().unwrap().report_path.as_deref(),
        Some(rel.as_str())
    );
    assert_eq!(rec.status, RunStatus::Reported);
}

#[tokio::test]
async fn without_report_md_the_last_result_event_is_the_report() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    finish_round(&r, "GD5", 0, None, STREAM_TEXT);
    r.svc.pass(now_ms()).await;
    let text =
        std::fs::read_to_string(s.notes().join(format!("Inbox/{}-run-GD5.md", today()))).unwrap();
    assert!(
        text.ends_with("---\n\nFinal answer from the result event."),
        "{text}"
    );
}

#[tokio::test]
async fn with_neither_the_log_tail_is_the_report_under_a_line_saying_so() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    let no_result: String = STREAM_TEXT
        .lines()
        .take(3)
        .map(|l| format!("{l}\n"))
        .collect();
    finish_round(&r, "GD5", 1, None, &no_result);
    r.svc.pass(now_ms()).await;
    let text =
        std::fs::read_to_string(s.notes().join(format!("Inbox/{}-run-GD5.md", today()))).unwrap();
    let body = text.split("---\n\n").nth(1).unwrap();
    assert_eq!(
        body,
        "No report was written. The last lines of the run's log follow.\n\nReading the README.\nBash: cargo test\nEdit: bridge/src/x.rs\n"
    );
    assert!(!text.contains("SECRET OUTPUT BODY"));
    assert!(text.contains("\noutcome: failed\n"));
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Failed);
}

#[tokio::test]
async fn needs_you_then_a_reply_round_resumes_and_reports_as_round_two() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    finish_round(
        &r,
        "GD5",
        0,
        Some("NEEDS YOU: main or release?\nmore\n"),
        "",
    );
    r.svc.pass(now_ms()).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::NeedsYou);
    assert_eq!(lock(&r.rec.pushes)[0].0, "Run GD5 needs you");
    assert_eq!(lock(&r.rec.pushes)[0].1, "NEEDS YOU: main or release?");
    // A needs_you run is still live for a second start.
    assert!(matches!(
        r.svc.start(item("GD5")).await,
        Err(Refusal::Live(_))
    ));
    // Reply: round 2, same worktree, queued then running.
    let rec = r.svc.reply_and_pump("GD5", "main").await.unwrap();
    assert_eq!(rec.status, RunStatus::Running);
    assert_eq!(rec.rounds.len(), 2);
    let dir = r.svc.settings.run_dir("GD5");
    assert!(dir.join("report-r1.md").is_file());
    assert!(!dir.join(REPORT_MD).exists());
    finish_round(&r, "GD5", 0, Some("Merged.\n"), "");
    r.svc.pass(now_ms()).await;
    let rel2 = format!("Inbox/{}-run-GD5-r2.md", today());
    let text = std::fs::read_to_string(s.notes().join(&rel2)).unwrap();
    assert!(text.contains("\nround: 2\n"));
    assert!(text.ends_with("---\n\nMerged.\n"));
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Reported);
    // A reported run takes a reply; a queued one does not.
    assert!(r.svc.reply("GD5", CONTINUE_TEXT).is_ok());
    let e = r.svc.reply("GD5", CONTINUE_TEXT).unwrap_err();
    assert!(matches!(e, Refusal::State(_)), "{e:?}");
}

#[tokio::test]
async fn a_needs_you_run_does_not_hold_a_slot() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let mut st = settings(&s);
    st.max_concurrent = 1;
    let r = rig_with(&s, st);
    r.svc.start(item("GD5")).await.unwrap();
    finish_round(&r, "GD5", 0, Some("NEEDS YOU: which?\n"), "");
    r.svc.pass(now_ms()).await;
    let next = r.svc.start(free("other")).await.unwrap();
    assert_eq!(next.status, RunStatus::Running);
}

#[tokio::test]
async fn a_quiet_run_is_marked_stalled_once_and_never_killed() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    let dir = r.svc.settings.run_dir("GD5");
    let t0 = now_ms();
    update_run(&dir, |x| x.last_event_at = Some(rfc(t0))).unwrap();
    let (fx, fin) = r.svc.scan(t0 + 59 * 60_000);
    assert!(fx.is_empty() && fin.is_empty());
    let later = t0 + 61 * 60_000;
    let (fx, _) = r.svc.scan(later);
    assert_eq!(
        fx,
        vec![Effect::Push {
            title: "Run quiet".into(),
            body: "GD5 quiet for 60 min".into(),
            run_id: "GD5".into()
        }]
    );
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Running);
    assert_eq!(rec.stalled_since, Some(rfc(later)));
    assert!(lock(&r.launcher.booted_out).is_empty(), "never killed");
    assert!(r.launcher.is_running(&test_label("GD5")));
    // Once, not every pass.
    assert!(r.svc.scan(later + 3_600_000).0.is_empty());
    // A new event clears it.
    update_run(&dir, |x| x.last_event_at = Some(rfc(later + 3_700_000))).unwrap();
    r.svc.scan(later + 3_700_001);
    assert_eq!(r.svc.get("GD5").unwrap().stalled_since, None);
}

#[tokio::test]
async fn a_run_whose_label_is_gone_without_an_exit_is_interrupted() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    r.launcher.gone("GD5");
    // Not straight after the start.
    let (fx, _) = r.svc.scan(now_ms());
    assert!(fx.is_empty());
    let (fx, fin) = r.svc.scan(now_ms() + 11_000);
    assert!(fin.is_empty());
    assert_eq!(fx.len(), 1);
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Interrupted);
    // Nothing restarts it by itself.
    r.svc.pass(now_ms() + 12_000).await;
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Interrupted);
    assert_eq!(lock(&r.launcher.bootstrapped).len(), 1);
    // A reply resumes it.
    let rec = r.svc.reply_and_pump("GD5", CONTINUE_TEXT).await.unwrap();
    assert_eq!(rec.status, RunStatus::Running);
}

#[tokio::test]
async fn cancel_of_a_queued_run_just_cancels_and_of_a_running_one_reports_cancelled() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(free("one")).await.unwrap();
    let q = r.svc.start(free("two")).await.unwrap();
    assert_eq!(q.status, RunStatus::Queued);
    assert_eq!(q.waiting_for.as_deref(), Some("repo o/n"));
    let c = r.svc.cancel("s-two").unwrap();
    assert_eq!(c.status, RunStatus::Cancelled);
    // Running: the round records the request; the exit then reports it as cancelled.
    r.svc.cancel("s-one").unwrap();
    assert!(r
        .svc
        .get("s-one")
        .unwrap()
        .round()
        .unwrap()
        .cancel_requested_at
        .is_some());
    finish_round(&r, "s-one", 143, None, STREAM_TEXT);
    r.svc.pass(now_ms()).await;
    let rec = r.svc.get("s-one").unwrap();
    assert_eq!(rec.status, RunStatus::Cancelled);
    let text =
        std::fs::read_to_string(s.notes().join(format!("Inbox/{}-run-s-one.md", today()))).unwrap();
    assert!(text.contains("\noutcome: cancelled\n"));
    // No strand, so no strand turn.
    assert!(lock(&r.rec.turns).is_empty());
    assert!(matches!(r.svc.cancel("s-one"), Err(Refusal::State(_))));
}

// ---- 3. WORKTREES AND THE DISK ------------------------------------------------------------

#[test]
fn two_worktrees_of_one_repository_at_the_same_instant_never_collide() {
    let s = Scratch::new();
    remote_fixture(&s);
    let st = settings(&s);
    let url = st.repo_url("o/n");
    for i in 0..20 {
        let handles: Vec<_> = ["a", "b"]
            .iter()
            .map(|x| {
                let root = st.root.clone();
                let url = url.clone();
                let wt = st.run_dir(&format!("R{i}{x}")).join(WORKTREE_DIR);
                let branch = format!("r{i}{x}-work");
                std::thread::spawn(move || {
                    git::add_worktree(&root, "o/n", &url, &wt, &branch).map(|_| (wt, branch))
                })
            })
            .collect();
        for h in handles {
            let (wt, branch) = h.join().unwrap().unwrap();
            assert_eq!(
                sh(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
                branch
            );
        }
    }
    let bare = git::bare_path(&st.root, "o/n");
    assert_eq!(git::registered_worktrees(&bare).len(), 40);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_max_per_repo_two_both_start_and_with_one_the_second_waits_for_the_repo() {
    let s = Scratch::new();
    remote_fixture(&s);
    let mut st = settings(&s);
    st.max_per_repo = 2;
    let r = rig_with(&s, st);
    for i in 0..20 {
        let (sa, sb) = (r.svc.clone(), r.svc.clone());
        let (a, b) = tokio::join!(
            tokio::spawn(async move { sa.start(free(&format!("p{i}-a"))).await }),
            tokio::spawn(async move { sb.start(free(&format!("p{i}-b"))).await })
        );
        let (a, b) = (a.unwrap().unwrap(), b.unwrap().unwrap());
        assert_eq!(
            (
                r.svc.get(&a.run_id).unwrap().status,
                r.svc.get(&b.run_id).unwrap().status
            ),
            (RunStatus::Running, RunStatus::Running),
            "{i}"
        );
        for id in [&a.run_id, &b.run_id] {
            let rec = r.svc.get(id).unwrap();
            let wt = r.svc.settings.run_dir(id).join(WORKTREE_DIR);
            assert_eq!(
                sh(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
                rec.branch
            );
            finish_round(&r, id, 0, Some("ok\n"), "");
        }
        r.svc.pass(now_ms()).await;
    }

    // The default, one per repository.
    let s = Scratch::new();
    remote_fixture(&s);
    let r = rig(&s);
    let a = r.svc.start(free("first")).await.unwrap();
    let b = r.svc.start(free("second")).await.unwrap();
    assert_eq!(a.status, RunStatus::Running);
    assert_eq!(b.status, RunStatus::Queued);
    assert_eq!(b.waiting_for.as_deref(), Some("repo o/n"));
    finish_round(&r, "s-first", 0, Some("ok\n"), "");
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get("s-second").unwrap().status, RunStatus::Running);
    assert_eq!(r.svc.get("s-second").unwrap().waiting_for, None);
}

#[test]
fn each_run_has_its_own_cargo_target_dir_and_no_bridge_secret() {
    let a = Path::new("/r/runs/GD5");
    let b = Path::new("/r/runs/GD5b");
    let from = |k: &str| match k {
        "HOME" => Some("/Users/j".to_string()),
        "PATH" => Some("/usr/bin".to_string()),
        "JESSE_TOKEN" | "JESSE_APNS_KEY_PATH" | "SLACK_MCP_XOXP_TOKEN" => Some("secret".into()),
        _ => None,
    };
    let ea = runner::agent_env(a, "GD5", from);
    let eb = runner::agent_env(b, "GD5b", from);
    let get =
        |e: &[(String, String)], k: &str| e.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
    assert_eq!(
        get(&ea, "CARGO_TARGET_DIR").as_deref(),
        Some("/r/runs/GD5/target")
    );
    assert_eq!(
        get(&eb, "CARGO_TARGET_DIR").as_deref(),
        Some("/r/runs/GD5b/target")
    );
    assert_eq!(
        get(&ea, "JESSE_RUN_REPORT").as_deref(),
        Some("/r/runs/GD5/report.md")
    );
    assert_eq!(get(&ea, "JESSE_RUN_ID").as_deref(), Some("GD5"));
    assert_eq!(get(&ea, "HOME").as_deref(), Some("/Users/j"));
    for k in ["JESSE_TOKEN", "JESSE_APNS_KEY_PATH", "SLACK_MCP_XOXP_TOKEN"] {
        assert_eq!(get(&ea, k), None, "{k}");
    }
    // The flags: the first round names the session, later rounds resume it.
    let rec = RunRecord {
        permission_mode: "bypassPermissions".into(),
        model: Some("claude-opus-5-5".into()),
        ..RunRecord::default()
    };
    assert_eq!(
        runner::claude_args(&rec, true, "S"),
        [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--session-id",
            "S",
            "--permission-mode",
            "bypassPermissions",
            "--model",
            "claude-opus-5-5"
        ]
    );
    assert_eq!(runner::claude_args(&rec, false, "S")[4], "--resume");
}

#[tokio::test]
async fn a_merged_branch_removes_artifacts_then_the_worktree_and_keeps_the_record() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    r.svc.start(free("live")).await.unwrap(); // queued behind GD5 (same repository)
    let dir = r.svc.settings.run_dir("GD5");
    std::fs::write(dir.join("target/big.o"), vec![0u8; 4096]).unwrap();
    // Xcode's folders: one built from this worktree, one from elsewhere.
    let mine = s.derived().join("Jesse-abc");
    let other = s.derived().join("Jesse-xyz");
    for (d, ws) in [
        (&mine, dir.join("worktree/Jesse.xcodeproj")),
        (
            &other,
            PathBuf::from("/Users/j/devel/tag1/jesse-app/Jesse.xcodeproj"),
        ),
    ] {
        std::fs::create_dir_all(d).unwrap();
        std::fs::write(
            d.join("info.plist"),
            format!("<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict><key>LastAccessedDate</key><date>2026-11-09T00:00:00Z</date><key>WorkspacePath</key><string>{}</string></dict></plist>\n", ws.display()),
        )
        .unwrap();
    }
    finish_round(&r, "GD5", 0, Some("ok\n"), "");
    r.svc.pass(now_ms()).await; // reports GD5, starts s-live, first cleanup (not merged yet)
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.derived_data, vec![mine.to_string_lossy().into_owned()]);
    assert!(dir.join("worktree").is_dir() && dir.join("target").is_dir());
    assert_eq!(r.svc.get("s-live").unwrap().status, RunStatus::Running);

    lock(&r.forge.states).insert(rec.branch.clone(), PrState::Merged);
    r.svc.cleanup(now_ms());
    let rec = r.svc.get("GD5").unwrap();
    assert!(!dir.join("target").exists());
    assert!(!mine.exists());
    assert!(other.exists(), "a folder this run did not record stays");
    assert!(!dir.join("worktree").exists());
    assert!(rec.cleanup.artifacts_removed.is_some() && rec.cleanup.worktree_removed.is_some());
    assert!(!rec.cleanup.kept);
    // The record and the report stay; the local branch is gone.
    assert!(dir.join(RUN_JSON).is_file() && dir.join(REPORT_MD).is_file());
    let bare = git::bare_path(&r.svc.settings.root, "o/n");
    assert!(git::git(
        &bare,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{}", rec.branch)
        ]
    )
    .is_err());
    // git worktree list names only the live run.
    let live = r.svc.settings.run_dir("s-live").join(WORKTREE_DIR);
    let listed: Vec<PathBuf> = git::registered_worktrees(&bare)
        .into_iter()
        .map(|p| std::fs::canonicalize(p).unwrap())
        .collect();
    assert_eq!(listed, vec![std::fs::canonicalize(live).unwrap()]);
}

#[tokio::test]
async fn a_branch_seen_on_origin_and_then_deleted_counts_as_gone() {
    let s = Scratch::new();
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(free("pushed")).await.unwrap();
    let dir = r.svc.settings.run_dir("s-pushed");
    let rec = r.svc.get("s-pushed").unwrap();
    sh(
        &dir.join(WORKTREE_DIR),
        &["push", "-q", "origin", &rec.branch],
    );
    finish_round(&r, "s-pushed", 0, Some("ok\n"), "");
    r.svc.pass(now_ms()).await;
    assert!(r.svc.get("s-pushed").unwrap().cleanup.branch_seen_on_origin);
    assert!(dir.join(WORKTREE_DIR).exists());
    sh(
        &s.base.join("remote/o/n.git"),
        &["branch", "-q", "-D", &rec.branch],
    );
    r.svc.cleanup(now_ms());
    assert!(!dir.join(WORKTREE_DIR).exists());
    assert!(!dir.join(TARGET_DIR).exists());
}

#[tokio::test]
async fn a_paused_run_keeps_its_worktree_past_keep_days_and_loses_only_artifacts() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    let dir = r.svc.settings.run_dir("GD5");
    finish_round(&r, "GD5", 0, Some("NEEDS YOU: which?\n"), "");
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::NeedsYou);
    // Merged or not, a paused run is not cleaned by the pull request rule.
    lock(&r.forge.states).insert(r.svc.get("GD5").unwrap().branch, PrState::Merged);
    r.svc.cleanup(now_ms());
    assert!(dir.join("target").exists() && dir.join("worktree").exists());
    let day = 24 * 3_600_000;
    r.svc.cleanup(now_ms() + 8 * day);
    assert!(!dir.join("target").exists(), "artifacts go after keep_days");
    assert!(dir.join("worktree").exists(), "the worktree stays");
    // After 30 days: one push first, then the worktree goes at the next cleanup.
    let fx = r.svc.cleanup(now_ms() + 31 * day);
    assert_eq!(fx.len(), 1);
    assert!(dir.join("worktree").exists());
    r.svc.cleanup(now_ms() + 31 * day + 3_600_000);
    assert!(!dir.join("worktree").exists());
    assert!(dir.join(RUN_JSON).is_file());
    // A reply now has nothing to continue from (the branch was never pushed).
    assert!(matches!(r.svc.reply("GD5", "go"), Err(Refusal::State(_))));
}

#[tokio::test]
async fn an_interrupted_run_keeps_its_worktree() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    r.launcher.gone("GD5");
    r.svc.scan(now_ms() + 11_000);
    let dir = r.svc.settings.run_dir("GD5");
    r.svc.cleanup(now_ms() + 8 * 24 * 3_600_000);
    assert!(dir.join("worktree").exists());
    assert!(!dir.join("target").exists());
}

fn fake_run(
    st: &HostRunSettings,
    id: &str,
    status: RunStatus,
    finished: Option<u64>,
    bytes: usize,
) {
    let dir = st.run_dir(id);
    std::fs::create_dir_all(dir.join(TARGET_DIR)).unwrap();
    std::fs::write(dir.join(TARGET_DIR).join("blob"), vec![1u8; bytes]).unwrap();
    let rec = RunRecord {
        run_id: id.into(),
        repo: "o/n".into(),
        branch: id.to_lowercase(),
        status,
        created_at: rfc(finished.unwrap_or(1)),
        finished_at: finished.map(rfc),
        ..RunRecord::default()
    };
    write_run(&dir, &rec).unwrap();
}

#[test]
fn the_space_pass_removes_the_oldest_finished_artifacts_first_and_never_a_running_run() {
    let s = Scratch::new();
    let mut st = settings(&s);
    let mb = 1024 * 1024;
    let t = now_ms() - 3_600_000;
    fake_run(&st, "OLD", RunStatus::Reported, Some(t), mb);
    fake_run(&st, "MID", RunStatus::Failed, Some(t + 1000), mb);
    fake_run(&st, "NEW", RunStatus::NeedsYou, Some(t + 2000), mb);
    fake_run(&st, "RUN", RunStatus::Running, None, mb);
    let total = sys::tree_bytes(&st.root);
    st.max_total_bytes = total - mb as u64 / 2;
    let r = rig_with(&s, st.clone());
    r.svc.space_pass(now_ms());
    assert!(!st.run_dir("OLD").join(TARGET_DIR).exists());
    assert!(st.run_dir("MID").join(TARGET_DIR).exists());
    assert!(st.run_dir("NEW").join(TARGET_DIR).exists());
    assert!(st.run_dir("RUN").join(TARGET_DIR).exists());
    // A tiny cap empties every finished run's artifacts, still never the running one's.
    let mut tiny = st.clone();
    tiny.max_total_bytes = 1;
    let r = rig_with(&s, tiny);
    r.svc.space_pass(now_ms());
    for id in ["MID", "NEW"] {
        assert!(!st.run_dir(id).join(TARGET_DIR).exists(), "{id}");
    }
    assert!(st.run_dir("RUN").join(TARGET_DIR).join("blob").is_file());
    for id in ["OLD", "MID", "NEW", "RUN"] {
        assert!(st.run_dir(id).join(RUN_JSON).is_file());
    }
    let v = r.svc.list_json();
    assert!(
        v["disk"]["last_space_pass"]["freed_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(v["counts"]["running"], 1);
    assert_eq!(v["counts"]["needs_you"], 1);
}

#[tokio::test]
async fn reconcile_prunes_boots_out_and_reports_but_keeps_an_unknown_directory() {
    let s = Scratch::new();
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(free("gone")).await.unwrap();
    // The worktree disappears from under git.
    let wt = r.svc.settings.run_dir("s-gone").join(WORKTREE_DIR);
    std::fs::remove_dir_all(&wt).unwrap();
    let bare = git::bare_path(&r.svc.settings.root, "o/n");
    assert_eq!(git::registered_worktrees(&bare).len(), 1);
    // A label of this root with no run directory, one of another root, and a directory with
    // no run.json.
    for (id, root) in [
        ("ghost", r.svc.settings.runs_dir()),
        ("elsewhere", s.base.join("other/runs")),
    ] {
        let label = test_label(id);
        lock(&r.launcher.loaded_extra).push(label.clone());
        lock(&r.launcher.plists).insert(label, root.join(id).join(JOB_PLIST));
    }
    let stray = r.svc.settings.runs_dir().join("stray");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(stray.join("note.txt"), "keep me").unwrap();
    r.svc.reconcile(now_ms());
    assert!(git::registered_worktrees(&bare).is_empty());
    assert_eq!(
        *lock(&r.launcher.booted_out),
        vec!["com.example.jesse-run.ghost".to_string()],
        "another root's run is never touched"
    );
    assert!(
        r.launcher.is_running(&test_label("s-gone")),
        "its own label stays"
    );
    assert!(stray.join("note.txt").is_file());
    assert_eq!(r.svc.list_json()["orphans"], json!(["stray"]));
}

// ---- 4. THE SNAPSHOT AND THE CHAT TOOLS -----------------------------------------------------

#[tokio::test]
async fn the_strands_snapshot_gains_studio_runnable_and_runs_active() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    let snap = crate::strands::snapshot(&s.notes(), "2026-11-09");
    let mut v = serde_json::to_value(&snap).unwrap();
    annotate_strands(&mut v, &snap, &s.notes(), Some(&r.svc));
    let gd = &v["strands"][0];
    assert_eq!(gd["runs_active"], 0);
    let drafts = gd["drafts"].as_array().unwrap();
    let runnable: Vec<&str> = drafts
        .iter()
        .filter(|d| d["studio_runnable"] == true)
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert_eq!(runnable, vec!["GD5"]);
    r.svc.start(item("GD5")).await.unwrap();
    let mut v = serde_json::to_value(&snap).unwrap();
    annotate_strands(&mut v, &snap, &s.notes(), Some(&r.svc));
    assert_eq!(v["strands"][0]["runs_active"], 1);
    // With [hostrun] off, nothing is runnable.
    let mut v = serde_json::to_value(&snap).unwrap();
    annotate_strands(&mut v, &snap, &s.notes(), None);
    assert!(v["strands"][0]["drafts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|d| d["studio_runnable"] == false));
}

fn tool_text(v: &Value) -> Value {
    serde_json::from_str(v["content"][0]["text"].as_str().unwrap()).unwrap_or(json!(null))
}

#[tokio::test]
async fn the_devjob_tools_start_list_reply_and_cancel_studio_runs() {
    use crate::gateway::InProcessServer;
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    let kc = s.base.join("kc.yaml");
    std::fs::write(
        &kc,
        "apiVersion: v1\nkind: Config\nclusters:\n- name: c\n  cluster:\n    server: https://127.0.0.1:1\n\
         contexts:\n- name: x\n  context:\n    cluster: c\n    user: u\ncurrent-context: x\n\
         users:\n- name: u\n  user:\n    token: not-a-real-token\n",
    )
    .unwrap();
    let dj = devjob::DevJobs::connect(devjob::DevJobSettings::from_toml(
        Some(devjob::DevJobToml {
            enabled: true,
            kubeconfig: Some(kc),
            ..devjob::DevJobToml::default()
        }),
        "/home/u",
    ))
    .await
    .unwrap();
    // Before the Studio is attached: exactly the three cluster tools.
    let names = |t: Vec<Value>| -> Vec<String> {
        t.iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names(dj.tools()), devjob::DEVJOB_TOOLS);
    dj.attach_studio(r.svc.clone());
    assert_eq!(names(dj.tools()), devjob::STUDIO_TOOLS);

    let started = dj
        .call(
            devjob::TOOL_START,
            json!({ "host": "studio", "strand": "Gd", "item_id": "GD5" }),
        )
        .await
        .unwrap();
    assert_eq!(started["isError"], false, "{started}");
    assert_eq!(tool_text(&started)["run_id"], "GD5");
    assert_eq!(tool_text(&started)["host"], "studio");
    let again = dj
        .call(
            devjob::TOOL_START,
            json!({ "host": "studio", "strand": "Gd", "item_id": "GD5" }),
        )
        .await
        .unwrap();
    assert_eq!(again["isError"], true);

    let listed = dj.call(devjob::TOOL_LIST, json!({})).await.unwrap();
    let l = tool_text(&listed);
    assert_eq!(l["studio"]["active"][0]["run_id"], "GD5");
    assert_eq!(l["studio"]["active"][0]["host"], "studio");
    assert!(l["cluster_error"].is_string());

    let status = dj
        .call(devjob::TOOL_STATUS, json!({ "job_name": "GD5" }))
        .await
        .unwrap();
    assert_eq!(tool_text(&status)["host"], "studio");

    let cancelled = dj
        .call(devjob::TOOL_CANCEL, json!({ "run_id": "GD5" }))
        .await
        .unwrap();
    assert_eq!(cancelled["isError"], false, "{cancelled}");
    finish_round(&r, "GD5", 143, None, STREAM_TEXT);
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Cancelled);
    // A cancelled run takes no reply; a failed one does.
    let reply = dj
        .call(
            devjob::TOOL_REPLY,
            json!({ "run_id": "GD5", "text": "go on" }),
        )
        .await
        .unwrap();
    assert_eq!(reply["isError"], true);
    update_run(&r.svc.settings.run_dir("GD5"), |x| {
        x.status = RunStatus::Failed
    })
    .unwrap();
    let reply = dj
        .call(
            devjob::TOOL_REPLY,
            json!({ "run_id": "GD5", "text": "go on" }),
        )
        .await
        .unwrap();
    assert_eq!(reply["isError"], false, "{reply}");
    assert_eq!(r.svc.get("GD5").unwrap().rounds.len(), 2);

    // Without host, the same cluster spec as before (the cluster itself is unreachable here).
    let args = json!({ "repo": "a/b", "prompt": "p", "branch_slug": "s" });
    let plan = devjob::plan_start(
        serde_json::from_value(args.clone()).unwrap(),
        devjob::Agent::Claude,
    )
    .unwrap();
    assert_eq!(
        plan,
        devjob::StartPlan::Cluster(
            DevJobSpec::validate(serde_json::from_value(args).unwrap(), devjob::Agent::Claude)
                .unwrap()
        )
    );
}

// ---- 5. SETTINGS AND READING ----------------------------------------------------------------

#[test]
fn hostrun_is_off_by_default_and_the_table_parses() {
    let d = HostRunSettings::default();
    assert!(!d.enabled);
    assert!(d.problems().is_empty());
    let t: HostRunToml = toml::from_str(
        "enabled = true\nroot = \"~/runs\"\nmax_concurrent = 4\nmax_per_repo = 1\nstall_minutes = 30\n\
         [repos.\"o/n\"]\nmax_concurrent = 2\n",
    )
    .unwrap();
    let s = HostRunSettings::from_toml(Some(t), "/Users/j", "claude");
    assert_eq!(s.root, PathBuf::from("/Users/j/runs"));
    assert_eq!(s.repo_cap("o/n"), 2);
    assert_eq!(s.repo_cap("o/other"), 1);
    assert_eq!(s.min_free_bytes, 100 * GB);
    assert_eq!(s.max_total_bytes, 300 * GB);
    assert!(toml::from_str::<HostRunToml>("enabled = true\nimage = \"x\"\n").is_err());
}

#[test]
fn hostrun_refuses_to_boot_on_a_missing_claude_or_a_bad_root() {
    let s = Scratch::new();
    let mut st = settings(&s);
    st.claude = "/no/such/claude".into();
    let p = st.problems();
    assert!(
        p.iter()
            .any(|m| m.contains("claude") && m.contains("missing")),
        "{p:?}"
    );
    let mut st = settings(&s);
    std::fs::write(s.base.join("afile"), "x").unwrap();
    st.root = s.base.join("afile/root");
    let p = st.problems();
    assert!(p.iter().any(|m| m.contains("unwritable")), "{p:?}");
    assert!(
        settings(&s).problems().is_empty(),
        "{:?}",
        settings(&s).problems()
    );
}

#[test]
fn readable_lines_never_carry_raw_json_or_tool_output() {
    let lines = stream::readable_lines(STREAM_TEXT);
    assert_eq!(
        lines,
        [
            "Reading the README.",
            "Bash: cargo test",
            "Edit: bridge/src/x.rs"
        ]
    );
    assert_eq!(
        stream::last_result(STREAM_TEXT).as_deref(),
        Some("Final answer from the result event.")
    );
    assert_eq!(
        pull_request_url("see (https://github.com/o/n/pull/7).").as_deref(),
        Some("https://github.com/o/n/pull/7")
    );
    assert_eq!(pull_request_url("https://github.com/o/n/issues/7"), None);
}

#[tokio::test]
async fn a_strand_entry_routes_a_studio_draft_to_the_studio_and_counts_its_live_run_in_flight() {
    use crate::strandjobs::{candidates, choose, Pick};
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    let t = crate::schedule::StrandTemplate {
        note: "Gd".into(),
        agent: None,
        model: None,
        timeout_minutes: None,
    };
    let strand = crate::strandjobs::load_strand(&s.notes(), "Gd").unwrap();
    let cands = candidates(&s.notes(), &t, &strand, devjob::Agent::Claude, "20261109");
    let first = cands.iter().find_map(|c| c.as_ref().ok()).unwrap();
    assert_eq!(first.step.id, "GD5");
    assert!(first.studio);
    // devjob-host: cluster (GD12) stays on the cluster.
    let gd12 = cands
        .iter()
        .filter_map(|c| c.as_ref().ok())
        .find(|c| c.step.id == "GD12")
        .unwrap();
    assert!(!gd12.studio);
    // A live Studio run of GD5 makes it in flight; the next eligible line (GD10, which names
    // no host and so runs on the cluster) is chosen.
    r.svc.start(item("GD5")).await.unwrap();
    let in_flight = |step: &devjob::StrandRef| r.svc.live_run_of(&step.note, &step.id).is_some();
    match choose(cands, in_flight) {
        Pick::Run(c) => assert_eq!((c.step.id.as_str(), c.studio), ("GD10", false)),
        Pick::Nothing(w) => panic!("{w}"),
    }
    // A Studio report in the Inbox names the step too.
    finish_round(&r, "GD5", 0, Some("ok\n"), "");
    r.svc.pass(now_ms()).await;
    assert!(crate::strandjobs::inbox_reports_step(
        &s.notes(),
        "Gd",
        "GD5"
    ));
    assert!(!crate::strandjobs::inbox_reports_step(
        &s.notes(),
        "Gd",
        "GD12"
    ));
}

#[test]
fn the_run_label_prefix_comes_from_the_bridges_own_label() {
    assert_eq!(
        sys::label_prefix_from(Some("com.tag1.jesse-bridge")),
        "com.tag1.jesse-run."
    );
    assert_eq!(sys::label_prefix_from(Some("0")), sys::DEFAULT_LABEL_PREFIX);
    assert_eq!(sys::label_prefix_from(None), sys::DEFAULT_LABEL_PREFIX);
    assert_eq!(
        sys::label_prefix_from(Some("jesse-bridge")),
        sys::DEFAULT_LABEL_PREFIX
    );
}
