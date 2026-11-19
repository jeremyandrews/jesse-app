//! `compute_next_fire`, `due`, the tick and the actions, all without a clock: every instant is
//! a literal, every zone is pinned by name.

use super::*;
use chrono::TimeZone;

fn rome() -> SchedulerZone {
    SchedulerZone::Named(chrono_tz::Europe::Rome)
}

fn london() -> SchedulerZone {
    SchedulerZone::Named(chrono_tz::Europe::London)
}

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
}

fn date(y: i32, mo: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, mo, d).unwrap()
}

/// Tuesday 10 November 2026, 09:00 in Rome.
fn now() -> DateTime<Utc> {
    utc(2026, 11, 10, 8, 0)
}

fn rec(v: Value) -> Option<Recurrence> {
    Some(Recurrence::from_json(&v).unwrap())
}

fn make(at: &str, recurrence: Option<Recurrence>, now: DateTime<Utc>) -> Reminder {
    build_reminder(
        &CreateRequest {
            text: "start the pancakes".into(),
            at: at.into(),
            recurrence,
            zone: None,
        },
        "rtest01".into(),
        now,
        &rome(),
    )
    .unwrap()
}

/// Consume the pending occurrence the way a fire does, and return the next one.
fn advance(r: &mut Reminder, zone: &SchedulerZone) -> Option<DateTime<Utc>> {
    let fired = r.next_fire.expect("a pending fire");
    r.last_occurrence = r.next_date;
    r.fired_count += 1;
    let next = next_occurrence(r, fired, zone);
    r.next_date = next.map(|(d, _)| d);
    r.next_fire = next.map(|(_, t)| t);
    r.next_fire
}

/// Every pending fire from the first, `n` of them (fewer if the series ends).
fn series(mut r: Reminder, n: usize) -> Vec<DateTime<Utc>> {
    let mut out = vec![r.next_fire.unwrap()];
    while out.len() < n {
        match advance(&mut r, &rome()) {
            Some(t) => out.push(t),
            None => break,
        }
    }
    out
}

// ---- compute_next_fire ---------------------------------------------------------------

#[test]
fn a_one_off_fires_once_at_its_minute() {
    let r = make("2026-11-11T06:45", None, now());
    // 06:45 in Rome in November is 05:45 UTC.
    assert_eq!(r.next_fire, Some(utc(2026, 11, 11, 5, 45)));
    assert_eq!(
        compute_next_fire(&r, now(), &rome()),
        Some(utc(2026, 11, 11, 5, 45))
    );
    assert_eq!(
        compute_next_fire(&r, utc(2026, 11, 11, 5, 45), &rome()),
        None
    );
    let mut consumed = r.clone();
    consumed.last_occurrence = Some(date(2026, 11, 11));
    assert_eq!(compute_next_fire(&consumed, now(), &rome()), None);
}

#[test]
fn a_bare_time_is_the_next_such_minute() {
    // 09:00 in Rome: 06:45 has passed today, so tomorrow; 21:15 has not.
    assert_eq!(
        make("06:45", None, now()).next_fire,
        Some(utc(2026, 11, 11, 5, 45))
    );
    assert_eq!(
        make("21:15", None, now()).next_fire,
        Some(utc(2026, 11, 10, 20, 15))
    );
}

#[test]
fn daily_fires_every_day() {
    let r = make("07:30", rec(json!({"kind": "daily"})), now());
    assert_eq!(
        series(r, 3),
        vec![
            utc(2026, 11, 11, 6, 30),
            utc(2026, 11, 12, 6, 30),
            utc(2026, 11, 13, 6, 30)
        ]
    );
}

