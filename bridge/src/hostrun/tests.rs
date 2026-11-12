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

const NOTE: &str = "---\ngroup: tag1\nstate: active\nupdated: 2026-11-09\n---\n# Gd\n\n**Now:** GD6 merged (PR 12): the thing runs. GD5 is next.\n**Waiting on:** you: pick `main` or a release branch for GD5 (asked 9 Nov); then the rest.\n\n## Drafts\n\
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
    /// Every repository the forge was asked about, in order.
    asked: Mutex<Vec<String>>,
}

impl Forge for FakeForge {
    fn pr_state(&self, repo: &str, branch: &str) -> Option<PrState> {
        lock(&self.asked).push(repo.to_string());
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
            repo: None,
            scheduled: false,
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
    // A strand schedule entry: the draft must still say devjob-repo and devjob-host: studio.
    let ok = studio_item(&n, "Gd", "GD5", OptIn::Schedule).unwrap();
    assert_eq!(ok.repo, "o/n");
    assert_eq!(ok.prompt_path, "Projects/drafts/gd5-prompt.md");
    assert_eq!(ok.section, SECTION);
    let why = |id: &str| studio_item(&n, "Gd", id, OptIn::Schedule).unwrap_err();
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
    assert!(studio_item(&n, "../etc", "GD5", OptIn::Schedule).is_err());
    // A person's start keeps every other check.
    let asked =
        |id: &str| studio_item(&n, "Gd", id, OptIn::Request { repo: Some("o/n") }).unwrap_err();
    assert!(asked("GD6").contains("checked"), "{}", asked("GD6"));
    assert!(asked("GD7").contains("archived"), "{}", asked("GD7"));
    assert!(asked("GD8").contains("no ## PROMPT"), "{}", asked("GD8"));
    assert!(
        asked("GD11").contains("is step GD99, not GD11"),
        "{}",
        asked("GD11")
    );
    assert!(asked("GD404").contains("no line GD404"));
}

#[test]
fn the_served_prompt_is_the_section_then_the_generated_section_exactly() {
    let place = RunPlace {
        run_id: "GD5b",
        repo: "jeremyandrews/jesse-app",
        branch: "gd5b-run-the-thing",
        default_branch: "main",
        run_dir: Path::new("/Users/j/jesse-runs/runs/GD5b"),
        pr_title_tag: true,
    };
    let expected = format!(
        "{SECTION}### Run ID and report\n\nYou are running on the Mac Studio as a Studio run, in a fresh worktree of jeremyandrews/jesse-app on branch gd5b-run-the-thing, cut from origin/main. Wherever the prompt above names another machine, host, user or directory to work in, this worktree replaces it: do not change machines, do not stop because the host differs, and do not work in any other checkout of this repository. If the work needs another repository, clone it under /Users/j/jesse-runs/runs/GD5b/extra/ and work there.\n\nThis run is GD5b. Start every branch name with gd5b- and put [GD5b] at the start of the pull request title. Other runs and people work on this repository at the same time: before your final verification, and again right before any merge, fetch the latest default branch, merge or rebase it into your branch, resolve any conflicts, and re-run your checks. Keep everything you create inside your working directory or the system temporary directory. When you finish, or stop early, write your complete report back, the same text you would answer with, to the file named in the environment variable JESSE_RUN_REPORT, then answer as usual. If you need a decision from Jeremy to continue, stop, make the first line of that report NEEDS YOU: followed by the one question, and end the run.\n"
    );
    assert_eq!(served_prompt(SECTION, &place), expected);
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
    let rec = read_run(&dir).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join(PROMPT_MD)).unwrap(),
        served_prompt(
            SECTION,
            &RunPlace {
                run_id: "GD5",
                repo: "o/n",
                branch: "gd5-run-the-thing-on-the-studio",
                default_branch: "main",
                run_dir: &dir,
                pr_title_tag: false,
            }
        )
    );
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
                item_id: "GD5".into(),
                repo: None,
                scheduled: false,
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

/// Settings with `auto_resume_interrupted = false`.
fn no_auto_resume(s: &Scratch) -> HostRunSettings {
    let mut st = settings(s);
    st.auto_resume_interrupted = false;
    st
}

#[tokio::test]
async fn a_run_whose_label_is_gone_without_an_exit_is_interrupted() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    // With `auto_resume_interrupted = false`, exactly as before it existed.
    let r = rig_with(&s, no_auto_resume(&s));
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
    let r = rig_with(&s, no_auto_resume(&s));
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
    annotate_strands(&mut v, &snap, &s.notes(), Some(&r.svc), "2026-11-09");
    let gd = &v["strands"][0];
    assert_eq!(gd["runs_active"], 0);
    assert_eq!(gd["runs_needs_you"], 0);
    let drafts = gd["drafts"].as_array().unwrap();
    let runnable: Vec<&str> = drafts
        .iter()
        .filter(|d| d["studio_runnable"] == true)
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    // A person's start is the opt in: GD10 (no devjob-host) and GD12 (devjob-host: cluster) run
    // on request too. GD9 names no repository and the strand lists none, so it needs one passed.
    assert_eq!(runnable, vec!["GD5", "GD10", "GD12"]);
    r.svc.start(item("GD5")).await.unwrap();
    let mut v = serde_json::to_value(&snap).unwrap();
    annotate_strands(&mut v, &snap, &s.notes(), Some(&r.svc), "2026-11-09");
    assert_eq!(v["strands"][0]["runs_active"], 1);
    // With [hostrun] off, nothing is runnable.
    let mut v = serde_json::to_value(&snap).unwrap();
    annotate_strands(&mut v, &snap, &s.notes(), None, "2026-11-09");
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

// ---- 6. ROAD READY: ANY DRAFT ON REQUEST, LIMITS, REBOOTS, ENV FILES ---------------------------

/// A draft with no Studio keys at all, for strand `slug` and line `id`.
fn bare_draft(slug: &str, id: &str) -> String {
    format!(
        "---\nstrand: {slug}\nstrand-id: {id}\n---\n# {id}\n\nContext.\n\n## PROMPT\n{SECTION}---\n- [ ] archived\n"
    )
}

/// Strands `One` (repos: [o/n]) and `Many` (repos: [o/n, o/m]) and `None` (no repos), each with
/// one line whose draft carries no `devjob-repo` and no `devjob-host`, plus a loose draft.
fn bare_fixture(s: &Scratch) {
    let n = s.notes();
    std::fs::create_dir_all(n.join("Strands")).unwrap();
    std::fs::create_dir_all(n.join("Projects/drafts/archive")).unwrap();
    for (slug, repos, id) in [
        ("One", "repos: [o/n]\n", "ON1"),
        ("Many", "repos: [o/n, o/m]\n", "MA1"),
        ("None", "", "NO1"),
    ] {
        std::fs::write(
            n.join(format!("Strands/{slug}.md")),
            format!(
                "---\ngroup: tag1\nstate: active\n{repos}updated: 2026-11-09\n---\n# {slug}\n\n## Drafts\n\
                 - [ ] **{id}** Bare draft. [[todo-list/Projects/drafts/{}-prompt]]\n### Done\n",
                id.to_lowercase()
            ),
        )
        .unwrap();
        std::fs::write(
            n.join(format!("Projects/drafts/{}-prompt.md", id.to_lowercase())),
            bare_draft(slug, id),
        )
        .unwrap();
    }
    std::fs::write(
        n.join("Projects/drafts/loose-fix-prompt.md"),
        "---\nstrand: Elsewhere\nstrand-id: ZZ9\n---\n# Loose\n\n## PROMPT\nFix it.\n",
    )
    .unwrap();
    std::fs::write(
        n.join("Projects/drafts/archive/old-prompt.md"),
        "---\n---\n## PROMPT\nOld.\n",
    )
    .unwrap();
}

fn asked(strand: &str, id: &str, repo: Option<&str>) -> StartRequest {
    StartRequest {
        kind: StartKind::Item {
            strand: strand.into(),
            item_id: id.into(),
            repo: repo.map(str::to_string),
            scheduled: false,
        },
        origin: "turn".into(),
        again: false,
    }
}

#[tokio::test]
async fn a_person_launches_any_queued_draft_and_the_repository_resolves_in_order() {
    let s = Scratch::new();
    bare_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    // No front matter keys, the caller's repo: it runs.
    let a = r
        .svc
        .start(asked("None", "NO1", Some("o/n")))
        .await
        .unwrap();
    assert_eq!(a.run_id, "NO1");
    assert_eq!(r.svc.get("NO1").unwrap().repo, "o/n");
    // No repo passed, a one repository strand: that one.
    let b = r.svc.start(asked("One", "ON1", None)).await.unwrap();
    assert_eq!(b.run_id, "ON1");
    assert_eq!(r.svc.get("ON1").unwrap().repo, "o/n");
    // No repo passed, several: refused, naming them.
    let e = r.svc.start(asked("Many", "MA1", None)).await.unwrap_err();
    assert!(matches!(e, Refusal::Invalid(_)), "{e:?}");
    assert!(e.message().contains("o/n, o/m"), "{}", e.message());
    assert!(e.message().contains("pass repo"), "{}", e.message());
    // With one picked, it runs.
    let c = r
        .svc
        .start(asked("Many", "MA1", Some("o/n")))
        .await
        .unwrap();
    assert_eq!(c.run_id, "MA1");
    // None at all: refused.
    let n = s.notes();
    let e = studio_item(&n, "None", "NO1", OptIn::Request { repo: None }).unwrap_err();
    assert!(e.contains("lists no repos"), "{e}");
    // A bad repo is refused by pattern.
    let e = studio_item(&n, "None", "NO1", OptIn::Request { repo: Some("nope") }).unwrap_err();
    assert!(e.contains("not owner/name"), "{e}");
    // The draft's own devjob-repo wins over the caller's.
    vault_fixture(&s);
    let gd = studio_item(&n, "Gd", "GD10", OptIn::Request { repo: Some("x/y") }).unwrap();
    assert_eq!(gd.repo, "o/n");
}

#[tokio::test]
async fn a_strand_schedule_entry_still_needs_both_keys() {
    let s = Scratch::new();
    bare_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    let e = r
        .svc
        .start(StartRequest {
            kind: StartKind::Item {
                strand: "One".into(),
                item_id: "ON1".into(),
                repo: Some("o/n".into()),
                scheduled: true,
            },
            origin: "schedule:x".into(),
            again: false,
        })
        .await
        .unwrap_err();
    assert!(
        e.message().contains("has no devjob-repo"),
        "{}",
        e.message()
    );
    // And a draft with devjob-repo but no devjob-host: studio, as before.
    vault_fixture(&s);
    let e = studio_item(&s.notes(), "Gd", "GD10", OptIn::Schedule).unwrap_err();
    assert!(e.contains("devjob-host: studio"), "{e}");
    assert!(r.svc.get("ON1").is_none());
}

