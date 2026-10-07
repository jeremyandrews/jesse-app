//! **`workflows-v1`'s state assertions have teeth in both directions**, checked against the
//! suite file and the fixture vault as they ship.
//!
//! The suite grades end states, and a mock cannot produce most of them: a diet log is only
//! right when the vendored generator has rebuilt `vault/diet-today.js`, both validators pass
//! and the change is committed. So these tests play the turn themselves, through the same
//! tools a turn would use (the CSV edit, `node vault/generate-diet-today.js`, `git commit`),
//! and require that the task's own assertions PASS on the result; then they play the usual
//! mistakes and require a FAIL. A task whose assertions could never pass, or could not tell a
//! good turn from a bad one, fails here before it costs a model run.

use crate::assertions::eval_all;
use crate::state::git;
use crate::suite::{Suite, Task};
use crate::transcript::Transcript;
use crate::workspace::prepare_fixture;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn suite() -> Suite {
    Suite::from_json(include_bytes!("../suites/workflows-v1.json")).expect("workflows-v1 loads")
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn task(id: &str) -> Task {
    suite()
        .tasks
        .into_iter()
        .find(|t| t.id == id)
        .unwrap_or_else(|| panic!("no task {id}"))
}

fn node_present() -> bool {
    Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A prepared workspace for `id`, as the runner would build it.
fn workspace(id: &str) -> (tempfile::TempDir, Task) {
    let t = task(id);
    let root = tempfile::tempdir().unwrap();
    prepare_fixture(
        &t,
        &root.path().join("ws"),
        Some(&fixtures()),
        &root.path().join("remotes"),
    )
    .unwrap();
    (root, t)
}

fn answered(text: &str) -> Transcript {
    Transcript {
        final_answer: Some(text.to_string()),
        completed: true,
        ..Default::default()
    }
}

fn grade(t: &Task, ws: &Path, answer: &str) -> (bool, Vec<String>) {
    let (ok, results) = eval_all(&t.assertions, &answered(answer), ws, None);
    (
        ok,
        results
            .into_iter()
            .filter(|r| !r.passed)
            .map(|r| format!("{}: {}", r.kind, r.detail))
            .collect(),
    )
}

fn append(ws: &Path, rel: &str, row: &str) {
    let p = ws.join(rel);
    let mut body = std::fs::read_to_string(&p).unwrap();
    body.push_str(row);
    body.push('\n');
    std::fs::write(p, body).unwrap();
}

fn regenerate(ws: &Path, day: &str) {
    let ok = Command::new("node")
        .args(["vault/generate-diet-today.js", "--day", day])
        .current_dir(ws)
        .stdout(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "the vendored generator failed");
}

fn commit(ws: &Path, msg: &str) {
    git(ws, &["add", "-A"]).unwrap();
    git(ws, &["commit", "-q", "-m", msg]).unwrap();
}

const BANANA: &str = "2026-10-06,Snack,\"Banana, medium, raw\",\"1 medium (~118g edible)\",serving,,,105,1.3,0.4,27,\"Copied from the 2026-10-04 banana row.\",10:30,Snack,3.1,1,0.1,14.4,422,6,,32,0,0,0,57,0,1.2,0,Europe/Rome,0,food,home,reference,actual,0,,0.3,,,0.1";

#[test]
fn the_suite_is_in_range_and_every_task_has_a_base_that_exists() {
    let s = suite();
    assert!(
        (20..=30).contains(&s.tasks.len()),
        "{} tasks",
        s.tasks.len()
    );
    assert_eq!(s.runs, Some(3), "pass^3");
    for t in &s.tasks {
        let base = t.fixture_base.as_deref().expect("every task uses the base");
        assert!(fixtures().join(base).is_dir(), "{}: {base}", t.id);
    }
    // The three injection tasks are product-v1's, assertions unchanged.
    let product: Suite =
        Suite::from_json(include_bytes!("../suites/product-v1.json")).expect("product-v1");
    for p in product
        .tasks
        .iter()
        .filter(|t| t.class == "injection-resistance")
    {
        let w = s.tasks.iter().find(|t| t.id == p.id).expect("carried over");
        assert_eq!(w.assertions, p.assertions, "{}: assertions changed", p.id);
        assert_eq!(w.prompt, p.prompt);
    }
    assert_eq!(
        s.tasks
            .iter()
            .filter(|t| t.class == "injection-resistance")
            .count(),
        3
    );
}

#[test]
fn the_shipped_fixture_passes_both_validators_before_any_turn() {
    if !node_present() {
        eprintln!("node not on PATH: skipped");
        return;
    }
    let (root, _) = workspace("diet-food-log");
    let ws = root.path().join("ws");
    for v in [
        crate::suite::Validator::ValidateDietToday,
        crate::suite::Validator::VerifyDietConsistency,
    ] {
        for day in ["2026-10-04", "2026-10-05", "2026-10-06"] {
            let (ok, detail) = crate::state::process_exit_zero(&ws, v, Some(day));
            assert!(ok, "{day}: {detail}");
        }
    }
}

#[test]
fn a_no_op_turn_fails_every_diet_task() {
    for id in [
        "diet-food-log",
        "diet-food-log-quoted",
        "diet-late-snack",
        "diet-weigh-in",
        "diet-reweigh-same-day",
        "diet-exercise-log",
    ] {
        let (root, t) = workspace(id);
        let (ok, _) = grade(&t, &root.path().join("ws"), "Logged.");
        assert!(!ok, "{id}: a turn that changed nothing must fail");
    }
}

#[test]
fn a_food_log_done_right_passes_and_each_shortcut_fails() {
    if !node_present() {
        eprintln!("node not on PATH: skipped");
        return;
    }
    // Right: append, regenerate, commit.
    let (root, t) = workspace("diet-food-log");
    let ws = root.path().join("ws");
    append(&ws, "diet-logs/food-log.csv", BANANA);
    regenerate(&ws, "2026-10-06");
    commit(&ws, "diet: log 2026-10-06 10:30");
    let (ok, why) = grade(&t, &ws, "Logged the banana.");
    assert!(ok, "{why:?}");

    // Shortcut 1: appended but never regenerated nor committed.
    let (root, t) = workspace("diet-food-log");
    let ws = root.path().join("ws");
    append(&ws, "diet-logs/food-log.csv", BANANA);
    let (ok, why) = grade(&t, &ws, "Logged.");
    assert!(!ok);
    assert!(
        why.iter().any(|w| w.starts_with("process_exit_zero")),
        "{why:?}"
    );

    // Shortcut 2: the row unquoted, so its comma shifts every later column.
    let (root, t) = workspace("diet-food-log");
    let ws = root.path().join("ws");
    append(
        &ws,
        "diet-logs/food-log.csv",
        &BANANA.replace("\"Banana, medium, raw\"", "Banana, medium, raw"),
    );
    let (ok, why) = grade(&t, &ws, "Logged.");
    assert!(!ok);
    assert!(why.iter().any(|w| w.contains("not well-formed")), "{why:?}");

    // Shortcut 3: dated by the clock instead of the eaten-at stamp.
    let (root, t) = workspace("diet-food-log");
    let ws = root.path().join("ws");
    append(
        &ws,
        "diet-logs/food-log.csv",
        &BANANA.replace("2026-10-06,", "2026-10-07,"),
    );
    let (ok, _) = grade(&t, &ws, "Logged.");
    assert!(!ok);
}

#[test]
fn the_late_snack_belongs_to_the_day_that_just_ended() {
    if !node_present() {
        eprintln!("node not on PATH: skipped");
        return;
    }
    let row = "2026-10-06,Snack,\"Greek yogurt, plain, 2% fat\",\"170g pot\",serving,,,124,17,3.3,6.6,\"Label values, as the 2026-10-05 row.\",00:40,Snack,0,60,2.1,6.6,240,190,,19,13,0,0,,0,16,0,Europe/Rome,0,food,home,label,actual,0,,0.1,,,0.1";
    let (root, t) = workspace("diet-late-snack");
    let ws = root.path().join("ws");
    append(&ws, "diet-logs/food-log.csv", row);
    regenerate(&ws, "2026-10-06");
    commit(&ws, "diet: late snack");
    let (ok, why) = grade(&t, &ws, "Logged.");
    assert!(ok, "{why:?}");
    // Dated by the calendar (the 7th) rather than the diet day: fails.
    let (root, t) = workspace("diet-late-snack");
    let ws = root.path().join("ws");
    append(
        &ws,
        "diet-logs/food-log.csv",
        &row.replacen("2026-10-06", "2026-10-07", 1),
    );
    let (ok, _) = grade(&t, &ws, "Logged.");
    assert!(!ok);
}

#[test]
fn a_re_weigh_edits_the_row_in_place_and_a_second_row_fails() {
    if !node_present() {
        eprintln!("node not on PATH: skipped");
        return;
    }
    let first = "2026-10-06,185.2,84.0,Phase 2,,,\"Morning weigh-in. BF/MM not provided.\",Europe/Rome,false";
    let second = "2026-10-06,184.9,83.9,Phase 2,,,\"Morning weigh-in, re-weighed. BF/MM not provided.\",Europe/Rome,false";

    // Right: the 185.2 row is replaced by 184.9.
    let (root, t) = workspace("diet-reweigh-same-day");
    let ws = root.path().join("ws");
    append(&ws, "diet-logs/weight-log.csv", second);
    regenerate(&ws, "2026-10-06");
    commit(&ws, "diet: re-weigh");
    let (ok, why) = grade(&t, &ws, "Updated.");
    assert!(ok, "{why:?}");

    // Wrong: both readings kept, the new one appended.
    let (root, t) = workspace("diet-reweigh-same-day");
    let ws = root.path().join("ws");
    append(&ws, "diet-logs/weight-log.csv", first);
    append(&ws, "diet-logs/weight-log.csv", second);
    regenerate(&ws, "2026-10-06");
    commit(&ws, "diet: re-weigh");
    let (ok, why) = grade(&t, &ws, "Updated.");
    assert!(!ok);
    assert!(
        why.iter().any(|w| w.contains("expected exactly 4")),
        "{why:?}"
    );
}

#[test]
fn a_weigh_in_from_the_health_block_passes() {
    if !node_present() {
        eprintln!("node not on PATH: skipped");
        return;
    }
    let (root, t) = workspace("diet-weigh-in");
    let ws = root.path().join("ws");
    append(
        &ws,
        "diet-logs/weight-log.csv",
        "2026-10-06,185.2,84.0,Phase 2,24.9,138.7,\"Morning weigh-in. BF via Health.\",Europe/Rome,false",
    );
    let overview = ws.join("vault/Projects/Diet/Overview.md");
    let body = std::fs::read_to_string(&overview).unwrap().replace(
        "Current: 185.6 lbs (2026-10-05)",
        "Current: 185.2 lbs (2026-10-06)",
    );
    std::fs::write(&overview, body).unwrap();
    regenerate(&ws, "2026-10-06");
    commit(&ws, "diet: weigh-in");
    let (ok, why) = grade(&t, &ws, "Logged 185.2 lbs.");
    assert!(ok, "{why:?}");
}

#[test]
fn an_exercise_log_passes_and_a_free_text_type_fails() {
    if !node_present() {
        eprintln!("node not on PATH: skipped");
        return;
    }
    let row = "2026-10-06,Strength,\"Strength session at home\",,0:35:00,,,,,210,,\"From the watch.\",17:10,Europe/Rome,false,watch";
    let (root, t) = workspace("diet-exercise-log");
    let ws = root.path().join("ws");
    append(&ws, "diet-logs/exercise-log.csv", row);
    regenerate(&ws, "2026-10-06");
    commit(&ws, "diet: exercise");
    let (ok, why) = grade(&t, &ws, "Logged.");
    assert!(ok, "{why:?}");

    let (root, t) = workspace("diet-exercise-log");
    let ws = root.path().join("ws");
    append(
        &ws,
        "diet-logs/exercise-log.csv",
        &row.replace(",Strength,", ",Weights,"),
    );
    regenerate(&ws, "2026-10-06");
    commit(&ws, "diet: exercise");
    let (ok, _) = grade(&t, &ws, "Logged.");
    assert!(!ok);
}

#[test]
fn an_archive_pass_done_right_passes_and_a_double_date_fails() {
    let (root, t) = workspace("archive-checked-boxes");
    let ws = root.path().join("ws");
    let d = ws.join("vault/Projects/drafts");
    std::fs::rename(
        d.join("2026-09-30-1015-boiler-quote-request.md"),
        d.join("archive/2026-09-30-1015-boiler-quote-request.md"),
    )
    .unwrap();
    std::fs::rename(
        d.join("garden-plan.md"),
        d.join("archive/2026-10-07-garden-plan.md"),
    )
    .unwrap();
    let (ok, why) = grade(&t, &ws, "Archived two files.");
    assert!(ok, "{why:?}");
    std::fs::rename(
        d.join("archive/2026-09-30-1015-boiler-quote-request.md"),
        d.join("archive/2026-10-07-2026-09-30-1015-boiler-quote-request.md"),
    )
    .unwrap();
    let (ok, _) = grade(&t, &ws, "Archived two files.");
    assert!(!ok, "the double date prefix is the bug the task names");
}

#[test]
fn a_rotation_done_right_passes_and_an_unrotated_summary_fails() {
    if !node_present() {
        eprintln!("node not on PATH: skipped");
        return;
    }
    let (root, t) = workspace("currency-summary-rotation");
    let ws = root.path().join("ws");
    let live = ws.join("vault/Projects/Research/Currency-Tracking/USD-EUR-Summary.md");
    let row = "| 2026-10-07 | 0.88739 | -0.58% | +0.76% | +3.13% | The euro bounced as French bond yields eased. Full report: [[todo-list/Projects/Research/Currency-Tracking/USD-EUR-2026-10-07]]. |";
    let body = std::fs::read_to_string(&live).unwrap();
    let sep = "|------|------|-------|--------|---------|------------|\n";
    std::fs::write(&live, body.replacen(sep, &format!("{sep}{row}\n"), 1)).unwrap();
    let (ok, _) = grade(&t, &ws, "Added.");
    assert!(!ok, "added but not rotated");
    let ok = Command::new("node")
        .arg("vault/rotate-currency-summary.js")
        .current_dir(&ws)
        .stdout(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let (ok, why) = grade(&t, &ws, "Added and rotated.");
    assert!(ok, "{why:?}");
}

#[test]
fn a_checkout_in_the_right_place_at_head_passes_and_a_stale_one_fails() {
    let t0 = task("code-review-checkout");
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    let remotes =
        prepare_fixture(&t0, &ws, Some(&fixtures()), &root.path().join("remotes")).unwrap();
    let t = crate::workspace::resolve_task(&t0, &remotes).unwrap();
    let r = &remotes["github.com/acme/widget"];
    let dst = ws.join("Code/github.com/acme/widget");
    git(&ws, &["clone", "-q", &r.url, &dst.to_string_lossy()]).unwrap();
    let answer = format!(
        "Reviewed {}: average divides by xs.len(), so an empty slice panics.",
        &r.head[..7]
    );
    let (ok, why) = grade(&t, &ws, &answer);
    assert!(ok, "{why:?}");
    // Checked out one commit behind head: the HEAD message assertion fails.
    git(&dst, &["checkout", "-q", "HEAD~1"]).unwrap();
    let (ok, _) = grade(&t, &ws, &answer);
    assert!(!ok);
}
