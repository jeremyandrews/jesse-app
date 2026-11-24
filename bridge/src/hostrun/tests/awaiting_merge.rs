//! A run that waits for Jeremy's merge (0.218.0): a report whose first line is
//! `AWAITING MERGE: <pull request URL>` makes the run `awaiting_merge`, pushes "Merge needed",
//! and the watcher reads the pull request every [`MERGE_CHECK_EVERY_MS`] until it is merged
//! (the run resumes in its session), closed (the run needs Jeremy) or the run is answered,
//! closed or archived.

use super::*;

const URL: &str = "https://github.com/jeremyandrews/k3s/pull/412";
const TITLE: &str = "Give dev jobs a second cloud slot";
const MERGED_AT: &str = "2026-11-24T09:12:31Z";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn open() -> Result<PullMerge, String> {
    Ok(PullMerge {
        state: PrState::Open,
        merged_at: None,
        merge_commit: None,
        title: Some(TITLE.into()),
    })
}

fn merged() -> Result<PullMerge, String> {
    Ok(PullMerge {
        state: PrState::Merged,
        merged_at: Some(MERGED_AT.into()),
        merge_commit: Some(SHA.into()),
        title: Some(TITLE.into()),
    })
}

fn closed() -> Result<PullMerge, String> {
    Ok(PullMerge {
        state: PrState::Closed,
        merged_at: None,
        merge_commit: None,
        title: Some(TITLE.into()),
    })
}

fn set_pull(r: &Rig, state: Result<PullMerge, String>) {
    lock(&r.forge.pulls).insert(URL.to_string(), state);
}

fn pull_asks(r: &Rig) -> usize {
    lock(&r.forge.pull_asks).len()
}

fn pushes(r: &Rig) -> Vec<(String, String, String)> {
    lock(&r.rec.pushes).clone()
}

/// One watcher pass at `now`, then every merge check it started, answered.
async fn step(r: &Rig, now: u64) {
    r.svc.pass(now).await;
    r.svc.merge_checks_settled().await;
}

const MERGE_REPORT: &str = "AWAITING MERGE: https://github.com/jeremyandrews/k3s/pull/412\n\n\
The k3s change is green and needs your merge before the bridge side can land.\n";

/// GD5 started, its first round reported `AWAITING MERGE:` with the pull request open, and the
/// pass that finalized it run.
async fn awaiting(s: &Scratch) -> Rig {
    vault_fixture(s);
    remote_fixture(s);
    let r = rig(s);
    set_pull(&r, open());
    r.svc.start(item("GD5")).await.unwrap();
    finish_round(&r, "GD5", 0, Some(MERGE_REPORT), "");
    step(&r, now_ms()).await;
    r
}

fn the_push_body() -> String {
    format!("GD5 (GD5): {TITLE}\n{URL}")
}

#[tokio::test]
async fn an_awaiting_merge_report_waits_pushes_once_and_keeps_the_worktree_and_session() {
    let s = Scratch::new();
    let r = awaiting(&s).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::AwaitingMerge);
    assert_eq!(rec.status.label(), "awaiting_merge");
    assert_eq!(rec.round().unwrap().outcome, Some(RunStatus::AwaitingMerge));
    let w = rec.merge_wait.clone().expect("the wait is recorded");
    assert_eq!(w.url, URL);
    assert_eq!(w.owner, "jeremyandrews");
    assert_eq!(w.repo, "k3s");
    assert_eq!(w.number, 412);
    assert_eq!(w.title.as_deref(), Some(TITLE));
    assert!(w.last_push_at.is_some());
    assert_eq!(w.ended, None);
    // Exactly one push, naming the run, its strand ID and the pull request.
    let p = pushes(&r);
    assert_eq!(p.len(), 1, "{p:?}");
    assert_eq!(p[0].0, "Merge needed");
    assert_eq!(p[0].1, the_push_body());
    assert_eq!(p[0].2, "GD5");
    // The worktree and the session stay; the run holds no slot and counts as needing Jeremy.
    let dir = r.svc.settings.run_dir("GD5");
    assert!(dir.join(WORKTREE_DIR).is_dir());
    assert_eq!(
        rec.session_id.as_deref(),
        Some("11111111-2222-3333-4444-555555555555")
    );
    assert_eq!(r.svc.list_json()["counts"]["needs_me"], 1);
    assert!(r.svc.live_run_of("Gd", "GD5").is_some());
    // The Inbox report is written as for any round, with its outcome.
    let rel = format!("Inbox/{}-run-GD5.md", today());
    let text = std::fs::read_to_string(s.notes().join(&rel)).unwrap();
    assert!(text.contains("AWAITING MERGE: https://github.com/jeremyandrews/k3s/pull/412"));
    // The cleanup leaves the worktree alone however long it waits.
    r.svc.cleanup(now_ms() + 90 * DAY_MS);
    assert!(dir.join(WORKTREE_DIR).is_dir());
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::AwaitingMerge);
}