#[tokio::test]
async fn a_draft_by_its_path_runs_as_a_free_run_with_an_s_id() {
    let s = Scratch::new();
    bare_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    let started = r
        .svc
        .start(StartRequest {
            kind: StartKind::Draft {
                draft: "Projects/drafts/loose-fix-prompt.md".into(),
                repo: Some("o/n".into()),
            },
            origin: "turn".into(),
            again: false,
        })
        .await
        .unwrap();
    assert_eq!(started.run_id, "s-loose-fix-prompt");
    let rec = r.svc.get("s-loose-fix-prompt").unwrap();
    assert_eq!(rec.strand, None);
    assert_eq!(
        rec.prompt_path.as_deref(),
        Some("Projects/drafts/loose-fix-prompt.md")
    );
    let dir = r.svc.settings.run_dir("s-loose-fix-prompt");
    assert!(std::fs::read_to_string(dir.join(PROMPT_MD))
        .unwrap()
        .starts_with("Fix it.\n\n### Run ID and report\n\n"));
    // A wiki target resolves too; an archived draft, a path outside the drafts and a missing
    // repository are refused.
    let n = s.notes();
    assert!(studio_draft(
        &n,
        "todo-list/Projects/drafts/loose-fix-prompt",
        Some("o/n")
    )
    .is_ok());
    assert!(
        studio_draft(&n, "Projects/drafts/archive/old-prompt.md", Some("o/n"))
            .unwrap_err()
            .contains("archived")
    );
    assert!(studio_draft(&n, "Strands/One.md", Some("o/n"))
        .unwrap_err()
        .contains("not under"));
    assert!(
        studio_draft(&n, "Projects/drafts/loose-fix-prompt.md", None)
            .unwrap_err()
            .contains("pass repo")
    );
    // The route body and the tool's arguments name the same three ways.
    let body = |v: Value| serde_json::from_value::<StartBody>(v).unwrap().kind();
    assert_eq!(
        body(json!({ "draft": "Projects/drafts/x.md", "repo": "o/n" })).unwrap(),
        StartKind::Draft {
            draft: "Projects/drafts/x.md".into(),
            repo: Some("o/n".into())
        }
    );
    assert_eq!(
        body(json!({ "strand": "Gd", "item_id": "GD5", "repo": "o/n" })).unwrap(),
        StartKind::Item {
            strand: "Gd".into(),
            item_id: "GD5".into(),
            repo: Some("o/n".into()),
            scheduled: false
        }
    );
    assert!(body(json!({ "strand": "Gd", "item_id": "GD5", "repo": "bad" })).is_err());
    assert!(body(json!({ "strand": "Gd" })).is_err());
    assert!(serde_json::from_value::<StartBody>(
        json!({ "strand": "Gd", "item_id": "GD5", "x": 1 })
    )
    .is_err());
    let plan =
        |v: Value| devjob::plan_start(serde_json::from_value(v).unwrap(), devjob::Agent::Claude);
    assert_eq!(
        plan(json!({ "host": "studio", "draft": "Projects/drafts/x.md", "repo": "o/n" })).unwrap(),
        devjob::StartPlan::Studio {
            kind: StartKind::Draft {
                draft: "Projects/drafts/x.md".into(),
                repo: Some("o/n".into())
            },
            again: false
        }
    );
    assert_eq!(
        plan(json!({ "host": "studio", "strand": "Gd", "item_id": "GD5", "repo": "o/n" })).unwrap(),
        devjob::StartPlan::Studio {
            kind: StartKind::Item {
                strand: "Gd".into(),
                item_id: "GD5".into(),
                repo: Some("o/n".into()),
                scheduled: false
            },
            again: false
        }
    );
    assert!(plan(json!({ "host": "studio", "draft": "x", "prompt": "p" })).is_err());
    assert!(
        plan(json!({ "draft": "x", "repo": "a/b", "prompt": "p", "branch_slug": "s" }))
            .unwrap_err()
            .contains("host \"studio\"")
    );
}

#[test]
fn the_generated_section_tags_the_title_only_where_pr_title_tag_says() {
    let dir = Path::new("/Users/j/jesse-runs/runs/AB1");
    let place = |repo: &'static str, tag: bool| RunPlace {
        run_id: "AB1",
        repo,
        branch: "ab1-thing",
        default_branch: "main",
        run_dir: dir,
        pr_title_tag: tag,
    };
    let where_ = |repo: &str| {
        format!(
            "### Run ID and report\n\nYou are running on the Mac Studio as a Studio run, in a fresh worktree of {repo} on branch ab1-thing, cut from origin/main. Wherever the prompt above names another machine, host, user or directory to work in, this worktree replaces it: do not change machines, do not stop because the host differs, and do not work in any other checkout of this repository. If the work needs another repository, clone it under /Users/j/jesse-runs/runs/AB1/extra/ and work there.\n\n"
        )
    };
    let rest = "Other runs and people work on this repository at the same time: before your final verification, and again right before any merge, fetch the latest default branch, merge or rebase it into your branch, resolve any conflicts, and re-run your checks. Keep everything you create inside your working directory or the system temporary directory. When you finish, or stop early, write your complete report back, the same text you would answer with, to the file named in the environment variable JESSE_RUN_REPORT, then answer as usual. If you need a decision from Jeremy to continue, stop, make the first line of that report NEEDS YOU: followed by the one question, and end the run.\n";
    let with = |repo: &str| {
        format!(
            "Do it.\n\n{}This run is AB1. Start every branch name with ab1- and put [AB1] at the start of the pull request title. {rest}",
            where_(repo)
        )
    };
    let without = |repo: &str| {
        format!(
            "Do it.\n\n{}This run is AB1. Start every branch name with ab1-. {rest}",
            where_(repo)
        )
    };
    // The defaults: jeremyandrews tagged, tag1consulting not.
    let st = HostRunSettings::default();
    assert!(st.pr_title_tag("jeremyandrews/jesse-app"));
    assert!(!st.pr_title_tag("tag1consulting/goose"));
    assert_eq!(
        served_prompt("Do it.\n", &place("jeremyandrews/jesse-app", true)),
        with("jeremyandrews/jesse-app")
    );
    assert_eq!(
        served_prompt("Do it.\n", &place("tag1consulting/goose", false)),
        without("tag1consulting/goose")
    );
    // Overridden each way.
    let t: HostRunToml = toml::from_str(
        "enabled = true\n[repos.\"jeremyandrews/quiet\"]\npr_title_tag = false\n\
         [repos.\"tag1consulting/goose\"]\npr_title_tag = true\n",
    )
    .unwrap();
    let st = HostRunSettings::from_toml(Some(t), "/Users/j", "claude");
    assert!(!st.pr_title_tag("jeremyandrews/quiet"));
    assert!(st.pr_title_tag("tag1consulting/goose"));
    assert!(st.pr_title_tag("jeremyandrews/other"));
    // A repos table with no max_concurrent keeps the global per repository cap.
    assert_eq!(st.repo_cap("jeremyandrews/quiet"), st.max_per_repo);
}

#[tokio::test]
async fn the_rendered_prompt_follows_the_repositorys_pr_title_tag() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let mut st = settings(&s);
    st.repo_title_tags.insert("o/n".into(), true);
    let r = rig_with(&s, st);
    r.svc.start(item("GD5")).await.unwrap();
    let dir = r.svc.settings.run_dir("GD5");
    let prompt = std::fs::read_to_string(dir.join(PROMPT_MD)).unwrap();
    assert!(prompt.contains("put [GD5] at the start of the pull request title"));
    assert!(prompt.contains("in a fresh worktree of o/n on branch gd5-run-the-thing-on-the-studio, cut from origin/main."));
    assert!(prompt.contains(&format!("clone it under {}/extra/", dir.display())));
}

// -- Grants --------------------------------------------------------------------------------

#[test]
fn reply_and_cancel_reach_exactly_the_turns_start_reaches_on_every_harness() {
    use crate::harness::direct_gateway_mcp_names;
    use jesse_agent::tools::Level;
    let cfg = crate::testutil::test_config();
    let names = [
        "devjob_start",
        "devjob_reply",
        "devjob_cancel",
        "devjob_close",
    ];
    let row = crate::mcpservers::row(devjob::DEVJOB_SERVER).unwrap();
    for t in names {
        assert_eq!(
            row.class_of(t),
            row.class_of("devjob_start"),
            "{t} is classed as devjob_start is"
        );
    }
    // Claude Code: the write grant names all four; the read grant names none.
    let main = main_mcp_config(&cfg, &ClaudeCode);
    let set = McpSet::from_config(main).unwrap_or(McpSet::None);
    let write = row_allowed_tools(&cfg, Capability::Write, set);
    let read = row_allowed_tools(&cfg, Capability::Read, set);
    for t in names {
        let g = format!("mcp__devjob__{t}");
        assert!(write.split(',').any(|x| x == g), "write turns get {g}");
        assert!(!read.split(',').any(|x| x == g), "read turns never get {g}");
    }
    // Codex: the server's enabled_tools.
    let granted = row.granted_tools(&cfg.allowed_tools);
    for t in names {
        assert!(granted.iter().any(|x| x == t), "{t}");
    }
    // Direct: through the gateway, at write and not at read.
    let w = direct_gateway_mcp_names(&cfg, Level::Write);
    let r = direct_gateway_mcp_names(&cfg, Level::Read);
    for t in names {
        let g = format!("mcp__devjob__{t}");
        assert!(w.contains(&g), "direct write gets {g}");
        assert!(!r.contains(&g), "direct read never gets {g}");
    }
}

// -- The usage limit ------------------------------------------------------------------------

const LIMIT_FIXTURE: &str = include_str!("../../tests/fixtures/hostrun/usage-limit.jsonl");
const LIMIT_NO_RESET_FIXTURE: &str =
    include_str!("../../tests/fixtures/hostrun/usage-limit-no-reset.jsonl");
/// The reset the fixture names: 2026-11-09T17:00:00Z.
const FIXTURE_RESET_SECS: u64 = 1_794_243_600;

#[test]
fn the_usage_limit_is_read_from_the_structured_fields() {
    assert_eq!(
        stream::usage_limit(LIMIT_FIXTURE),
        Some(stream::UsageLimit {
            resets_at_ms: Some(FIXTURE_RESET_SECS * 1000)
        })
    );
    assert_eq!(
        stream::usage_limit(LIMIT_NO_RESET_FIXTURE),
        Some(stream::UsageLimit { resets_at_ms: None })
    );
    // A clean finish, a failure that is not the limit, and prose alone are not the limit.
    assert_eq!(stream::usage_limit(STREAM_TEXT), None);
    let other = r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":500,"result":"API Error: 500"}"#;
    assert_eq!(stream::usage_limit(other), None);
    let prose = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"You've hit your limit · resets 6pm"}]}}
{"type":"result","subtype":"success","is_error":true,"result":"You've hit your limit · resets 6pm"}"#;
    assert_eq!(stream::usage_limit(prose), None);
    // A throttle that shares error rate_limit but is not the account's limit.
    let throttle = r#"{"type":"assistant","message":{"content":[]},"error":"rate_limit","is_api_error_message":true}
{"type":"result","subtype":"success","is_error":true,"api_error_status":429,"result":"Rate limited"}"#;
    assert_eq!(stream::usage_limit(throttle), None);
    // The result's own api_error alone is enough; the reset then comes from a rejected
    // rate_limit_event.
    let result_only = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1794243600}}
{"type":"result","subtype":"success","is_error":true,"api_error":"usage_limit_reached","result":"x"}"#;
    assert_eq!(
        stream::usage_limit(result_only),
        Some(stream::UsageLimit {
            resets_at_ms: Some(FIXTURE_RESET_SECS * 1000)
        })
    );
    // A limit that a later round got past is not this round's: only the last result counts.
    let past = format!("{LIMIT_FIXTURE}{STREAM_TEXT}");
    assert_eq!(stream::usage_limit(&past), None);
}

