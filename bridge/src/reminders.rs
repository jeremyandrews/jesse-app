//! **TIMED REMINDERS**: "remind me at 6:45 to start the pancakes", "every weekday at 7:30 remind
//! me to take the bins out". The model, the store and every rule about when one fires.
//!
//! ---- WHAT A REMINDER IS, AND IS NOT --------------------------------------------
//!
//! A reminder is a NOTIFICATION, never an agent turn. At its minute the bridge's reminder ticker
//! ([`crate::remindersvc`]) sends one APNs push with `Done` and `Snooze 10 min` actions, and that
//! is all: no model process is spawned, no prompt is built, nothing reaches the vault. The
//! `[[schedule]]` table is the wrong tool for this on both counts: it lives in the operator's
//! local TOML outside every turn's reach, and every entry in it fires an agent turn.
//!
//! ---- ONE STORE, TWO WRITERS ----------------------------------------------------
//!
//! The store is ONE JSON file, `<state_dir>/reminders.json`, and two processes write it: the
//! `jesse-reminders-mcp` child a turn talks to (create, cancel, snooze) and the bridge (the ticker
//! and the notification-action routes). Every read-modify-write therefore runs under an exclusive
//! advisory `flock` on a sibling `reminders.json.lock` ([`ReminderStore::update`]), and every write
//! is [`crate::write_atomic`]'s unique-temp-file-then-rename, so a reader never sees a torn file
//! and no writer's change is lost to another's. A file that does not parse is NEVER overwritten:
//! the update fails, says so, and leaves the bytes for a human.
//!
//! ---- WHEN A REMINDER FIRES ------------------------------------------------------
//!
//! Every reminder carries its wall-clock `local_time` (`HH:MM`) and a zone mode: `profile` (the
//! default — the active away profile's zone, else the host's) or one fixed IANA zone. Its pending
//! occurrence is named by its LOCAL DATE (`next_date`), and `next_fire` is that date at that time
//! resolved in the zone, as an absolute UTC instant. Naming the occurrence by its date is what
//! makes a profile change safe: the same occurrence is re-resolved in the new zone, never a
//! second one invented ([`reresolve`]).
//!
//! DST, the same two rules the job scheduler keeps ([`resolve_local`]): a local time that does
//! not exist on the spring-forward day fires at the first valid minute after it; a local time that
//! happens twice on the fall-back day fires ONCE, at the first occurrence.
//!
//! ---- NEVER TWICE ---------------------------------------------------------------
//!
//! The tick ([`tick`]) records each fired occurrence in the store BEFORE the push is sent, so a
//! restart between the two loses at most one push and can never send one twice. After a long
//! downtime a reminder is fired at most [`LATE_LIMIT_HOURS`] late (with `(late: due HH:MM)` on its
//! body); older than that it is marked missed and one summary push lists every missed one. A
//! repeating reminder advances to its first occurrence after NOW, so it never fires more than
//! once per catch-up.

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, LocalResult, NaiveDate, NaiveDateTime,
    NaiveTime, TimeZone, Utc, Weekday,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{parse_iana, write_atomic, ProfileStore, SchedulerZone};

/// The store's file name under the bridge's state directory.
pub const REMINDERS_FILE: &str = "reminders.json";

/// How late a reminder may still fire after downtime. Older than this it is MISSED.
pub const LATE_LIMIT_HOURS: i64 = 6;

/// How long a finished (done, cancelled or missed) reminder is kept before it is pruned.
pub const KEEP_CLOSED_DAYS: i64 = 30;

/// A fire this many seconds or more after its time says so on its body. The ticker runs every
/// 30 s, so an on-time fire is always well inside it.
pub const LATE_NOTE_SECS: i64 = 90;

/// The longest reminder text accepted. It lands on a lock screen, so it is a line, not a page.
pub const MAX_TEXT_CHARS: usize = 300;

/// The snooze bounds, in minutes.
pub const MAX_SNOOZE_MINUTES: i64 = 24 * 60;
pub const DEFAULT_SNOOZE_MINUTES: i64 = 10;

/// The schema version written into the file.
pub const STORE_VERSION: u32 = 1;

/// How many days ahead an occurrence is searched for. A monthly rule finds one within 31 days
/// and `every_n_days` is capped at [`MAX_EVERY_N_DAYS`], so this bound is never the answer for a
/// rule that has one.
const DAY_SEARCH_LIMIT: u32 = 800;

/// The largest `n` an `every_n_days` rule takes.
pub const MAX_EVERY_N_DAYS: u32 = 366;

/// How far a nonexistent local time is probed forward for the first valid minute.
const GAP_PROBE_MINUTES: i64 = 180;

// ---- The model ---------------------------------------------------------------------------

/// A reminder's lifecycle.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ReminderState {
    /// Waiting for its next fire (or for a snooze).
    Active,
    /// Finished: a one-off that fired (listed as `fired` until it is acknowledged), a series
    /// whose last occurrence fired, or one marked done from the notification.
    Done,
    /// Cancelled by the owner.
    Cancelled,
    /// Its occurrence passed more than [`LATE_LIMIT_HOURS`] ago while the bridge was down.
    Missed,
}

impl ReminderState {
    pub fn label(self) -> &'static str {
        match self {
            ReminderState::Active => "active",
            ReminderState::Done => "done",
            ReminderState::Cancelled => "cancelled",
            ReminderState::Missed => "missed",
        }
    }
}

/// Which zone a reminder's wall-clock time is read in.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ZoneMode {
    /// The active profile's zone when it fires: an away profile's, else the host's.
    Profile,
    /// One fixed IANA zone, whatever the profile says.
    Fixed(String),
}

impl ZoneMode {
    pub fn label(&self) -> &str {
        match self {
            ZoneMode::Profile => "profile",
            ZoneMode::Fixed(tz) => tz,
        }
    }
}