#[tokio::test]
async fn while_the_pull_request_is_open_a_pass_changes_nothing_and_a_push_repeats_daily() {
    let s = Scratch::new();
    let r = awaiting(&s).await;
    let t0 = now_ms();
    let before = r.svc.get("GD5").unwrap();
    // Passes inside the check interval do not even ask.
    let asked = pull_asks(&r);
    step(&r, t0 + 60_000).await;
    step(&r, t0 + 120_000).await;
    let after = r.svc.get("GD5").unwrap();
    assert_eq!(after.status, RunStatus::AwaitingMerge);
    assert_eq!(after.rounds, before.rounds);
    assert_eq!(pushes(&r).len(), 1);
    // Every five minutes it asks, and nothing changes but when it last asked.
    let mut t = t0;
    for _ in 0..12 {
        t += MERGE_CHECK_EVERY_MS;
        step(&r, t).await;
    }
    assert!(pull_asks(&r) >= asked + 12, "asked {}", pull_asks(&r));
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::AwaitingMerge);
    assert_eq!(rec.rounds.len(), 1);
    assert_eq!(pushes(&r).len(), 1, "no second push within a day");
    // A day on: one fresh push, the same one, and only one however many passes follow.
    let day = t0 + DAY_MS + 1000;
    step(&r, day).await;
    step(&r, day + MERGE_CHECK_EVERY_MS).await;
    step(&r, day + 2 * MERGE_CHECK_EVERY_MS).await;
    let p = pushes(&r);
    assert_eq!(p.len(), 2, "{p:?}");
    assert_eq!(p[1].0, "Merge needed");
    assert_eq!(p[1].1, the_push_body());
    // A second day, a third push.
    step(&r, day + DAY_MS + 1000).await;
    assert_eq!(pushes(&r).len(), 3);
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::AwaitingMerge);
}

#[tokio::test]
async fn a_failed_check_is_logged_and_retried_and_never_changes_the_run() {
    let s = Scratch::new();
    let r = awaiting(&s).await;
    set_pull(&r, Err("gh: API rate limit exceeded".into()));
    let t0 = now_ms();
    for i in 1..=4 {
        step(&r, t0 + i * MERGE_CHECK_EVERY_MS).await;
    }
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::AwaitingMerge);
    assert_eq!(rec.rounds.len(), 1);
    assert_eq!(pushes(&r).len(), 1);
    // Then it merges, and the next check resumes it.
    set_pull(&r, merged());
    step(&r, t0 + 5 * MERGE_CHECK_EVERY_MS).await;
    assert_eq!(r.svc.get("GD5").unwrap().rounds.len(), 2);
}

#[tokio::test]
async fn on_merge_the_run_resumes_in_its_session_with_the_merge_facts() {
    let s = Scratch::new();
    let r = awaiting(&s).await;
    set_pull(&r, merged());
    step(&r, now_ms() + MERGE_CHECK_EVERY_MS).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Running);
    assert_eq!(rec.rounds.len(), 2);
    let round = rec.round().unwrap();
    assert_eq!(
        round.text.as_deref(),
        Some(
            "Jeremy merged https://github.com/jeremyandrews/k3s/pull/412 at 2026-11-24T09:12:31Z \
             as 0123456789abcdef0123456789abcdef01234567. Continue from the next step."
        )
    );
    assert_eq!(round.auto.as_deref(), Some(AUTO_MERGE));
    // The same session: the runner is told to resume it.
    assert_eq!(
        rec.session_id.as_deref(),
        Some("11111111-2222-3333-4444-555555555555")
    );
    let w = rec.merge_wait.clone().unwrap();
    assert!(w.ended.as_deref().unwrap().starts_with("merged"), "{w:?}");
    // No push for the resume itself; one message, once.
    assert_eq!(pushes(&r).len(), 1);
    step(&r, now_ms() + 2 * MERGE_CHECK_EVERY_MS).await;
    assert_eq!(r.svc.get("GD5").unwrap().rounds.len(), 2);
    // Its next report is classified as any report is.
    finish_round(&r, "GD5", 0, Some("Done: the bridge side merged.\n"), "");
    step(&r, now_ms()).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    let p = pushes(&r);
    assert_eq!(p.last().unwrap().0, "Run GD5 reported");
}