#[tokio::test]
async fn a_round_on_the_usage_limit_pauses_until_the_reset_with_one_push_and_holds_the_queue() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    r.svc.start(free("after")).await.unwrap();
    assert_eq!(
        r.svc.get("s-after").unwrap().waiting_for.as_deref(),
        Some("repo o/n")
    );
    let reset_secs = now_ms() / 1000 + 2 * 3600;
    let stream = LIMIT_FIXTURE.replace(&FIXTURE_RESET_SECS.to_string(), &reset_secs.to_string());
    finish_round(&r, "GD5", 1, None, &stream);
    r.svc.pass(now_ms()).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Paused);
    assert_eq!(rec.resume_at, Some(rfc(reset_secs * 1000)));
    assert_eq!(rec.round().unwrap().outcome, Some(RunStatus::Paused));
    assert!(rec.limit_pushed);
    // One push, exactly; no Inbox report and no strand turn for a pause.
    let pushes = lock(&r.rec.pushes).clone();
    assert_eq!(pushes.len(), 1, "{pushes:?}");
    assert_eq!(pushes[0].0, "Run paused");
    assert_eq!(
        pushes[0].1,
        format!(
            "GD5 paused for usage limit until {}",
            local_clock(reset_secs * 1000, now_ms())
        )
    );
    assert!(inbox_files(&s).is_empty(), "{:?}", inbox_files(&s));
    assert_eq!(lock(&r.rec.turns).len(), 1, "only the start turn");
    // The queue waits for the limit, and a paused run holds no slot.
    let q = r.svc.get("s-after").unwrap();
    assert_eq!(q.status, RunStatus::Queued);
    assert_eq!(q.waiting_for.as_deref(), Some("limit"));
    assert_eq!(r.svc.list_json()["counts"]["paused"], 1);
    assert!(
        r.svc.live_run_of("Gd", "GD5").is_some(),
        "a paused run is live"
    );
    // Before the reset nothing happens.
    r.svc.pass(now_ms() + 3_600_000).await;
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Paused);
    assert_eq!(lock(&r.launcher.bootstrapped).len(), 1);
    // At the reset: one round, "Continue where you left off.", through the queue.
    let at = reset_secs * 1000 + 1000;
    r.svc.pass(at).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Running);
    assert_eq!(rec.rounds.len(), 2);
    assert_eq!(rec.round().unwrap().text.as_deref(), Some(CONTINUE_TEXT));
    assert_eq!(rec.round().unwrap().auto.as_deref(), Some(AUTO_LIMIT));
    assert_eq!(rec.resume_at, None);
    // Later passes queue nothing more (the launch stamped the real clock, so this asks the
    // two steps directly rather than a whole pass two simulated hours on).
    r.svc.resume_due(at + 60_000);
    r.svc.pump(now_ms());
    assert_eq!(
        r.svc.get("GD5").unwrap().rounds.len(),
        2,
        "one round, not one a pass"
    );
    assert_eq!(
        lock(&r.launcher.bootstrapped)
            .iter()
            .filter(|l| l.ends_with(".GD5"))
            .count(),
        2
    );
    // No longer paused: the queue moves on its usual rules (the repository's one slot).
    assert_eq!(
        r.svc.get("s-after").unwrap().waiting_for.as_deref(),
        Some("repo o/n")
    );
    let pushes = lock(&r.rec.pushes).clone();
    assert_eq!(pushes.len(), 1, "{pushes:?}");
}

#[tokio::test]
async fn with_no_reset_named_it_retries_hourly_with_no_further_push() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    let before = now_ms();
    finish_round(&r, "GD5", 1, None, LIMIT_NO_RESET_FIXTURE);
    r.svc.pass(now_ms()).await;
    let after = now_ms();
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Paused);
    let resume = ms_of(rec.resume_at.as_deref().unwrap()).unwrap();
    assert!(resume >= before + LIMIT_RETRY_MS && resume <= after + LIMIT_RETRY_MS);
    assert_eq!(lock(&r.rec.pushes).len(), 1);
    // Each hour: a round; it meets the limit again; paused again, an hour on, no push.
    for hour in 1..=3u64 {
        r.svc.pass(now_ms() + hour * LIMIT_RETRY_MS + 1000).await;
        let rec = r.svc.get("GD5").unwrap();
        assert_eq!(rec.status, RunStatus::Running, "hour {hour}");
        assert_eq!(rec.rounds.len() as u64, 1 + hour);
        let before = now_ms();
        finish_round(&r, "GD5", 1, None, LIMIT_NO_RESET_FIXTURE);
        r.svc.pass(now_ms()).await;
        let rec = r.svc.get("GD5").unwrap();
        assert_eq!(rec.status, RunStatus::Paused, "hour {hour}");
        let resume = ms_of(rec.resume_at.as_deref().unwrap()).unwrap();
        assert!(resume >= before + LIMIT_RETRY_MS, "hour {hour}");
        assert_eq!(lock(&r.rec.pushes).len(), 1, "no push per retry");
    }
    // A round that gets past the limit reports as usual and clears the pause's push.
    r.svc.pass(now_ms() + 5 * LIMIT_RETRY_MS).await;
    finish_round(&r, "GD5", 0, Some("Done.\n"), STREAM_TEXT);
    r.svc.pass(now_ms()).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    assert!(!rec.limit_pushed);
    assert_eq!(rec.resume_at, None);
    assert_eq!(lock(&r.rec.pushes).len(), 2);
}

#[tokio::test]
async fn a_round_past_the_limit_resumes_every_other_paused_run_and_paused_runs_cancel() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let mut st = settings(&s);
    st.max_per_repo = 3;
    let r = rig_with(&s, st);
    r.svc.start(item("GD5")).await.unwrap();
    r.svc.start(free("one")).await.unwrap();
    r.svc.start(free("two")).await.unwrap();
    finish_round(&r, "GD5", 1, None, LIMIT_NO_RESET_FIXTURE);
    finish_round(&r, "s-one", 1, None, LIMIT_NO_RESET_FIXTURE);
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Paused);
    assert_eq!(r.svc.get("s-one").unwrap().status, RunStatus::Paused);
    assert_eq!(lock(&r.rec.pushes).len(), 2, "one per run that pauses");
    // s-two was already running and gets past it: the others resume at the next pass.
    finish_round(&r, "s-two", 0, Some("ok\n"), STREAM_TEXT);
    r.svc.pass(now_ms()).await;
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Running);
    assert_eq!(r.svc.get("s-one").unwrap().status, RunStatus::Running);
    // A paused run can be cancelled and answered.
    finish_round(&r, "s-one", 1, None, LIMIT_NO_RESET_FIXTURE);
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get("s-one").unwrap().status, RunStatus::Paused);
    let c = r.svc.cancel("s-one").unwrap();
    assert_eq!(c.status, RunStatus::Cancelled);
    assert_eq!(c.resume_at, None);
    finish_round(&r, "GD5", 1, None, LIMIT_NO_RESET_FIXTURE);
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Paused);
    let replied = r.svc.reply("GD5", "Try now.").unwrap();
    assert_eq!(replied.status, RunStatus::Queued);
    assert_eq!(replied.round().unwrap().auto, None);
}

// -- Reboots ------------------------------------------------------------------------------------

#[tokio::test]
async fn an_interrupted_run_gets_one_automatic_round_and_a_second_interruption_waits() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    assert!(r.svc.settings.auto_resume_interrupted, "on by default");
    r.svc.start(item("GD5")).await.unwrap();
    r.launcher.gone("GD5");
    let (fx, _) = r.svc.scan(now_ms() + 11_000);
    assert_eq!(fx.len(), 1, "one push");
    let Effect::Push { body, .. } = &fx[0];
    assert!(body.contains("Resuming it once"), "{body}");
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Queued);
    assert!(rec.restart_resumed);
    assert_eq!(rec.rounds.len(), 2);
    assert_eq!(rec.rounds[0].outcome, Some(RunStatus::Interrupted));
    assert_eq!(rec.round().unwrap().text.as_deref(), Some(RESTART_TEXT));
    assert_eq!(rec.round().unwrap().auto.as_deref(), Some(AUTO_RESTART));
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Running);
    assert_eq!(lock(&r.launcher.bootstrapped).len(), 2);
    // Interrupted again with no completed round between: it waits for a person.
    r.launcher.gone("GD5");
    let (fx, _) = r.svc.scan(now_ms() + 11_000);
    assert_eq!(fx.len(), 1);
    let Effect::Push { body, .. } = &fx[0];
    assert!(body.contains("Reply to resume it"), "{body}");
    r.svc.pass(now_ms() + 12_000).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Interrupted);
    assert_eq!(rec.rounds.len(), 2);
    assert_eq!(lock(&r.launcher.bootstrapped).len(), 2);
    // A person resumes it; that round completes; the next interruption is resumed again.
    r.svc.reply_and_pump("GD5", CONTINUE_TEXT).await.unwrap();
    finish_round(&r, "GD5", 0, Some("done\n"), "");
    r.svc.pass(now_ms()).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    assert!(!rec.restart_resumed);
    r.svc.reply_and_pump("GD5", "More.").await.unwrap();
    r.launcher.gone("GD5");
    r.svc.scan(now_ms() + 11_000);
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Queued);
}

#[test]
fn auto_resume_interrupted_parses_and_defaults_on() {
    let t: HostRunToml =
        toml::from_str("enabled = true\nauto_resume_interrupted = false\n").unwrap();
    assert!(!HostRunSettings::from_toml(Some(t), "/h", "claude").auto_resume_interrupted);
    assert!(HostRunSettings::from_toml(None, "/h", "claude").auto_resume_interrupted);
}

// -- Env files ---------------------------------------------------------------------------------

const ENV_SECRET: &str = "s3cr3t-env-value-7f19";
const ENV_PLAIN: &str = "plain-env-value-c2d4";

#[test]
fn an_env_file_parses_and_its_errors_name_no_value() {
    let s = Scratch::new();
    let p = s.base.join("o-n.env");
    std::fs::write(
        &p,
        format!("# a comment\n\nexport FOO_TOKEN=\"{ENV_SECRET}\"\nBAR = {ENV_PLAIN}\nEMPTY=\n"),
    )
    .unwrap();
    assert_eq!(
        read_env_file(&p).unwrap(),
        vec![
            ("FOO_TOKEN".to_string(), ENV_SECRET.to_string()),
            ("BAR".to_string(), ENV_PLAIN.to_string()),
            ("EMPTY".to_string(), String::new()),
        ]
    );
    std::fs::write(&p, format!("GOOD=1\n{ENV_SECRET}\n")).unwrap();
    let e = read_env_file(&p).unwrap_err();
    assert!(e.contains("line 2"), "{e}");
    assert!(!e.contains(ENV_SECRET), "{e}");
    std::fs::write(&p, format!("9BAD={ENV_SECRET}\n")).unwrap();
    let e = read_env_file(&p).unwrap_err();
    assert!(!e.contains(ENV_SECRET), "{e}");
    let e = read_env_file(&s.base.join("missing.env")).unwrap_err();
    assert!(e.contains("is missing"), "{e}");
    // The runner's three always win over the file.
    let env = runner::agent_env_with(
        Path::new("/r/runs/X"),
        "X",
        |_| None,
        vec![
            ("JESSE_RUN_ID".into(), "forged".into()),
            ("FOO_TOKEN".into(), ENV_SECRET.into()),
        ],
    );
    let last = |k: &str| {
        env.iter()
            .rev()
            .find(|(x, _)| x == k)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(last("JESSE_RUN_ID").as_deref(), Some("X"));
    assert_eq!(last("FOO_TOKEN").as_deref(), Some(ENV_SECRET));
}

#[tokio::test]
async fn an_env_file_is_recorded_by_path_only_and_a_missing_one_refuses_the_start() {
    use crate::gateway::InProcessServer;
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let envf = s.base.join("o-n.env");
    std::fs::write(
        &envf,
        format!("FOO_TOKEN={ENV_SECRET}\nBAR='{ENV_PLAIN}'\n"),
    )
    .unwrap();
    // A missing file refuses the start, and leaves no run behind.
    let mut st = settings(&s);
    st.repo_env_files
        .insert("o/n".into(), s.base.join("nowhere.env"));
    let r = rig_with(&s, st);
    let e = r.svc.start(free("envless")).await.unwrap_err();
    assert!(matches!(e, Refusal::Invalid(_)), "{e:?}");
    assert!(e.message().contains("is missing"), "{}", e.message());
    assert!(r.svc.get("s-envless").is_none());
    drop(r);

    let mut st = settings(&s);
    st.repo_env_files.insert("o/n".into(), envf.clone());
    let r = rig_with(&s, st);
    r.svc.start(item("GD5")).await.unwrap();
    let dir = r.svc.settings.run_dir("GD5");
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.env_file.as_deref(), Some(envf.to_str().unwrap()));
    finish_round(&r, "GD5", 0, Some("Done.\n"), STREAM_TEXT);
    r.svc.pass(now_ms()).await;
    // Every file the bridge writes, every route body and every tool's output.
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
    dj.attach_studio(r.svc.clone());
    let mut seen: Vec<(String, String)> = Vec::new();
    for f in [RUN_JSON, JOB_PLIST, PROMPT_MD, SECTION_MD, REPORT_MD] {
        seen.push((
            f.to_string(),
            std::fs::read_to_string(dir.join(f)).unwrap_or_default(),
        ));
    }
    for f in inbox_files(&s) {
        seen.push((
            f.clone(),
            std::fs::read_to_string(s.notes().join("Inbox").join(&f)).unwrap(),
        ));
    }
    seen.push(("run_json".into(), run_json(&rec).to_string()));
    seen.push(("list_json".into(), r.svc.list_json().to_string()));
    seen.push((
        "log".into(),
        r.svc.log_lines("GD5", LOG_MAX).unwrap().join("\n"),
    ));
    for (tool, args) in [
        (devjob::TOOL_STATUS, json!({ "job_name": "GD5" })),
        (devjob::TOOL_LIST, json!({})),
        (
            devjob::TOOL_REPLY,
            json!({ "run_id": "GD5", "text": "More." }),
        ),
        (devjob::TOOL_CANCEL, json!({ "run_id": "GD5" })),
    ] {
        seen.push((
            tool.to_string(),
            dj.call(tool, args).await.unwrap().to_string(),
        ));
    }
    seen.push((
        "pushes".into(),
        format!("{:?}{:?}", lock(&r.rec.pushes), lock(&r.rec.turns)),
    ));
    for (what, text) in &seen {
        assert!(!text.is_empty() || what == REPORT_MD, "{what} was read");
        for v in [ENV_SECRET, ENV_PLAIN] {
            assert!(!text.contains(v), "{what} carries an env file value");
        }
    }
}