impl Serialize for ZoneMode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.label())
    }
}

impl<'de> Deserialize<'de> for ZoneMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(if raw.trim().is_empty() || raw == "profile" {
            ZoneMode::Profile
        } else {
            ZoneMode::Fixed(raw)
        })
    }
}

/// How a reminder repeats.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RecurrenceRule {
    Daily,
    /// Monday to Friday.
    Weekdays,
    /// On each listed weekday.
    Weekly(Vec<Weekday>),
    /// On this day of the month; a month without it uses its last day (the 31st → 30 April,
    /// 28 or 29 February).
    Monthly(u32),
    /// Every `n` days from the first occurrence.
    EveryNDays(u32),
}

/// A recurrence and its optional end.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Recurrence {
    pub rule: RecurrenceRule,
    /// The last LOCAL date an occurrence may fall on, inclusive.
    pub until: Option<NaiveDate>,
    /// How many occurrences in all (fired or missed).
    pub count: Option<u32>,
}

impl Recurrence {
    pub fn new(rule: RecurrenceRule) -> Recurrence {
        Recurrence {
            rule,
            until: None,
            count: None,
        }
    }

    /// The wire and disk form: `{"kind": "weekly", "days": ["mon","thu"], "until": "…"}`.
    pub fn to_json(&self) -> Value {
        let mut v = match &self.rule {
            RecurrenceRule::Daily => json!({"kind": "daily"}),
            RecurrenceRule::Weekdays => json!({"kind": "weekdays"}),
            RecurrenceRule::Weekly(days) => json!({
                "kind": "weekly",
                "days": days.iter().map(|d| weekday_label(*d)).collect::<Vec<_>>(),
            }),
            RecurrenceRule::Monthly(day) => json!({"kind": "monthly", "day": day}),
            RecurrenceRule::EveryNDays(n) => json!({"kind": "every_n_days", "n": n}),
        };
        if let Some(u) = self.until {
            v["until"] = json!(u.format("%Y-%m-%d").to_string());
        }
        if let Some(c) = self.count {
            v["count"] = json!(c);
        }
        v
    }

    /// Parse the wire form, or a bare kind (`"daily"`, `"weekdays"`). Every refusal names what
    /// was wrong, because the caller is a model that will read it and try again.
    pub fn from_json(v: &Value) -> Result<Recurrence, String> {
        let (kind, obj) = match v {
            Value::String(s) => (s.trim().to_ascii_lowercase(), None),
            Value::Object(o) => (
                o.get("kind")
                    .and_then(Value::as_str)
                    .ok_or("`recurrence.kind` is required: daily, weekdays, weekly, monthly or every_n_days")?
                    .trim()
                    .to_ascii_lowercase(),
                Some(o),
            ),
            _ => return Err("`recurrence` must be an object like {\"kind\": \"daily\"}".into()),
        };
        let field = |k: &str| obj.and_then(|o| o.get(k)).filter(|v| !v.is_null());
        let rule = match kind.as_str() {
            "daily" => RecurrenceRule::Daily,
            "weekdays" => RecurrenceRule::Weekdays,
            "weekly" => {
                let days = field("days")
                    .and_then(Value::as_array)
                    .ok_or("a weekly recurrence needs `days`, e.g. [\"mon\", \"thu\"]")?;
                let mut out = Vec::new();
                for d in days {
                    let s = d.as_str().unwrap_or_default();
                    let wd = parse_weekday(s)
                        .ok_or_else(|| format!("`{s}` is not a weekday (mon … sun)"))?;
                    if !out.contains(&wd) {
                        out.push(wd);
                    }
                }
                if out.is_empty() {
                    return Err("a weekly recurrence needs at least one day in `days`".into());
                }
                out.sort_by_key(|d| d.num_days_from_monday());
                RecurrenceRule::Weekly(out)
            }
            "monthly" => {
                let day = field("day")
                    .and_then(Value::as_u64)
                    .ok_or("a monthly recurrence needs `day`, the day of the month (1-31)")?;
                if !(1..=31).contains(&day) {
                    return Err(format!("`day` must be 1-31, got {day}"));
                }
                RecurrenceRule::Monthly(day as u32)
            }
            "every_n_days" => {
                let n = field("n")
                    .and_then(Value::as_u64)
                    .ok_or("an every_n_days recurrence needs `n`, a whole number of days")?;
                if n == 0 || n > u64::from(MAX_EVERY_N_DAYS) {
                    return Err(format!("`n` must be 1-{MAX_EVERY_N_DAYS}, got {n}"));
                }
                RecurrenceRule::EveryNDays(n as u32)
            }
            other => {
                return Err(format!(
                    "unknown recurrence kind `{other}`: use daily, weekdays, weekly, monthly or \
                     every_n_days"
                ))
            }
        };
        let until = match field("until") {
            None => None,
            Some(u) => {
                let s = u.as_str().unwrap_or_default();
                Some(
                    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
                        .map_err(|_| format!("`until` must be a date YYYY-MM-DD, got {u}"))?,
                )
            }
        };
        let count = match field("count") {
            None => None,
            Some(c) => match c.as_u64() {
                Some(n) if n >= 1 && n <= u64::from(u32::MAX) => Some(n as u32),
                _ => {
                    return Err(format!(
                        "`count` must be a whole number of at least 1, got {c}"
                    ))
                }
            },
        };
        Ok(Recurrence { rule, until, count })
    }

    /// Whether `d` is an occurrence date for a series first falling on `start`.
    pub fn matches(&self, start: NaiveDate, d: NaiveDate) -> bool {
        match &self.rule {
            RecurrenceRule::Daily => true,
            RecurrenceRule::Weekdays => d.weekday().num_days_from_monday() < 5,
            RecurrenceRule::Weekly(days) => days.contains(&d.weekday()),
            RecurrenceRule::Monthly(day) => d.day() == (*day).min(days_in_month(d)),
            RecurrenceRule::EveryNDays(n) => {
                d >= start && (d - start).num_days() % i64::from(*n) == 0
            }
        }
    }
}

