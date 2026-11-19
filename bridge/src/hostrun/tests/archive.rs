//! Archiving (0.204.0): the record's two fields, archive and unarchive, the list's three
//! modes and `counts`, the watcher's three rules and what they never touch, and the routes.

use super::*;
use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

/// A run of `status` whose last status change was `days` ago, of strand item `item` when given.
fn aged_run(st: &HostRunSettings, id: &str, status: RunStatus, days: u64, item: Option<&str>) {
    let at = now_ms() - days * DAY_MS;
    fake_run(st, id, status, Some(at), 1);
    if let Some(item) = item {
        update_run(&st.run_dir(id), |r| {
            r.strand = Some("Gd".into());
            r.item_id = Some(item.into());
        })
        .unwrap();
    }
}

fn rec(r: &Rig, id: &str) -> RunRecord {
    r.svc.get(id).unwrap()
}

fn ids(v: &Value) -> Vec<String> {
    v["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["run_id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_record_from_before_archived_still_reads_and_an_archived_one_round_trips() {
    // Written by bridge 0.198.0: no archived_* keys.
    let old = include_str!(
        "../../../../JesseKit/Tests/JesseNetworkingTests/Fixtures/studio-run-needs-you.json"
    );
    let rec: RunRecord = serde_json::from_str(old).unwrap();
    assert_eq!((rec.archived_at, rec.archived_reason), (None, None));
    // A record that is not archived serializes exactly the keys it did before.
    let v = serde_json::to_value(RunRecord::default()).unwrap();
    assert!(v.get("archived_at").is_none() && v.get("archived_reason").is_none());
    let s = Scratch::new();
    let st = settings(&s);
    fake_run(&st, "A1", RunStatus::Reported, Some(1_000), 1);
    let dir = st.run_dir("A1");
    update_run(&dir, |r| {
        r.archived_at = Some(rfc(2_000));
        r.archived_reason = Some(ARCHIVED_BY_JEREMY.into());
    })
    .unwrap();
    let text = std::fs::read_to_string(dir.join(RUN_JSON)).unwrap();
    assert!(
        text.contains("\"archived_reason\": \"archived by Jeremy\""),
        "{text}"
    );
    let back = read_run(&dir).unwrap();
    assert_eq!(back.archived_at, Some(rfc(2_000)));
    assert!(back.is_archived());
}

#[test]
fn archive_refuses_a_moving_run_naming_its_status_and_an_unknown_one() {
    let s = Scratch::new();
    let st = settings(&s);
    for (id, status) in [
        ("Q", RunStatus::Queued),
        ("R", RunStatus::Running),
        ("P", RunStatus::Paused),
    ] {
        fake_run(&st, id, status, None, 1);
    }
    let r = rig_with(&s, st);
    for (id, word) in [("Q", "queued"), ("R", "running"), ("P", "paused")] {
        let e = r.svc.archive(id).unwrap_err();
        assert_eq!(e.status(), StatusCode::CONFLICT, "{id}");
        assert!(
            e.message().contains(&format!("{id} is {word}")),
            "{}",
            e.message()
        );
        assert_eq!(rec(&r, id).archived_at, None);
    }
    assert_eq!(
        r.svc.archive("NOPE").unwrap_err().status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        r.svc.unarchive("NOPE").unwrap_err().status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        r.svc.archive("../x").unwrap_err().status(),
        StatusCode::NOT_FOUND
    );
}

#[test]
fn archive_closes_a_waiting_run_first_and_flags_a_finished_one_in_place() {
    let s = Scratch::new();
    let st = settings(&s);
    let t = now_ms() - DAY_MS;
    for (id, status) in [
        ("NY", RunStatus::NeedsYou),
        ("IN", RunStatus::Interrupted),
        ("FA", RunStatus::Failed),
        ("RE", RunStatus::Reported),
        ("CA", RunStatus::Cancelled),
        ("CL", RunStatus::Closed),
    ] {
        fake_run(&st, id, status, Some(t), 64);
    }
    let r = rig_with(&s, st.clone());
    for id in ["NY", "IN"] {
        let a = r.svc.archive(id).unwrap();
        assert_eq!(a.status, RunStatus::Closed, "{id}");
        assert_eq!(a.closed_reason.as_deref(), Some(ARCHIVED_BY_JEREMY));
        assert_eq!(a.archived_reason.as_deref(), Some(ARCHIVED_BY_JEREMY));
        assert!(a.archived_at.is_some() && a.is_archived());
    }
    for (id, status) in [
        ("FA", RunStatus::Failed),
        ("RE", RunStatus::Reported),
        ("CA", RunStatus::Cancelled),
        ("CL", RunStatus::Closed),
    ] {
        let a = r.svc.archive(id).unwrap();
        assert_eq!(
            a.status, status,
            "{id}: archiving never changes a finished status"
        );
        assert_eq!(a.closed_reason, None, "{id}");
        assert_eq!(a.archived_reason.as_deref(), Some(ARCHIVED_BY_JEREMY));
    }
    // A flag only: the directory and everything in it stay.
    for id in ["NY", "FA", "RE"] {
        assert!(st.run_dir(id).join(RUN_JSON).is_file());
        assert!(st.run_dir(id).join(TARGET_DIR).join("blob").is_file());
    }
    // Silent.
    assert!(lock(&r.rec.pushes).is_empty());
    assert!(lock(&r.rec.turns).is_empty());
}

#[test]
fn archive_and_unarchive_are_idempotent_and_unarchive_clears_only_the_two_fields() {
    let s = Scratch::new();
    let st = settings(&s);
    fake_run(&st, "RE", RunStatus::Reported, Some(now_ms() - DAY_MS), 1);
    let r = rig_with(&s, st);
    let before = rec(&r, "RE");
    let first = r.svc.archive("RE").unwrap();
    let again = r.svc.archive("RE").unwrap();
    assert_eq!(first, again, "a second archive changes nothing");
    let back = r.svc.unarchive("RE").unwrap();
    assert_eq!(
        back, before,
        "unarchive clears archived_at and archived_reason and nothing else"
    );
    assert_eq!(r.svc.unarchive("RE").unwrap(), before);
    // A closed waiting run stays closed after an unarchive.
    fake_run(
        &r.svc.settings,
        "NY",
        RunStatus::NeedsYou,
        Some(now_ms()),
        1,
    );
    r.svc.archive("NY").unwrap();
    let ny = r.svc.unarchive("NY").unwrap();
    assert_eq!(ny.status, RunStatus::Closed);
    assert_eq!(ny.closed_reason.as_deref(), Some(ARCHIVED_BY_JEREMY));
    assert_eq!((ny.archived_at, ny.archived_reason), (None, None));
}

#[test]
fn the_list_hides_archived_runs_and_serves_them_only_or_all_with_counts() {
    let s = Scratch::new();
    let st = settings(&s);
    let t = now_ms() - 3 * DAY_MS;
    for (i, (id, status)) in [
        ("NY", RunStatus::NeedsYou),
        ("IN", RunStatus::Interrupted),
        ("FA", RunStatus::Failed),
        ("QU", RunStatus::Queued),
        ("RU", RunStatus::Running),
        ("PA", RunStatus::Paused),
        ("RE", RunStatus::Reported),
        ("CA", RunStatus::Cancelled),
        ("CL", RunStatus::Closed),
        ("OLD", RunStatus::Reported),
        ("OLDER", RunStatus::Reported),
    ]
    .into_iter()
    .enumerate()
    {
        fake_run(&st, id, status, Some(t + i as u64 * 1000), 1);
    }
    let r = rig_with(&s, st.clone());
    // OLDER archived first, then OLD: newest archived first is OLD.
    let dir = |id: &str| st.run_dir(id);
    update_run(&dir("OLDER"), |r| {
        r.archived_at = Some(rfc(t + 100_000));
        r.archived_reason = Some(ARCHIVED_AGED.into());
    })
    .unwrap();
    update_run(&dir("OLD"), |r| {
        r.archived_at = Some(rfc(t + 200_000));
        r.archived_reason = Some(ARCHIVED_BY_JEREMY.into());
    })
    .unwrap();

    let hide = r.svc.list_json();
    assert_eq!(
        hide["runs"],
        r.svc.list_json_mode(ArchivedMode::Hide)["runs"]
    );
    let shown = ids(&hide);
    assert_eq!(shown.len(), 9);
    assert!(!shown.contains(&"OLD".to_string()) && !shown.contains(&"OLDER".to_string()));
    assert_eq!(shown[0], "CL", "newest created first");

    let only = r.svc.list_json_mode(ArchivedMode::Only);
    assert_eq!(ids(&only), vec!["OLD", "OLDER"]);
    assert_eq!(only["runs"][0]["archived_reason"], ARCHIVED_BY_JEREMY);

    let all = r.svc.list_json_mode(ArchivedMode::All);
    assert_eq!(ids(&all).len(), 11);

    let want = json!({
        "running": 1, "queued": 1, "needs_you": 1, "paused": 1,
        "needs_me": 3, "moving": 3, "finished": 3, "archived": 2,
    });
    for v in [&hide, &only, &all] {
        assert_eq!(
            v["counts"], want,
            "counts cover the whole store in every mode"
        );
    }

    assert_eq!(ArchivedMode::parse(None), Ok(ArchivedMode::Hide));
    assert_eq!(ArchivedMode::parse(Some("only")), Ok(ArchivedMode::Only));
    assert_eq!(ArchivedMode::parse(Some("all")), Ok(ArchivedMode::All));
    assert!(ArchivedMode::parse(Some("yes")).is_err());
}

#[test]
fn a_run_a_reply_put_back_in_the_queue_shows_again_while_still_flagged() {
    let s = Scratch::new();
    let st = settings(&s);
    fake_run(&st, "RE", RunStatus::Reported, Some(now_ms()), 1);
    let r = rig_with(&s, st.clone());
    r.svc.archive("RE").unwrap();
    assert!(ids(&r.svc.list_json()).is_empty());
    update_run(&st.run_dir("RE"), |r| r.status = RunStatus::Queued).unwrap();
    assert!(!rec(&r, "RE").is_archived());
    assert_eq!(ids(&r.svc.list_json()), vec!["RE"]);
    assert_eq!(r.svc.list_json()["counts"]["archived"], 0);
}

#[test]
fn the_watcher_archives_a_finished_run_whose_line_is_ticked() {
    let s = Scratch::new();
    vault_fixture(&s);
    let st = settings(&s);
    // GD6 is checked in the fixture note; GD5 is not.
    aged_run(&st, "GD6", RunStatus::Reported, 1, Some("GD6"));
    aged_run(&st, "GD6b", RunStatus::Closed, 1, Some("GD6"));
    aged_run(&st, "GD5", RunStatus::Reported, 1, Some("GD5"));
    let r = rig_with(&s, st);
    let mut got = r.svc.archive_settled(now_ms());
    got.sort();
    assert_eq!(
        got,
        vec![
            ("GD6".to_string(), ARCHIVED_ITEM_TICKED.to_string()),
            ("GD6b".to_string(), ARCHIVED_ITEM_TICKED.to_string()),
        ]
    );
    assert_eq!(rec(&r, "GD5").archived_at, None);
    // Once, and silently.
    assert!(r.svc.archive_settled(now_ms()).is_empty());
    assert!(lock(&r.rec.pushes).is_empty() && lock(&r.rec.turns).is_empty());
}

#[test]
fn the_watcher_archives_a_finished_run_whose_reports_all_moved_to_the_archive() {
    let s = Scratch::new();
    let st = settings(&s);
    let archive = s.notes().join(INBOX_ARCHIVE_DIR);
    std::fs::create_dir_all(&archive).unwrap();
    for (id, status) in [
        ("RE", RunStatus::Reported),
        ("CA", RunStatus::Cancelled),
        ("KEPT", RunStatus::Reported),
        ("GONE", RunStatus::Reported),
    ] {
        aged_run(&st, id, status, 1, None);
        update_run(&st.run_dir(id), |r| {
            r.rounds = vec![Round {
                n: 1,
                report_path: Some(format!("Inbox/{id}.md")),
                ..Round::default()
            }]
        })
        .unwrap();
    }
    std::fs::write(archive.join("RE.md"), "x").unwrap();
    std::fs::write(archive.join("CA.md"), "x").unwrap();
    // Still in the Inbox: kept. Deleted, not archived: kept.
    std::fs::write(s.notes().join("Inbox/KEPT.md"), "x").unwrap();
    let r = rig_with(&s, st);
    let mut got = r.svc.archive_settled(now_ms());
    got.sort();
    assert_eq!(
        got,
        vec![
            ("CA".to_string(), ARCHIVED_REPORT_ARCHIVED.to_string()),
            ("RE".to_string(), ARCHIVED_REPORT_ARCHIVED.to_string()),
        ]
    );
    assert_eq!(rec(&r, "KEPT").archived_at, None);
    assert_eq!(rec(&r, "GONE").archived_at, None);
    // The reports were read, never moved.
    assert!(archive.join("RE.md").is_file() && s.notes().join("Inbox/KEPT.md").is_file());
}

#[test]
fn the_watcher_archives_a_run_finished_over_fourteen_days_ago_and_not_one_newer() {
    let s = Scratch::new();
    let st = settings(&s);
    aged_run(
        &st,
        "OLD",
        RunStatus::Reported,
        AUTO_ARCHIVE_AFTER_DAYS + 1,
        None,
    );
    aged_run(&st, "OLDCL", RunStatus::Closed, 20, None);
    aged_run(
        &st,
        "NEW",
        RunStatus::Reported,
        AUTO_ARCHIVE_AFTER_DAYS - 1,
        None,
    );
    // Created long ago, closed yesterday: its last status change is yesterday.
    aged_run(&st, "RECLOSED", RunStatus::Closed, 30, None);
    update_run(&st.run_dir("RECLOSED"), |r| {
        r.closed_at = Some(rfc(now_ms() - DAY_MS))
    })
    .unwrap();
    let r = rig_with(&s, st);
    let mut got = r.svc.archive_settled(now_ms());
    got.sort();
    assert_eq!(
        got,
        vec![
            ("OLD".to_string(), ARCHIVED_AGED.to_string()),
            ("OLDCL".to_string(), ARCHIVED_AGED.to_string()),
        ]
    );
    assert_eq!(AUTO_ARCHIVE_AFTER_DAYS, 14);
}

#[test]
fn the_watcher_never_archives_a_run_that_waits_for_jeremy_or_moves() {
    let s = Scratch::new();
    vault_fixture(&s);
    let st = settings(&s);
    // Every rule would apply: 30 days old and its line (GD6) ticked.
    for (id, status) in [
        ("FA", RunStatus::Failed),
        ("NY", RunStatus::NeedsYou),
        ("IN", RunStatus::Interrupted),
        ("QU", RunStatus::Queued),
        ("RU", RunStatus::Running),
        ("PA", RunStatus::Paused),
    ] {
        aged_run(&st, id, status, 30, Some("GD6"));
    }
    let r = rig_with(&s, st);
    assert!(r.svc.archive_settled(now_ms()).is_empty());
    for id in ["FA", "NY", "IN", "QU", "RU", "PA"] {
        assert_eq!(rec(&r, id).archived_at, None, "{id}");
        assert_eq!(
            r.svc.auto_archive_reason(&rec(&r, id), now_ms()),
            None,
            "{id}"
        );
    }
    // A failed run 30 days old stays, pass after pass.
    assert!(r.svc.archive_settled(now_ms() + 30 * DAY_MS).is_empty());
    assert_eq!(rec(&r, "FA").status, RunStatus::Failed);
}

#[test]
fn the_watcher_keeps_the_first_archive_and_leaves_a_run_jeremy_unarchived() {
    let s = Scratch::new();
    let st = settings(&s);
    aged_run(&st, "BYHAND", RunStatus::Reported, 30, None);
    aged_run(&st, "UNDONE", RunStatus::Reported, 30, None);
    let r = rig_with(&s, st);
    let by_hand = r.svc.archive("BYHAND").unwrap();
    r.svc.archive("UNDONE").unwrap();
    r.svc.unarchive("UNDONE").unwrap();
    assert!(r.svc.archive_settled(now_ms()).is_empty());
    assert_eq!(
        rec(&r, "BYHAND"),
        by_hand,
        "an archived run is not archived again"
    );
    assert_eq!(
        rec(&r, "UNDONE").archived_at,
        None,
        "an Undo is not taken back"
    );
    // Archiving it by hand again lifts the hold.
    r.svc.archive("UNDONE").unwrap();
    r.svc.unarchive("UNDONE").unwrap();
    r.svc.archive("UNDONE").unwrap();
    assert!(rec(&r, "UNDONE").is_archived());
}

#[tokio::test]
async fn the_watcher_pass_closes_then_archives_a_waiting_run_whose_line_is_ticked_only_closing_it()
{
    // `close_settled` closes a ticked needs_you run; the same pass then archives the closed run.
    let s = Scratch::new();
    vault_fixture(&s);
    let st = settings(&s);
    aged_run(&st, "GD6", RunStatus::NeedsYou, 1, Some("GD6"));
    let r = rig_with(&s, st);
    r.svc.pass(now_ms()).await;
    let g = rec(&r, "GD6");
    assert_eq!(g.status, RunStatus::Closed);
    assert_eq!(g.closed_reason.as_deref(), Some(CLOSED_ITEM_TICKED));
    assert_eq!(g.archived_reason.as_deref(), Some(ARCHIVED_ITEM_TICKED));
}

// ---- THE ROUTES -----------------------------------------------------------------------

fn route_state(r: &Rig) -> AppState {
    let cfg = crate::testutil::test_config();
    let _ = cfg.hostrun.service.0.set(r.svc.clone());
    AppState::new(cfg)
}

async fn call(st: &AppState, method: &str, path: &str, auth: bool) -> (StatusCode, String) {
    let mut req = Request::builder().method(method).uri(path);
    if auth {
        req = req.header("authorization", "Bearer test-token");
    }
    let res = crate::handlers::app(st.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn the_archive_route_archives_refuses_a_moving_run_and_needs_the_paired_token() {
    let s = Scratch::new();
    let st = settings(&s);
    fake_run(&st, "RE", RunStatus::Reported, Some(now_ms()), 1);
    fake_run(&st, "NY", RunStatus::NeedsYou, Some(now_ms()), 1);
    fake_run(&st, "RU", RunStatus::Running, None, 1);
    let r = rig_with(&s, st);
    let app = route_state(&r);
    assert_eq!(
        call(&app, "POST", "/jesse/runs/RE/archive", false).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(rec(&r, "RE").archived_at, None);
    let (status, body) = call(&app, "POST", "/jesse/runs/RE/archive", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["archived_reason"], ARCHIVED_BY_JEREMY);
    assert!(v["archived_at"].is_string() && v.get("title").is_some());
    // Idempotent.
    let (status, again) = call(&app, "POST", "/jesse/runs/RE/archive", true).await;
    assert_eq!((status, again), (StatusCode::OK, body));
    // needs_you: closed first.
    let (_, body) = call(&app, "POST", "/jesse/runs/NY/archive", true).await;
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (v["status"].as_str(), v["closed_reason"].as_str()),
        (Some("closed"), Some(ARCHIVED_BY_JEREMY))
    );
    // Running: 409, the bridge's sentence as the body.
    let (status, body) = call(&app, "POST", "/jesse/runs/RU/archive", true).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("RU is running"), "{body}");
    assert_eq!(
        call(&app, "POST", "/jesse/runs/NOPE/archive", true).await.0,
        StatusCode::NOT_FOUND
    );
    // The list leaves both archived runs out, serves them under only, and counts them.
    let (_, list) = call(&app, "GET", "/jesse/runs", true).await;
    let list: Value = serde_json::from_str(&list).unwrap();
    assert_eq!(ids(&list), vec!["RU"]);
    assert_eq!(list["counts"]["archived"], 2);
    let (_, only) = call(&app, "GET", "/jesse/runs?archived=only", true).await;
    assert_eq!(ids(&serde_json::from_str(&only).unwrap()).len(), 2);
    let (_, all) = call(&app, "GET", "/jesse/runs?archived=all", true).await;
    assert_eq!(ids(&serde_json::from_str(&all).unwrap()).len(), 3);
    assert_eq!(
        call(&app, "GET", "/jesse/runs?archived=maybe", true)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn the_unarchive_route_puts_a_run_back_and_needs_the_paired_token() {
    let s = Scratch::new();
    let st = settings(&s);
    fake_run(&st, "RE", RunStatus::Reported, Some(now_ms()), 1);
    let r = rig_with(&s, st);
    let app = route_state(&r);
    r.svc.archive("RE").unwrap();
    assert_eq!(
        call(&app, "POST", "/jesse/runs/RE/unarchive", false)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert!(rec(&r, "RE").is_archived());
    for _ in 0..2 {
        let (status, body) = call(&app, "POST", "/jesse/runs/RE/unarchive", true).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("archived_at").is_none() && v.get("archived_reason").is_none());
        assert_eq!(v["status"], "reported");
    }
    let (_, list) = call(&app, "GET", "/jesse/runs", true).await;
    assert_eq!(ids(&serde_json::from_str(&list).unwrap()), vec!["RE"]);
    assert_eq!(
        call(&app, "POST", "/jesse/runs/NOPE/unarchive", true)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert!(lock(&r.rec.pushes).is_empty() && lock(&r.rec.turns).is_empty());
}
