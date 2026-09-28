//! The HTTP boundary of recorded-audio transcription — and the ONLY file under `speech/` that
//! may see the application state. `scripts/ci-guards.sh` holds every other file here to that.
//!
//! * `POST /jesse/transcriptions` — THE ONE DOOR. The body is the recording itself, streamed to
//!   disk through the intake gate; `Content-Type` declares its type and is checked against the
//!   magic bytes; `?language=`, `?conditioning=` and `?second_reading=` tune the run. Answers
//!   `202` with the run's first status.
//! * `GET /jesse/transcriptions/{id}` — the run's status, then its transcript and
//!   disagreement list. Polled by the app; not rate-limited, because a poll is not work.
//! * `POST /jesse/transcriptions/{id}/cancel` — stop the run; its audio is deleted.
//! * `GET /jesse/speech` — whether this bridge transcribes, with which models, and which
//!   engines a run may choose (`engines`: the Studio's own, then every armed registry entry
//!   that declares a transcription capability).
//!
//! Same bearer auth as every other route. The upload route carries its OWN body limit (the
//! audio cap, enforced while streaming) instead of the router's, which is sized for base64
//! photos and would refuse a recording long before its cap.
//!
//! This file is also where a hosted engine is RESOLVED: the registry is read here and only
//! here, and what leaves is a [`HostedTarget`], plain data the pipeline cannot use to reach
//! anything but that engine's transcription request.

use super::hosted::{HostedTarget, HOSTED_PREFIX};
use super::intake::{over_cap, AudioCustody, SniffedUpload, UploadGate};
use super::service::{
    parse_second_engine, EngineChoice, EnginePlan, EngineSource, FinishHook, JobOptions,
};
use crate::*;
use tokio::io::AsyncWriteExt;

/// Every hosted speech engine this configuration ARMS: a registry entry that declares a
/// transcription capability and whose backend resolved (its token is set). In registry order.
pub fn hosted_targets(cfg: &Config) -> Vec<HostedTarget> {
    cfg.model_registry
        .models
        .iter()
        .filter(|m| m.configured)
        .filter_map(|m| {
            let cap = m.transcription.clone()?;
            let (base_url, token, _) = m.backend.clone()?;
            Some(HostedTarget {
                id: m.id.clone(),
                label: m.label.clone(),
                base_url: cap.endpoint.clone().unwrap_or(base_url),
                token,
                cap,
            })
        })
        .collect()
}

/// The armed hosted engine a choice names, by id or alias, or the sentence that says why not.
fn armed_target(cfg: &Config, id: &str) -> Result<HostedTarget, String> {
    let canonical = cfg
        .model_registry
        .get(id)
        .map(|m| m.id.clone())
        .unwrap_or_else(|| id.to_string());
    let armed = hosted_targets(cfg);
    armed
        .iter()
        .find(|t| t.id == canonical)
        .cloned()
        .ok_or_else(|| {
            let names: Vec<String> = armed.iter().map(HostedTarget::engine_id).collect();
            format!(
                "engine \"{HOSTED_PREFIX}{id}\" is not an armed transcription engine on this \
                 bridge; the choices are local{}{}",
                if names.is_empty() { "" } else { ", " },
                names.join(", ")
            )
        })
}

/// Resolve a run's engines from its `engine` and `second_engine` (or the configured defaults).
/// Nothing is hosted unless one of the four names it.
pub fn resolve_plan(
    cfg: &Config,
    engine: Option<&str>,
    second_engine: Option<&str>,
) -> Result<EnginePlan, String> {
    let choice = match engine.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => EngineChoice::parse(raw).map_err(|e| format!("engine: {e}"))?,
        None => cfg.speech.engine.clone(),
    };
    let second_id = match second_engine.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => parse_second_engine(raw).map_err(|e| format!("second_engine: {e}"))?,
        None => cfg.speech.second_engine.clone(),
    };
    let (primary, fallback) = match &choice {
        EngineChoice::Local => (EngineSource::Local, None),
        EngineChoice::Hosted(id) => (EngineSource::Hosted(armed_target(cfg, id)?), None),
        EngineChoice::LocalThenHosted(id) => (EngineSource::Local, Some(armed_target(cfg, id)?)),
    };
    let second = match second_id {
        None => EngineSource::Local,
        Some(id) => EngineSource::Hosted(armed_target(cfg, &id)?),
    };
    Ok(EnginePlan {
        primary,
        fallback,
        second,
        choice,
    })
}

/// The tuning a recording may carry.
#[derive(Deserialize, Default)]
pub struct TranscribeQuery {
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub conditioning: Option<String>,
    #[serde(default)]
    pub second_reading: Option<String>,
    /// `local`, `hosted:<id>` or `local,hosted:<id>`: this run's engine, overriding
    /// `JESSE_SPEECH_ENGINE`. An id that is not an armed transcription engine is refused.
    #[serde(default)]
    pub engine: Option<String>,
    /// `local` or `hosted:<id>`: this run's second reading, overriding
    /// `JESSE_SPEECH_SECOND_ENGINE`.
    #[serde(default)]
    pub second_engine: Option<String>,
    /// `1` asks for a completion push to the registered device when the run ends, so a phone
    /// that went to the background (or was killed) while the Studio worked still hears about
    /// it. An app that does not send it gets exactly the old behaviour: it polls, nothing is
    /// pushed.
    #[serde(default)]
    pub notify: Option<String>,
    /// The conversation the recording was attached in, carried in the push so the app can
    /// deliver the transcript to that conversation and a tap can open it. An opaque id: the
    /// bridge only echoes it.
    #[serde(default)]
    pub conversation_id: Option<String>,
}