// -- extra/ ------------------------------------------------------------------------------------

#[tokio::test]
async fn extra_goes_with_the_artifacts_and_never_while_the_run_is_live() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    let day = 24 * 3_600_000;
    r.svc.start(item("GD5")).await.unwrap();
    let dir = r.svc.settings.run_dir("GD5");
    std::fs::create_dir_all(dir.join("extra/o-m/.git")).unwrap();
    std::fs::write(dir.join("extra/o-m/work.txt"), "unpushed").unwrap();
    // Running: neither the cleanup nor the space pass touches it.
    r.svc.cleanup(now_ms() + 30 * day);
    let mut st = r.svc.settings.clone();
    st.max_total_bytes = 0;
    let tight = HostRuns::new(st, s.notes(), r.launcher.clone(), r.forge.clone());
    tight.space_pass(now_ms());
    assert!(dir.join("extra/o-m/work.txt").is_file());
    // Paused for the usage limit: still kept, past keep_days and under a space pass.
    finish_round(&r, "GD5", 1, None, LIMIT_NO_RESET_FIXTURE);
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Paused);
    r.svc.cleanup(now_ms() + 30 * day);
    tight.space_pass(now_ms() + 30 * day);
    assert!(dir.join("extra/o-m/work.txt").is_file());
    // Needs you, past keep_days: target/ goes, extra/ stays (the run is still live).
    update_run(&dir, |x| {
        x.status = RunStatus::NeedsYou;
        x.finished_at = Some(rfc(now_ms()));
    })
    .unwrap();
    r.svc.cleanup(now_ms() + 8 * day);
    assert!(!dir.join(TARGET_DIR).exists());
    assert!(dir.join("extra/o-m/work.txt").is_file());
    // Reported and merged: extra/ goes with the artifacts; the record stays.
    std::fs::create_dir_all(dir.join(TARGET_DIR)).unwrap();
    update_run(&dir, |x| x.status = RunStatus::Reported).unwrap();
    let rec = r.svc.get("GD5").unwrap();
    lock(&r.forge.states).insert(rec.branch.clone(), PrState::Merged);
    r.svc.cleanup(now_ms());
    assert!(!dir.join(EXTRA_DIR).exists());
    assert!(!dir.join(TARGET_DIR).exists());
    assert!(dir.join(RUN_JSON).is_file());
}

// ---- 6. THE APP'S WIRE FIXTURES ---------------------------------------------------------------

/// Every key path in `v`, with an array's elements folded into `[]`, so two documents with the
/// same shape and different values compare equal.
fn key_paths(v: &Value, at: &str, out: &mut std::collections::BTreeSet<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                let p = format!("{at}.{k}");
                out.insert(p.clone());
                key_paths(x, &p, out);
            }
        }
        Value::Array(a) => {
            for x in a {
                key_paths(x, &format!("{at}[]"), out);
            }
        }
        _ => {}
    }
}

/// The app decodes the run routes from JSON these serializers wrote: `run_json`, `list_json`,
/// `started_json`, `log_json` and `annotate_strands`, over a scratch vault and real rounds driven
/// to `needs_you`, `reported`, `paused` and `queued`. The files live in JesseKit's
/// test fixtures. `JESSE_RUN_FIXTURE_WRITE=1 cargo test the_app_wire_fixtures` rewrites them;
/// otherwise this fails when a serializer gains or loses a key the committed files do not have,
/// which is the moment the app's decoding has to be looked at again.
#[tokio::test]
async fn the_app_wire_fixtures_match_what_the_run_routes_serialize() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    // GD5: a round that ends asking a question.
    r.svc.start(item("GD5")).await.unwrap();
    finish_round(
        &r,
        "GD5",
        0,
        Some("NEEDS YOU: main or release?\nThe branch is pushed.\n"),
        STREAM_TEXT,
    );
    r.svc.pass(now_ms()).await;
    // A free run that reports.
    r.svc.start(free("done")).await.unwrap();
    finish_round(&r, "s-done", 0, Some("Merged.\n"), STREAM_TEXT);
    r.svc.pass(now_ms()).await;
    // GD10: a round on the usage limit, paused until the reset.
    r.svc.start(item("GD10")).await.unwrap();
    let reset_secs = now_ms() / 1000 + 2 * 3600;
    let limit = LIMIT_FIXTURE.replace(&FIXTURE_RESET_SECS.to_string(), &reset_secs.to_string());
    finish_round(&r, "GD10", 1, None, &limit);
    r.svc.pass(now_ms()).await;
    // GD12 is queued behind the limit.
    let started = r.svc.start(item("GD12")).await.unwrap();

    let status = |id: &str| r.svc.get(id).unwrap().status;
    assert_eq!(status("GD5"), RunStatus::NeedsYou);
    assert_eq!(status("GD10"), RunStatus::Paused);
    assert_eq!(status("s-done"), RunStatus::Reported);
    assert_eq!(status("GD12"), RunStatus::Queued);
    assert_eq!(started.waiting_for.as_deref(), Some("limit"));

    let snap = crate::strands::snapshot(&s.notes(), "2026-11-09");
    let plain = serde_json::to_value(&snap).unwrap();
    let mut annotated = plain.clone();
    annotate_strands(
        &mut annotated,
        &snap,
        &s.notes(),
        Some(&r.svc),
        "2026-11-09",
    );
    // Needs you and paused; a queued run is not counted. One of the two needs Jeremy.
    assert_eq!(annotated["strands"][0]["runs_active"], 2);
    assert_eq!(annotated["strands"][0]["runs_needs_you"], 1);
    let log = log_json("GD5", r.svc.log_lines("GD5", LOG_DEFAULT).unwrap());
    assert!(!log["lines"].as_array().unwrap().is_empty());

    let base = s.base.to_string_lossy().into_owned();
    // The list as the app polls it, before anything is archived.
    let list = r.svc.list_json();
    // Then the reported run archived by hand: `?archived=all` carries `archived_at` and
    // `archived_reason` on it. (`studio-runs-list-0.203.json` is this test's output at bridge
    // 0.203.0, before either field or the filter counts, and is kept as written then.)
    r.svc.archive("s-done").unwrap();
    let with_archived = r.svc.list_json_mode(ArchivedMode::All);
    assert_eq!(with_archived["counts"]["archived"], 1);
    let files: Vec<(&str, Value)> = vec![
        ("studio-runs-list.json", list),
        ("studio-runs-list-archived.json", with_archived),
        // A run as `run_json` alone serialized it before the Work view: none of its fields.
        (
            "studio-run-needs-you.json",
            run_json(&r.svc.get("GD5").unwrap()),
        ),
        // The same run as the routes serve it now: `title`, `last_line`, `report_line`.
        (
            "studio-run-needs-you-served.json",
            r.svc.served_one(&r.svc.get("GD5").unwrap()),
        ),
        // `GET /jesse/work/last-look`'s body, before the first look and after one.
        ("work-last-look-none.json", work::last_look_json(None)),
        (
            "work-last-look.json",
            work::last_look_json(Some(&rfc(now_ms()))),
        ),
        (
            "studio-run-paused.json",
            run_json(&r.svc.get("GD10").unwrap()),
        ),
        ("studio-run-started.json", started_json(&started)),
        ("studio-run-log.json", log),
        ("strands-annotated.json", annotated),
        ("strands-before-runs.json", plain),
    ];
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../JesseKit/Tests/JesseNetworkingTests/Fixtures");
    for (name, v) in files {
        // The scratch folder's path is this machine's and this run's; the app never reads it.
        let text = serde_json::to_string_pretty(&v)
            .unwrap()
            .replace(&base, "/scratch")
            + "\n";
        let path = dir.join(name);
        if std::env::var("JESSE_RUN_FIXTURE_WRITE").is_ok() {
            std::fs::write(&path, text).unwrap();
            continue;
        }
        let committed: Value = serde_json::from_str(
            &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
        )
        .unwrap();
        let (mut want, mut have) = (Default::default(), Default::default());
        key_paths(&committed, "", &mut want);
        key_paths(&v, "", &mut have);
        assert_eq!(
            want, have,
            "{name}: the serializer's keys moved; regenerate with JESSE_RUN_FIXTURE_WRITE=1 and \
             check the app's decoding"
        );
    }
}

// ---- SETTLED RUNS CLOSE THEMSELVES (0.200.0) ---------------------------------------------

/// Drive `id`'s current round to `needs_you`.
async fn to_needs_you(r: &Rig, id: &str) {
    finish_round(r, id, 0, Some("NEEDS YOU: which one?\n"), "");
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get(id).unwrap().status, RunStatus::NeedsYou);
}

/// Drive `id`'s current round to `interrupted` (with `auto_resume_interrupted` off).
fn to_interrupted(r: &Rig, id: &str) {
    r.launcher.gone(id);
    r.svc.scan(now_ms() + 11_000);
    assert_eq!(r.svc.get(id).unwrap().status, RunStatus::Interrupted);
}

fn again(id: &str) -> StartRequest {
    StartRequest {
        again: true,
        ..item(id)
    }
}

fn closed_reason(r: &Rig, id: &str) -> Option<String> {
    let rec = r.svc.get(id).unwrap();
    assert_eq!(
        rec.status == RunStatus::Closed,
        rec.closed_reason.is_some(),
        "{id}: closed_reason is set exactly while closed"
    );
    rec.closed_reason
}

#[test]
fn closed_is_terminal_not_live_and_serializes_as_closed() {
    let c = RunStatus::Closed;
    assert_eq!(c.label(), "closed");
    assert_eq!(c.outcome(), "closed");
    assert!(c.is_terminal() && c.is_finished() && !c.is_live() && !c.is_settleable());
    assert_eq!(serde_json::to_value(c).unwrap(), json!("closed"));
    assert_eq!(
        serde_json::from_value::<RunStatus>(json!("closed")).unwrap(),
        c
    );
    // Only the two waiting statuses are settled by the automatic paths.
    for s in [
        RunStatus::Queued,
        RunStatus::Running,
        RunStatus::Paused,
        RunStatus::Reported,
        RunStatus::Failed,
        RunStatus::Cancelled,
    ] {
        assert!(!s.is_settleable(), "{s:?}");
    }
    assert!(RunStatus::NeedsYou.is_settleable() && RunStatus::Interrupted.is_settleable());
}