#[tokio::test]
async fn a_merge_resume_queues_for_a_slot_like_any_reply() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let mut st = settings(&s);
    st.max_concurrent = 1;
    let r = rig_with(&s, st);
    set_pull(&r, open());
    r.svc.start(item("GD5")).await.unwrap();
    finish_round(&r, "GD5", 0, Some(MERGE_REPORT), "");
    step(&r, now_ms()).await;
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::AwaitingMerge);
    // An awaiting run holds no slot: another run starts.
    let other = r.svc.start(free("other")).await.unwrap();
    assert_eq!(other.status, RunStatus::Running);
    set_pull(&r, merged());
    step(&r, now_ms() + MERGE_CHECK_EVERY_MS).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Queued);
    assert!(rec.waiting_for.is_some(), "{rec:?}");
    assert_eq!(rec.rounds.len(), 2);
}

#[tokio::test]
async fn a_pull_request_closed_without_merging_makes_the_run_need_jeremy() {
    let s = Scratch::new();
    let r = awaiting(&s).await;
    set_pull(&r, closed());
    step(&r, now_ms() + MERGE_CHECK_EVERY_MS).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::NeedsYou);
    assert_eq!(rec.rounds.len(), 1);
    let w = rec.merge_wait.clone().unwrap();
    assert_eq!(
        w.ended.as_deref(),
        Some("https://github.com/jeremyandrews/k3s/pull/412 was closed without merging")
    );
    let p = pushes(&r);
    assert_eq!(p.len(), 2, "{p:?}");
    assert_eq!(p[1].0, "Run GD5 needs you");
    assert_eq!(
        p[1].1,
        "GD5 (GD5): https://github.com/jeremyandrews/k3s/pull/412 was closed without merging"
    );
    // The watch is over: later passes ask nothing and send nothing.
    let asked = pull_asks(&r);
    step(&r, now_ms() + 3 * MERGE_CHECK_EVERY_MS).await;
    assert_eq!(pull_asks(&r), asked);
    assert_eq!(pushes(&r).len(), 2);
    // And it is answered as any needs_you run is.
    let rec = r.svc.reply_and_pump("GD5", "Reopen it.").await.unwrap();
    assert_eq!(rec.status, RunStatus::Running);
}

#[tokio::test]
async fn a_reply_to_an_awaiting_run_works_as_on_needs_you_and_stops_the_watch() {
    let s = Scratch::new();
    let r = awaiting(&s).await;
    let rec = r
        .svc
        .reply_and_pump("GD5", "I will not merge it; do it another way.")
        .await
        .unwrap();
    assert_eq!(rec.status, RunStatus::Running);
    assert_eq!(rec.rounds.len(), 2);
    assert_eq!(
        rec.round().unwrap().text.as_deref(),
        Some("I will not merge it; do it another way.")
    );
    assert_eq!(rec.round().unwrap().auto, None);
    assert_eq!(rec.merge_wait.unwrap().ended.as_deref(), Some("answered"));
    // A merge seen later starts nothing.
    set_pull(&r, merged());
    let asked = pull_asks(&r);
    step(&r, now_ms() + MERGE_CHECK_EVERY_MS).await;
    assert_eq!(pull_asks(&r), asked);
    assert_eq!(r.svc.get("GD5").unwrap().rounds.len(), 2);
}