impl Serialize for Recurrence {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Recurrence {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        Recurrence::from_json(&v).map_err(serde::de::Error::custom)
    }
}

/// One reminder, as stored.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Reminder {
    /// Short and stable: `r` and six hex digits.
    pub id: String,
    pub text: String,
    #[serde(with = "utc")]
    pub created_at: DateTime<Utc>,
    /// The pending occurrence as an absolute instant. `None` once there is none.
    #[serde(with = "utc_opt", default)]
    pub next_fire: Option<DateTime<Utc>>,
    /// The pending occurrence's LOCAL date: what [`reresolve`] re-resolves on a zone change.
    #[serde(with = "date_opt", default)]
    pub next_date: Option<NaiveDate>,
    /// `HH:MM`, read in the reminder's zone.
    pub local_time: String,
    pub zone_mode: ZoneMode,
    /// The zone `next_fire` was last resolved in, so a profile change is noticed.
    #[serde(default)]
    pub resolved_zone: String,
    /// The first occurrence's local date: a one-off's date, and a series' anchor.
    #[serde(with = "date")]
    pub start_date: NaiveDate,
    #[serde(default)]
    pub recurrence: Option<Recurrence>,
    pub state: ReminderState,
    #[serde(default)]
    pub fired_count: u32,
    #[serde(with = "utc_opt", default)]
    pub last_fired_at: Option<DateTime<Utc>>,
    /// Occurrences that passed unfired (more than [`LATE_LIMIT_HOURS`] late).
    #[serde(default)]
    pub missed_count: u32,
    /// The local date of the last occurrence consumed (fired or missed). An occurrence on or
    /// before it is never fired again.
    #[serde(with = "date_opt", default)]
    pub last_occurrence: Option<NaiveDate>,
    /// A one-time extra fire from `Snooze`, beside (never replacing) a series' own.
    #[serde(with = "utc_opt", default)]
    pub snooze_until: Option<DateTime<Utc>>,
    /// Whether the latest fire was acknowledged with `Done`. A fired one-off is `done` and
    /// unacknowledged — "done-pending-ack" — and is listed as `fired` until then.
    #[serde(default)]
    pub acked: bool,
    /// When it stopped being active: the clock [`KEEP_CLOSED_DAYS`] runs from.
    #[serde(with = "utc_opt", default)]
    pub closed_at: Option<DateTime<Utc>>,
}

impl Reminder {
    /// The instant it is next due: its pending occurrence or its snooze, whichever is first.
    pub fn fire_at(&self) -> Option<DateTime<Utc>> {
        match (self.next_fire, self.snooze_until) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The state as a list shows it: a fired, unacknowledged one-off reads `fired`.
    pub fn display_state(&self) -> &'static str {
        if self.state == ReminderState::Done && !self.acked && self.fired_count > 0 {
            "fired"
        } else {
            self.state.label()
        }
    }

    fn time(&self) -> Option<NaiveTime> {
        parse_hhmm(&self.local_time)
    }

    fn close(&mut self, state: ReminderState, now: DateTime<Utc>) {
        self.state = state;
        self.next_fire = None;
        self.next_date = None;
        self.snooze_until = None;
        self.closed_at = Some(now);
    }
}

/// The whole file.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct ReminderFile {
    pub version: u32,
    #[serde(default)]
    pub reminders: Vec<Reminder>,
}

impl Default for ReminderFile {
    fn default() -> Self {
        ReminderFile {
            version: STORE_VERSION,
            reminders: Vec::new(),
        }
    }
}