#[test]
fn a_record_from_before_closed_still_reads_and_a_closed_one_round_trips() {
    // Written by bridge 0.198.0: no closed_* keys.
    let old = include_str!(
        "../../../JesseKit/Tests/JesseNetworkingTests/Fixtures/studio-run-needs-you.json"
    );
    let rec: RunRecord = serde_json::from_str(old).unwrap();
    assert_eq!(rec.status, RunStatus::NeedsYou);
    assert_eq!((rec.closed_reason, rec.closed_at), (None, None));
    // A record that is not closed serializes exactly the keys it did before.
    let v = serde_json::to_value(RunRecord::default()).unwrap();
    assert!(v.get("closed_reason").is_none() && v.get("closed_at").is_none());
    // A closed one round trips through run.json.
    let s = Scratch::new();
    let st = settings(&s);
    fake_run(&st, "GD5", RunStatus::Closed, Some(1_000), 1);
    let dir = st.run_dir("GD5");
    update_run(&dir, |r| {
        r.closed_reason = Some("superseded by GD5b".into());
        r.closed_at = Some(rfc(2_000));
    })
    .unwrap();
    let back = read_run(&dir).unwrap();
    assert_eq!(back.status, RunStatus::Closed);
    assert_eq!(back.closed_reason.as_deref(), Some("superseded by GD5b"));
    assert_eq!(back.closed_at, Some(rfc(2_000)));
    let text = std::fs::read_to_string(dir.join(RUN_JSON)).unwrap();
    assert!(text.contains("\"status\": \"closed\""), "{text}");
    assert_eq!(serde_json::from_str::<RunRecord>(&text).unwrap(), back);
}

#[tokio::test]
async fn again_closes_the_earlier_waiting_runs_of_the_item_and_leaves_live_ones() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig_with(&s, no_auto_resume(&s));
    // GD5 asks a question; GD5b supersedes it.
    r.svc.start(item("GD5")).await.unwrap();
    to_needs_you(&r, "GD5").await;
    let pushes = lock(&r.rec.pushes).len();
    let turns = lock(&r.rec.turns).len();
    assert_eq!(r.svc.start(again("GD5")).await.unwrap().run_id, "GD5b");
    assert_eq!(
        closed_reason(&r, "GD5").as_deref(),
        Some("superseded by GD5b")
    );
    assert!(r.svc.get("GD5").unwrap().closed_at.is_some());
    // GD5b is interrupted; GD5c supersedes it.
    to_interrupted(&r, "GD5b");
    assert_eq!(r.svc.start(again("GD5")).await.unwrap().run_id, "GD5c");
    assert_eq!(r.svc.get("GD5c").unwrap().status, RunStatus::Running);
    assert_eq!(
        closed_reason(&r, "GD5b").as_deref(),
        Some("superseded by GD5c")
    );
    // GD5d waits for the repository behind GD5c; GD5e touches neither.
    let d = r.svc.start(again("GD5")).await.unwrap();
    assert_eq!((d.run_id.as_str(), d.status), ("GD5d", RunStatus::Queued));
    r.svc.start(again("GD5")).await.unwrap();
    assert_eq!(r.svc.get("GD5c").unwrap().status, RunStatus::Running);
    assert_eq!(r.svc.get("GD5d").unwrap().status, RunStatus::Queued);
    assert_eq!(closed_reason(&r, "GD5c"), None);
    assert_eq!(closed_reason(&r, "GD5d"), None);
    // Closing is silent: the only new turns are the start turns, and no push at all.
    assert_eq!(lock(&r.rec.pushes).len(), pushes);
    let new_turns: Vec<String> = lock(&r.rec.turns)[turns..].to_vec();
    assert!(
        new_turns.iter().all(|t| t.starts_with("Run GD5")),
        "{new_turns:?}"
    );
    assert_eq!(new_turns.len(), 4);
}

#[tokio::test]
async fn a_plain_start_does_not_supersede_and_a_paused_run_is_never_closed_by_again() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig_with(&s, no_auto_resume(&s));
    // An interrupted run is not live, so a plain start goes ahead, and leaves it as it was.
    r.svc.start(item("GD5")).await.unwrap();
    to_interrupted(&r, "GD5");
    assert_eq!(r.svc.start(item("GD5")).await.unwrap().run_id, "GD5b");
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Interrupted);
    // GD5b pauses for the usage limit; a further start with again leaves it paused.
    let reset_secs = now_ms() / 1000 + 2 * 3600;
    let limit = LIMIT_FIXTURE.replace(&FIXTURE_RESET_SECS.to_string(), &reset_secs.to_string());
    finish_round(&r, "GD5b", 1, None, &limit);
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get("GD5b").unwrap().status, RunStatus::Paused);
    r.svc.start(again("GD5")).await.unwrap();
    assert_eq!(r.svc.get("GD5b").unwrap().status, RunStatus::Paused);
    assert_eq!(closed_reason(&r, "GD5b"), None);
    assert_eq!(
        closed_reason(&r, "GD5").as_deref(),
        Some("superseded by GD5c")
    );
}

#[tokio::test]
async fn a_closed_run_is_not_live_not_active_and_no_longer_blocks_a_start() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    to_needs_you(&r, "GD5").await;
    let snap = crate::strands::snapshot(&s.notes(), "2026-11-09");
    let annotated = |r: &Rig| {
        let mut v = serde_json::to_value(&snap).unwrap();
        annotate_strands(&mut v, &snap, &s.notes(), Some(&r.svc), "2026-11-09");
        (
            v["strands"][0]["runs_active"].clone(),
            v["strands"][0]["runs_needs_you"].clone(),
        )
    };
    assert_eq!(annotated(&r), (json!(1), json!(1)));
    assert_eq!(r.svc.active_count("Gd"), 1);
    assert!(r.svc.live_run_of("Gd", "GD5").is_some());
    assert!(matches!(
        r.svc.start(item("GD5")).await,
        Err(Refusal::Live(_))
    ));
    let closed = r.svc.close("GD5", None).unwrap();
    assert_eq!(closed.status, RunStatus::Closed);
    assert_eq!(closed.closed_reason.as_deref(), Some(CLOSED_BY_JEREMY));
    assert_eq!(annotated(&r), (json!(0), json!(0)));
    assert_eq!(r.svc.active_count("Gd"), 0);
    assert!(r.svc.live_run_of("Gd", "GD5").is_none());
    let counts = &r.svc.list_json()["counts"];
    assert_eq!(counts["needs_you"], 0);
    // A plain start of the item goes ahead.
    assert_eq!(r.svc.start(item("GD5")).await.unwrap().run_id, "GD5b");
}

#[tokio::test]
async fn a_ticked_line_closes_a_waiting_run_and_an_unticked_one_does_not() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig_with(&s, no_auto_resume(&s));
    r.svc.start(item("GD5")).await.unwrap();
    to_needs_you(&r, "GD5").await;
    r.svc.start(item("GD10")).await.unwrap();
    to_interrupted(&r, "GD10");
    r.svc.start(item("GD12")).await.unwrap();
    assert_eq!(r.svc.get("GD12").unwrap().status, RunStatus::Running);
    let pushes = lock(&r.rec.pushes).len();
    let turns = lock(&r.rec.turns).len();
    // Nothing ticked: nothing closes.
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.close_settled(now_ms()), vec![]);
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::NeedsYou);
    // A note that can no longer be found closes nothing.
    let note = s.notes().join("Strands/Gd.md");
    std::fs::rename(&note, s.notes().join("Strands/Gd.md.away")).unwrap();
    assert_eq!(r.svc.close_settled(now_ms()), vec![]);
    // Nor does a line that is gone from the note.
    std::fs::write(
        &note,
        NOTE.replace(
            "- [ ] **GD5** Run the thing on the Studio. [[todo-list/Projects/drafts/gd5-prompt]]\n",
            "",
        ),
    )
    .unwrap();
    assert_eq!(r.svc.close_settled(now_ms()), vec![]);
    // GD5 ticked, GD10 moved under ### Done unchecked, GD12 (running) ticked.
    let ticked = NOTE
        .replace("- [ ] **GD5**", "- [x] **GD5**")
        .replace("- [ ] **GD12**", "- [x] **GD12**")
        .replace(
            "- [ ] **GD10** No host. [[todo-list/Projects/drafts/gd10-prompt]]\n",
            "",
        )
        + "- [ ] **GD10** No host. [[todo-list/Projects/drafts/gd10-prompt]]\n";
    std::fs::write(&note, ticked).unwrap();
    let parsed = parse_strand("Gd", &std::fs::read_to_string(&note).unwrap());
    let gd10 = parsed.items.iter().find(|i| i.id == "GD10").unwrap();
    assert_eq!((gd10.section, gd10.checked), (StrandSection::Done, false));
    r.svc.pass(now_ms()).await;
    assert_eq!(
        closed_reason(&r, "GD5").as_deref(),
        Some(CLOSED_ITEM_TICKED)
    );
    assert_eq!(
        closed_reason(&r, "GD10").as_deref(),
        Some(CLOSED_ITEM_TICKED)
    );
    // Never a running run.
    assert_eq!(r.svc.get("GD12").unwrap().status, RunStatus::Running);
    // Silent.
    assert_eq!(lock(&r.rec.pushes).len(), pushes);
    assert_eq!(lock(&r.rec.turns).len(), turns);
    // Once.
    assert_eq!(r.svc.close_settled(now_ms()), vec![]);
}

#[tokio::test]
async fn an_archived_report_closes_a_needs_you_run_and_a_deleted_one_does_not() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    to_needs_you(&r, "GD5").await;
    r.svc.start(free("other")).await.unwrap();
    to_needs_you(&r, "s-other").await;
    let rel = |id: &str| {
        r.svc
            .get(id)
            .unwrap()
            .round()
            .unwrap()
            .report_path
            .clone()
            .unwrap()
    };
    let (gd5, other) = (rel("GD5"), rel("s-other"));
    let archive = s.notes().join(INBOX_ARCHIVE_DIR);
    std::fs::create_dir_all(&archive).unwrap();
    let name = |p: &str| p.rsplit('/').next().unwrap().to_string();
    // A copy in the archive while the report is still in the Inbox: not settled.
    std::fs::copy(s.notes().join(&gd5), archive.join(name(&gd5))).unwrap();
    assert_eq!(r.svc.close_settled(now_ms()), vec![]);
    // Moved: settled. Deleted: not.
    std::fs::remove_file(s.notes().join(&gd5)).unwrap();
    std::fs::remove_file(s.notes().join(&other)).unwrap();
    r.svc.pass(now_ms()).await;
    assert_eq!(
        closed_reason(&r, "GD5").as_deref(),
        Some(CLOSED_REPORT_ARCHIVED)
    );
    assert_eq!(r.svc.get("s-other").unwrap().status, RunStatus::NeedsYou);
    // A second round's report counts too: both have to be gone.
    r.svc.reply_and_pump("s-other", "go on").await.unwrap();
    to_needs_you(&r, "s-other").await;
    let rounds: Vec<String> = r
        .svc
        .get("s-other")
        .unwrap()
        .rounds
        .iter()
        .filter_map(|x| x.report_path.clone())
        .collect();
    assert_eq!(rounds.len(), 2);
    // Round 1's is deleted; round 2's still in the Inbox.
    assert_eq!(r.svc.close_settled(now_ms()), vec![]);
    std::fs::rename(s.notes().join(&rounds[1]), archive.join(name(&rounds[1]))).unwrap();
    assert_eq!(
        r.svc.close_settled(now_ms()),
        vec![("s-other".to_string(), CLOSED_REPORT_ARCHIVED.to_string())]
    );
}

#[tokio::test]
async fn an_archived_report_does_not_close_an_interrupted_or_reported_run() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig_with(&s, no_auto_resume(&s));
    r.svc.start(free("done")).await.unwrap();
    finish_round(&r, "s-done", 0, Some("Merged.\n"), "");
    r.svc.pass(now_ms()).await;
    // An interrupted round after a reported one: its only report is round 1's.
    r.svc.reply_and_pump("s-done", "more").await.unwrap();
    to_interrupted(&r, "s-done");
    let rel = r.svc.get("s-done").unwrap().rounds[0]
        .report_path
        .clone()
        .unwrap();
    let archive = s.notes().join(INBOX_ARCHIVE_DIR);
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::rename(
        s.notes().join(&rel),
        archive.join(rel.rsplit('/').next().unwrap()),
    )
    .unwrap();
    assert_eq!(r.svc.close_settled(now_ms()), vec![]);
    assert_eq!(r.svc.get("s-done").unwrap().status, RunStatus::Interrupted);
}