#[test]
fn weekdays_skip_the_weekend() {
    // From Friday 13 November: Friday, then Monday 16 and Tuesday 17.
    let r = make("2026-11-13T07:30", rec(json!("weekdays")), now());
    assert_eq!(
        series(r, 3),
        vec![
            utc(2026, 11, 13, 6, 30),
            utc(2026, 11, 16, 6, 30),
            utc(2026, 11, 17, 6, 30)
        ]
    );
    // Created on a Saturday, the first fire is Monday.
    let sat = utc(2026, 11, 14, 12, 0);
    let r = make("07:30", rec(json!("weekdays")), sat);
    assert_eq!(r.next_fire, Some(utc(2026, 11, 16, 6, 30)));
}

#[test]
fn weekly_on_monday_and_thursday() {
    // From Tuesday 10 November: Thursday 12, Monday 16, Thursday 19, Monday 23.
    let r = make(
        "08:00",
        rec(json!({"kind": "weekly", "days": ["thu", "mon"]})),
        now(),
    );
    assert_eq!(
        series(r, 4),
        vec![
            utc(2026, 11, 12, 7, 0),
            utc(2026, 11, 16, 7, 0),
            utc(2026, 11, 19, 7, 0),
            utc(2026, 11, 23, 7, 0)
        ]
    );
}

#[test]
fn monthly_on_the_31st_uses_the_last_day_of_a_short_month() {
    let r = make(
        "2027-01-31T09:00",
        rec(json!({"kind": "monthly", "day": 31})),
        now(),
    );
    let got: Vec<NaiveDate> = series(r, 4)
        .into_iter()
        .map(|t| t.with_timezone(&chrono_tz::Europe::Rome).date_naive())
        .collect();
    assert_eq!(
        got,
        vec![
            date(2027, 1, 31),
            date(2027, 2, 28),
            date(2027, 3, 31),
            date(2027, 4, 30)
        ]
    );
    // And a leap February is the 29th.
    let r = make(
        "2028-01-31T09:00",
        rec(json!({"kind": "monthly", "day": 31})),
        now(),
    );
    let feb = series(r, 2)[1]
        .with_timezone(&chrono_tz::Europe::Rome)
        .date_naive();
    assert_eq!(feb, date(2028, 2, 29));
}

#[test]
fn every_n_days_counts_from_the_first_fire() {
    let r = make(
        "2026-11-11T07:00",
        rec(json!({"kind": "every_n_days", "n": 3})),
        now(),
    );
    assert_eq!(
        series(r, 3),
        vec![
            utc(2026, 11, 11, 6, 0),
            utc(2026, 11, 14, 6, 0),
            utc(2026, 11, 17, 6, 0)
        ]
    );
}

#[test]
fn until_and_count_end_a_series() {
    let until = make(
        "2026-11-11T07:00",
        rec(json!({"kind": "daily", "until": "2026-11-12"})),
        now(),
    );
    assert_eq!(
        series(until, 10),
        vec![utc(2026, 11, 11, 6, 0), utc(2026, 11, 12, 6, 0)]
    );

    let count = make(
        "2026-11-11T07:00",
        rec(json!({"kind": "daily", "count": 3})),
        now(),
    );
    assert_eq!(
        series(count, 10),
        vec![
            utc(2026, 11, 11, 6, 0),
            utc(2026, 11, 12, 6, 0),
            utc(2026, 11, 13, 6, 0)
        ]
    );
}

/// Europe/Rome springs forward on the last Sunday of March: 28 March 2027, 02:00 → 03:00. A
/// daily 02:30 fires at 03:00 that day, the first valid minute after it, and at 02:30 again on
/// either side.
#[test]
fn the_spring_forward_gap_fires_at_the_first_valid_minute() {
    let r = make("2027-03-27T02:30", rec(json!("daily")), now());
    assert_eq!(
        series(r, 3),
        vec![
            utc(2027, 3, 27, 1, 30), // 02:30 CET
            utc(2027, 3, 28, 1, 0),  // 03:00 CEST: 02:30 does not exist
            utc(2027, 3, 29, 0, 30), // 02:30 CEST
        ]
    );
}