impl ReminderFile {
    pub fn get(&self, id: &str) -> Option<&Reminder> {
        self.reminders.iter().find(|r| r.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Reminder> {
        self.reminders.iter_mut().find(|r| r.id == id)
    }

    /// A fresh id no stored reminder has.
    pub fn fresh_id(&self) -> String {
        loop {
            let id = format!("r{}", &crate::random_hex()[..6]);
            if self.get(&id).is_none() {
                return id;
            }
        }
    }
}

// ---- Time --------------------------------------------------------------------------------

/// Resolve a LOCAL date and time in `zone` to an instant, handling both DST edges exactly as the
/// job scheduler does (`schedule::resolve_local`): an ambiguous time (fall back) is the EARLIER
/// instant, and a nonexistent one (spring forward) is the first valid minute after it.
pub fn resolve_local<Tz: TimeZone>(zone: &Tz, date: NaiveDate, at: NaiveTime) -> DateTime<Utc> {
    let base = date.and_time(at);
    for m in 0..=GAP_PROBE_MINUTES {
        match zone.from_local_datetime(&(base + ChronoDuration::minutes(m))) {
            LocalResult::Single(dt) => return dt.with_timezone(&Utc),
            LocalResult::Ambiguous(earliest, _) => return earliest.with_timezone(&Utc),
            LocalResult::None => continue,
        }
    }
    // No real zone has a gap this long; read the wall time as UTC rather than lose the fire.
    Utc.from_utc_datetime(&base)
}

/// The name a zone is recorded under: its IANA name, else its offset.
pub fn zone_label(zone: &SchedulerZone) -> String {
    zone.iana_name().unwrap_or_else(|| {
        let off = zone.offset_from_utc_datetime(&Utc::now().naive_utc());
        format!("UTC{off}")
    })
}

/// The zone a reminder is read in, given the profile's.
pub fn zone_for(mode: &ZoneMode, profile_zone: &SchedulerZone) -> SchedulerZone {
    match mode {
        ZoneMode::Profile => *profile_zone,
        ZoneMode::Fixed(tz) => parse_iana(tz).unwrap_or(*profile_zone),
    }
}

/// `HH:MM`, 24-hour, one or two hour digits.
pub fn parse_hhmm(raw: &str) -> Option<NaiveTime> {
    let (h, m) = raw.trim().split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    NaiveTime::from_hms_opt(h, m, 0)
}

fn days_in_month(d: NaiveDate) -> u32 {
    let (y, m) = (d.year(), d.month());
    let first_next = if m == 12 {
        NaiveDate::from_ymd_opt(y + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1)
    };
    first_next
        .and_then(|f| f.pred_opt())
        .map(|l| l.day())
        .unwrap_or(28)
}

pub fn weekday_label(d: Weekday) -> &'static str {
    match d {
        Weekday::Mon => "mon",
        Weekday::Tue => "tue",
        Weekday::Wed => "wed",
        Weekday::Thu => "thu",
        Weekday::Fri => "fri",
        Weekday::Sat => "sat",
        Weekday::Sun => "sun",
    }
}

pub fn parse_weekday(raw: &str) -> Option<Weekday> {
    let s = raw.trim().to_ascii_lowercase();
    let s = s.get(..3)?;
    Some(match s {
        "mon" => Weekday::Mon,
        "tue" => Weekday::Tue,
        "wed" => Weekday::Wed,
        "thu" => Weekday::Thu,
        "fri" => Weekday::Fri,
        "sat" => Weekday::Sat,
        "sun" => Weekday::Sun,
        _ => return None,
    })
}

// ---- The pure rules ----------------------------------------------------------------------

/// The reminder's next occurrence STRICTLY AFTER `after`, read in `zone`: its local date and
/// its instant. Never an occurrence on or before [`Reminder::last_occurrence`], never past the
/// recurrence's `until` or `count`. A one-off has exactly one occurrence, its `start_date`.
pub fn next_occurrence(
    r: &Reminder,
    after: DateTime<Utc>,
    zone: &SchedulerZone,
) -> Option<(NaiveDate, DateTime<Utc>)> {
    let time = r.time()?;
    let Some(rec) = &r.recurrence else {
        if r.last_occurrence.is_some() {
            return None;
        }
        let t = resolve_local(zone, r.start_date, time);
        return (t > after).then_some((r.start_date, t));
    };
    if let Some(c) = rec.count {
        if r.fired_count.saturating_add(r.missed_count) >= c {
            return None;
        }
    }
    let after_local = after.with_timezone(zone).date_naive();
    // One day early, because a local date's instant can precede `after` in UTC terms.
    let mut d = r.start_date.max(after_local.pred_opt()?);
    if let Some(last) = r.last_occurrence {
        d = d.max(last.succ_opt()?);
    }
    for _ in 0..DAY_SEARCH_LIMIT {
        if rec.until.is_some_and(|u| d > u) {
            return None;
        }
        if rec.matches(r.start_date, d) {
            let t = resolve_local(zone, d, time);
            if t > after {
                return Some((d, t));
            }
        }
        d = d.succ_opt()?;
    }
    None
}

/// The instant of the reminder's next occurrence strictly after `after`, in `zone`. Pure: no
/// clock is read.
pub fn compute_next_fire(
    r: &Reminder,
    after: DateTime<Utc>,
    zone: &SchedulerZone,
) -> Option<DateTime<Utc>> {
    next_occurrence(r, after, zone).map(|(_, t)| t)
}

/// The ids of every active reminder due at `now`, earliest first. Pure.
pub fn due(reminders: &[Reminder], now: DateTime<Utc>) -> Vec<String> {
    let mut hits: Vec<(DateTime<Utc>, &str)> = reminders
        .iter()
        .filter(|r| r.state == ReminderState::Active)
        .filter_map(|r| {
            r.fire_at()
                .filter(|t| *t <= now)
                .map(|t| (t, r.id.as_str()))
        })
        .collect();
    hits.sort();
    hits.into_iter().map(|(_, id)| id.to_string()).collect()
}

/// Re-resolve every active `profile`-mode reminder whose `next_fire` was resolved in a zone
/// other than `profile_zone`: the SAME occurrence (its local date and time) read in the new
/// zone. Fixed-zone reminders are left alone. Returns how many moved.
pub fn reresolve(file: &mut ReminderFile, profile_zone: &SchedulerZone) -> usize {
    let label = zone_label(profile_zone);
    let mut moved = 0;
    for r in file.reminders.iter_mut() {
        if r.state != ReminderState::Active
            || r.zone_mode != ZoneMode::Profile
            || r.resolved_zone == label
        {
            continue;
        }
        if let (Some(date), Some(time)) = (r.next_date, r.time()) {
            r.next_fire = Some(resolve_local(profile_zone, date, time));
        }
        r.resolved_zone = label.clone();
        moved += 1;
    }
    moved
}

/// Drop every reminder closed more than [`KEEP_CLOSED_DAYS`] ago. Returns how many went.
pub fn prune(file: &mut ReminderFile, now: DateTime<Utc>) -> usize {
    let cutoff = now - ChronoDuration::days(KEEP_CLOSED_DAYS);
    let before = file.reminders.len();
    file.reminders
        .retain(|r| r.state == ReminderState::Active || r.closed_at.is_none_or(|c| c > cutoff));
    before - file.reminders.len()
}

// ---- The tick ----------------------------------------------------------------------------

/// One push the tick wants sent.
#[derive(Clone, PartialEq, Debug)]
pub struct ReminderFire {
    pub id: String,
    /// The text, plus `(late: due HH:MM)` when it fires late.
    pub body: String,
}

/// One occurrence the tick gave up on.
#[derive(Clone, PartialEq, Debug)]
pub struct MissedReminder {
    pub id: String,
    pub text: String,
    /// When it was due, `HH:MM` in its zone.
    pub due_local: String,
}

/// What one tick decided. Already recorded in the store by the time the caller sees it.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct TickOutcome {
    pub fires: Vec<ReminderFire>,
    pub missed: Vec<MissedReminder>,
    pub reresolved: usize,
    pub pruned: usize,
}

