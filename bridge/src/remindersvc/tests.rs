//! The ticker with an injected clock and a fake push sink, and the two notification routes.

use super::*;
use crate::reminders::{build_reminder, CreateRequest, Recurrence, ReminderState};
use chrono::TimeZone;

fn rome() -> SchedulerZone {
    SchedulerZone::Named(chrono_tz::Europe::Rome)
}

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
}

/// Tuesday 10 November 2026, 09:00 in Rome.
fn created() -> DateTime<Utc> {
    utc(2026, 11, 10, 8, 0, 0)
}

/// A sink that records every payload, or fails every send.
#[derive(Default)]
struct FakeSink {
    sent: Mutex<Vec<Value>>,
    fail: bool,
}

impl ReminderSink for FakeSink {
    fn send(
        &self,
        payload: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            self.sent
                .lock_ok()
                .push(serde_json::from_slice(&payload).unwrap());
            if self.fail {
                Err("apns status 500".into())
            } else {
                Ok(())
            }
        })
    }
}

impl FakeSink {
    fn bodies(&self) -> Vec<String> {
        self.sent
            .lock_ok()
            .iter()
            .map(|p| p["aps"]["alert"]["body"].as_str().unwrap().to_string())
            .collect()
    }
}

fn scratch_store() -> (PathBuf, ReminderStore) {
    let dir = std::env::temp_dir().join(format!("jesse-remindersvc-{}", random_hex()));
    let store = ReminderStore::in_state_dir(&dir);
    (dir, store)
}

fn add(store: &ReminderStore, text: &str, at: &str, recurrence: Option<Value>) -> String {
    let recurrence = recurrence.map(|v| Recurrence::from_json(&v).unwrap());
    store
        .update(|f| {
            let r = build_reminder(
                &CreateRequest {
                    text: text.into(),
                    at: at.into(),
                    recurrence,
                    zone: None,
                },
                f.fresh_id(),
                created(),
                &rome(),
            )
            .unwrap();
            let id = r.id.clone();
            f.reminders.push(r);
            id
        })
        .unwrap()
}

async fn tick_at(store: &ReminderStore, now: DateTime<Utc>, sink: &FakeSink) -> TickOutcome {
    run_tick(store, now, rome(), sink).await.unwrap()
}

#[tokio::test]
async fn fires_once_at_the_minute() {
    let (dir, store) = scratch_store();
    let id = add(&store, "start the pancakes", "2026-11-10T10:00", None);
    let sink = FakeSink::default();
    tick_at(&store, utc(2026, 11, 10, 8, 59, 30), &sink).await;
    assert!(sink.sent.lock_ok().is_empty());
    tick_at(&store, utc(2026, 11, 10, 9, 0, 10), &sink).await;
    tick_at(&store, utc(2026, 11, 10, 9, 0, 40), &sink).await;
    tick_at(&store, utc(2026, 11, 10, 9, 1, 10), &sink).await;
    let sent = sink.sent.lock_ok().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(
        sent[0],
        json!({
            "aps": {
                "alert": {"title": "Reminder", "body": "start the pancakes"},
                "sound": "default",
                "interruption-level": "time-sensitive",
                "category": "JESSE_REMINDER",
            },
            "reminder_id": id,
        })
    );
    let r = store.load().unwrap().reminders[0].clone();
    assert_eq!(r.display_state(), "fired");
    let _ = std::fs::remove_dir_all(dir);
}

