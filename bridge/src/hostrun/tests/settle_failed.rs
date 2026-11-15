//! A failed run settles like a waiting one (0.205.3): a ticked line or a later run of its item
//! closes it, so an archived later run never leaves the failure as the item's current run.

use super::*;

/// A run of strand item `item` in `status`, created (and finished) `mins` minutes ago.
fn item_run(st: &HostRunSettings, id: &str, status: RunStatus, mins: u64, item: &str) {
    fake_run(st, id, status, Some(now_ms() - mins * 60_000), 1);
    update_run(&st.run_dir(id), |r| {
        r.strand = Some("Gd".into());
        r.item_id = Some(item.into());
        if !status.is_finished() {
            r.finished_at = None;
        }
    })
    .unwrap();
}

fn rec(r: &Rig, id: &str) -> RunRecord {
    r.svc.get(id).unwrap()
}

#[test]
fn a_failed_run_whose_line_is_ticked_is_closed_then_archived() {
    let s = Scratch::new();
    vault_fixture(&s);
    let st = settings(&s);
    // GD6's line is ticked in the fixture.
    item_run(&st, "GD6", RunStatus::Failed, 60, "GD6");
    let r = rig_with(&s, st);
    assert_eq!(
        r.svc.close_settled(now_ms()),
        vec![("GD6".to_string(), CLOSED_ITEM_TICKED.to_string())]
    );
    assert_eq!(
        closed_reason(&r, "GD6").as_deref(),
        Some(CLOSED_ITEM_TICKED)
    );
    assert_eq!(
        r.svc.archive_settled(now_ms()),
        vec![("GD6".to_string(), ARCHIVED_ITEM_TICKED.to_string())]
    );
    assert!(rec(&r, "GD6").is_archived());
    assert_eq!(r.svc.list_json()["counts"]["needs_me"], 0);
}

#[test]
fn a_failed_run_with_a_later_run_of_its_item_is_closed_as_superseded() {
    let s = Scratch::new();
    vault_fixture(&s);
    let st = settings(&s);
    // GD5's line is open. Each failure has a later run: reported, reported and archived,
    // running. GD5 has two later runs and is superseded by the newest.
    item_run(&st, "GD5", RunStatus::Failed, 50, "GD5");
    item_run(&st, "GD5b", RunStatus::Reported, 40, "GD5");
    item_run(&st, "GD9", RunStatus::Failed, 30, "GD9");
    item_run(&st, "GD9b", RunStatus::Reported, 20, "GD9");
    update_run(&st.run_dir("GD9b"), |r| {
        r.archived_at = Some(rfc(now_ms()));
        r.archived_reason = Some(ARCHIVED_ITEM_TICKED.into());
    })
    .unwrap();
    item_run(&st, "GD10", RunStatus::Failed, 15, "GD10");
    item_run(&st, "GD10b", RunStatus::Running, 10, "GD10");
    item_run(&st, "GD5c", RunStatus::Reported, 5, "GD5");
    let r = rig_with(&s, st);
    let mut closed = r.svc.close_settled(now_ms());
    closed.sort();
    assert_eq!(
        closed,
        vec![
            ("GD10".to_string(), superseded_reason("GD10b")),
            ("GD5".to_string(), superseded_reason("GD5c")),
            ("GD9".to_string(), superseded_reason("GD9b")),
        ]
    );
    for id in ["GD5", "GD9", "GD10"] {
        assert_eq!(rec(&r, id).status, RunStatus::Closed, "{id}");
    }
    // The later runs are left as they were.
    assert_eq!(rec(&r, "GD5b").status, RunStatus::Reported);
    assert_eq!(rec(&r, "GD5c").status, RunStatus::Reported);
    assert_eq!(rec(&r, "GD10b").status, RunStatus::Running);
    // Once.
    assert_eq!(r.svc.close_settled(now_ms()), vec![]);
}

#[tokio::test]
async fn starting_a_run_again_closes_an_earlier_failed_run_of_the_item() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    finish_round(&r, "GD5", 1, None, "");
    r.svc.pass(now_ms()).await;
    assert_eq!(rec(&r, "GD5").status, RunStatus::Failed);
    assert_eq!(r.svc.start(again("GD5")).await.unwrap().run_id, "GD5b");
    // Closed by the start itself, before any watcher pass.
    assert_eq!(
        closed_reason(&r, "GD5").as_deref(),
        Some("superseded by GD5b")
    );
}

#[test]
fn a_failed_run_with_no_later_run_and_an_open_line_stays_failed() {
    let s = Scratch::new();
    vault_fixture(&s);
    let st = settings(&s);
    item_run(&st, "GD5", RunStatus::Failed, 60, "GD5");
    // A later run of another item, and an earlier run of the same item, change nothing.
    item_run(&st, "GD9", RunStatus::Reported, 30, "GD9");
    item_run(&st, "GD5a", RunStatus::Reported, 90, "GD5");
    let r = rig_with(&s, st);
    for _ in 0..2 {
        r.svc.close_settled(now_ms());
        r.svc.archive_settled(now_ms());
    }
    let g = rec(&r, "GD5");
    assert_eq!(
        (g.status, g.closed_reason, g.archived_at),
        (RunStatus::Failed, None, None)
    );
    assert_eq!(r.svc.list_json()["counts"]["needs_me"], 1);
}

#[test]
fn a_running_run_is_never_closed_or_archived_by_a_ticked_line_or_a_later_run() {
    let s = Scratch::new();
    vault_fixture(&s);
    let st = settings(&s);
    // GD6 is ticked and has a later run; so has GD5 (open line).
    item_run(&st, "GD6", RunStatus::Running, 60, "GD6");
    item_run(&st, "GD6b", RunStatus::Failed, 30, "GD6");
    item_run(&st, "GD5", RunStatus::Running, 60, "GD5");
    item_run(&st, "GD5b", RunStatus::Reported, 30, "GD5");
    let r = rig_with(&s, st);
    r.svc.close_settled(now_ms());
    r.svc.archive_settled(now_ms());
    for id in ["GD6", "GD5"] {
        let g = rec(&r, id);
        assert_eq!(
            (g.status, g.closed_reason, g.archived_at),
            (RunStatus::Running, None, None),
            "{id}"
        );
    }
    // The ticked line still settles the later failed run.
    assert_eq!(
        closed_reason(&r, "GD6b").as_deref(),
        Some(CLOSED_ITEM_TICKED)
    );
}