/// Decide and RECORD one tick at `now`. Pure over the file: the caller loads it, calls this,
/// writes it back, and only then sends what it returns.
///
/// Order: re-resolve profile-mode reminders for a zone change, prune, then for each due
/// reminder fire it (at most [`LATE_LIMIT_HOURS`] late) or mark it missed.
pub fn tick(
    file: &mut ReminderFile,
    now: DateTime<Utc>,
    profile_zone: &SchedulerZone,
) -> TickOutcome {
    let mut out = TickOutcome {
        reresolved: reresolve(file, profile_zone),
        pruned: prune(file, now),
        ..TickOutcome::default()
    };
    for id in due(&file.reminders, now) {
        let Some(r) = file.get_mut(&id) else { continue };
        let zone = zone_for(&r.zone_mode, profile_zone);
        let snooze_due = r.snooze_until.filter(|s| *s <= now);
        let mut series_due = r.next_fire.filter(|t| *t <= now);
        // A series that came due more than once while the bridge was down is judged by its
        // LATEST passed occurrence; the earlier ones are skipped, and counted as missed.
        if series_due.is_some() && r.recurrence.is_some() {
            for _ in 0..DAY_SEARCH_LIMIT {
                let (Some(t), Some(d)) = (r.next_fire, r.next_date) else {
                    break;
                };
                let mut probe = r.clone();
                probe.last_occurrence = Some(d);
                probe.missed_count = probe.missed_count.saturating_add(1);
                match next_occurrence(&probe, t, &zone) {
                    Some((nd, nt)) if nt <= now => {
                        r.last_occurrence = Some(d);
                        r.missed_count = probe.missed_count;
                        r.next_date = Some(nd);
                        r.next_fire = Some(nt);
                        series_due = Some(nt);
                    }
                    _ => break,
                }
            }
        }
        // The most recent instant that came due decides how late this fire is.
        let Some(fire_at) = [snooze_due, series_due].into_iter().flatten().max() else {
            continue;
        };
        let late = now - fire_at;
        let due_local = fire_at.with_timezone(&zone).format("%H:%M").to_string();
        let missed = late > ChronoDuration::hours(LATE_LIMIT_HOURS);

        if snooze_due.is_some() {
            r.snooze_until = None;
        }
        if series_due.is_some() {
            if let Some(d) = r.next_date {
                r.last_occurrence = Some(d);
            }
            if missed {
                r.missed_count = r.missed_count.saturating_add(1);
            }
        }
        if !missed {
            r.fired_count = r.fired_count.saturating_add(1);
            r.last_fired_at = Some(now);
            r.acked = false;
        }
        // Advance the series past NOW, so a catch-up fires a repeating reminder once.
        if series_due.is_some() || r.next_fire.is_none() {
            match r
                .recurrence
                .is_some()
                .then(|| next_occurrence(r, now, &zone))
                .flatten()
            {
                Some((d, t)) => {
                    r.next_date = Some(d);
                    r.next_fire = Some(t);
                    r.resolved_zone = zone_label(&zone);
                }
                None => {
                    r.next_date = None;
                    r.next_fire = None;
                }
            }
        }
        if r.next_fire.is_none() && r.snooze_until.is_none() {
            let end = if missed {
                ReminderState::Missed
            } else {
                ReminderState::Done
            };
            r.close(end, now);
        }
        if missed {
            out.missed.push(MissedReminder {
                id: r.id.clone(),
                text: r.text.clone(),
                due_local,
            });
        } else {
            let body = if late >= ChronoDuration::seconds(LATE_NOTE_SECS) {
                format!("{} (late: due {due_local})", r.text)
            } else {
                r.text.clone()
            };
            out.fires.push(ReminderFire {
                id: r.id.clone(),
                body,
            });
        }
    }
    out
}

// ---- Actions (the MCP tools and the notification routes share these) ---------------------

/// Why an action on one reminder was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionError {
    NotFound(String),
    Invalid(String),
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionError::NotFound(id) => write!(f, "no reminder with id {id:?}"),
            ActionError::Invalid(why) => f.write_str(why),
        }
    }
}

/// `Done`: acknowledge the latest fire. A one-off that has not fired yet is finished early; a
/// series keeps running and only its pending snooze is dropped.
pub fn mark_done(
    file: &mut ReminderFile,
    id: &str,
    now: DateTime<Utc>,
) -> Result<Reminder, ActionError> {
    let r = file
        .get_mut(id)
        .ok_or_else(|| ActionError::NotFound(id.to_string()))?;
    r.acked = true;
    r.snooze_until = None;
    if r.state == ReminderState::Active && (r.recurrence.is_none() || r.next_fire.is_none()) {
        r.close(ReminderState::Done, now);
    }
    Ok(r.clone())
}

/// `Snooze`: one extra fire `minutes` from `now`. A one-off's pending occurrence is replaced by
/// it (a fired one-off is reopened); a series keeps its own schedule beside it.
pub fn snooze(
    file: &mut ReminderFile,
    id: &str,
    minutes: i64,
    now: DateTime<Utc>,
) -> Result<Reminder, ActionError> {
    if !(1..=MAX_SNOOZE_MINUTES).contains(&minutes) {
        return Err(ActionError::Invalid(format!(
            "`minutes` must be 1-{MAX_SNOOZE_MINUTES}, got {minutes}"
        )));
    }
    let r = file
        .get_mut(id)
        .ok_or_else(|| ActionError::NotFound(id.to_string()))?;
    if r.state == ReminderState::Cancelled {
        return Err(ActionError::Invalid(format!(
            "reminder {id} is cancelled; create a new one instead"
        )));
    }
    if r.recurrence.is_none() {
        if r.next_date.is_some() {
            r.last_occurrence = r.next_date;
        }
        r.next_fire = None;
        r.next_date = None;
    } else if r.state != ReminderState::Active {
        // A finished series: the snooze is its one remaining fire.
        r.next_fire = None;
        r.next_date = None;
    }
    r.state = ReminderState::Active;
    r.closed_at = None;
    r.acked = false;
    r.snooze_until = Some(now + ChronoDuration::minutes(minutes));
    Ok(r.clone())
}