/// Europe/Rome falls back on the last Sunday of October: 31 October 2027, 03:00 → 02:00. 02:30
/// happens twice; the reminder fires once, at the first (CEST) one.
#[test]
fn the_fall_back_overlap_fires_once_at_the_first_occurrence() {
    let r = make("2027-10-30T02:30", rec(json!("daily")), now());
    assert_eq!(
        series(r.clone(), 3),
        vec![
            utc(2027, 10, 30, 0, 30), // 02:30 CEST
            utc(2027, 10, 31, 0, 30), // the FIRST 02:30, still CEST
            utc(2027, 11, 1, 1, 30),  // 02:30 CET
        ]
    );
    // Even with no occurrence recorded, nothing after the first 02:30 lands on the second.
    let mut open = r;
    open.last_occurrence = None;
    assert_eq!(
        compute_next_fire(&open, utc(2027, 10, 31, 0, 30), &rome()),
        Some(utc(2027, 11, 1, 1, 30))
    );
    // A one-off on the overlap is the first instant too.
    let once = make("2027-10-31T02:30", None, now());
    assert_eq!(once.next_fire, Some(utc(2027, 10, 31, 0, 30)));
}

#[test]
fn a_profile_zone_change_re_resolves_the_same_occurrence() {
    let mut file = ReminderFile::default();
    let daily = make("07:30", rec(json!("daily")), now());
    let mut fixed = make("07:30", rec(json!("daily")), now());
    fixed.id = "rfixed1".into();
    fixed.zone_mode = ZoneMode::Fixed("Europe/Rome".into());
    file.reminders = vec![daily, fixed];
    assert_eq!(file.reminders[0].next_fire, Some(utc(2026, 11, 11, 6, 30)));

    assert_eq!(reresolve(&mut file, &london()), 1);
    let moved = &file.reminders[0];
    // 07:30 in London on the same date is 07:30 UTC.
    assert_eq!(moved.next_fire, Some(utc(2026, 11, 11, 7, 30)));
    assert_eq!(moved.next_date, Some(date(2026, 11, 11)));
    assert_eq!(moved.resolved_zone, "Europe/London");
    // The fixed-zone one is untouched, and a second pass moves nothing.
    assert_eq!(file.reminders[1].next_fire, Some(utc(2026, 11, 11, 6, 30)));
    assert_eq!(reresolve(&mut file, &london()), 0);
    // Coming home moves it back.
    assert_eq!(reresolve(&mut file, &rome()), 1);
    assert_eq!(file.reminders[0].next_fire, Some(utc(2026, 11, 11, 6, 30)));
    // And the next fire after London's is computed in London.
    let mut r = file.reminders[0].clone();
    r.last_occurrence = r.next_date;
    assert_eq!(
        compute_next_fire(&r, utc(2026, 11, 11, 7, 30), &london()),
        Some(utc(2026, 11, 12, 7, 30))
    );
}

// ---- due -----------------------------------------------------------------------------

#[test]
fn due_lists_active_reminders_at_or_past_their_time() {
    let a = make("2026-11-10T10:00", None, now());
    let mut b = make("2026-11-10T09:30", None, now());
    b.id = "rtest02".into();
    let mut c = make("2026-11-10T09:15", None, now());
    c.id = "rtest03".into();
    c.state = ReminderState::Cancelled;
    let list = vec![a, b, c];
    assert!(due(&list, utc(2026, 11, 10, 8, 29)).is_empty());
    assert_eq!(due(&list, utc(2026, 11, 10, 8, 30)), vec!["rtest02"]);
    assert_eq!(
        due(&list, utc(2026, 11, 10, 9, 0)),
        vec!["rtest02", "rtest01"]
    );
}

// ---- validation ------------------------------------------------------------------------