pub async fn jesse_transcribe(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TranscribeQuery>,
    body: axum::body::Body,
) -> Result<Response, ApiError> {
    check_auth(&headers, &st.cfg.token)?;
    if !st.limiter.allow() {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded".to_string(),
        ));
    }
    let speech = st.speech.clone();
    speech
        .availability()
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, e))?;
    let mut opts = JobOptions::parse(
        q.language.as_deref(),
        q.conditioning.as_deref(),
        q.second_reading.as_deref(),
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    // Before a byte of audio is read: a run that names an engine this bridge cannot use is
    // refused by name, and a configured default that names one is a 503 (the bridge's fault,
    // not the request's).
    let asked = q.engine.is_some() || q.second_engine.is_some();
    opts.plan =
        resolve_plan(&st.cfg, q.engine.as_deref(), q.second_engine.as_deref()).map_err(|e| {
            let code = if asked {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            (code, e)
        })?;
    let declared = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "declare the recording's type in Content-Type, for example audio/mp4".to_string(),
            )
        })?;
    let cap = speech.config.max_audio_bytes;
    // Refuse a declared over-cap body before a byte of it is read. A body with no length (or
    // one that lies) is held to the same cap by the gate as it streams.
    if let Some(len) = headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        if len > cap {
            return Err(over_cap(cap));
        }
    }
    let root = speech.intake_dir().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "this bridge has no intake directory".to_string(),
        )
    })?;
    let custody = AudioCustody::open(&root).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not open a private intake directory: {e}"),
        )
    })?;
    // From here every early return drops `custody`, which deletes whatever arrived.
    let partial = custody.file("upload.part");
    let sniffed = receive(body, &partial, UploadGate::new(&declared, cap)).await?;
    let upload = custody.file(&format!("upload.{}", sniffed.ext));
    std::fs::rename(&partial, &upload).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not keep the upload: {e}"),
        )
    })?;
    // The completion push, when the app asked for one AND one can actually be sent. The
    // answer says which, as `notify`, so an app that gets `false` knows to tell the owner
    // itself rather than wait for a push that is not coming.
    let hook = completion_hook(&st, &q);
    let will_push = hook.is_some();
    let mut status = speech.start(custody, upload, sniffed, opts, hook);
    if let Some(obj) = status.as_object_mut() {
        obj.insert("notify".to_string(), json!(will_push));
    }
    Ok((StatusCode::ACCEPTED, Json(status)).into_response())
}

/// Stream the body to `dest` (0600) through the gate. Memory is bounded by one chunk.
async fn receive(
    body: axum::body::Body,
    dest: &Path,
    mut gate: UploadGate,
) -> Result<SniffedUpload, ApiError> {
    let internal = |what: &str, e: std::io::Error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not {what} the upload: {e}"),
        )
    };
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dest)
        .await
        .map_err(|e| internal("store", e))?;
    let mut stream = Box::pin(body.into_data_stream());
    while let Some(chunk) = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
        let chunk = chunk.map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("the upload was interrupted: {e}"),
            )
        })?;
        gate.accept(&chunk)?;
        file.write_all(&chunk)
            .await
            .map_err(|e| internal("write", e))?;
    }
    file.sync_all().await.map_err(|e| internal("flush", e))?;
    gate.finish()
}

/// A conversation id worth echoing: short, and nothing but letters, digits and hyphens (a
/// UUID in practice). Anything else is dropped rather than refused, because the push is a
/// convenience and the upload must not fail over it.
fn echoable_conversation_id(raw: Option<&str>) -> Option<String> {
    let id = raw?.trim();
    let ok = !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    ok.then(|| id.to_string())
}

/// The hook that pushes a run's ending to the registered device, or `None` when the app did
/// not ask for one or none could be sent (push not configured, no device registered).
///
/// The push carries ids and the outcome and NOTHING ELSE: never a word of the transcript,
/// which the app fetches over the paired connection like every other status.
fn completion_hook(st: &AppState, q: &TranscribeQuery) -> Option<FinishHook> {
    let asked = matches!(
        q.notify
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1") | Some("true") | Some("on")
    );
    if !asked || st.apns.is_none() || st.devices.get().is_none() {
        return None;
    }
    let st = st.clone();
    let conversation = echoable_conversation_id(q.conversation_id.as_deref());
    Some(Box::new(move |id: &str, state: &'static str| {
        // A cancel is the owner's own decision, made in the app: nothing to tell them.
        if state == "cancelled" {
            return;
        }
        let id = id.to_string();
        tokio::spawn(async move {
            push_transcription_outcome(&st, &id, conversation.as_deref(), state).await;
        });
    }))
}

/// Send one completion push. Every failure is logged and swallowed, as every other push is:
/// the result is already kept for the app to collect, and the app also collects it on its
/// next launch or foreground, so a lost push delays delivery and loses nothing.
async fn push_transcription_outcome(
    st: &AppState,
    id: &str,
    conversation_id: Option<&str>,
    state: &str,
) {
    let Some(apns) = st.apns.as_deref() else {
        return;
    };
    let Some(token) = st.devices.get() else {
        eprintln!("jesse-bridge: speech PUSH id={id} — no device registered, nothing sent");
        return;
    };
    let payload = build_transcription_payload(id, conversation_id, state);
    let badge = Some(unread_conversation_count(&st.conversations, &st.flags));
    match apns.push_payload(&token, payload, badge).await {
        PushOutcome::Sent => eprintln!("jesse-bridge: speech PUSH id={id} state={state} sent"),
        PushOutcome::DeadToken => {
            st.devices.clear();
            eprintln!("jesse-bridge: speech PUSH id={id} — device token rejected (410) — cleared");
        }
        PushOutcome::Failed(e) => {
            eprintln!("jesse-bridge: speech PUSH id={id} failed: {e} — swallowed")
        }
    }
}

fn not_found() -> ApiError {
    (
        StatusCode::NOT_FOUND,
        "no such transcription — a finished one is kept for a day, and none survives a \
         bridge restart"
            .to_string(),
    )
}

pub async fn jesse_transcription(
    State(st): State<AppState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &st.cfg.token)?;
    st.speech.status(&id).map(Json).ok_or_else(not_found)
}

pub async fn jesse_transcription_cancel(
    State(st): State<AppState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &st.cfg.token)?;
    st.speech.cancel(&id).map(Json).ok_or_else(not_found)
}

pub async fn jesse_speech(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &st.cfg.token)?;
    let mut v = st.speech.overview();
    if let Some(obj) = v.as_object_mut() {
        obj.insert("engines".to_string(), Value::Array(engine_rows(&st.cfg)));
    }
    Ok(Json(v))
}