/// `cancel_reminder`.
pub fn cancel(
    file: &mut ReminderFile,
    id: &str,
    now: DateTime<Utc>,
) -> Result<Reminder, ActionError> {
    let r = file
        .get_mut(id)
        .ok_or_else(|| ActionError::NotFound(id.to_string()))?;
    if r.state != ReminderState::Cancelled {
        r.close(ReminderState::Cancelled, now);
    }
    Ok(r.clone())
}

/// What `create_reminder` was asked for, before validation.
#[derive(Debug, Clone, Default)]
pub struct CreateRequest {
    pub text: String,
    pub at: String,
    pub recurrence: Option<Recurrence>,
    /// An IANA zone, or `None` for the profile's.
    pub zone: Option<String>,
}

/// Validate and build a reminder created at `now`. Refuses an empty text, an unknown zone, an
/// unreadable or past `at`, and a recurrence whose `until` falls before the first fire.
///
/// `at` is a bare `HH:MM` (the next such minute in the zone) or a local date-time
/// (`2026-11-11T06:45`, seconds optional). A date-time with an offset or `Z` is converted to the
/// zone's wall clock first.
pub fn build_reminder(
    req: &CreateRequest,
    id: String,
    now: DateTime<Utc>,
    profile_zone: &SchedulerZone,
) -> Result<Reminder, String> {
    let text: String = req
        .text
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.is_empty() {
        return Err("`text` is empty: say what the reminder is for".into());
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(format!(
            "`text` is {} characters; the limit is {MAX_TEXT_CHARS}",
            text.chars().count()
        ));
    }
    let zone_mode = match req.zone.as_deref().map(str::trim).filter(|z| !z.is_empty()) {
        None | Some("profile") => ZoneMode::Profile,
        Some(z) => {
            let tz = parse_iana(z).ok_or_else(|| {
                format!("`zone` must be an IANA time zone like \"Europe/London\", got {z:?}")
            })?;
            ZoneMode::Fixed(tz.iana_name().unwrap_or_else(|| z.to_string()))
        }
    };
    let zone = zone_for(&zone_mode, profile_zone);
    let at = req.at.trim();
    let now_local = now.with_timezone(&zone).naive_local();
    let (start_date, time, explicit) = if let Some(t) = parse_hhmm(at) {
        let today = now_local.date();
        let date = if resolve_local(&zone, today, t) > now {
            today
        } else {
            today.succ_opt().ok_or("date out of range")?
        };
        (date, t, false)
    } else {
        let local = parse_local_datetime(at, &zone).ok_or_else(|| {
            format!("`at` must be HH:MM or a local date-time like 2026-11-11T06:45, got {at:?}")
        })?;
        let t = NaiveTime::from_hms_opt(
            chrono::Timelike::hour(&local),
            chrono::Timelike::minute(&local),
            0,
        )
        .ok_or("bad time")?;
        (local.date(), t, true)
    };
    let mut r = Reminder {
        id,
        text,
        created_at: now,
        next_fire: None,
        next_date: None,
        local_time: time.format("%H:%M").to_string(),
        zone_mode,
        resolved_zone: zone_label(&zone),
        start_date,
        recurrence: req.recurrence.clone(),
        state: ReminderState::Active,
        fired_count: 0,
        last_fired_at: None,
        missed_count: 0,
        last_occurrence: None,
        snooze_until: None,
        acked: false,
        closed_at: None,
    };
    if explicit {
        let first = resolve_local(&zone, start_date, time);
        if first <= now {
            return Err(format!(
                "`at` {at} is in the past (it is now {} in {})",
                now_local.format("%Y-%m-%d %H:%M"),
                zone_label(&zone)
            ));
        }
    }
    match next_occurrence(&r, now, &zone) {
        Some((d, t)) => {
            r.next_date = Some(d);
            r.next_fire = Some(t);
        }
        None => {
            if let Some(until) = r.recurrence.as_ref().and_then(|rec| rec.until) {
                let mut open = r.clone();
                if let Some(rec) = open.recurrence.as_mut() {
                    rec.until = None;
                }
                let first = next_occurrence(&open, now, &zone)
                    .map(|(d, _)| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "never".into());
                return Err(format!(
                    "`until` {until} falls before the first fire ({first})"
                ));
            }
            return Err("that reminder would never fire".into());
        }
    }
    Ok(r)
}

fn parse_local_datetime(raw: &str, zone: &SchedulerZone) -> Option<NaiveDateTime> {
    for fmt in [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(t) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(t);
        }
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|t| t.with_timezone(zone).naive_local())
}

// ---- Views -------------------------------------------------------------------------------