fn create_err(text: &str, at: &str, recurrence: Option<Value>, zone: Option<&str>) -> String {
    let recurrence = match recurrence {
        Some(v) => match Recurrence::from_json(&v) {
            Ok(r) => Some(r),
            Err(e) => return e,
        },
        None => None,
    };
    build_reminder(
        &CreateRequest {
            text: text.into(),
            at: at.into(),
            recurrence,
            zone: zone.map(str::to_string),
        },
        "rx".into(),
        now(),
        &rome(),
    )
    .unwrap_err()
}

#[test]
fn invalid_input_is_refused_with_a_reason() {
    assert!(create_err("  ", "07:00", None, None).contains("`text` is empty"));
    assert!(create_err("x", "2026-11-09T07:00", None, None).contains("in the past"));
    assert!(create_err("x", "2026-11-10T08:59", None, None).contains("in the past"));
    assert!(create_err("x", "tomorrow-ish", None, None).contains("`at` must be"));
    assert!(create_err("x", "07:00", None, Some("Mars/Olympus")).contains("IANA"));
    assert!(create_err(
        "x",
        "2026-11-20T07:00",
        Some(json!({"kind": "daily", "until": "2026-11-15"})),
        None
    )
    .contains("falls before the first fire"));
    assert!(create_err(
        "x",
        "07:00",
        Some(json!({"kind": "weekly", "days": []})),
        None
    )
    .contains("at least one day"));
    assert!(create_err(
        "x",
        "07:00",
        Some(json!({"kind": "every_n_days", "n": 0})),
        None
    )
    .contains("`n` must be"));
    assert!(
        create_err("x", "07:00", Some(json!({"kind": "hourly"})), None)
            .contains("unknown recurrence")
    );
}

#[test]
fn a_fixed_zone_is_read_in_that_zone() {
    let r = build_reminder(
        &CreateRequest {
            text: "call mum".into(),
            at: "2026-11-11T09:00".into(),
            recurrence: None,
            zone: Some("Europe/London".into()),
        },
        "rz".into(),
        now(),
        &rome(),
    )
    .unwrap();
    assert_eq!(r.zone_mode, ZoneMode::Fixed("Europe/London".into()));
    assert_eq!(r.next_fire, Some(utc(2026, 11, 11, 9, 0)));
}

// ---- the tick ------------------------------------------------------------------------

fn file_with(r: Reminder) -> ReminderFile {
    ReminderFile {
        reminders: vec![r],
        ..ReminderFile::default()
    }
}

#[test]
fn the_tick_fires_a_one_off_once_and_marks_it_fired() {
    let mut f = file_with(make("2026-11-10T10:00", None, now()));
    assert_eq!(
        tick(&mut f, utc(2026, 11, 10, 8, 59), &rome()),
        TickOutcome::default()
    );
    let out = tick(&mut f, utc(2026, 11, 10, 9, 0), &rome());
    assert_eq!(
        out.fires,
        vec![ReminderFire {
            id: "rtest01".into(),
            body: "start the pancakes".into()
        }]
    );
    let r = &f.reminders[0];
    assert_eq!(r.state, ReminderState::Done);
    assert_eq!(r.display_state(), "fired");
    assert_eq!(r.fired_count, 1);
    assert!(tick(&mut f, utc(2026, 11, 10, 9, 0), &rome())
        .fires
        .is_empty());
}

#[test]
fn the_tick_prunes_reminders_closed_over_30_days_ago() {
    let mut f = file_with(make("2026-11-10T10:00", None, now()));
    cancel(&mut f, "rtest01", now()).unwrap();
    assert_eq!(
        tick(&mut f, now() + ChronoDuration::days(29), &rome()).pruned,
        0
    );
    assert_eq!(
        tick(&mut f, now() + ChronoDuration::days(31), &rome()).pruned,
        1
    );
    assert!(f.reminders.is_empty());
}

// ---- actions -------------------------------------------------------------------------