#[tokio::test]
async fn an_empty_continue_is_refused_as_on_needs_you_and_the_watch_goes_on() {
    // A blind continue would carry on as if the merge had happened.
    let s = Scratch::new();
    let r = awaiting(&s).await;
    let e = r.svc.reply("GD5", CONTINUE_TEXT).unwrap_err();
    assert!(matches!(e, Refusal::State(_)), "{e:?}");
    assert_eq!(e.status(), StatusCode::CONFLICT);
    assert!(e.message().contains(URL), "{}", e.message());
    let e = r
        .svc
        .reply_and_pump("GD5", &format!("  {CONTINUE_TEXT}\n"))
        .await
        .unwrap_err();
    assert!(matches!(e, Refusal::State(_)), "{e:?}");
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::AwaitingMerge);
    assert_eq!(rec.rounds.len(), 1);
    assert_eq!(rec.merge_wait.unwrap().ended, None);
    // The watch is intact: the merge still resumes it.
    set_pull(&r, merged());
    step(&r, now_ms() + MERGE_CHECK_EVERY_MS).await;
    assert_eq!(r.svc.get("GD5").unwrap().rounds.len(), 2);
}

#[tokio::test]
async fn cancel_refuses_an_awaiting_run_as_it_refuses_a_needs_you_one() {
    let s = Scratch::new();
    let r = awaiting(&s).await;
    let e = r.svc.cancel("GD5").unwrap_err();
    assert!(matches!(e, Refusal::State(_)), "{e:?}");
    assert_eq!(e.status(), StatusCode::CONFLICT);
    // The needs_you refusal, word for word, with the status named.
    assert_eq!(
        e.message(),
        "GD5 is awaiting_merge; only a queued, running, paused or reviewing run can be cancelled"
    );
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::AwaitingMerge);
}

#[tokio::test]
async fn close_settles_an_awaiting_run_silently_and_ends_the_watch() {
    let s = Scratch::new();
    let r = awaiting(&s).await;
    let pushed = pushes(&r).len();
    let rec = r.svc.close("GD5", None).unwrap();
    assert_eq!(rec.status, RunStatus::Closed);
    assert_eq!(rec.closed_reason.as_deref(), Some(CLOSED_BY_JEREMY));
    assert_eq!(
        rec.merge_wait.clone().unwrap().ended,
        Some(format!("closed ({CLOSED_BY_JEREMY})"))
    );
    set_pull(&r, merged());
    let asked = pull_asks(&r);
    step(&r, now_ms() + MERGE_CHECK_EVERY_MS).await;
    assert_eq!(pull_asks(&r), asked);
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Closed);
    assert_eq!(rec.rounds.len(), 1);
    assert_eq!(pushes(&r).len(), pushed);
}

#[tokio::test]
async fn archive_closes_an_awaiting_run_first_as_it_does_a_needs_you_one() {
    let s = Scratch::new();
    let r = awaiting(&s).await;
    let rec = r.svc.archive("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Closed);
    assert!(rec.is_archived());
}

#[tokio::test]
async fn an_awaiting_merge_line_with_no_pull_request_url_is_needs_you() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    let line = "AWAITING MERGE: the k3s change, once CI is green";
    r.svc.start(item("GD5")).await.unwrap();
    finish_round(&r, "GD5", 0, Some(&format!("{line}\nmore\n")), "");
    step(&r, now_ms()).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::NeedsYou);
    assert_eq!(rec.merge_wait, None);
    let p = pushes(&r);
    assert_eq!(p.len(), 1, "{p:?}");
    assert_eq!(p[0].0, "Run GD5 needs you");
    assert_eq!(p[0].1, line);
    assert_eq!(pull_asks(&r), 0);
}

#[test]
fn every_awaiting_merge_line_without_a_pull_request_url_classifies_as_needs_you() {
    let round = Round::default();
    let exit = ExitInfo {
        round: 1,
        code: Some(0),
        signal: None,
        ended_at: rfc(1),
    };
    for line in [
        "AWAITING MERGE: the k3s change, once CI is green",
        "AWAITING MERGE: https://github.com/jeremyandrews/k3s/issues/412",
        "AWAITING MERGE: https://gitlab.com/o/n/-/merge_requests/3",
        "AWAITING MERGE:",
    ] {
        assert_eq!(
            round_status(&round, &exit, &format!("{line}\nmore\n")),
            RunStatus::NeedsYou,
            "{line}"
        );
    }
    assert_eq!(
        round_status(&round, &exit, MERGE_REPORT),
        RunStatus::AwaitingMerge
    );
    // A failed exit with the prefix waits too, as a failed exit with NEEDS YOU: asks.
    let failed = ExitInfo {
        code: Some(1),
        ..exit
    };
    assert_eq!(
        round_status(&round, &failed, MERGE_REPORT),
        RunStatus::AwaitingMerge
    );
    assert_eq!(round_status(&round, &failed, "x\n"), RunStatus::Failed);
}