/// A reminder as a turn or the app reads it: both clocks for the next fire.
pub fn reminder_view(r: &Reminder, profile_zone: &SchedulerZone) -> Value {
    let zone = zone_for(&r.zone_mode, profile_zone);
    let local = |t: DateTime<Utc>| {
        t.with_timezone(&zone)
            .format("%Y-%m-%dT%H:%M%:z")
            .to_string()
    };
    let utc = |t: DateTime<Utc>| t.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let next = r.fire_at().filter(|_| r.state == ReminderState::Active);
    json!({
        "id": r.id,
        "text": r.text,
        "state": r.display_state(),
        "local_time": r.local_time,
        "zone_mode": r.zone_mode.label(),
        "zone": zone_label(&zone),
        "recurrence": r.recurrence.as_ref().map(Recurrence::to_json),
        "next_fire_local": next.map(local),
        "next_fire_utc": next.map(utc),
        "next_fire_readable": next.map(|t| t.with_timezone(&zone).format("%a %-d %b %Y %H:%M").to_string()),
        "snoozed_until_utc": r.snooze_until.map(utc),
        "fired_count": r.fired_count,
        "last_fired_at": r.last_fired_at.map(utc),
        "created_at": utc(r.created_at),
    })
}

// ---- The store ---------------------------------------------------------------------------

/// `reminders.json`, shared by the bridge and the MCP child. Cheap to clone: a path.
#[derive(Clone, Debug)]
pub struct ReminderStore {
    path: PathBuf,
}

impl ReminderStore {
    pub fn new(path: PathBuf) -> ReminderStore {
        ReminderStore { path }
    }

    /// `<state_dir>/reminders.json`.
    pub fn in_state_dir(dir: &Path) -> ReminderStore {
        ReminderStore::new(dir.join(REMINDERS_FILE))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock_path(&self) -> PathBuf {
        let mut name = self
            .path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        name.push(".lock");
        self.path.with_file_name(name)
    }

    fn lock(&self, exclusive: bool) -> Result<File, String> {
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        let lp = self.lock_path();
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&lp)
            .map_err(|e| format!("could not open {}: {e}", lp.display()))?;
        let op = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        loop {
            // SAFETY: flock on a descriptor this function owns for the life of `f`.
            let r = unsafe { libc::flock(f.as_raw_fd(), op) };
            if r == 0 {
                return Ok(f);
            }
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(format!("could not lock {}: {e}", lp.display()));
            }
        }
    }

    fn read(&self) -> Result<ReminderFile, String> {
        match std::fs::read(&self.path) {
            Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Ok(ReminderFile::default()),
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                format!(
                    "{} does not parse ({e}); it is left untouched",
                    self.path.display()
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ReminderFile::default()),
            Err(e) => Err(format!("could not read {}: {e}", self.path.display())),
        }
    }

    /// Read the store under a shared lock. An absent file is an empty store.
    pub fn load(&self) -> Result<ReminderFile, String> {
        let _lock = self.lock(false)?;
        self.read()
    }

    /// Read, change and write the store under the exclusive lock. `f`'s result is returned
    /// once the change is on disk; the file is written only when `f` changed it.
    pub fn update<R>(&self, f: impl FnOnce(&mut ReminderFile) -> R) -> Result<R, String> {
        let _lock = self.lock(true)?;
        let mut file = self.read()?;
        let before = file.clone();
        let out = f(&mut file);
        if file != before {
            let bytes = serde_json::to_vec_pretty(&file)
                .map_err(|e| format!("could not serialise reminders: {e}"))?;
            write_atomic(&self.path, &bytes)
                .map_err(|e| format!("could not write {}: {e}", self.path.display()))?;
        }
        Ok(out)
    }
}

/// The state directory the bridge uses, read the way the bridge's config reads it:
/// `JESSE_STATE_DIR`, else `$HOME/.jesse-bridge`.
pub fn state_dir_from_env() -> Option<PathBuf> {
    if let Some(d) = std::env::var("JESSE_STATE_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        return Some(PathBuf::from(d));
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .map(|h| PathBuf::from(h).join(".jesse-bridge"))
}

/// The profile zone, read exactly as the bridge reads it: `<state_dir>/profile.json` through
/// [`ProfileStore`] (an away period in force names the zone, else the host's).
pub fn profile_zone_in(state_dir: &Path, now: DateTime<Utc>) -> SchedulerZone {
    ProfileStore::new(Some(state_dir.join("profile.json")))
        .zone(now.timestamp_millis().max(0) as u64)
}

// ---- The MCP tools -----------------------------------------------------------------------

/// The four tools `jesse-reminders-mcp` serves: the whole server, and all four are granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderTool {
    Create,
    List,
    Cancel,
    Snooze,
}

impl ReminderTool {
    pub const ALL: [ReminderTool; 4] = [
        ReminderTool::Create,
        ReminderTool::List,
        ReminderTool::Cancel,
        ReminderTool::Snooze,
    ];