#[tokio::test]
async fn close_accepts_needs_you_interrupted_and_failed_and_refuses_the_rest() {
    let s = Scratch::new();
    let st = settings(&s);
    let r = rig_with(&s, st.clone());
    for (id, status) in [
        ("n", RunStatus::NeedsYou),
        ("i", RunStatus::Interrupted),
        ("f", RunStatus::Failed),
        ("q", RunStatus::Queued),
        ("r", RunStatus::Running),
        ("p", RunStatus::Paused),
        ("d", RunStatus::Reported),
        ("c", RunStatus::Cancelled),
        ("x", RunStatus::Closed),
    ] {
        fake_run(&st, id, status, Some(1_000), 1);
    }
    let ok = r.svc.close("n", None).unwrap();
    assert_eq!(ok.closed_reason.as_deref(), Some("closed by Jeremy"));
    assert_eq!(
        r.svc
            .close("i", Some("  done by hand  "))
            .unwrap()
            .closed_reason
            .as_deref(),
        Some("done by hand")
    );
    assert_eq!(
        r.svc.close("f", Some("")).unwrap().closed_reason.as_deref(),
        Some(CLOSED_BY_JEREMY)
    );
    for (id, label) in [
        ("q", "queued"),
        ("r", "running"),
        ("p", "paused"),
        ("d", "reported"),
        ("c", "cancelled"),
        ("x", "closed"),
    ] {
        let e = r.svc.close(id, None).unwrap_err();
        assert_eq!(e.status(), StatusCode::CONFLICT, "{id}");
        assert_eq!(
            e.message(),
            format!("{id} is {label}; only a needs_you, interrupted or failed run can be closed")
        );
    }
    // The refused ones are exactly as they were.
    assert_eq!(r.svc.get("r").unwrap().status, RunStatus::Running);
    assert_eq!(r.svc.get("q").unwrap().status, RunStatus::Queued);
    assert_eq!(r.svc.get("p").unwrap().status, RunStatus::Paused);
    assert_eq!(
        r.svc.close("nope", None).unwrap_err().status(),
        StatusCode::NOT_FOUND
    );
    let long = "x".repeat(MAX_CLOSE_REASON_BYTES + 1);
    fake_run(&st, "n2", RunStatus::NeedsYou, Some(1_000), 1);
    assert_eq!(
        r.svc.close("n2", Some(&long)).unwrap_err().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        r.svc.close("n2", Some("two\nlines")).unwrap_err().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(r.svc.get("n2").unwrap().status, RunStatus::NeedsYou);
    // The route's body: empty, or the object with only `reason`.
    assert_eq!(CloseBody::parse(b"").unwrap().reason, None);
    assert_eq!(CloseBody::parse(b"  \n").unwrap().reason, None);
    assert_eq!(CloseBody::parse(b"{}").unwrap().reason, None);
    assert_eq!(
        CloseBody::parse(br#"{"reason":"merged"}"#)
            .unwrap()
            .reason
            .as_deref(),
        Some("merged")
    );
    assert!(CloseBody::parse(br#"{"why":"x"}"#).is_err());
    assert!(CloseBody::parse(b"not json").is_err());
}

#[tokio::test]
async fn a_reply_to_a_closed_run_is_taken_as_a_reply_to_a_reported_one() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    to_needs_you(&r, "GD5").await;
    r.svc.close("GD5", Some("settled")).unwrap();
    let rec = r.svc.reply_and_pump("GD5", "one more thing").await.unwrap();
    assert_eq!(rec.status, RunStatus::Running);
    assert_eq!(rec.rounds.len(), 2);
    assert_eq!((rec.closed_reason, rec.closed_at), (None, None));
}

#[tokio::test]
async fn the_devjob_close_tool_closes_by_status_and_says_why_it_refuses() {
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
    // Without the Studio the tool is not there.
    assert!(dj
        .call(devjob::TOOL_CLOSE, json!({ "run_id": "GD5" }))
        .await
        .is_err());
    dj.attach_studio(r.svc.clone());
    let listed = dj.tools();
    let close = listed
        .iter()
        .find(|t| t["name"] == devjob::TOOL_CLOSE)
        .expect("listed with the Studio");
    assert_eq!(close["inputSchema"]["required"], json!(["run_id"]));
    r.svc.start(item("GD5")).await.unwrap();
    // Running: refused, and the run goes on.
    let refused = dj
        .call(devjob::TOOL_CLOSE, json!({ "run_id": "GD5" }))
        .await
        .unwrap();
    assert_eq!(refused["isError"], true);
    assert_eq!(
        refused["content"][0]["text"],
        "GD5 is running; only a needs_you, interrupted or failed run can be closed"
    );
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Running);
    to_needs_you(&r, "GD5").await;
    let closed = dj
        .call(
            devjob::TOOL_CLOSE,
            json!({ "run_id": "GD5", "reason": "merged by hand" }),
        )
        .await
        .unwrap();
    assert_eq!(closed["isError"], false, "{closed}");
    let v = tool_text(&closed);
    assert_eq!(v["status"], "closed");
    assert_eq!(v["closed_reason"], "merged by hand");
    // Unknown arguments are refused, as on every devjob tool.
    let bad = dj
        .call(devjob::TOOL_CLOSE, json!({ "run_id": "GD5", "why": "x" }))
        .await
        .unwrap();
    assert_eq!(bad["isError"], true);
    // devjob_list files it among the finished.
    let list = tool_text(&dj.call(devjob::TOOL_LIST, json!({})).await.unwrap());
    assert!(list["studio"]["finished"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["run_id"] == "GD5" && x["status"] == "closed"));
    assert!(list["studio"]["active"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_closed_run_follows_the_terminal_retention() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    to_needs_you(&r, "GD5").await;
    r.svc.close("GD5", None).unwrap();
    let dir = r.svc.settings.run_dir("GD5");
    assert!(dir.join("worktree").exists());
    // Past keep_days (7), well short of the 30 a waiting run keeps: the worktree goes.
    r.svc.cleanup(now_ms() + 8 * 24 * 3_600_000);
    assert!(!dir.join("worktree").exists());
    assert!(!dir.join("target").exists());
    assert!(dir.join(RUN_JSON).is_file());
}

#[tokio::test]
async fn a_report_filed_under_the_inbox_archive_is_served_where_it_now_lives() {
    // THE DEFECT: the Inbox is processed, a read report moves to `Inbox/archive/`, and the
    // path `run.json` recorded names a note that is gone; the app's Report link opened
    // "doesn't exist on the Studio".
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    finish_round(&r, "GD5", 0, Some("Done.\n"), STREAM_TEXT);
    r.svc.pass(now_ms()).await;
    let rel = format!("Inbox/{}-run-GD5.md", today());
    let archived = format!("Inbox/archive/{}-run-GD5.md", today());

    // Before the move, the recorded path.
    let listed = r.svc.list_json();
    assert_eq!(listed["runs"][0]["report_paths"], json!([rel.clone()]));

    std::fs::create_dir_all(s.notes().join("Inbox/archive")).unwrap();
    std::fs::rename(s.notes().join(&rel), s.notes().join(&archived)).unwrap();

    // After it, the list and the run both serve the note's current path; the record keeps
    // what it recorded.
    let listed = r.svc.list_json();
    assert_eq!(listed["runs"][0]["report_paths"], json!([archived.clone()]));
    let one = run_json_located(&r.svc.get("GD5").unwrap(), r.svc.notes_root());
    assert_eq!(one["report_paths"], json!([archived.clone()]));
    assert_eq!(
        r.svc
            .get("GD5")
            .unwrap()
            .round()
            .unwrap()
            .report_path
            .as_deref(),
        Some(rel.as_str())
    );

    // A report gone from both places is served as recorded: nothing better is known.
    std::fs::remove_file(s.notes().join(&archived)).unwrap();
    assert_eq!(
        locate_report(r.svc.notes_root(), &rel),
        rel,
        "a missing note keeps its recorded path"
    );
}

// ---- LOCAL REPOSITORIES (0.202.0) -----------------------------------------------------

fn free_in(repo: &str, slug: &str) -> StartRequest {
    StartRequest {
        kind: StartKind::Free {
            repo: repo.into(),
            prompt: "Write a file.".into(),
            slug: slug.into(),
            model: None,
        },
        origin: "turn".into(),
        again: false,
    }
}

/// What an agent on a local repository does: commit a file on its branch, then fast forward
/// the repository's `main` to it.
fn commit_and_merge(wt: &Path, file: &str, text: &str) {
    std::fs::write(wt.join(file), text).unwrap();
    sh(wt, &["add", file]);
    sh(wt, &["commit", "-q", "-m", file]);
    sh(wt, &["push", "-q", ".", "HEAD:main"]);
}

fn has_branch(bare: &Path, branch: &str) -> bool {
    git::local_branch_exists(bare, branch)
}

#[test]
fn a_local_repository_is_local_owner_and_a_valid_name() {
    for ok in ["local/scratch", "local/ho3-smoke", "local/a.b_c"] {
        assert!(is_local_repo(ok), "{ok}");
        assert!(valid_repo(ok), "{ok}");
    }
    for no in [
        "local",
        "local/",
        "local/..",
        "local/a/b",
        "localx/a",
        "o/local",
        "Local/a",
        "",
    ] {
        assert!(!is_local_repo(no), "{no}");
    }
    assert_eq!(
        devjob::local_repo("scratch").as_deref(),
        Some("local/scratch")
    );
    assert_eq!(devjob::local_repo("a b"), None);
    assert_eq!(strand_local_repo("Jesse-App").unwrap(), "local/jesse-app");
    // Its bare repository lives under <root>/local, never under <root>/repos.
    let root = Path::new("/r");
    assert_eq!(
        git::bare_path(root, "local/x"),
        PathBuf::from("/r/local/x.git")
    );
    assert_eq!(
        git::bare_path(root, "o/n"),
        PathBuf::from("/r/repos/o/n.git")
    );
}

#[test]
fn a_local_repository_is_created_with_one_empty_commit_on_main_and_its_worktree_needs_no_network() {
    let s = Scratch::new();
    let root = s.root();
    let wt = s.base.join("wt");
    // The URL is never used: nothing is cloned or fetched.
    let made = git::add_worktree(&root, "local/ho3-x", "", &wt, "ho3-x-branch").unwrap();
    let bare = root.join("local/ho3-x.git");
    assert!(bare.join("HEAD").is_file());
    assert_eq!(made.default_branch, "main");
    assert_eq!(
        sh(&bare, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main"
    );
    assert_eq!(sh(&bare, &["rev-parse", "main"]).trim(), made.base_sha);
    assert_eq!(
        sh(&bare, &["log", "--format=%an|%cn|%s|%P", "main"]).trim(),
        "Jesse Bridge|Jesse Bridge|init|"
    );
    // The empty tree.
    assert_eq!(sh(&bare, &["ls-tree", "main"]).trim(), "");
    // No remote at all.
    assert_eq!(sh(&bare, &["remote"]).trim(), "");
    assert_eq!(sh(&wt, &["remote"]).trim(), "");
    assert_eq!(
        sh(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "ho3-x-branch"
    );
    // A second run on it reuses the repository and its main as it is.
    commit_and_merge(&wt, "a.txt", "a\n");
    let main = sh(&bare, &["rev-parse", "main"]).trim().to_string();
    let wt2 = s.base.join("wt2");
    let again = git::add_worktree(&root, "local/ho3-x", "", &wt2, "ho3-x-two").unwrap();
    assert_eq!(again.base_sha, main);
    assert!(wt2.join("a.txt").is_file());
    assert_eq!(sh(&bare, &["rev-list", "--count", "main"]).trim(), "2");
}

#[tokio::test]
async fn with_nothing_named_a_studio_run_falls_back_to_a_local_repository() {
    let s = Scratch::new();
    bare_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    let n = s.notes();
    // A strand item whose strand lists no repos: local/<strand slug, lowercased>.
    let a = r.svc.start(asked("None", "NO1", None)).await.unwrap();
    let rec = r.svc.get(&a.run_id).unwrap();
    assert_eq!(rec.repo, "local/none");
    assert!(s.root().join("local/none.git/HEAD").is_file());
    assert!(!s.root().join("repos/local").exists());
    // The pure check still refuses it for a plain request, as before.
    assert!(
        studio_item(&n, "None", "NO1", OptIn::Request { repo: None })
            .unwrap_err()
            .contains("lists no repos")
    );
    // A strand listing several repositories is still refused.
    let e = r.svc.start(asked("Many", "MA1", None)).await.unwrap_err();
    assert!(e.message().contains("o/n, o/m"), "{}", e.message());
    assert!(
        studio_item(&n, "Many", "MA1", OptIn::RequestOrLocal { repo: None })
            .unwrap_err()
            .contains("several")
    );
    // A strand with one repository keeps it.
    assert_eq!(
        studio_item(&n, "One", "ON1", OptIn::RequestOrLocal { repo: None })
            .unwrap()
            .repo,
        "o/n"
    );
    // A loose draft: local/scratch.
    assert_eq!(
        studio_draft_or_local(&n, "Projects/drafts/loose-fix-prompt.md", None)
            .unwrap()
            .repo,
        "local/scratch"
    );
    // A draft of a strand that lists no repos: local/<strand slug>; of one that lists some:
    // still refused, as before.
    assert_eq!(
        studio_draft_or_local(&n, "Projects/drafts/no1-prompt.md", None)
            .unwrap()
            .repo,
        "local/none"
    );
    assert!(
        studio_draft_or_local(&n, "Projects/drafts/on1-prompt.md", None)
            .unwrap_err()
            .contains("pass repo")
    );
    // A caller's repo still wins over the fallback.
    assert_eq!(
        studio_draft_or_local(&n, "Projects/drafts/loose-fix-prompt.md", Some("o/n"))
            .unwrap()
            .repo,
        "o/n"
    );
    let d = r
        .svc
        .start(StartRequest {
            kind: StartKind::Draft {
                draft: "Projects/drafts/loose-fix-prompt.md".into(),
                repo: None,
            },
            origin: "turn".into(),
            again: false,
        })
        .await
        .unwrap();
    assert_eq!(r.svc.get(&d.run_id).unwrap().repo, "local/scratch");
    // A free prompt with no repo: local/<branch_slug>.
    let f = r.svc.start(free_in("", "one-off")).await.unwrap();
    assert_eq!(f.run_id, "s-one-off");
    assert_eq!(r.svc.get("s-one-off").unwrap().repo, "local/one-off");
    // Through the tool's arguments: host studio with no repo plans a free run with none.
    let plan =
        |v: Value| devjob::plan_start(serde_json::from_value(v).unwrap(), devjob::Agent::Claude);
    assert_eq!(
        plan(json!({ "host": "studio", "prompt": "p", "branch_slug": "x" })).unwrap(),
        devjob::StartPlan::Studio {
            kind: StartKind::Free {
                repo: String::new(),
                prompt: "p".into(),
                slug: "x".into(),
                model: None
            },
            again: false
        }
    );
}

#[tokio::test]
async fn an_explicit_local_repository_is_honoured_as_written() {
    let s = Scratch::new();
    bare_fixture(&s);
    let r = rig(&s);
    let n = s.notes();
    // In a strand's repos.
    std::fs::write(
        n.join("Strands/Loc.md"),
        "---\ngroup: tag1\nstate: active\nrepos: [local/kept]\nupdated: 2026-11-09\n---\n# Loc\n\n## Drafts\n\
         - [ ] **LO1** Local. [[todo-list/Projects/drafts/lo1-prompt]]\n### Done\n",
    )
    .unwrap();
    std::fs::write(
        n.join("Projects/drafts/lo1-prompt.md"),
        bare_draft("Loc", "LO1"),
    )
    .unwrap();
    assert_eq!(
        studio_item(&n, "Loc", "LO1", OptIn::Request { repo: None })
            .unwrap()
            .repo,
        "local/kept"
    );
    // As the caller's repo, over a strand that lists one.
    assert_eq!(
        studio_item(
            &n,
            "One",
            "ON1",
            OptIn::RequestOrLocal {
                repo: Some("local/mine")
            }
        )
        .unwrap()
        .repo,
        "local/mine"
    );
    // In devjob-repo, from a schedule entry too.
    std::fs::write(
        n.join("Projects/drafts/lo1-prompt.md"),
        draft_in(
            "Loc",
            "LO1",
            "devjob-repo: local/sched\ndevjob-host: studio\n",
        ),
    )
    .unwrap();
    assert_eq!(
        studio_item(&n, "Loc", "LO1", OptIn::Schedule).unwrap().repo,
        "local/sched"
    );
    let a = r.svc.start(free_in("local/named", "named")).await.unwrap();
    assert_eq!(r.svc.get(&a.run_id).unwrap().repo, "local/named");
    assert!(s.root().join("local/named.git/HEAD").is_file());
}

fn draft_in(strand: &str, id: &str, extra: &str) -> String {
    format!("---\nstrand: {strand}\nstrand-id: {id}\n{extra}---\n# {id}\n\n## PROMPT\n{SECTION}")
}

#[test]
fn the_cluster_refuses_a_local_repository_and_a_start_with_none() {
    let plan =
        |v: Value| devjob::plan_start(serde_json::from_value(v).unwrap(), devjob::Agent::Claude);
    let want = "local repositories run only on the Studio (host: \"studio\")";
    for v in [
        json!({ "repo": "local/x", "prompt": "p", "branch_slug": "s" }),
        json!({ "host": "cluster", "repo": "local/x", "prompt": "p", "branch_slug": "s" }),
        json!({ "prompt": "p", "branch_slug": "s" }),
        json!({ "host": "cluster", "prompt": "p", "branch_slug": "s" }),
    ] {
        let e = plan(v.clone()).unwrap_err();
        assert!(e.contains(want), "{v}: {e}");
    }
    // The spec check refuses it too, so a cluster schedule entry cannot name one.
    let e = DevJobSpec::validate(
        DevJobArgs {
            repo: Some("local/x".into()),
            prompt: Some("p".into()),
            branch_slug: Some("s".into()),
            ..DevJobArgs::default()
        },
        devjob::Agent::Claude,
    )
    .unwrap_err();
    assert_eq!(e, want);
    // A GitHub repository on the cluster is unchanged.
    assert!(matches!(
        plan(json!({ "repo": "a/b", "prompt": "p", "branch_slug": "s" })).unwrap(),
        devjob::StartPlan::Cluster(_)
    ));
}

#[test]
fn the_generated_section_for_a_local_repository_has_no_push_and_no_pull_request() {
    let dir = Path::new("/Users/j/jesse-runs/runs/s-x");
    let place = |repo: &'static str| RunPlace {
        run_id: "s-x",
        repo,
        branch: "s-x",
        default_branch: "main",
        run_dir: dir,
        pr_title_tag: true,
    };
    let local = served_prompt("Do it.\n", &place("local/x"));
    let expected = "Do it.\n\n### Run ID and report\n\nYou are running on the Mac Studio as a Studio run, in a fresh worktree of local/x (Studio only) on branch s-x, cut from main. Wherever the prompt above names another machine, host, user or directory to work in, this worktree replaces it: do not change machines, do not stop because the host differs, and do not work in any other checkout of this repository. If the work needs another repository, clone it under /Users/j/jesse-runs/runs/s-x/extra/ and work there.\n\nThis run is s-x. Start every branch name with s-x-. This repository exists only on the Mac Studio and has no remote: there is no push, no pull request and no CI. Wherever the prompt above asks for a branch push, a pull request, CI or a merge, do this instead: commit on your branch, and once your verification passes, merge your branch into main of this repository with a fast forward (rebase onto main first if it moved), then report the main commit. Keep everything you create inside your working directory or the system temporary directory. When you finish, or stop early, write your complete report back, the same text you would answer with, to the file named in the environment variable JESSE_RUN_REPORT, then answer as usual. If you need a decision from Jeremy to continue, stop, make the first line of that report NEEDS YOU: followed by the one question, and end the run.\n";
    assert_eq!(local, expected);
    assert!(!local.contains("pull request title"));
    assert!(!local.contains("origin/"));
    // A GitHub repository's section is the one it always was.
    let gh = served_prompt("Do it.\n", &place("o/n"));
    assert!(gh.contains("worktree of o/n on branch s-x, cut from origin/main."));
    assert!(gh.contains(&generated_paragraph("s-x", true)));
    assert!(!gh.contains("Studio only"));
}

#[tokio::test]
async fn the_watcher_never_asks_the_forge_about_a_local_run_and_no_pull_request_is_no_failure() {
    let s = Scratch::new();
    let r = rig(&s);
    r.svc.start(free_in("", "quiet")).await.unwrap();
    let dir = r.svc.settings.run_dir("s-quiet");
    assert_eq!(read_run(&dir).unwrap().repo, "local/quiet");
    finish_round(&r, "s-quiet", 0, Some("Done, merged into main.\n"), "");
    r.svc.pass(now_ms()).await;
    let rec = r.svc.get("s-quiet").unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    assert!(lock(&r.rec.pushes)
        .iter()
        .all(|p| !p.0.contains("needs you")));
    r.svc.cleanup(now_ms());
    r.svc.cleanup(now_ms() + 8 * DAY_MS);
    r.svc.pass(now_ms() + 9 * DAY_MS).await;
    assert!(
        lock(&r.forge.asked).is_empty(),
        "{:?}",
        lock(&r.forge.asked)
    );
    // A GitHub run on the same rig still asks.
    remote_fixture(&s);
    r.svc.start(free("gh")).await.unwrap();
    finish_round(&r, "s-gh", 0, Some("ok\n"), "");
    r.svc.pass(now_ms()).await;
    r.svc.cleanup(now_ms());
    assert_eq!(lock(&r.forge.asked).as_slice(), ["o/n".to_string()]);
}

#[tokio::test]
async fn cleanup_keeps_an_unmerged_local_branch_and_removes_a_merged_one() {
    let s = Scratch::new();
    let mut st = settings(&s);
    st.max_per_repo = 2;
    let r = rig_with(&s, st);
    r.svc
        .start(free_in("local/keep", "unmerged"))
        .await
        .unwrap();
    r.svc.start(free_in("local/keep", "merged")).await.unwrap();
    let bare = git::bare_path(&s.root(), "local/keep");
    let du = r.svc.settings.run_dir("s-unmerged");
    let dm = r.svc.settings.run_dir("s-merged");
    let bu = r.svc.get("s-unmerged").unwrap().branch;
    let bm = r.svc.get("s-merged").unwrap().branch;
    // One commits and never merges; the other commits and fast forwards main.
    let wu = du.join(WORKTREE_DIR);
    std::fs::write(wu.join("draft.txt"), "x\n").unwrap();
    sh(&wu, &["add", "draft.txt"]);
    sh(&wu, &["commit", "-q", "-m", "draft"]);
    commit_and_merge(&dm.join(WORKTREE_DIR), "done.txt", "y\n");
    finish_round(&r, "s-unmerged", 0, Some("ok\n"), "");
    finish_round(&r, "s-merged", 0, Some("ok\n"), "");
    r.svc.pass(now_ms()).await;
    // At once: the merged one is done with, the unmerged one stays whole.
    r.svc.cleanup(now_ms());
    assert!(!dm.join(WORKTREE_DIR).exists());
    assert!(!has_branch(&bare, &bm), "a merged branch goes");
    assert!(du.join(WORKTREE_DIR).exists());
    assert!(has_branch(&bare, &bu));
    // Past keep_days the unmerged worktree goes, but its branch, the only copy, stays.
    r.svc.cleanup(now_ms() + 8 * DAY_MS);
    assert!(!du.join(WORKTREE_DIR).exists());
    assert!(
        has_branch(&bare, &bu),
        "an unmerged local branch is never deleted"
    );
    // The local repository itself is never touched, even by the disk cap and reconcile.
    let mut st = settings(&s);
    st.max_total_bytes = 0;
    let r2 = rig_with(&s, st);
    r2.svc.space_pass(now_ms() + 9 * DAY_MS);
    r2.svc.reconcile(now_ms() + 9 * DAY_MS);
    assert!(bare.join("HEAD").is_file());
    assert!(has_branch(&bare, &bu));
    // And a reply restores the worktree from the local branch.
    let rec = r.svc.reply_and_pump("s-unmerged", "go on").await.unwrap();
    assert_eq!(rec.status, RunStatus::Running);
    assert!(wu.join("draft.txt").is_file());
}

#[tokio::test]
async fn a_local_run_end_to_end_creates_merges_and_lands_in_the_readable_checkout() {
    let s = Scratch::new();
    let r = rig(&s);
    // Studio, no repository: local/<branch_slug>.
    let started = r.svc.start(free_in("", "ho3-smoke")).await.unwrap();
    let dir = r.svc.settings.run_dir(&started.run_id);
    let rec = read_run(&dir).unwrap();
    assert_eq!(rec.repo, "local/ho3-smoke");
    assert_eq!(rec.default_branch.as_deref(), Some("main"));
    let bare = s.root().join("local/ho3-smoke.git");
    assert!(bare.join("HEAD").is_file());
    let prompt = std::fs::read_to_string(dir.join(PROMPT_MD)).unwrap();
    assert!(prompt.contains("worktree of local/ho3-smoke (Studio only)"));
    assert!(prompt.contains(LOCAL_REPO_SENTENCE));
    // The agent commits on its branch and fast forwards main.
    let wt = dir.join(WORKTREE_DIR);
    commit_and_merge(&wt, "result.txt", "first\n");
    finish_round(&r, &started.run_id, 0, Some("Merged into main.\n"), "");
    r.svc.pass(now_ms()).await;
    let main = sh(&bare, &["rev-parse", "main"]).trim().to_string();
    // The Inbox report names the repository, the branch and main.
    let rel = format!("Inbox/{}-run-s-ho3-smoke.md", today());
    let text = std::fs::read_to_string(s.notes().join(&rel)).unwrap();
    assert!(text.contains("\nrepo: local/ho3-smoke\n"), "{text}");
    assert!(
        text.ends_with(&format!(
            "Merged into main.\n\nRepository: local/ho3-smoke (Studio only)\nBranch: {}\nmain: {main}\n",
            rec.branch
        )),
        "{text}"
    );
    // The readable checkout: cloned on first use.
    let checkout = s.base.join("jesse/Code/local/ho3-smoke");
    assert_eq!(
        r.svc.settings.local_checkout("local/ho3-smoke"),
        Some(checkout.clone())
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("result.txt")).unwrap(),
        "first\n"
    );
    // Merged and clean, the run's worktree and branch are done with at once; main holds the
    // work, so a reply still continues, cut again from main.
    assert!(!wt.exists());
    assert!(!git::local_branch_exists(&bare, &rec.branch));
    r.svc.reply_and_pump(&started.run_id, "more").await.unwrap();
    assert!(wt.join("result.txt").is_file());
    commit_and_merge(&wt, "second.txt", "second\n");
    finish_round(&r, &started.run_id, 0, Some("Merged again.\n"), "");
    r.svc.pass(now_ms()).await;
    assert_eq!(
        std::fs::read_to_string(checkout.join("second.txt")).unwrap(),
        "second\n"
    );
    assert_eq!(
        sh(&checkout, &["rev-parse", "HEAD"]).trim(),
        sh(&bare, &["rev-parse", "main"]).trim()
    );
    assert_eq!(
        r.svc.get(&started.run_id).unwrap().status,
        RunStatus::Reported
    );
}

#[test]
fn local_checkout_root_defaults_under_home_expands_tilde_and_may_not_overlap_root() {
    let s = HostRunSettings::from_toml(None, "/Users/j", "claude");
    assert_eq!(
        s.local_checkout_root,
        PathBuf::from("/Users/j/jesse/Code/local")
    );
    let t: HostRunToml =
        toml::from_str("enabled = true\nlocal_checkout_root = \"~/elsewhere\"\n").unwrap();
    let s = HostRunSettings::from_toml(Some(t), "/Users/j", "claude");
    assert_eq!(s.local_checkout_root, PathBuf::from("/Users/j/elsewhere"));
    assert_eq!(s.local_checkout("o/n"), None);
    assert_eq!(
        s.local_checkout("local/x"),
        Some(PathBuf::from("/Users/j/elsewhere/x"))
    );
    let sc = Scratch::new();
    let mut st = settings(&sc);
    st.local_checkout_root = sc.root().join("checkouts");
    let p = st.problems();
    assert!(
        p.iter()
            .any(|m| m.contains("local_checkout_root") && m.contains("overlaps")),
        "{p:?}"
    );
    let mut st = settings(&sc);
    st.local_checkout_root = PathBuf::from("relative/x");
    let p = st.problems();
    assert!(
        p.iter().any(|m| m.contains("not an absolute path")),
        "{p:?}"
    );
}

// ---- THE WORK VIEW (0.203.0) ---------------------------------------------------------------

/// `GET /jesse/runs` serves each run's `title` (its Drafts line's, else its prompt's H1, else its
/// branch, never its id), `last_line` (the last non blank line of the log the log route reads)
/// and `report_line` (the first line of its newest report).
#[tokio::test]
async fn the_run_list_serves_a_title_a_last_line_and_a_report_line() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    finish_round(
        &r,
        "GD5",
        0,
        Some("NEEDS YOU: main or release?\nThe branch is pushed.\n"),
        STREAM_TEXT,
    );
    r.svc.pass(now_ms()).await;
    r.svc.start(free("other")).await.unwrap();
    let list = r.svc.list_json();
    let run = |id: &str| {
        list["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["run_id"] == id)
            .unwrap()
            .clone()
    };
    let gd5 = run("GD5");
    assert_eq!(gd5["title"], "Run the thing on the Studio");
    // The log route's own tail: the last readable line is the Edit tool line.
    let tail = r.svc.log_lines("GD5", LOG_DEFAULT).unwrap();
    assert_eq!(gd5["last_line"], json!(tail.last().unwrap()));
    assert_eq!(gd5["last_line"], "Edit: bridge/src/x.rs");
    assert_eq!(gd5["report_line"], "NEEDS YOU: main or release?");
    // A free run with no H1 in its prompt is named after its branch, never its id.
    let free = run("s-other");
    let branch = free["branch"].as_str().unwrap();
    assert_eq!(free["title"], json!(work::branch_title(branch)));
    assert_ne!(free["title"], "s-other");
    // Nothing logged yet, no report yet: both null, never missing.
    assert!(free.as_object().unwrap().contains_key("last_line"));
    assert!(free["last_line"].is_null());
    assert!(free["report_line"].is_null());
    // One run's route serves the same fields.
    let one = r.svc.served_one(&r.svc.get("GD5").unwrap());
    assert_eq!(one["title"], gd5["title"]);
    assert_eq!(one["report_line"], gd5["report_line"]);
}

/// A run whose Drafts line is gone is named by its prompt's H1, its leading id removed.
#[tokio::test]
async fn a_run_whose_line_is_gone_is_named_by_its_prompt_heading() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    let d = s.notes().join("Projects/drafts/gd5-prompt.md");
    let src = std::fs::read_to_string(&d).unwrap();
    std::fs::write(
        &d,
        src.replace("# GD5\n", "# GD5 Ship the thing: then tidy up\n"),
    )
    .unwrap();
    r.svc.start(item("GD5")).await.unwrap();
    let note = s.notes().join("Strands/Gd.md");
    let text = std::fs::read_to_string(&note).unwrap();
    std::fs::write(&note, text.replace("**GD5**", "**GD5x**")).unwrap();
    let v = r.svc.served_one(&r.svc.get("GD5").unwrap());
    assert_eq!(v["title"], "Ship the thing");
}

/// The last line is at most 200 characters, and is read again only when the stream grows.
#[tokio::test]
async fn the_last_line_is_clipped_and_follows_the_stream() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    let dir = r.svc.settings.run_dir("GD5");
    let line = |t: &str| {
        format!(
            "{}\n",
            json!({"type":"assistant","message":{"content":[{"type":"text","text":t}]}})
        )
    };
    let append = |t: &str| {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(STREAM))
            .unwrap();
        std::io::Write::write_all(&mut f, line(t).as_bytes()).unwrap();
    };
    append("First.\n\n   \n");
    let first = r.svc.served_one(&r.svc.get("GD5").unwrap());
    assert_eq!(first["last_line"], "First.");
    append(&"x".repeat(300));
    let long = r.svc.served_one(&r.svc.get("GD5").unwrap());
    let got = long["last_line"].as_str().unwrap();
    assert_eq!(got.chars().count(), work::LAST_LINE_MAX);
    assert!(got.ends_with('\u{2026}'));
}

/// `startable` over the real annotate: a runnable line with no live run is startable; the same
/// line with a live run is not; a line whose draft does not name this machine is not; a line
/// that is not runnable at all carries the runnable check's own reason.
#[tokio::test]
async fn startable_is_runnable_and_studio_and_not_live() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    let annotate = || {
        let snap = crate::strands::snapshot(&s.notes(), "2026-11-09");
        let mut v = serde_json::to_value(&snap).unwrap();
        annotate_strands(&mut v, &snap, &s.notes(), Some(&r.svc), "2026-11-09");
        v["strands"][0]["drafts"].as_array().unwrap().clone()
    };
    let draft = |all: &[Value], id: &str| all.iter().find(|d| d["id"] == id).unwrap().clone();
    let all = annotate();
    let gd5 = draft(&all, "GD5");
    assert_eq!(gd5["startable"], true);
    assert!(gd5["startable_reason"].is_null());
    assert_eq!(gd5["title"], "Run the thing on the Studio");
    // GD10 is runnable on request but its draft does not say devjob-host: studio.
    let gd10 = draft(&all, "GD10");
    assert_eq!(gd10["studio_runnable"], true);
    assert_eq!(gd10["startable"], false);
    assert!(gd10["startable_reason"]
        .as_str()
        .unwrap()
        .contains("devjob-host"));
    // GD8 is not runnable: its reason is the runnable check's.
    let gd8 = draft(&all, "GD8");
    assert_eq!(gd8["startable"], false);
    assert!(gd8["startable_reason"].as_str().unwrap().contains("PROMPT"));
    r.svc.start(item("GD5")).await.unwrap();
    let gd5 = draft(&annotate(), "GD5");
    // `studio_runnable` is unchanged for compatibility: a live run does not touch it.
    assert_eq!(gd5["studio_runnable"], true);
    assert_eq!(gd5["startable"], false);
    assert_eq!(gd5["startable_reason"], "Already has a live run.");
}

/// A `needs_you` run asked a question: the empty continue would answer it blind, so the bridge
/// refuses it with a 409 that says so. An answer goes through; an interrupted run still takes
/// the continue.
#[tokio::test]
async fn a_blind_continue_to_a_question_is_refused() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig_with(&s, no_auto_resume(&s));
    r.svc.start(item("GD5")).await.unwrap();
    to_needs_you(&r, "GD5").await;
    let e = r.svc.reply("GD5", CONTINUE_TEXT).unwrap_err();
    assert_eq!(e.status(), StatusCode::CONFLICT);
    assert!(
        e.message().contains("asked a question and needs an answer"),
        "{e:?}"
    );
    // Surrounding whitespace is still the same blind continue.
    assert!(r
        .svc
        .reply("GD5", "  Continue where you left off.\n")
        .is_err());
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::NeedsYou);
    // An interrupted run asked nothing: the continue is what resumes it.
    r.svc.start(item("GD10")).await.unwrap();
    to_interrupted(&r, "GD10");
    assert!(r.svc.reply("GD10", CONTINUE_TEXT).is_ok());
    // A real answer goes through.
    assert!(r.svc.reply("GD5", "main").is_ok());
}

mod archive;