/// The engines a run may choose: the Studio first, then each armed hosted engine. The
/// configured default is the overview's `default_engine`; a run that sends no `engine` gets
/// it. Never a token.
pub fn engine_rows(cfg: &Config) -> Vec<Value> {
    let mut rows = vec![json!({
        "id": "local",
        "label": format!("On the Studio ({} tier)", cfg.speech.tier.label()),
        "hosted": false,
    })];
    rows.extend(hosted_targets(cfg).iter().map(HostedTarget::overview));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speech::decode::SystemDecoder;
    use crate::speech::engine::fakes::{ScriptedEngine, ScriptedLoader};
    use crate::speech::engine::{EngineError, Segment};
    use crate::speech::models::fakes::{entry, FakeFetcher};
    use crate::speech::models::SpeechTier;
    use crate::speech::service::{SpeechConfig, SpeechService};
    use crate::speech::wav::encode_wav16;
    use crate::testutil::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    struct Rig {
        st: AppState,
        root: PathBuf,
        fetcher: Arc<FakeFetcher>,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn engine(id: &str, lines: &[(u64, u64, &str)]) -> ScriptedEngine {
        ScriptedEngine::new(
            id,
            lines
                .iter()
                .map(|(a, b, t)| Segment::new(a * 1_000, b * 1_000, t))
                .collect(),
        )
    }

    fn rig_with(mut cfg: Config, primary: ScriptedEngine, second: ScriptedEngine) -> Rig {
        let root = std::env::temp_dir().join(format!("jesse-speech-http-{}", random_hex()));
        // A test that sets the engine choice keeps it; everything else is the default.
        let (engine, second_engine) = (cfg.speech.engine.clone(), cfg.speech.second_engine.clone());
        cfg.speech = SpeechConfig::at(&root);
        cfg.speech.engine = engine;
        cfg.speech.second_engine = second_engine;
        let p = entry(
            "primary-model",
            SpeechTier::Accurate,
            10,
            b"primary weights",
        );
        let s = entry("second-model", SpeechTier::Fast, 10, b"second weights");
        let fetcher = Arc::new(FakeFetcher::serving(&[
            (&p, b"primary weights"),
            (&s, b"second weights"),
        ]));
        let loader = Arc::new(ScriptedLoader::with(vec![primary, second]));
        let mut st = AppState::new(cfg);
        st.speech = Arc::new(SpeechService::with_parts(
            st.cfg.speech.clone(),
            vec![p, s],
            fetcher.clone(),
            loader,
            // No system decoder: a 16 kHz WAV is read directly, so the pipeline runs anywhere.
            Arc::new(SystemDecoder::with_tool(root.join("no-afconvert"))),
        ));
        Rig { st, root, fetcher }
    }

    fn rig(primary: ScriptedEngine, second: ScriptedEngine) -> Rig {
        rig_with(test_config(), primary, second)
    }

    /// A 16 kHz WAV of a quiet-ish tone: enough samples to exercise the whole pipeline.
    fn recording(seconds: f32) -> Vec<u8> {
        let n = (16_000.0 * seconds) as usize;
        let samples: Vec<f32> = (0..n)
            .map(|i| 0.3 * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / 16_000.0).sin())
            .collect();
        encode_wav16(&samples, 16_000)
    }

    fn upload(body: Vec<u8>, content_type: Option<&str>, query: &str) -> Request<Body> {
        let mut b = Request::post(format!("/jesse/transcriptions{query}"))
            .header("authorization", "Bearer test-token");
        if let Some(ct) = content_type {
            b = b.header("content-type", ct);
        }
        b.body(Body::from(body)).unwrap()
    }

    fn get(path: &str) -> Request<Body> {
        Request::get(path)
            .header("authorization", "Bearer test-token")
            .body(Body::empty())
            .unwrap()
    }

    async fn body_json(resp: Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }

    async fn settle(app: &Router, id: &str) -> Value {
        for _ in 0..1_000 {
            let v = body_json(
                app.clone()
                    .oneshot(get(&format!("/jesse/transcriptions/{id}")))
                    .await
                    .unwrap(),
            )
            .await;
            if v["state"] != "running" {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("run {id} never finished");
    }

    async fn start(app: &Router, body: Vec<u8>, query: &str) -> String {
        let resp = app
            .clone()
            .oneshot(upload(body, Some("audio/wav"), query))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let v = body_json(resp).await;
        v["id"].as_str().expect("an id").to_string()
    }

    fn intake_is_empty(root: &Path) -> bool {
        std::fs::read_dir(root.join("speech-intake"))
            .map(|d| d.count() == 0)
            .unwrap_or(true)
    }

    #[tokio::test]
    async fn a_recording_is_transcribed_on_the_studio_and_its_audio_is_gone_afterwards() {
        let r = rig(
            engine(
                "primary-model",
                &[
                    (0, 6, "The collection will be picked up"),
                    (6, 11, "on Thursday the 14th."),
                ],
            ),
            engine(
                "second-model",
                &[(
                    0,
                    11,
                    "The collection will be picked up on Thursday the 15th.",
                )],
            ),
        );
        let app = app(r.st.clone());
        let id = start(&app, recording(2.0), "?language=it-IT").await;
        let v = settle(&app, &id).await;
        assert_eq!(v["state"], "done", "{v}");
        assert_eq!(
            v["transcript"],
            "The collection will be picked up on Thursday the 14th."
        );
        assert_eq!(v["language"], "it");
        assert_eq!(v["disagreements"][0]["primary"], "14th.");
        assert_eq!(v["disagreements"][0]["alternative"], "15th.");
        assert_eq!(v["engines"][0]["role"], "primary");
        assert_eq!(v["engines"][1]["role"], "second");
        assert!(v["conditioning"]["applied"].is_boolean());
        assert_eq!(v["duration_secs"], 2.0);
        assert!(
            intake_is_empty(&r.root),
            "the audio is deleted when the run ends"
        );
        let mut fetched = r.fetcher.fetched.lock_ok().clone();
        fetched.sort();
        assert_eq!(
            fetched,
            vec!["primary-model", "second-model"],
            "installed on first need"
        );
    }

    #[tokio::test]
    async fn refused_uploads_leave_nothing_behind() {
        let mut cfg = test_config();
        cfg.rate_per_min = 1_000;
        let r = rig_with(
            cfg,
            engine("primary-model", &[]),
            engine("second-model", &[]),
        );
        let app = app(r.st.clone());
        let m4a = b"\x00\x00\x00\x1cftypM4A \x00\x00\x00\x00M4A mp42isom".to_vec();
        let png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        for (body, ct, want) in [
            (m4a.clone(), Some("audio/wav"), StatusCode::BAD_REQUEST),
            (png, Some("image/png"), StatusCode::BAD_REQUEST),
            (m4a, None, StatusCode::BAD_REQUEST),
            (Vec::new(), Some("audio/wav"), StatusCode::BAD_REQUEST),
        ] {
            let resp = app.clone().oneshot(upload(body, ct, "")).await.unwrap();
            assert_eq!(resp.status(), want);
        }
        let resp = app
            .clone()
            .oneshot(upload(
                recording(0.1),
                Some("audio/wav"),
                "?conditioning=maybe",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(
            intake_is_empty(&r.root),
            "a refused upload leaves no bytes on disk"
        );
    }

    #[tokio::test]
    async fn the_audio_cap_is_enforced_while_streaming() {
        let r = rig(engine("primary-model", &[]), engine("second-model", &[]));
        let mut speech = r.st.speech.config.clone();
        speech.max_audio_bytes = 1_000;
        let mut st = r.st.clone();
        st.speech = Arc::new(SpeechService::with_parts(
            speech,
            Vec::new(),
            Arc::new(FakeFetcher::default()),
            Arc::new(ScriptedLoader::default()),
            Arc::new(SystemDecoder::default()),
        ));
        let resp = app(st)
            .oneshot(upload(recording(1.0), Some("audio/wav"), ""))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(intake_is_empty(&r.root));
    }

    /// The router's body limit is sized for base64 photos. A recording far past it is still
    /// taken, because the upload route carries the audio cap instead.
    #[tokio::test]
    async fn a_recording_bigger_than_any_turn_body_is_accepted() {
        let mut cfg = test_config();
        cfg.max_attachments_total_bytes = 64 * 1024;
        let limit = attachment_body_limit(&cfg);
        let r = rig_with(
            cfg,
            engine("primary-model", &[(0, 1, "hello")]),
            engine("second-model", &[(0, 1, "hello")]),
        );
        let big = recording(40.0);
        assert!(big.len() > limit * 3, "{} vs {limit}", big.len());
        let app = app(r.st.clone());
        let id = start(&app, big, "?conditioning=off&second_reading=off").await;
        assert_eq!(settle(&app, &id).await["state"], "done");
    }

    #[tokio::test]
    async fn the_door_is_shut_without_a_token_and_when_speech_is_off() {
        let r = rig(engine("primary-model", &[]), engine("second-model", &[]));
        let unauthenticated = Request::post("/jesse/transcriptions")
            .header("content-type", "audio/wav")
            .body(Body::from(recording(0.1)))
            .unwrap();
        let resp = app(r.st.clone()).oneshot(unauthenticated).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let mut st = r.st.clone();
        st.speech = Arc::new(SpeechService::from_config(SpeechConfig::disabled()));
        let resp = app(st)
            .oneshot(upload(recording(0.1), Some("audio/wav"), ""))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(body_text(resp).await.contains("JESSE_SPEECH"));
    }

    async fn body_text(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[tokio::test]
    async fn a_cancelled_run_deletes_its_audio() {
        let mut holding = engine("primary-model", &[(0, 1, "never")]);
        holding.hold_until_cancelled = true;
        let r = rig(holding, engine("second-model", &[]));
        let app = app(r.st.clone());
        let id = start(&app, recording(0.5), "?conditioning=off").await;
        for _ in 0..500 {
            let v = body_json(
                app.clone()
                    .oneshot(get(&format!("/jesse/transcriptions/{id}")))
                    .await
                    .unwrap(),
            )
            .await;
            if v["phase"] == "transcribing" {
                assert_eq!(
                    v["engine"], "Scripted primary-model",
                    "the phase names the engine"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            !intake_is_empty(&r.root),
            "the audio is in custody while it is read"
        );
        let resp = app
            .clone()
            .oneshot(
                Request::post(format!("/jesse/transcriptions/{id}/cancel"))
                    .header("authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = settle(&app, &id).await;
        assert_eq!(v["state"], "cancelled");
        assert!(
            intake_is_empty(&r.root),
            "a cancel deletes the audio like any other ending"
        );
    }

    #[tokio::test]
    async fn every_failure_ends_with_its_kind_and_no_audio() {
        // No decoder for a 44.1 kHz file on a host with no system decoder.
        let r = rig(
            engine("primary-model", &[(0, 1, "x")]),
            engine("second-model", &[]),
        );
        let app1 = app(r.st.clone());
        let id = start(&app1, encode_wav16(&[0.1; 4_410], 44_100), "").await;
        let v = settle(&app1, &id).await;
        assert_eq!(v["state"], "failed");
        assert_eq!(v["error"]["kind"], "no_decoder");
        assert!(intake_is_empty(&r.root));

        // Music the engine decorated is no speech at all.
        let r = rig(
            engine("primary-model", &[(0, 8, "[Music]"), (8, 9, "♪ ♪")]),
            engine("second-model", &[]),
        );
        let app2 = app(r.st.clone());
        let id = start(&app2, recording(0.5), "").await;
        let v = settle(&app2, &id).await;
        assert_eq!(v["error"]["kind"], "no_speech");
        assert!(intake_is_empty(&r.root));

        // An engine that does not know the language says so.
        let mut unknown = engine("primary-model", &[]);
        unknown.fail = Some(EngineError::UnknownLanguage("xx".into()));
        let r = rig(unknown, engine("second-model", &[]));
        let app3 = app(r.st.clone());
        let id = start(&app3, recording(0.5), "?language=xx").await;
        assert_eq!(
            settle(&app3, &id).await["error"]["kind"],
            "unknown_language"
        );
    }

    #[tokio::test]
    async fn a_failed_second_reading_still_delivers_the_first_and_says_so() {
        let mut broken = engine("second-model", &[]);
        broken.fail = Some(EngineError::Failed("out of memory".into()));
        let r = rig(
            engine("primary-model", &[(0, 2, "Buonasera a tutti.")]),
            broken,
        );
        let app = app(r.st.clone());
        let id = start(&app, recording(0.5), "").await;
        let v = settle(&app, &id).await;
        assert_eq!(v["state"], "done");
        assert_eq!(v["transcript"], "Buonasera a tutti.");
        assert_eq!(v["engines"].as_array().unwrap().len(), 1);
        let notes = v["notes"].to_string();
        assert!(notes.contains("second reading failed"), "{notes}");
        assert!(notes.contains("cross-checked"), "{notes}");
    }

    #[tokio::test]
    async fn unknown_runs_are_404_and_the_overview_names_the_models() {
        let r = rig(engine("primary-model", &[]), engine("second-model", &[]));
        let app = app(r.st.clone());
        let resp = app
            .clone()
            .oneshot(get("/jesse/transcriptions/tr-nope"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(app.oneshot(get("/jesse/speech")).await.unwrap()).await;
        assert_eq!(v["available"], true);
        assert_eq!(v["tier"], "accurate");
        let roles: Vec<&str> = v["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["role"].as_str())
            .collect();
        assert_eq!(roles, vec!["primary", "second"]);
    }

    // ---- The phone is not always there ------------------------------------------------

    /// What the phone's background, lock and termination cases all come down to on this
    /// side: nobody is polling. The run must not care, and its result must still be there
    /// when the phone comes back, for a day.
    #[tokio::test]
    async fn a_run_nobody_polls_still_finishes_and_its_result_is_kept_for_a_day() {
        let r = rig(
            engine(
                "primary-model",
                &[(0, 2, "Kept for when the phone returns.")],
            ),
            engine(
                "second-model",
                &[(0, 2, "Kept for when the phone returns.")],
            ),
        );
        let app = app(r.st.clone());
        let id = start(&app, recording(0.5), "?conditioning=off").await;
        // Not a single GET while it runs: wait on the overview's running count instead.
        for _ in 0..1_000 {
            if r.st.speech.overview()["running"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let hour = 3_600_000;
        r.st.speech.backdate_finish(&id, 23 * hour);
        let v = body_json(
            app.clone()
                .oneshot(get(&format!("/jesse/transcriptions/{id}")))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            v["state"], "done",
            "23 hours on, the result is still there: {v}"
        );
        assert_eq!(v["transcript"], "Kept for when the phone returns.");
        r.st.speech.backdate_finish(&id, hour + 1_000);
        let resp = app
            .clone()
            .oneshot(get(&format!("/jesse/transcriptions/{id}")))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "after a day it is gone"
        );
        assert!(body_text(resp).await.contains("kept for a day"));
    }

    fn pushing_rig(primary: ScriptedEngine) -> (Rig, crate::apns::tests::MockApns) {
        let r = rig(primary, engine("second-model", &[(0, 2, "whatever")]));
        let mock = crate::apns::tests::MockApns::default();
        let mut st = r.st.clone();
        st.apns = Some(crate::apns::tests::test_apns(Arc::new(mock.clone())));
        st.devices.set("phonetoken0123".to_string());
        (
            Rig {
                st,
                root: r.root.clone(),
                fetcher: r.fetcher.clone(),
            },
            mock,
        )
    }

    async fn pushes(mock: &crate::apns::tests::MockApns, want: usize) -> Vec<Value> {
        for _ in 0..500 {
            if mock.calls.lock_ok().len() >= want {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        mock.calls
            .lock_ok()
            .iter()
            .map(|c| serde_json::from_slice(&c.payload).unwrap())
            .collect()
    }

    async fn first_status(app: &Router, query: &str) -> Value {
        let resp = app
            .clone()
            .oneshot(upload(recording(0.5), Some("audio/wav"), query))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        body_json(resp).await
    }

    /// The push that lets a suspended or killed phone finish the job: ids and the outcome,
    /// to the registered device, and not one word of what was said.
    #[tokio::test]
    async fn a_finished_run_the_app_asked_about_pushes_ids_only() {
        let (r, mock) = pushing_rig(engine(
            "primary-model",
            &[(0, 2, "The plumber comes Thursday at nine.")],
        ));
        let app = app(r.st.clone());
        let conversation = "6F9619FF-8B86-D011-B42D-00C04FC964FF";
        let v = first_status(
            &app,
            &format!("?conditioning=off&notify=1&conversation_id={conversation}"),
        )
        .await;
        assert_eq!(v["notify"], true, "the answer says a push is coming");
        let id = v["id"].as_str().unwrap().to_string();
        assert_eq!(settle(&app, &id).await["state"], "done");
        let sent = pushes(&mock, 1).await;
        assert_eq!(sent.len(), 1, "exactly one push per run");
        assert!(mock.calls.lock_ok()[0].path.ends_with("/phonetoken0123"));
        let p = &sent[0];
        assert_eq!(p["transcription_id"], id.as_str());
        assert_eq!(p["conversation_id"], conversation);
        assert_eq!(p["outcome"], "done");
        assert_eq!(p["aps"]["content-available"], 1);
        let keys: Vec<&String> = p.as_object().unwrap().keys().collect();
        let mut keys: Vec<&str> = keys.iter().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["aps", "conversation_id", "outcome", "transcription_id"],
            "ids and the outcome, nothing else"
        );
        let wire = String::from_utf8(mock.calls.lock_ok()[0].payload.clone()).unwrap();
        for word in ["plumber", "Thursday", "nine"] {
            assert!(!wire.contains(word), "no transcript text in a push: {wire}");
        }
    }

    #[tokio::test]
    async fn a_failed_run_pushes_its_failure_and_a_cancelled_one_pushes_nothing() {
        let (r, mock) = pushing_rig(engine("primary-model", &[]));
        let app = app(r.st.clone());
        let v = first_status(&app, "?conditioning=off&notify=1&conversation_id=abc").await;
        let id = v["id"].as_str().unwrap().to_string();
        assert_eq!(settle(&app, &id).await["state"], "failed");
        let sent = pushes(&mock, 1).await;
        assert_eq!(sent[0]["outcome"], "failed");
        assert_eq!(sent[0]["transcription_id"], id.as_str());

        let mut holding = engine("primary-model", &[(0, 1, "never")]);
        holding.hold_until_cancelled = true;
        let (r, mock) = pushing_rig(holding);
        let app = crate::handlers::app(r.st.clone());
        let v = first_status(&app, "?conditioning=off&notify=1").await;
        let id = v["id"].as_str().unwrap().to_string();
        r.st.speech.cancel(&id);
        assert_eq!(settle(&app, &id).await["state"], "cancelled");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            mock.calls.lock_ok().is_empty(),
            "the owner cancelled; nothing to tell"
        );
    }

    /// An app that does not ask, or a bridge that cannot push, changes nothing: no push, and
    /// the answer says so, so the app knows to tell the owner itself.
    #[tokio::test]
    async fn no_push_unless_asked_for_and_possible() {
        let (r, mock) = pushing_rig(engine("primary-model", &[(0, 2, "hello")]));
        let app = app(r.st.clone());
        let v = first_status(&app, "?conditioning=off").await;
        assert_eq!(v["notify"], false);
        settle(&app, v["id"].as_str().unwrap()).await;

        r.st.devices.clear();
        let v = first_status(&app, "?conditioning=off&notify=1").await;
        assert_eq!(
            v["notify"], false,
            "no device registered, so no push is coming"
        );
        settle(&app, v["id"].as_str().unwrap()).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(mock.calls.lock_ok().is_empty());
    }

    #[test]
    fn only_a_plain_id_is_echoed_into_a_push() {
        assert_eq!(
            echoable_conversation_id(Some(" 6F9619FF-8B86-D011-B42D-00C04FC964FF ")).as_deref(),
            Some("6F9619FF-8B86-D011-B42D-00C04FC964FF")
        );
        assert_eq!(echoable_conversation_id(Some("a\"b")), None);
        assert_eq!(echoable_conversation_id(Some("")), None);
        assert_eq!(echoable_conversation_id(Some(&"a".repeat(65))), None);
        assert_eq!(echoable_conversation_id(None), None);
    }
    // ---- THE EGRESS BAN ---------------------------------------------------------------

    /// A server that stands in for every hosted surface and counts every connection made to
    /// it. It answers nothing useful; the only thing that matters is whether anyone called.
    async fn hosted_backend() -> (String, Arc<AtomicU64>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicU64::new(0));
        let c = calls.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                c.fetch_add(1, Ordering::SeqCst);
                drop(sock);
            }
        });
        (url, calls)
    }

    fn backend_model(id: &str, url: &str) -> RegistryModel {
        RegistryModel {
            family: None,
            effort: None,
            login_model: None,
            version: None,
            aliases: Vec::new(),
            codex: Default::default(),
            id: id.to_string(),
            label: id.to_string(),
            kind: ModelKind::Hosted,
            wire: Wire::default_for_kind(ModelKind::Hosted),
            backend: Some((url.to_string(), "tok".to_string(), format!("{id}-v1"))),
            subagent_model: None,
            configured: true,
            level: Capability::Read,
            harness: CLAUDE_CODE_ID.to_string(),
            auth_scheme: None,
            quirks: DirectQuirks::default(),
            thinking: None,
            price: PriceDeck::ZERO,
            health: HealthConfig::default(),
            vision: Vec::new(),
            vision_complementary: false,
            transcription: None,
        }
    }

    const CANARY: &[u8] = b"JESSE-AUDIO-EGRESS-CANARY";

    /// THE AUDIO EGRESS BAN, on the wire.
    ///
    /// The active model AND the vision helper paired with it both point at a server that
    /// counts connections — every hosted surface a turn can reach from this process. A
    /// recording carrying a canary is then pushed through BOTH doors:
    ///
    /// 1. the transcription door, where it must be transcribed by the local engines and
    ///    nowhere else;
    /// 2. the turn door, as an attachment to the hosted model — the route by which audio
    ///    would reach a vision helper or a hosted child — where it must be refused before a
    ///    single byte is sent.
    ///
    /// The server must see NO connection at all. This fails loudly if a later change routes
    /// audio through the assistant, a vision helper, or any registered model — the property
    /// the retired `AudioIsNeverAnAttachmentTests` guarded, moved from "no audio on any wire"
    /// to "no audio on any wire that leaves the Studio".
    ///
    /// Since Bridge 0.159.0 a hosted SPEECH engine exists, and this is the half of the rule
    /// that says it is used only when chosen: an armed engine that declares a transcription
    /// capability points at the same counting server, and with no engine selected (the
    /// default) it too must see nothing.
    #[tokio::test]
    async fn recorded_audio_never_reaches_a_hosted_backend() {
        let (url, calls) = hosted_backend().await;
        let mut cfg = test_config();
        let mut hosted = backend_model("hosted", &url);
        hosted.vision = vec![VisionPartner {
            id: "helper".to_string(),
            role: VisionRole::Any,
        }];
        let mut models = cfg.model_registry.models.clone();
        models.push(hosted);
        models.push(backend_model("helper", &url));
        models.push(speech_model("speech", &url));
        cfg.model_registry = ModelRegistry { models };
        let r = rig_with(
            cfg,
            engine("primary-model", &[(0, 2, "Pickup is on Thursday.")]),
            engine("second-model", &[(0, 2, "Pickup is on Thursday.")]),
        );
        r.st.models.set_active("hosted");
        assert!(
            !vision::resolve_partners(&r.st.cfg, &r.st.resolve_active_model().vision).is_empty(),
            "the rig must really route a turn's attachments to the recording helper"
        );
        let app = app(r.st.clone());

        let mut bytes = recording(1.0);
        for _ in 0..8 {
            bytes.extend_from_slice(CANARY);
        }
        // 1. The transcription door.
        let id = start(&app, bytes.clone(), "?conditioning=on").await;
        let v = settle(&app, &id).await;
        assert_eq!(v["state"], "done", "{v}");
        assert!(
            !v.to_string().contains("CANARY"),
            "the status never carries audio"
        );
        assert!(!v.to_string().contains(&base64_encode(CANARY)[..16]));

        // 2. The turn door: the same recording as an attachment to the hosted model.
        let turn = json!({
            "mode": "ask",
            "text": "What is in this recording?",
            "conversation_id": uuid::Uuid::new_v4().to_string(),
            "request_id": "egress-test",
            "attachments": [{
                "filename": "memo.wav",
                "mime": "audio/wav",
                "data_base64": base64_encode(&bytes),
            }],
        });
        let resp = app
            .clone()
            .oneshot(
                Request::post("/jesse")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(turn.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            StatusCode::ACCEPTED,
            "audio must not start a turn"
        );
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(resp).await.contains("unsupported"));

        // Anything that was going to reach the network has had time to.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a hosted backend was contacted while recorded audio was in the bridge"
        );
        assert!(intake_is_empty(&r.root));
    }

    /// The same boundary one layer down: the turn attachment gate refuses every audio
    /// container the transcription door accepts, whatever MIME it is declared as.
    #[test]
    fn the_turn_attachment_gate_refuses_every_recording_format() {
        let cfg = test_config();
        let m4a = b"\x00\x00\x00\x1cftypM4A \x00\x00\x00\x00M4A mp42isom".to_vec();
        for (bytes, mime) in [
            (m4a.clone(), "audio/mp4"),
            (m4a, "image/heic"),
            (recording(0.1), "audio/wav"),
            (b"ID3\x04\x00\x00\x00\x00\x00".to_vec(), "audio/mpeg"),
        ] {
            let att = Attachment {
                filename: "memo".into(),
                mime: mime.into(),
                data_base64: base64_encode(&bytes),
            };
            let err = validate_and_decode_attachments(&cfg, &[att]).unwrap_err();
            assert_eq!(err.0, StatusCode::BAD_REQUEST, "{mime}");
        }
    }

    // ---- The hosted speech engine: used only when chosen, and only for transcription ----

    /// A registry entry whose provider takes audio on the OpenAI transcription shape.
    fn speech_model(id: &str, url: &str) -> RegistryModel {
        let mut m = backend_model(id, url);
        m.transcription = Some(crate::speech::hosted::TranscriptionCapability {
            wire: crate::speech::hosted::TranscriptionWire::AudioTranscriptions,
            model: "fake-whisper".to_string(),
            endpoint: None,
            max_chunk_bytes: 25 * 1024 * 1024,
            max_chunk_secs: 600,
            timestamps: true,
        });
        m
    }

    /// PCM whose 16-bit samples spell a canary. Each sample's high byte is 0x01, so it stays
    /// under half scale and survives the decode to float and the re-encode to 16 bits exactly:
    /// if the audio reaches a server, these bytes are in what it received.
    const PCM_CANARY: &[u8] = b"J\x01E\x01S\x01S\x01E\x01-\x01C\x01A\x01N\x01A\x01R\x01Y\x01";

    fn canary_recording() -> Vec<u8> {
        let mut samples: Vec<f32> = (0..16_000)
            .map(|i| 0.3 * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / 16_000.0).sin())
            .collect();
        for _ in 0..8 {
            for pair in PCM_CANARY.chunks(2) {
                samples.push(i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32_768.0);
            }
        }
        encode_wav16(&samples, 16_000)
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    async fn send_turn(app: &Router, bytes: &[u8]) -> Response {
        let turn = json!({
            "mode": "ask",
            "text": "What is in this recording?",
            "conversation_id": uuid::Uuid::new_v4().to_string(),
            "request_id": "egress-test",
            "attachments": [{
                "filename": "memo.wav",
                "mime": "audio/wav",
                "data_base64": base64_encode(bytes),
            }],
        });
        app.clone()
            .oneshot(
                Request::post("/jesse")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(turn.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn whisper_answer() -> (u16, String) {
        (
            200,
            json!({"text": "Pickup is on Thursday.", "segments": [
                {"start": 0.0, "end": 1.0, "text": "Pickup is on Thursday."}
            ]})
            .to_string(),
        )
    }

    /// THE HOSTED HALF OF THE EGRESS RULE, on the wire. With a hosted engine SELECTED for
    /// the run, the recording's canary reaches that engine's transcription endpoint and
    /// nothing else: the active model and its vision helper (a counting server) see no
    /// connection, the speech provider sees only `POST /v1/audio/transcriptions`, and the
    /// same audio as a turn attachment is still refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_selected_hosted_engine_is_the_only_place_the_audio_goes() {
        let (turn_url, turn_calls) = hosted_backend().await;
        let (speech_url, provider) =
            crate::speech::hosted::fakes::provider(|_, _| whisper_answer()).await;
        let mut cfg = test_config();
        let mut active = backend_model("hosted", &turn_url);
        active.vision = vec![VisionPartner {
            id: "helper".to_string(),
            role: VisionRole::Any,
        }];
        let mut models = cfg.model_registry.models.clone();
        models.push(active);
        models.push(backend_model("helper", &turn_url));
        models.push(speech_model("speech", &speech_url));
        cfg.model_registry = ModelRegistry { models };
        let r = rig_with(
            cfg,
            engine("primary-model", &[(0, 2, "never read locally")]),
            engine("second-model", &[]),
        );
        r.st.models.set_active("hosted");
        let app = app(r.st.clone());
        let bytes = canary_recording();

        let id = start(
            &app,
            bytes.clone(),
            "?engine=hosted:speech&conditioning=off",
        )
        .await;
        let v = settle(&app, &id).await;
        assert_eq!(v["state"], "done", "{v}");
        assert_eq!(v["transcript"], "Pickup is on Thursday.");
        assert_eq!(v["hosted"], true);
        assert_eq!(v["engine_choice"], "hosted:speech");
        assert_eq!(
            v["engines"].as_array().unwrap().len(),
            1,
            "no local second reading"
        );
        assert_eq!(v["engines"][0]["id"], "hosted:speech");
        assert_eq!(
            v["engines"][0]["host"],
            speech_url
                .trim_start_matches("http://")
                .trim_end_matches("/v1")
        );
        assert!(
            !v.to_string().contains("CANARY"),
            "the status never carries audio"
        );

        let resp = send_turn(&app, &bytes).await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "audio still starts no turn"
        );

        tokio::time::sleep(Duration::from_millis(300)).await;
        let requests = provider.requests.lock_ok().clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "/v1/audio/transcriptions");
        assert!(
            contains(&requests[0].1, PCM_CANARY),
            "the selected engine received the recording"
        );
        assert_eq!(
            turn_calls.load(Ordering::SeqCst),
            0,
            "the active model or its vision helper was contacted with recorded audio in the bridge"
        );
        assert!(intake_is_empty(&r.root), "chunks and upload are gone");
    }

    /// `local,hosted:<id>`: the local engine reads first; only when it FAILS does the hosted
    /// one, and the result says so. When the local reading succeeds, nothing is sent.
    #[tokio::test(flavor = "multi_thread")]
    async fn local_then_hosted_sends_audio_only_when_the_local_reading_fails() {
        let (speech_url, provider) =
            crate::speech::hosted::fakes::provider(|_, _| whisper_answer()).await;
        let mut cfg = test_config();
        let mut models = cfg.model_registry.models.clone();
        models.push(speech_model("speech", &speech_url));
        cfg.model_registry = ModelRegistry { models };
        cfg.speech.engine = EngineChoice::LocalThenHosted("speech".to_string());

        let r = rig_with(
            cfg.clone(),
            engine("primary-model", &[(0, 2, "Read on the Studio.")]),
            engine("second-model", &[(0, 2, "Read on the Studio.")]),
        );
        let app1 = app(r.st.clone());
        let id = start(&app1, recording(0.5), "?conditioning=off").await;
        let v = settle(&app1, &id).await;
        assert_eq!(v["transcript"], "Read on the Studio.");
        assert_eq!(v["hosted"], false);
        assert!(
            provider.requests.lock_ok().is_empty(),
            "a local success sends nothing"
        );

        let mut broken = engine("primary-model", &[]);
        broken.fail = Some(EngineError::Failed("the model looped".into()));
        let r = rig_with(cfg, broken, engine("second-model", &[(0, 2, "second")]));
        let app2 = app(r.st.clone());
        let id = start(&app2, recording(0.5), "?conditioning=off").await;
        let v = settle(&app2, &id).await;
        assert_eq!(v["state"], "done", "{v}");
        assert_eq!(v["transcript"], "Pickup is on Thursday.");
        assert_eq!(v["engines"][0]["id"], "hosted:speech");
        assert_eq!(v["engines"][0]["hosted"], true);
        assert!(
            v["notes"].to_string().contains("local engine failed"),
            "{v}"
        );
        assert_eq!(provider.requests.lock_ok().len(), 1);
        assert!(intake_is_empty(&r.root));
    }

    /// A hosted engine as the SECOND reading: the reconciler compares a local and a hosted
    /// reading exactly as it compares two local ones.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_hosted_second_reading_is_reconciled_with_the_local_one() {
        let (speech_url, _provider) = crate::speech::hosted::fakes::provider(|_, _| {
            (
                200,
                json!({"segments": [{"start": 0.0, "end": 1.0, "text": "Pickup is on Friday."}]})
                    .to_string(),
            )
        })
        .await;
        let mut cfg = test_config();
        let mut models = cfg.model_registry.models.clone();
        models.push(speech_model("speech", &speech_url));
        cfg.model_registry = ModelRegistry { models };
        let r = rig_with(
            cfg,
            engine("primary-model", &[(0, 1, "Pickup is on Thursday.")]),
            engine("second-model", &[]),
        );
        let app = app(r.st.clone());
        let id = start(
            &app,
            recording(1.0),
            "?conditioning=off&second_engine=hosted:speech",
        )
        .await;
        let v = settle(&app, &id).await;
        assert_eq!(v["state"], "done", "{v}");
        assert_eq!(v["engines"][0]["hosted"], false);
        assert_eq!(v["engines"][1]["id"], "hosted:speech");
        assert_eq!(v["disagreements"][0]["primary"], "Thursday.");
        assert_eq!(v["disagreements"][0]["alternative"], "Friday.");
    }

    /// An engine this bridge cannot use is refused by name before any audio is read; the
    /// overview lists the Studio and every ARMED hosted engine, and never a token.
    #[tokio::test]
    async fn unknown_engines_are_refused_and_the_overview_lists_the_armed_ones() {
        let mut cfg = test_config();
        let mut models = cfg.model_registry.models.clone();
        models.push(speech_model("speech", "http://127.0.0.1:9/v1"));
        let mut unarmed = speech_model("unarmed", "http://127.0.0.1:9/v1");
        unarmed.configured = false;
        models.push(unarmed);
        models.push(backend_model("chat-only", "http://127.0.0.1:9/v1"));
        cfg.model_registry = ModelRegistry { models };
        let r = rig_with(
            cfg,
            engine("primary-model", &[]),
            engine("second-model", &[]),
        );
        let app = app(r.st.clone());
        for (query, word) in [
            (
                "?engine=hosted:unarmed",
                "not an armed transcription engine",
            ),
            (
                "?engine=hosted:chat-only",
                "not an armed transcription engine",
            ),
            ("?engine=cloud", "is not"),
            (
                "?second_engine=hosted:nope",
                "not an armed transcription engine",
            ),
        ] {
            let resp = app
                .clone()
                .oneshot(upload(recording(0.1), Some("audio/wav"), query))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{query}");
            assert!(body_text(resp).await.contains(word), "{query}");
        }
        assert!(intake_is_empty(&r.root));

        let v = body_json(app.clone().oneshot(get("/jesse/speech")).await.unwrap()).await;
        assert_eq!(v["default_engine"], "local");
        let ids: Vec<&str> = v["engines"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["id"].as_str())
            .collect();
        assert_eq!(ids, vec!["local", "hosted:speech"]);
        assert!(!v.to_string().contains("\"tok\""), "never a token");
    }

    /// A configured default that names an engine the bridge cannot use refuses uploads with
    /// 503 (the bridge's fault), and a run can still choose the Studio by name.
    #[tokio::test]
    async fn a_default_engine_that_cannot_be_used_is_a_503_naming_it() {
        let mut cfg = test_config();
        cfg.speech.engine = EngineChoice::Hosted("gone".to_string());
        let r = rig_with(
            cfg,
            engine("primary-model", &[(0, 1, "hello")]),
            engine("second-model", &[(0, 1, "hello")]),
        );
        let app = app(r.st.clone());
        let resp = app
            .clone()
            .oneshot(upload(recording(0.1), Some("audio/wav"), ""))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(body_text(resp).await.contains("hosted:gone"));
        let id = start(&app, recording(0.5), "?engine=local&conditioning=off").await;
        assert_eq!(settle(&app, &id).await["state"], "done");
    }
}