#[tokio::test]
async fn the_wait_survives_a_bridge_restart() {
    let s = Scratch::new();
    let r = awaiting(&s).await;
    let t0 = now_ms();
    drop(r);
    // A new service over the same runs: the record on disk is the whole state.
    let r = rig_with(&s, settings(&s));
    set_pull(&r, open());
    step(&r, t0 + MERGE_CHECK_EVERY_MS).await;
    assert_eq!(pull_asks(&r), 1);
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::AwaitingMerge);
    // The daily push counts from the push before the restart, not from the restart.
    assert!(pushes(&r).is_empty());
    step(&r, t0 + DAY_MS + 1000).await;
    assert_eq!(pushes(&r).len(), 1);
    set_pull(&r, merged());
    step(&r, t0 + DAY_MS + MERGE_CHECK_EVERY_MS + 2000).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Running);
    assert_eq!(rec.rounds.len(), 2);
}

#[tokio::test]
async fn a_free_run_with_no_strand_id_names_only_itself() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    set_pull(&r, Err("offline".into()));
    let id = r.svc.start(free("fix-it")).await.unwrap().run_id;
    finish_round(&r, &id, 0, Some(MERGE_REPORT), "");
    step(&r, now_ms()).await;
    let rec = r.svc.get(&id).unwrap();
    assert_eq!(rec.status, RunStatus::AwaitingMerge);
    let p = pushes(&r);
    assert_eq!(p.len(), 1);
    // The title could not be read: the pull request stands in for it.
    assert_eq!(p[0].1, format!("{id}: jeremyandrews/k3s#412\n{URL}"));
}

#[tokio::test]
async fn a_run_with_neither_prefix_is_as_before() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    r.svc.start(item("GD5")).await.unwrap();
    finish_round(
        &r,
        "GD5",
        0,
        Some("Done. Opened https://github.com/jeremyandrews/k3s/pull/412 for you.\n"),
        "",
    );
    step(&r, now_ms()).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Reported);
    assert_eq!(rec.merge_wait, None);
    let p = pushes(&r);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].0, "Run GD5 reported");
    assert_eq!(pull_asks(&r), 0);
    // The record on disk has no trace of the new field.
    let raw = std::fs::read_to_string(r.svc.settings.run_dir("GD5").join(RUN_JSON)).unwrap();
    assert!(!raw.contains("merge_wait"), "{raw}");
    assert!(!run_json(&rec)
        .as_object()
        .unwrap()
        .contains_key("merge_wait"));
    step(&r, now_ms() + MERGE_CHECK_EVERY_MS).await;
    assert_eq!(pull_asks(&r), 0);
}

#[test]
fn the_url_is_read_from_the_first_line_only() {
    let p = |s: &str| awaiting_merge_pull(s).map(|w| (w.owner, w.repo, w.number, w.url));
    assert_eq!(
        p("AWAITING MERGE: https://github.com/o/n/pull/7\n"),
        Some((
            "o".into(),
            "n".into(),
            7,
            "https://github.com/o/n/pull/7".into()
        ))
    );
    // Trailing punctuation and a path past the number are tolerated.
    assert_eq!(
        p("  AWAITING MERGE: <https://github.com/o/n.rs/pull/12/files>.").map(|x| x.3),
        Some("https://github.com/o/n.rs/pull/12".into())
    );
    assert_eq!(p("NEEDS YOU: https://github.com/o/n/pull/7"), None);
    assert_eq!(
        p("AWAITING MERGE: soon\nhttps://github.com/o/n/pull/7"),
        None
    );
    assert_eq!(
        p("Done.\nAWAITING MERGE: https://github.com/o/n/pull/7"),
        None
    );
}

