//! Cloud runs on the app's Runs page: the status mapping, the merged list, the unavailable flag
//! and the refused actions.

use super::*;
use crate::devjob::SpecAnnotation;

/// 2026-11-23 17:08:27 UTC.
const T0: u64 = 1_795_453_707_000;

fn job(run_id: &str, created: u64) -> JobView {
    JobView {
        name: format!("devjob-{}", run_id.to_lowercase()),
        slug: format!("{}-slug", run_id.to_lowercase()),
        created_ms: created,
        started_ms: None,
        suspended: false,
        finished: None,
        finished_ms: None,
        reported: false,
        outcome: None,
        report_path: None,
        pull_request: None,
        origin: "turn".into(),
        spec: SpecAnnotation {
            repo: "jeremyandrews/trovato".into(),
            agent: "claude".into(),
            ..SpecAnnotation::default()
        },
        config_map: None,
        active_deadline_secs: None,
        failure_reason: None,
        run_id: Some(run_id.into()),
        cancelled: false,
        strand: None,
        strand_id: None,
    }
}

fn queued(id: &str) -> JobView {
    JobView {
        suspended: true,
        ..job(id, T0)
    }
}

fn running(id: &str) -> JobView {
    JobView {
        started_ms: Some(T0 + 1_000),
        ..job(id, T0)
    }
}

fn finished(id: &str, completed: bool, outcome: Option<&str>) -> JobView {
    JobView {
        started_ms: Some(T0 + 1_000),
        finished: Some(completed),
        finished_ms: Some(T0 + 60_000),
        reported: outcome.is_some(),
        outcome: outcome.map(str::to_string),
        ..job(id, T0)
    }
}

// ---- the status mapping ------------------------------------------------------------------------

#[test]
fn a_cloud_phase_and_outcome_each_map_to_a_studio_status() {
    let cases: Vec<(&str, JobView, RunStatus)> = vec![
        ("suspended", queued("A"), RunStatus::Queued),
        ("released", running("A"), RunStatus::Running),
        (
            "released and being cancelled",
            JobView {
                cancelled: true,
                ..running("A")
            },
            RunStatus::Running,
        ),
        (
            "pull request opened",
            finished("A", true, Some("pull request opened")),
            RunStatus::Reported,
        ),
        (
            "no changes",
            finished("A", true, Some("no changes")),
            RunStatus::Reported,
        ),
        (
            "failed outcome",
            finished("A", false, Some("failed")),
            RunStatus::Failed,
        ),
        (
            "failed outcome on a completed Job",
            finished("A", true, Some("failed")),
            RunStatus::Failed,
        ),
        (
            "cancelled",
            JobView {
                cancelled: true,
                ..finished("A", false, Some("failed"))
            },
            RunStatus::Cancelled,
        ),
        (
            "completed, not yet reported",
            finished("A", true, None),
            RunStatus::Running,
        ),
        (
            "failed, not yet reported",
            finished("A", false, None),
            RunStatus::Failed,
        ),
        (
            "an unknown outcome on a completed Job",
            finished("A", true, Some("merged")),
            RunStatus::Reported,
        ),
        (
            "an unknown outcome on a failed Job",
            finished("A", false, Some("merged")),
            RunStatus::Failed,
        ),
    ];
    for (what, v, want) in cases {
        assert_eq!(cloud_status(&v), want, "{what}");
    }
}

#[test]
fn a_cloud_row_has_the_run_shape_and_invents_nothing() {
    let mut v = finished("s-dj2-cloud-live-proof", true, Some("pull request opened"));
    v.report_path = Some("Inbox/2026-11-23-run-s-dj2-cloud-live-proof.md".into());
    v.spec.repo = "jeremyandrews/k3s".into();
    let row = cloud_run_json(&v, None);
    assert_eq!(row["run_id"], "s-dj2-cloud-live-proof");
    assert_eq!(row["host"], "cloud");
    assert_eq!(row["repo"], "jeremyandrews/k3s");
    assert_eq!(row["branch"], "jesse/s-dj2-cloud-live-proof-slug");
    assert_eq!(row["status"], "reported");
    assert_eq!(row["outcome"], "pull request opened");
    assert_eq!(row["created_at"], "2026-11-23T17:08:27.000Z");
    assert_eq!(row["queued_at"], row["created_at"]);
    assert_eq!(row["started_at"], "2026-11-23T17:08:28.000Z");
    assert_eq!(row["finished_at"], "2026-11-23T17:09:27.000Z");
    assert_eq!(
        row["report_paths"],
        json!(["Inbox/2026-11-23-run-s-dj2-cloud-live-proof.md"])
    );
    // What a cloud run lacks is null, never made up.
    for k in [
        "title",
        "strand",
        "item_id",
        "model",
        "error",
        "pull_request",
    ] {
        assert!(row[k].is_null(), "{k}: {}", row[k]);
    }
    assert!(row.get("rounds").is_none());
    assert!(row.get("reviewer").is_none());

    // A queued run has no start, and no finish.
    let q = cloud_run_json(&queued("DJ8"), None);
    assert_eq!(q["status"], "queued");
    assert!(q["started_at"].is_null());
    assert!(q["finished_at"].is_null());
    assert_eq!(q["report_paths"], json!([]));
}