#[test]
fn done_acknowledges_and_snooze_fires_once_more() {
    let mut f = file_with(make("2026-11-10T10:00", None, now()));
    tick(&mut f, utc(2026, 11, 10, 9, 0), &rome());
    let r = mark_done(&mut f, "rtest01", utc(2026, 11, 10, 9, 1)).unwrap();
    assert_eq!(r.display_state(), "done");

    let snoozed = snooze(&mut f, "rtest01", 10, utc(2026, 11, 10, 9, 2)).unwrap();
    assert_eq!(snoozed.state, ReminderState::Active);
    assert_eq!(snoozed.fire_at(), Some(utc(2026, 11, 10, 9, 12)));
    assert!(tick(&mut f, utc(2026, 11, 10, 9, 11), &rome())
        .fires
        .is_empty());
    assert_eq!(
        tick(&mut f, utc(2026, 11, 10, 9, 12), &rome()).fires.len(),
        1
    );
    assert_eq!(f.reminders[0].state, ReminderState::Done);

    assert!(matches!(
        snooze(&mut f, "nope", 10, now()),
        Err(ActionError::NotFound(_))
    ));
    assert!(matches!(
        snooze(&mut f, "rtest01", 0, now()),
        Err(ActionError::Invalid(_))
    ));
}

#[test]
fn a_snoozed_series_keeps_its_own_schedule() {
    let mut f = file_with(make("07:30", rec(json!("daily")), now()));
    let first = utc(2026, 11, 11, 6, 30);
    tick(&mut f, first, &rome());
    snooze(&mut f, "rtest01", 10, first).unwrap();
    let r = &f.reminders[0];
    assert_eq!(r.next_fire, Some(utc(2026, 11, 12, 6, 30)));
    assert_eq!(r.fire_at(), Some(first + ChronoDuration::minutes(10)));
    assert_eq!(
        tick(&mut f, first + ChronoDuration::minutes(10), &rome())
            .fires
            .len(),
        1
    );
    assert_eq!(f.reminders[0].next_fire, Some(utc(2026, 11, 12, 6, 30)));
    assert_eq!(f.reminders[0].snooze_until, None);
}

// ---- the store -----------------------------------------------------------------------

fn scratch() -> PathBuf {
    std::env::temp_dir().join(format!("jesse-reminders-{}", crate::random_hex()))
}

#[test]
fn the_store_round_trips_and_never_overwrites_a_corrupt_file() {
    let dir = scratch();
    let store = ReminderStore::in_state_dir(&dir);
    assert_eq!(store.load().unwrap(), ReminderFile::default());
    let r = make(
        "07:30",
        rec(json!({"kind": "weekly", "days": ["mon", "thu"], "count": 4})),
        now(),
    );
    store.update(|f| f.reminders.push(r.clone())).unwrap();
    assert_eq!(store.load().unwrap().reminders, vec![r]);
    let mode = std::fs::metadata(store.path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);

    std::fs::write(store.path(), b"{ not json").unwrap();
    assert!(store.load().unwrap_err().contains("does not parse"));
    assert!(store.update(|f| f.reminders.clear()).is_err());
    assert_eq!(std::fs::read(store.path()).unwrap(), b"{ not json");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn concurrent_writers_lose_nothing() {
    let dir = scratch();
    let store = ReminderStore::in_state_dir(&dir);
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let store = store.clone();
            std::thread::spawn(move || {
                for j in 0..10 {
                    store
                        .update(|f| {
                            let mut r = make("07:30", None, now());
                            r.id = format!("r{i}-{j}");
                            f.reminders.push(r);
                        })
                        .unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(store.load().unwrap().reminders.len(), 80);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_tools_are_exactly_four() {
    let names: Vec<&str> = ReminderTool::ALL.iter().map(|t| t.tool_name()).collect();
    assert_eq!(
        names,
        [
            "create_reminder",
            "list_reminders",
            "cancel_reminder",
            "snooze_reminder"
        ]
    );
    for t in ReminderTool::ALL {
        assert_eq!(ReminderTool::parse(t.tool_name()), Some(t));
    }
}

use std::os::unix::fs::PermissionsExt;