/// The app's `awaiting_merge` wire fixture, written by the run routes' serializer for a run
/// waiting on jeremyandrews/jesse-app#362. `JESSE_RUN_FIXTURE_WRITE=1` rewrites it, as for the
/// other fixtures, and the app's tests read only what this run always serves.
#[tokio::test]
async fn the_app_awaiting_merge_fixture_matches_what_the_routes_serve() {
    const APP_URL: &str = "https://github.com/jeremyandrews/jesse-app/pull/362";
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    lock(&r.forge.pulls).insert(
        APP_URL.into(),
        Ok(PullMerge {
            state: PrState::Open,
            merged_at: None,
            merge_commit: None,
            title: Some(
                "Bridge 0.217.1: read a quoted devjob-priority followed by a comment".into(),
            ),
        }),
    );
    r.svc.start(item("GD5")).await.unwrap();
    let report = format!("AWAITING MERGE: {APP_URL}\n\nThe change is green; merge it.\n");
    finish_round(&r, "GD5", 0, Some(&report), "");
    step(&r, now_ms()).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::AwaitingMerge);
    let served = r.svc.served_one(&rec);
    assert_eq!(served["status"], "awaiting_merge");
    assert_eq!(served["merge_wait"]["url"], APP_URL);
    assert!(served["merge_wait"]["last_push_at"].is_string());
    pin_app_fixture(&s, "studio-run-awaiting-merge.json", &served);
}

#[tokio::test]
async fn a_hung_check_holds_up_only_its_own_run() {
    const OTHER: &str = "https://github.com/o/n/pull/9";
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let mut st = settings(&s);
    st.max_per_repo = 2;
    let r = rig_with(&s, st);
    set_pull(&r, open());
    lock(&r.forge.pulls).insert(OTHER.into(), merged());
    // GD5's `gh` hangs from its first read.
    lock(&r.forge.hold).insert(URL.into());
    r.svc.start(item("GD5")).await.unwrap();
    let other = r.svc.start(free("other")).await.unwrap().run_id;
    finish_round(&r, "GD5", 0, Some(MERGE_REPORT), "");
    finish_round(
        &r,
        &other,
        0,
        Some(&format!("AWAITING MERGE: {OTHER}\n")),
        "",
    );
    // The pass comes back, and so does the next one, while GD5's check is stuck.
    r.svc.pass(now_ms()).await;
    r.svc.pass(now_ms()).await;
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::AwaitingMerge);
    // The other run's check answers, and its merge resumes it, beside the stuck one.
    let mut resumed = false;
    for _ in 0..400 {
        if r.svc.get(&other).unwrap().status == RunStatus::Running {
            resumed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(resumed, "{:?}", r.svc.get(&other).unwrap().status);
    // Only one check of GD5 is ever in flight, however many passes come.
    r.svc.pass(now_ms() + MERGE_CHECK_EVERY_MS).await;
    let asked_gd5 = lock(&r.forge.pull_asks)
        .iter()
        .filter(|u| *u == URL)
        .count();
    assert_eq!(asked_gd5, 1);
    // Unstuck, it answers like any other check.
    lock(&r.forge.hold).clear();
    r.svc.merge_checks_settled().await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::AwaitingMerge);
    assert!(rec.merge_wait.unwrap().last_push_at.is_some());
}

#[tokio::test]
async fn a_merge_without_its_time_or_commit_is_asked_again_and_resumes_once_complete() {
    let s = Scratch::new();
    let r = awaiting(&s).await;
    let t0 = now_ms();
    for partial in [
        PullMerge {
            state: PrState::Merged,
            merged_at: Some(MERGED_AT.into()),
            merge_commit: None,
            title: Some(TITLE.into()),
        },
        PullMerge {
            state: PrState::Merged,
            merged_at: None,
            merge_commit: Some(SHA.into()),
            title: Some(TITLE.into()),
        },
    ] {
        set_pull(&r, Ok(partial));
        step(&r, t0 + MERGE_CHECK_EVERY_MS).await;
        let rec = r.svc.get("GD5").unwrap();
        assert_eq!(rec.status, RunStatus::AwaitingMerge);
        assert_eq!(rec.rounds.len(), 1);
        assert_eq!(rec.merge_wait.unwrap().ended, None);
        // The next check asks again.
        update_run(&r.svc.settings.run_dir("GD5"), |x| {
            x.merge_wait.as_mut().unwrap().last_check_at = None
        })
        .unwrap();
    }
    // `gh` reads a null `mergeCommit` as no commit at all.
    let p = sys::parse_pull_merge(&json!({
        "state": "MERGED", "mergedAt": MERGED_AT, "mergeCommit": null, "title": TITLE
    }))
    .unwrap();
    assert_eq!(p.merge_commit, None);
    assert_eq!(pushes(&r).len(), 1, "no push for a merge");
    set_pull(&r, merged());
    step(&r, t0 + 2 * MERGE_CHECK_EVERY_MS).await;
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Running);
    assert_eq!(
        rec.round().unwrap().text.as_deref(),
        Some(
            "Jeremy merged https://github.com/jeremyandrews/k3s/pull/412 at 2026-11-24T09:12:31Z \
             as 0123456789abcdef0123456789abcdef01234567. Continue from the next step."
        )
    );
}