/// The decision is written before the push goes out. A process that dies between the two (here:
/// the store is updated by the tick and the sends never happen) restarts with the occurrence
/// already recorded, so the next ticks, from a fresh store handle, send nothing.
#[tokio::test]
async fn a_restart_mid_tick_does_not_double_fire() {
    let (dir, store) = scratch_store();
    add(
        &store,
        "take the bins out",
        "2026-11-10T10:00",
        Some(json!("daily")),
    );
    let at = utc(2026, 11, 10, 9, 0, 5);
    let decided = store
        .update(|f| crate::reminders::tick(f, at, &rome()))
        .unwrap();
    assert_eq!(decided.fires.len(), 1);
    // "Crash": nothing was sent. Restart with a new handle on the same file.
    let restarted = ReminderStore::new(store.path().to_path_buf());
    let sink = FakeSink::default();
    tick_at(&restarted, at, &sink).await;
    tick_at(&restarted, at + chrono::Duration::seconds(30), &sink).await;
    assert!(sink.sent.lock_ok().is_empty());
    let r = restarted.load().unwrap().reminders[0].clone();
    assert_eq!(r.fired_count, 1);
    assert_eq!(r.next_fire, Some(utc(2026, 11, 11, 9, 0, 0)));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn two_hours_down_fires_late_with_the_suffix() {
    let (dir, store) = scratch_store();
    add(&store, "start the pancakes", "2026-11-10T10:00", None);
    let sink = FakeSink::default();
    tick_at(&store, utc(2026, 11, 10, 11, 0, 0), &sink).await;
    assert_eq!(sink.bodies(), vec!["start the pancakes (late: due 10:00)"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn eight_hours_down_marks_missed_and_sends_one_summary() {
    let (dir, store) = scratch_store();
    let a = add(&store, "start the pancakes", "2026-11-10T10:00", None);
    let b = add(&store, "call the plumber", "2026-11-10T10:30", None);
    let c = add(
        &store,
        "water the plants",
        "2026-11-10T10:15",
        Some(json!("daily")),
    );
    let sink = FakeSink::default();
    tick_at(&store, utc(2026, 11, 10, 17, 0, 0), &sink).await;
    let sent = sink.sent.lock_ok().clone();
    assert_eq!(sent.len(), 1, "ONE summary push: {sent:?}");
    assert_eq!(sent[0]["outcome"], "reminders-missed");
    assert_eq!(sent[0]["aps"]["alert"]["title"], "3 missed reminders");
    let body = sent[0]["aps"]["alert"]["body"].as_str().unwrap();
    for part in [
        "start the pancakes (due 10:00)",
        "water the plants (due 10:15)",
        "call the plumber (due 10:30)",
    ] {
        assert!(body.contains(part), "{body}");
    }
    let file = store.load().unwrap();
    assert_eq!(file.get(&a).unwrap().state, ReminderState::Missed);
    assert_eq!(file.get(&b).unwrap().state, ReminderState::Missed);
    // The series is not over: it missed one occurrence and waits for tomorrow's.
    let series = file.get(&c).unwrap();
    assert_eq!(series.state, ReminderState::Active);
    assert_eq!(series.missed_count, 1);
    assert_eq!(series.next_fire, Some(utc(2026, 11, 11, 9, 15, 0)));
    // And nothing more goes out on the next tick.
    tick_at(&store, utc(2026, 11, 10, 17, 0, 30), &sink).await;
    assert_eq!(sink.sent.lock_ok().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_repeating_reminder_advances_after_firing_and_fires_once_per_catch_up() {
    let (dir, store) = scratch_store();
    let id = add(
        &store,
        "take the bins out",
        "07:30",
        Some(json!("weekdays")),
    );
    let sink = FakeSink::default();
    // Wednesday 11 November 07:30 Rome.
    tick_at(&store, utc(2026, 11, 11, 6, 30, 0), &sink).await;
    let r = store.load().unwrap().get(&id).unwrap().clone();
    assert_eq!(r.fired_count, 1);
    assert_eq!(r.next_fire, Some(utc(2026, 11, 12, 6, 30, 0)));
    // Down from Wednesday until Friday 09:00: Thursday's and Friday's occurrences both passed.
    // One push, for Friday's, late; Thursday's is skipped; next is Monday.
    tick_at(&store, utc(2026, 11, 13, 8, 0, 0), &sink).await;
    assert_eq!(
        sink.bodies(),
        vec!["take the bins out", "take the bins out (late: due 07:30)"]
    );
    let r = store.load().unwrap().get(&id).unwrap().clone();
    assert_eq!(r.fired_count, 2);
    assert_eq!(r.missed_count, 1);
    assert_eq!(r.next_fire, Some(utc(2026, 11, 16, 6, 30, 0)));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_push_failure_leaves_the_store_valid() {
    let (dir, store) = scratch_store();
    let id = add(&store, "start the pancakes", "2026-11-10T10:00", None);
    let failing = FakeSink {
        fail: true,
        ..FakeSink::default()
    };
    let out = tick_at(&store, utc(2026, 11, 10, 9, 0, 0), &failing).await;
    assert_eq!(out.fires.len(), 1);
    assert_eq!(failing.sent.lock_ok().len(), 1, "the send was attempted");
    // The file still parses, says the occurrence fired, and is never re-fired.
    let file = store.load().unwrap();
    assert_eq!(file.get(&id).unwrap().display_state(), "fired");
    let ok = FakeSink::default();
    tick_at(&store, utc(2026, 11, 10, 9, 0, 30), &ok).await;
    assert!(ok.sent.lock_ok().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_corrupt_store_sends_nothing() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (dir, store) = scratch_store();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(store.path(), b"[[[").unwrap();
    let sink = FakeSink::default();
    let err = rt
        .block_on(run_tick(&store, created(), rome(), &sink))
        .unwrap_err();
    assert!(err.contains("does not parse"));
    assert!(sink.sent.lock_ok().is_empty());
    assert_eq!(std::fs::read(store.path()).unwrap(), b"[[[");
    let _ = std::fs::remove_dir_all(dir);
}

// ---- Routes ------------------------------------------------------------------------------

fn route_state(dir: &Path) -> AppState {
    AppState::new(Config {
        state_dir: Some(dir.to_string_lossy().into_owned()),
        ..crate::testutil::test_config()
    })
}

fn auth(token: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    h
}

#[tokio::test]
async fn done_and_snooze_routes_act_and_refuse_without_auth() {
    let (dir, store) = scratch_store();
    let st = route_state(&dir);
    let id = add(&store, "start the pancakes", "2026-11-10T10:00", None);

    // Refused without the token, and with a wrong one; nothing changed.
    for headers in [HeaderMap::new(), auth("wrong")] {
        let done =
            jesse_reminder_done(State(st.clone()), UrlPath(id.clone()), headers.clone()).await;
        assert_eq!(done.unwrap_err().0, StatusCode::UNAUTHORIZED);
        let snooze = jesse_reminder_snooze(
            State(st.clone()),
            UrlPath(id.clone()),
            headers.clone(),
            None,
        )
        .await;
        assert_eq!(snooze.unwrap_err().0, StatusCode::UNAUTHORIZED);
        let list = jesse_reminders(State(st.clone()), headers).await;
        assert_eq!(list.unwrap_err().0, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        store.load().unwrap().get(&id).unwrap().state,
        ReminderState::Active
    );

    // Snooze with no body is ten minutes. The store keeps whole seconds, so `before` is
    // truncated to one.
    let before = Utc::now() - chrono::Duration::seconds(1);
    let Json(v) = jesse_reminder_snooze(
        State(st.clone()),
        UrlPath(id.clone()),
        auth("test-token"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(v["reminder"]["state"], "active");
    let r = store.load().unwrap().get(&id).unwrap().clone();
    let until = r.snooze_until.unwrap();
    assert!(until >= before + chrono::Duration::minutes(10));
    assert!(until <= Utc::now() + chrono::Duration::minutes(10));

    // With a body, its minutes.
    let Json(_) = jesse_reminder_snooze(
        State(st.clone()),
        UrlPath(id.clone()),
        auth("test-token"),
        Some(Json(SnoozeBody { minutes: Some(25) })),
    )
    .await
    .unwrap();
    let until = store
        .load()
        .unwrap()
        .get(&id)
        .unwrap()
        .snooze_until
        .unwrap();
    assert!(until >= before + chrono::Duration::minutes(25));

    // Out of range.
    let bad = jesse_reminder_snooze(
        State(st.clone()),
        UrlPath(id.clone()),
        auth("test-token"),
        Some(Json(SnoozeBody { minutes: Some(0) })),
    )
    .await;
    assert_eq!(bad.unwrap_err().0, StatusCode::BAD_REQUEST);

    // Done.
    let Json(v) = jesse_reminder_done(State(st.clone()), UrlPath(id.clone()), auth("test-token"))
        .await
        .unwrap();
    assert_eq!(v["reminder"]["state"], "done");
    let r = store.load().unwrap().get(&id).unwrap().clone();
    assert_eq!(r.state, ReminderState::Done);
    assert!(r.acked);
    assert_eq!(r.snooze_until, None);

    // Unknown ids are 404, for both verbs.
    let missing = jesse_reminder_done(
        State(st.clone()),
        UrlPath("rnope00".into()),
        auth("test-token"),
    )
    .await;
    assert_eq!(missing.unwrap_err().0, StatusCode::NOT_FOUND);
    let missing = jesse_reminder_snooze(
        State(st.clone()),
        UrlPath("rnope00".into()),
        auth("test-token"),
        None,
    )
    .await;
    assert_eq!(missing.unwrap_err().0, StatusCode::NOT_FOUND);

    // The list shows it.
    let Json(v) = jesse_reminders(State(st.clone()), auth("test-token"))
        .await
        .unwrap();
    assert_eq!(v["reminders"][0]["id"], id.as_str());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_routes_are_mounted_behind_auth() {
    use tower::ServiceExt;
    let (dir, _store) = scratch_store();
    let app = crate::app(route_state(&dir));
    for (method, uri) in [
        ("GET", "/jesse/reminders"),
        ("POST", "/jesse/reminders/rabc123/done"),
        ("POST", "/jesse/reminders/rabc123/snooze"),
    ] {
        let req = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
    let _ = std::fs::remove_dir_all(dir);
}
