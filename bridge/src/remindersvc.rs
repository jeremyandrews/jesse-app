//! **THE REMINDER TICKER AND ROUTES**: the bridge's half of [`crate::reminders`].
//!
//! Every [`TICK_SECS`] the ticker loads `reminders.json`, decides what is due
//! ([`crate::reminders::tick`]), WRITES that decision back, and only then sends each push through
//! [`ApnsClient::push_payload`]. Recording first is what makes "never twice" hold across a
//! restart: the worst a crash between the write and the send can do is lose one push. Every push
//! error is logged and swallowed, the way `push_schedule_outcome` does, and none can touch the
//! store, which was already written.
//!
//! A reminder is a notification and nothing else: nothing here builds a prompt or spawns a model.
//!
//! The routes are what the app's notification actions call, with the bearer auth and rate limiter
//! every `/jesse/schedule` route takes: `GET /jesse/reminders`, `POST /jesse/reminders/{id}/done`
//! and `POST /jesse/reminders/{id}/snooze` (`{"minutes": N}`, default 10).

use crate::reminders::{self, ReminderStore, TickOutcome};
use crate::*;
use chrono::{DateTime, Utc};

/// How often the ticker looks.
pub const TICK_SECS: u64 = 30;

/// The notification category the app registers with `Done` and `Snooze 10 min`.
pub const REMINDER_CATEGORY: &str = "JESSE_REMINDER";

/// The payload for one reminder: title `Reminder`, the text as the body, a sound, the
/// time-sensitive interruption level, the actions' category and the reminder's id. The badge is
/// stamped by [`ApnsClient::push_payload`], as on every push.
pub fn build_reminder_payload(id: &str, body: &str) -> Vec<u8> {
    json!({
        "aps": {
            "alert": { "title": "Reminder", "body": body },
            "sound": "default",
            "interruption-level": "time-sensitive",
            "category": REMINDER_CATEGORY,
        },
        "reminder_id": id,
    })
    .to_string()
    .into_bytes()
}

/// The ONE summary push for every reminder a catch-up gave up on. No category: there is no one
/// reminder for `Done` or `Snooze` to act on.
pub fn build_missed_payload(missed: &[reminders::MissedReminder]) -> Vec<u8> {
    let items: Vec<String> = missed
        .iter()
        .map(|m| format!("{} (due {})", m.text, m.due_local))
        .collect();
    let title = if missed.len() == 1 {
        "Missed reminder".to_string()
    } else {
        format!("{} missed reminders", missed.len())
    };
    json!({
        "aps": {
            "alert": { "title": title, "body": format!("While the bridge was down: {}", items.join("; ")) },
            "sound": "default",
        },
        "reminder_ids": missed.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        "outcome": "reminders-missed",
    })
    .to_string()
    .into_bytes()
}

/// Where the ticker's pushes go. The bridge's is [`ApnsSink`]; the tests' records them.
pub trait ReminderSink: Send + Sync {
    /// Send one payload. `Err` is logged and swallowed by the caller.
    fn send(
        &self,
        payload: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;
}

/// The real sink: the bridge's APNs client and registered device, badge and all.
pub struct ApnsSink {
    pub st: AppState,
}

impl ReminderSink for ApnsSink {
    fn send(
        &self,
        payload: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            let apns = self
                .st
                .apns
                .as_deref()
                .ok_or("push is not configured (JESSE_APNS_*)")?;
            let token = self.st.devices.get().ok_or("no device registered")?;
            let badge = Some(unread_conversation_count(
                &self.st.conversations,
                &self.st.flags,
            ));
            match apns.push_payload(&token, payload, badge).await {
                PushOutcome::Sent => Ok(()),
                PushOutcome::DeadToken => {
                    self.st.devices.clear();
                    Err("device token rejected (410 dead) — cleared".into())
                }
                PushOutcome::Failed(e) => Err(e),
            }
        })
    }
}

/// One tick at `now`: decide and record under the store's lock, then send. Returns what was
/// decided (already on disk). A store that cannot be read or written sends nothing.
pub async fn run_tick(
    store: &ReminderStore,
    now: DateTime<Utc>,
    profile_zone: SchedulerZone,
    sink: &dyn ReminderSink,
) -> Result<TickOutcome, String> {
    let s = store.clone();
    let zone_name = reminders::zone_label(&profile_zone);
    let outcome = tokio::task::spawn_blocking(move || {
        s.update(|file| reminders::tick(file, now, &profile_zone))
    })
    .await
    .map_err(|e| format!("reminder tick task: {e}"))??;
    for fire in &outcome.fires {
        match sink
            .send(build_reminder_payload(&fire.id, &fire.body))
            .await
        {
            Ok(()) => eprintln!("jesse-bridge: reminder PUSH id={} sent", fire.id),
            Err(e) => eprintln!(
                "jesse-bridge: reminder PUSH id={} failed: {e} — swallowed",
                fire.id
            ),
        }
    }
    if !outcome.missed.is_empty() {
        let ids: Vec<&str> = outcome.missed.iter().map(|m| m.id.as_str()).collect();
        match sink.send(build_missed_payload(&outcome.missed)).await {
            Ok(()) => eprintln!("jesse-bridge: reminder MISSED summary ids={ids:?} sent"),
            Err(e) => eprintln!(
                "jesse-bridge: reminder MISSED summary ids={ids:?} failed: {e} — swallowed"
            ),
        }
    }
    if outcome.reresolved > 0 {
        eprintln!(
            "jesse-bridge: reminders — {} profile-zone reminder(s) re-resolved in {}",
            outcome.reresolved, zone_name
        );
    }
    Ok(outcome)
}