#[tokio::test]
async fn entering_the_wait_reads_the_pull_request_once() {
    let s = Scratch::new();
    let r = awaiting(&s).await;
    assert_eq!(pull_asks(&r), 1);
    // The next read is a check interval later, not in the same or the next pass.
    step(&r, now_ms() + 60_000).await;
    assert_eq!(pull_asks(&r), 1);
    step(&r, now_ms() + MERGE_CHECK_EVERY_MS + 1000).await;
    assert_eq!(pull_asks(&r), 2);
}

#[tokio::test]
async fn a_pull_request_already_closed_gets_only_the_closing_push() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    set_pull(&r, closed());
    r.svc.start(item("GD5")).await.unwrap();
    finish_round(&r, "GD5", 0, Some(MERGE_REPORT), "");
    step(&r, now_ms()).await;
    assert_eq!(pull_asks(&r), 1);
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::NeedsYou);
    let p = pushes(&r);
    assert_eq!(p.len(), 1, "{p:?}");
    assert_eq!(p[0].0, "Run GD5 needs you");
}

#[tokio::test]
async fn a_pull_request_already_merged_resumes_with_no_merge_push() {
    let s = Scratch::new();
    vault_fixture(&s);
    remote_fixture(&s);
    let r = rig(&s);
    set_pull(&r, merged());
    r.svc.start(item("GD5")).await.unwrap();
    finish_round(&r, "GD5", 0, Some(MERGE_REPORT), "");
    step(&r, now_ms()).await;
    assert_eq!(pull_asks(&r), 1);
    let rec = r.svc.get("GD5").unwrap();
    assert_eq!(rec.status, RunStatus::Running);
    assert_eq!(rec.rounds.len(), 2);
    assert!(pushes(&r).is_empty(), "{:?}", pushes(&r));
}

#[tokio::test]
async fn filing_the_report_in_the_archive_does_not_drop_the_wait() {
    let s = Scratch::new();
    let r = awaiting(&s).await;
    let rel = format!("Inbox/{}-run-GD5.md", today());
    let archive = s.notes().join("Inbox/archive");
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::rename(
        s.notes().join(&rel),
        archive.join(format!("{}-run-GD5.md", today())),
    )
    .unwrap();
    assert!(reports_archived(&s.notes(), &r.svc.get("GD5").unwrap()));
    r.svc.close_settled(now_ms());
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::AwaitingMerge);
    set_pull(&r, merged());
    step(&r, now_ms() + MERGE_CHECK_EVERY_MS).await;
    assert_eq!(r.svc.get("GD5").unwrap().status, RunStatus::Running);
}

#[test]
fn a_query_or_fragment_after_the_number_still_names_the_pull_request() {
    let round = Round::default();
    let exit = ExitInfo {
        round: 1,
        code: Some(0),
        signal: None,
        ended_at: rfc(1),
    };
    for link in [
        "https://github.com/jeremyandrews/k3s/pull/412#issuecomment-123",
        "https://github.com/jeremyandrews/k3s/pull/412?diff=split",
        "https://github.com/jeremyandrews/k3s/pull/412/files?w=1#diff-abc",
    ] {
        let report = format!("AWAITING MERGE: {link}\n");
        assert_eq!(
            round_status(&round, &exit, &report),
            RunStatus::AwaitingMerge,
            "{link}"
        );
        let w = awaiting_merge_pull(&report).unwrap();
        assert_eq!(w.url, URL, "{link}");
        assert_eq!(w.number, 412);
    }
}