    pub fn tool_name(self) -> &'static str {
        match self {
            ReminderTool::Create => "create_reminder",
            ReminderTool::List => "list_reminders",
            ReminderTool::Cancel => "cancel_reminder",
            ReminderTool::Snooze => "snooze_reminder",
        }
    }

    pub fn parse(name: &str) -> Option<ReminderTool> {
        Self::ALL.into_iter().find(|t| t.tool_name() == name)
    }

    pub fn description(self) -> &'static str {
        match self {
            ReminderTool::Create => {
                "Set a timed reminder: at that minute the owner's phone gets a notification with \
                 the text, and Done and Snooze buttons. No agent turn runs. `at` is HH:MM (the \
                 next such minute) or a local date-time like 2026-11-11T06:45, read in the \
                 active profile's time zone unless `zone` names a fixed IANA zone. \
                 `recurrence` repeats it: {\"kind\":\"daily\"}, {\"kind\":\"weekdays\"}, \
                 {\"kind\":\"weekly\",\"days\":[\"mon\",\"thu\"]}, {\"kind\":\"monthly\",\"day\":31} \
                 (a short month uses its last day) or {\"kind\":\"every_n_days\",\"n\":3}, each \
                 with optional `until` (YYYY-MM-DD) or `count`. Returns the resolved next fire \
                 in local time and UTC: read it back to the owner."
            }
            ReminderTool::List => {
                "List reminders: id, text, state, recurrence and next fire (local and UTC). \
                 Active and fired-but-unacknowledged ones by default; include_inactive adds \
                 done, cancelled and missed ones from the last 30 days."
            }
            ReminderTool::Cancel => "Cancel a reminder by id. It will not fire again.",
            ReminderTool::Snooze => {
                "Snooze a reminder by id: it fires once more `minutes` from now (1-1440). A \
                 repeating reminder keeps its own schedule as well."
            }
        }
    }

    pub fn input_schema(self) -> Value {
        match self {
            ReminderTool::Create => json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "What to remind the owner of, as the notification will read."},
                    "at": {"type": "string", "description": "HH:MM (24-hour, the next such minute) or a local date-time YYYY-MM-DDTHH:MM."},
                    "recurrence": {"type": "object", "description": "Optional. {kind: daily|weekdays|weekly|monthly|every_n_days, days?: [mon..sun], day?: 1-31, n?: days, until?: YYYY-MM-DD, count?: N}"},
                    "zone": {"type": "string", "description": "Optional fixed IANA zone, e.g. Europe/London. Omit to follow the active profile's zone."}
                },
                "required": ["text", "at"]
            }),
            ReminderTool::List => json!({
                "type": "object",
                "properties": {
                    "include_inactive": {"type": "boolean", "description": "Also list done, cancelled and missed reminders."}
                }
            }),
            ReminderTool::Cancel => json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            }),
            ReminderTool::Snooze => json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "minutes": {"type": "integer", "minimum": 1, "maximum": MAX_SNOOZE_MINUTES}
                },
                "required": ["id", "minutes"]
            }),
        }
    }
}

/// Run one tool call against `store` at `now`, with the profile zone as the bridge would read
/// it. `Err` is a message the turn can read and act on.
pub fn run_reminder_tool(
    store: &ReminderStore,
    tool: ReminderTool,
    args: &Value,
    now: DateTime<Utc>,
    profile_zone: &SchedulerZone,
) -> Result<Value, String> {
    let str_arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let id_arg = || {
        str_arg("id")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "`id` is required".to_string())
    };
    match tool {
        ReminderTool::Create => {
            let recurrence = match args.get("recurrence") {
                None | Some(Value::Null) => None,
                Some(v) => Some(Recurrence::from_json(v)?),
            };
            let req = CreateRequest {
                text: str_arg("text").unwrap_or_default(),
                at: str_arg("at").ok_or("`at` is required: HH:MM or YYYY-MM-DDTHH:MM")?,
                recurrence,
                zone: str_arg("zone"),
            };
            let r = store.update(|file| {
                let r = build_reminder(&req, file.fresh_id(), now, profile_zone)?;
                file.reminders.push(r.clone());
                Ok::<_, String>(r)
            })??;
            let mut v = reminder_view(&r, profile_zone);
            v["confirmation"] = json!(format!(
                "Reminder {} set for {} {} ({} UTC){}",
                r.id,
                v["next_fire_readable"].as_str().unwrap_or_default(),
                v["zone"].as_str().unwrap_or_default(),
                r.next_fire
                    .map(|t| t.format("%H:%M").to_string())
                    .unwrap_or_default(),
                if r.recurrence.is_some() {
                    ", repeating"
                } else {
                    ""
                }
            ));
            Ok(v)
        }
        ReminderTool::List => {
            let all = args
                .get("include_inactive")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let file = store.load()?;
            let list: Vec<Value> = file
                .reminders
                .iter()
                .filter(|r| all || r.state == ReminderState::Active || r.display_state() == "fired")
                .map(|r| reminder_view(r, profile_zone))
                .collect();
            Ok(json!({"zone": zone_label(profile_zone), "reminders": list}))
        }
        ReminderTool::Cancel => {
            let id = id_arg()?;
            let r = store
                .update(|file| cancel(file, &id, now))?
                .map_err(|e| e.to_string())?;
            Ok(reminder_view(&r, profile_zone))
        }
        ReminderTool::Snooze => {
            let id = id_arg()?;
            let minutes = args
                .get("minutes")
                .and_then(Value::as_i64)
                .ok_or("`minutes` is required: a whole number, 1-1440")?;
            let r = store
                .update(|file| snooze(file, &id, minutes, now))?
                .map_err(|e| e.to_string())?;
            Ok(reminder_view(&r, profile_zone))
        }
    }
}

// ---- Serde helpers (chrono is built without its `serde` feature) -------------------------

mod utc {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
        let raw = String::deserialize(d)?;
        DateTime::parse_from_rfc3339(&raw)
            .map(|t| t.with_timezone(&Utc))
            .map_err(serde::de::Error::custom)
    }
}

mod utc_opt {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &Option<DateTime<Utc>>, s: S) -> Result<S::Ok, S::Error> {
        match t {
            Some(t) => s.serialize_str(&t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<DateTime<Utc>>, D::Error> {
        let raw = Option::<String>::deserialize(d)?;
        raw.map(|r| {
            DateTime::parse_from_rfc3339(&r)
                .map(|t| t.with_timezone(&Utc))
                .map_err(serde::de::Error::custom)
        })
        .transpose()
    }
}

mod date {
    use chrono::NaiveDate;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &NaiveDate, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&d.format("%Y-%m-%d").to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<NaiveDate, D::Error> {
        let raw = String::deserialize(d)?;
        NaiveDate::parse_from_str(&raw, "%Y-%m-%d").map_err(serde::de::Error::custom)
    }
}

mod date_opt {
    use chrono::NaiveDate;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Option<NaiveDate>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => s.serialize_str(&d.format("%Y-%m-%d").to_string()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<NaiveDate>, D::Error> {
        let raw = Option::<String>::deserialize(d)?;
        raw.map(|r| NaiveDate::parse_from_str(&r, "%Y-%m-%d").map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[cfg(test)]
mod tests;