/// The store, when the bridge has a state directory.
pub fn reminder_store(cfg: &Config) -> Option<ReminderStore> {
    cfg.state_dir
        .as_deref()
        .map(|d| ReminderStore::in_state_dir(Path::new(d)))
}

/// The profile zone at `now`, as every other date in the bridge reads it.
fn profile_zone(st: &AppState, now: DateTime<Utc>) -> SchedulerZone {
    st.profile.zone(now.timestamp_millis().max(0) as u64)
}

/// Start the ticker beside the scheduler. No state dir, no reminders: said once.
pub fn spawn_reminder_ticker(st: AppState) {
    let Some(store) = reminder_store(&st.cfg) else {
        eprintln!(
            "jesse-bridge: reminders are OFF — no state dir (JESSE_STATE_DIR), so there is \
             nowhere to keep reminders.json"
        );
        return;
    };
    tokio::spawn(async move {
        let sink = ApnsSink { st: st.clone() };
        let mut every = tokio::time::interval(Duration::from_secs(TICK_SECS));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            let now = Utc::now();
            if let Err(e) = run_tick(&store, now, profile_zone(&st, now), &sink).await {
                eprintln!("jesse-bridge: reminder tick failed: {e} — nothing sent");
            }
        }
    });
}

/// Re-resolve every profile-mode reminder in the zone now in force. Called by the profile
/// endpoint so the change is visible at once, not a tick later. Best effort: logged on failure.
pub async fn reresolve_reminders(st: &AppState) {
    let Some(store) = reminder_store(&st.cfg) else {
        return;
    };
    let zone = profile_zone(st, Utc::now());
    let result =
        tokio::task::spawn_blocking(move || store.update(|f| reminders::reresolve(f, &zone))).await;
    match result {
        Ok(Ok(n)) if n > 0 => {
            eprintln!("jesse-bridge: reminders — {n} re-resolved after a profile change")
        }
        Ok(Ok(_)) => {}
        Ok(Err(e)) => eprintln!("jesse-bridge: reminders — re-resolve failed: {e}"),
        Err(e) => eprintln!("jesse-bridge: reminders — re-resolve task: {e}"),
    }
}

// ---- Routes ------------------------------------------------------------------------------

fn admit(st: &AppState, headers: &HeaderMap) -> Result<ReminderStore, ApiError> {
    check_auth(headers, &st.cfg.token)?;
    if !st.limiter.allow() {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded".to_string(),
        ));
    }
    reminder_store(&st.cfg).ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "reminders need a state dir (JESSE_STATE_DIR)".to_string(),
    ))
}

async fn act(
    st: &AppState,
    store: ReminderStore,
    f: impl FnOnce(
            &mut reminders::ReminderFile,
            DateTime<Utc>,
        ) -> Result<reminders::Reminder, reminders::ActionError>
        + Send
        + 'static,
) -> Result<Json<Value>, ApiError> {
    let now = Utc::now();
    let r = tokio::task::spawn_blocking(move || store.update(|file| f(file, now)))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .map_err(|e| match e {
            reminders::ActionError::NotFound(_) => (StatusCode::NOT_FOUND, e.to_string()),
            reminders::ActionError::Invalid(_) => (StatusCode::BAD_REQUEST, e.to_string()),
        })?;
    Ok(Json(
        json!({"reminder": reminders::reminder_view(&r, &profile_zone(st, now))}),
    ))
}

/// `GET /jesse/reminders` — every stored reminder, inactive ones included.
pub async fn jesse_reminders(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let store = admit(&st, &headers)?;
    let file = tokio::task::spawn_blocking(move || store.load())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let zone = profile_zone(&st, Utc::now());
    let list: Vec<Value> = file
        .reminders
        .iter()
        .map(|r| reminders::reminder_view(r, &zone))
        .collect();
    Ok(Json(
        json!({"zone": reminders::zone_label(&zone), "reminders": list}),
    ))
}

/// `POST /jesse/reminders/{id}/done` — the notification's `Done`.
pub async fn jesse_reminder_done(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let store = admit(&st, &headers)?;
    act(&st, store, move |f, now| reminders::mark_done(f, &id, now)).await
}

/// The snooze body. Absent, or `minutes` absent, is ten minutes.
#[derive(Deserialize, Default)]
pub struct SnoozeBody {
    #[serde(default)]
    pub minutes: Option<i64>,
}

/// `POST /jesse/reminders/{id}/snooze` — the notification's `Snooze 10 min`.
pub async fn jesse_reminder_snooze(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    body: Option<Json<SnoozeBody>>,
) -> Result<Json<Value>, ApiError> {
    let store = admit(&st, &headers)?;
    let minutes = body
        .and_then(|Json(b)| b.minutes)
        .unwrap_or(reminders::DEFAULT_SNOOZE_MINUTES);
    act(&st, store, move |f, now| {
        reminders::snooze(f, &id, minutes, now)
    })
    .await
}

#[cfg(test)]
mod tests;