// ---- the merged list ---------------------------------------------------------------------------

/// A Studio list as `list_json_mode` serves it, newest first.
fn studio_list() -> Value {
    json!({
        "runs": [
            { "run_id": "B160", "host": "studio", "status": "running",
              "created_at": "2026-11-23T17:30:00.000Z" },
            { "run_id": "A9b", "host": "studio", "status": "queued",
              "created_at": "2026-11-23T16:00:00.000Z" },
        ],
        "counts": { "running": 1, "queued": 1, "needs_you": 0, "paused": 0,
                    "needs_me": 0, "moving": 2, "finished": 0, "archived": 0 },
        "disk": {},
        "orphans": [],
    })
}

#[test]
fn cloud_rows_join_the_list_in_its_order_with_counts_of_their_own() {
    let mut list = studio_list();
    let studio_counts = list["counts"].clone();
    let cloud = CloudRuns::Listed(vec![
        running("BG4-2"),
        queued("DJ8"),
        finished("s-dj2", true, Some("pull request opened")),
        finished("X1", false, Some("failed")),
    ]);
    merge_cloud(&mut list, &cloud, ArchivedMode::Hide, |_| None);
    let ids: Vec<&str> = list["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["run_id"].as_str().unwrap())
        .collect();
    // B160 is the newest; the four cloud runs (17:08:27) by id, descending; then A9b.
    assert_eq!(ids, ["B160", "s-dj2", "X1", "DJ8", "BG4-2", "A9b"]);
    assert_eq!(list["cloud_available"], true);
    assert!(list.get("cloud_unavailable").is_none());
    // The Studio's tallies are as they were; the cloud's are their own.
    let mut without_cloud = list["counts"].clone();
    without_cloud.as_object_mut().unwrap().remove("cloud");
    assert_eq!(without_cloud, studio_counts);
    assert_eq!(
        list["counts"]["cloud"],
        json!({ "running": 1, "queued": 1, "needs_me": 1, "moving": 2, "finished": 1 })
    );
}

#[test]
fn the_archived_list_gets_no_cloud_rows() {
    let mut list = studio_list();
    merge_cloud(
        &mut list,
        &CloudRuns::Listed(vec![running("BG4-2")]),
        ArchivedMode::Only,
        |_| None,
    );
    assert_eq!(list["runs"].as_array().unwrap().len(), 2);
    assert_eq!(list["cloud_available"], true);
}

#[test]
fn a_cloud_row_takes_its_strand_lines_title() {
    let mut list = studio_list();
    let mut v = running("DJ6");
    v.strand = Some("jesse-app".into());
    v.strand_id = Some("DJ6".into());
    merge_cloud(
        &mut list,
        &CloudRuns::Listed(vec![v, running("BG4-2")]),
        ArchivedMode::Hide,
        |j| j.strand_id.as_ref().map(|id| format!("Title of {id}")),
    );
    let rows = list["runs"].as_array().unwrap();
    let dj6 = rows.iter().find(|r| r["run_id"] == "DJ6").unwrap();
    assert_eq!(dj6["title"], "Title of DJ6");
    assert_eq!(dj6["strand"], "jesse-app");
    let bg = rows.iter().find(|r| r["run_id"] == "BG4-2").unwrap();
    assert!(bg["title"].is_null());
}

// ---- the unavailable flag ----------------------------------------------------------------------

#[test]
fn without_a_cloud_host_the_list_is_the_studios_and_says_so() {
    let mut list = studio_list();
    let before = list["runs"].clone();
    merge_cloud(&mut list, &CloudRuns::Off, ArchivedMode::Hide, |_| None);
    assert_eq!(list["runs"], before);
    assert_eq!(list["cloud_available"], false);
    assert_eq!(list["cloud_unavailable"], CLOUD_NOT_CONFIGURED);
    assert!(list["counts"].get("cloud").is_none());
}

#[tokio::test]
async fn a_cloud_that_fails_to_list_is_unavailable_and_the_studio_list_stands() {
    let cloud = bounded(
        async { Err("could not list the dev jobs: connection refused".to_string()) },
        CLOUD_LIST_TIMEOUT,
    )
    .await;
    assert_eq!(
        cloud,
        CloudRuns::Unavailable("could not list the dev jobs: connection refused".into())
    );
    let mut list = studio_list();
    let before = list["runs"].clone();
    merge_cloud(&mut list, &cloud, ArchivedMode::Hide, |_| None);
    assert_eq!(list["runs"], before);
    assert_eq!(list["cloud_available"], false);
    assert_eq!(
        list["cloud_unavailable"],
        "could not list the dev jobs: connection refused"
    );
}

#[tokio::test(start_paused = true)]
async fn a_cloud_that_does_not_answer_in_time_is_unavailable() {
    let started = tokio::time::Instant::now();
    let cloud = bounded(std::future::pending(), CLOUD_LIST_TIMEOUT).await;
    assert_eq!(
        cloud,
        CloudRuns::Unavailable("kc1 did not answer within 3 s".into())
    );
    // Given up at the bound, not later.
    assert_eq!(started.elapsed(), CLOUD_LIST_TIMEOUT);
}

#[tokio::test]
async fn a_bridge_with_no_cloud_host_lists_none() {
    assert_eq!(list_cloud(None).await, CloudRuns::Off);
    assert!(!cloud_has(None, "DJ6").await);
    assert!(matches!(cloud_detail(None, "DJ6").await, Ok(None)));
}

// ---- the refused actions -----------------------------------------------------------------------

#[tokio::test]
async fn an_action_on_a_cloud_run_is_refused_and_never_reaches_a_studio_run_of_its_id() {
    // `?host=cloud`: refused, even with a Studio run of the same id (a run the cloud took).
    let e = refuse_cloud("DJ6", Some("cloud"), true, async { true })
        .await
        .unwrap_err();
    assert_eq!(e.0, StatusCode::CONFLICT);
    assert_eq!(
        e.1,
        "DJ6 is a cloud run: cloud runs are managed through Jesse, not from the Runs page"
    );
    // An id only the cloud knows: refused.
    let e = refuse_cloud("BG4-2", None, false, async { true })
        .await
        .unwrap_err();
    assert_eq!(e.0, StatusCode::CONFLICT);
    assert!(e.1.contains("managed through Jesse"));
}

#[tokio::test]
async fn an_action_on_a_studio_run_goes_on_without_asking_the_cloud() {
    // The Studio knows the id: the cloud is never asked.
    let asked = std::sync::atomic::AtomicBool::new(false);
    let probe = async {
        asked.store(true, std::sync::atomic::Ordering::SeqCst);
        true
    };
    assert!(refuse_cloud("B160", None, true, probe).await.is_ok());
    assert!(!asked.load(std::sync::atomic::Ordering::SeqCst));
    assert!(refuse_cloud("B160", Some("studio"), true, async { true })
        .await
        .is_ok());
    // Neither knows it: on to the Studio's own 404.
    assert!(refuse_cloud("ZZ9", None, false, async { false })
        .await
        .is_ok());
    // A host that is neither.
    let e = refuse_cloud("B160", Some("mars"), true, async { false })
        .await
        .unwrap_err();
    assert_eq!(e.0, StatusCode::BAD_REQUEST);
}

/// What `devjob_status` read of a running cloud Job: its log tail.
pub(crate) fn status_run(log_tail: Vec<String>) -> DevJobRun {
    DevJobRun {
        job_name: "devjob-dj6".into(),
        pod_name: None,
        phase: crate::devjob::Phase::Running,
        slug: "dj6".into(),
        repo: "jeremyandrews/jesse-app".into(),
        agent: "claude".into(),
        queued_at: String::new(),
        started_at: None,
        finished_at: None,
        elapsed_secs: None,
        waiting_reason: None,
        reported: false,
        outcome: None,
        report_path: None,
        report: None,
        log_tail,
        queued_ms: 0,
        started_ms: Some(T0 + 125_000),
        finished_ms: None,
    }
}

#[test]
fn a_cloud_runs_log_is_the_kept_tail_continued_by_the_tail_read_now() {
    let lines = |r: std::ops::RangeInclusive<u32>| -> Vec<String> {
        r.map(|i| format!("line {i}")).collect()
    };
    // The watcher kept lines 1 to 5 while the pod ran; read now, after the pod exited, the
    // tail is lines 4 to 7. The last two lines are only in the tail read now.
    let finished = status_run(lines(4..=7));
    assert_eq!(
        cloud_log_lines(Some(lines(1..=5)), &finished, 200),
        lines(1..=7)
    );
    assert_eq!(
        cloud_log_lines(Some(lines(1..=5)), &finished, 3),
        lines(5..=7)
    );
    // A kept tail with nothing in common with the newer read is too old to join.
    let run = status_run(vec!["a".into(), "b".into()]);
    assert_eq!(
        cloud_log_lines(Some(lines(1..=5)), &run, 200),
        vec!["a", "b"]
    );
    // The pod is gone and nothing was read now: the kept tail is all there is.
    assert_eq!(
        cloud_log_lines(Some(lines(1..=5)), &status_run(vec![]), 200),
        lines(1..=5)
    );
    assert_eq!(cloud_log_lines(None, &run, 200), vec!["a", "b"]);
    assert_eq!(cloud_log_lines(Some(vec![]), &run, 200), vec!["a", "b"]);
    let detail = cloud_detail_json(&running("DJ6"), &run, None);
    assert_eq!(detail["last_line"], "b");
    assert_eq!(detail["host"], "cloud");
}

/// The cloud as it stood on 2026-11-23 at 18:20 CET: two running, one finished. The app's wire
/// fixtures (`the_app_wire_fixtures_match_what_the_run_routes_serialize`) and its screenshots
/// use it.
pub(crate) fn snapshot_views() -> Vec<JobView> {
    let mut bg = running("BG4-2");
    bg.slug = "bg4-2-one-install-hosts-many-blogs-by-host-nam".into();
    bg.strand = Some("trovato".into());
    bg.strand_id = Some("BG4-2".into());
    let mut dj6 = running("DJ6");
    dj6.slug = "dj6-longer-cloud-dev-jobs-devjob-cloud-max-t".into();
    dj6.spec.repo = "jeremyandrews/jesse-app".into();
    dj6.created_ms = T0 + 120_000;
    dj6.started_ms = Some(T0 + 125_000);
    let mut proof = finished("s-dj2-cloud-live-proof", true, Some("pull request opened"));
    proof.slug = "dj2-cloud-live-proof".into();
    proof.spec.repo = "jeremyandrews/k3s".into();
    proof.created_ms = T0 - 900_000;
    proof.finished_ms = Some(T0 - 17_000);
    proof.report_path = Some("Inbox/2026-11-23-run-s-dj2-cloud-live-proof.md".into());
    vec![bg, dj6, proof]
}

// ---- a cloud run the cloud could not look up ---------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_cloud_lookup_that_does_not_answer_is_unavailable_not_missing() {
    let started = tokio::time::Instant::now();
    let found: Result<Option<()>, String> =
        bounded_lookup(std::future::pending(), CLOUD_DETAIL_TIMEOUT).await;
    assert_eq!(found, Err("kc1 did not answer within 6 s".to_string()));
    assert_eq!(started.elapsed(), CLOUD_DETAIL_TIMEOUT);
    let e = cloud_lookup_answer("DJ6", found).unwrap_err();
    assert_eq!(e.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        e.1,
        "cloud runs are unavailable, so \"DJ6\" could not be looked up: kc1 did not answer within 6 s"
    );
}

#[tokio::test]
async fn a_cloud_lookup_that_fails_is_unavailable_and_one_that_finds_nothing_is_missing() {
    let failed: Result<Option<()>, String> = bounded_lookup(
        async { Err("could not read the Job devjob-dj6: connection refused".to_string()) },
        CLOUD_DETAIL_TIMEOUT,
    )
    .await;
    let e = cloud_lookup_answer("DJ6", failed).unwrap_err();
    assert_eq!(e.0, StatusCode::SERVICE_UNAVAILABLE);
    assert!(e.1.ends_with("connection refused"), "{}", e.1);

    let none: Result<Option<()>, String> =
        bounded_lookup(async { Ok(None) }, CLOUD_DETAIL_TIMEOUT).await;
    let e = cloud_lookup_answer("ZZ9", none).unwrap_err();
    assert_eq!(e.0, StatusCode::NOT_FOUND);
    assert_eq!(e.1, "no run \"ZZ9\"");

    let found = bounded_lookup(async { Ok(Some(7)) }, CLOUD_DETAIL_TIMEOUT).await;
    assert_eq!(cloud_lookup_answer("DJ6", found), Ok(7));
}
