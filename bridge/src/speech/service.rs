//! The transcription pipeline: one run per recording, from custody to transcript.
//!
//! # A run
//!
//! 1. **Queued** behind the one transcription slot: a large model on the GPU is not something
//!    to run two of at once, and a queue position is honest where a slow shared run is not.
//! 2. **Downloading model** — the first time only; see [`super::models`].
//! 3. **Preparing**: the upload is decoded to 16 kHz mono in its custody directory.
//! 4. **Conditioning**, when the recording looks like a room mic or the caller asked; see
//!    [`super::condition`].
//! 5. **Transcribing** with the primary engine, then the **second reading** with the other
//!    tier's engine, each reporting which engine is running and how far along it is, so a
//!    long run on a large model never reads as a hang.
//! 6. **Reconciling** the two into the transcript and its disagreement list.
//!
//! The run is handed ITS OWN PARTS — the model manager, the decoder, the slot — and nothing
//! else. That is the third of the four locks in `speech/mod.rs`: there is no application
//! state in scope from which a later change could reach a turn, a vision helper or a
//! registered model with these bytes.

use super::condition::{self, ConditioningDecision, ConditioningRequest};
use super::decode::{AudioDecoder, DecodeError, SystemDecoder};
use super::engine::{
    clean_segments, CancelFn, Cleaned, EngineError, EngineLoader, EngineRun, ProgressFn, Segment,
    SpeechEngine, WhisperLoader,
};
use super::hosted::{HostedEngine, HostedTarget, HOSTED_PREFIX};
use super::intake::{self, AudioCustody, SniffedUpload, DEFAULT_MAX_AUDIO_BYTES};
use super::models::{
    self, CatalogEntry, CheckReport, HttpFetcher, ModelFetcher, ModelManager, Readiness, SpeechTier,
};
use super::reconcile::{reconcile, transcript_text, Disagreement};
use super::wav::ENGINE_SAMPLE_RATE;
use crate::*;

/// The intake root's name under the state dir.
pub const INTAKE_DIR_NAME: &str = "speech-intake";
/// The models directory's name under the state dir, a sibling of `jobs/` and `artifacts/`.
pub const MODELS_DIR_NAME: &str = "speech-models";
/// How long a finished transcript is kept for the app to collect: a day. The phone may be
/// suspended, locked or killed while the Studio works, and it collects the result when a
/// completion push wakes it or the owner next opens the app, which can be hours later. An
/// hour (the old value) expired results the phone had every right to still come back for.
pub const DEFAULT_RESULT_TTL_SECS: u64 = 86_400;
/// The most finished runs kept at once, however recent.
pub const MAX_KEPT_RESULTS: usize = 64;

/// The feature's settings, resolved once from the environment.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechConfig {
    /// `JESSE_SPEECH`, on unless set to `off`.
    pub enabled: bool,
    pub state_dir: Option<PathBuf>,
    /// `JESSE_SPEECH_MAX_AUDIO_BYTES`.
    pub max_audio_bytes: u64,
    /// `JESSE_SPEECH_TIER`: `accurate` (the default) or `fast`.
    pub tier: SpeechTier,
    /// `JESSE_SPEECH_SECOND_READING`, on unless set to `off`.
    pub second_reading: bool,
    /// `JESSE_SPEECH_THREADS`, the CPU threads the decoder may use beside the GPU.
    pub threads: i32,
    /// `JESSE_SPEECH_RESULT_TTL_SECS`.
    pub result_ttl_secs: u64,
    /// `JESSE_SPEECH_ENGINE`: which engine reads a recording when the run does not say.
    /// `local` (the default) sends nothing anywhere.
    pub engine: EngineChoice,
    /// `JESSE_SPEECH_SECOND_ENGINE`: `local` (the default: the other local tier) or
    /// `hosted:<id>`, the engine of the second reading.
    pub second_engine: Option<String>,
}

fn env_off(name: &str) -> bool {
    env_string(name)
        .map(|v| {
            matches!(
                v.to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no"
            )
        })
        .unwrap_or(false)
}

impl SpeechConfig {
    pub fn from_env(state_dir: Option<&str>) -> Self {
        let tier = match env_string("JESSE_SPEECH_TIER") {
            None => SpeechTier::Accurate,
            Some(raw) => SpeechTier::parse(&raw).unwrap_or_else(|| {
                eprintln!(
                    "jesse-bridge: WARNING — JESSE_SPEECH_TIER={raw:?} is neither \"accurate\" \
                     nor \"fast\"; using accurate"
                );
                SpeechTier::Accurate
            }),
        };
        SpeechConfig {
            enabled: !env_off("JESSE_SPEECH"),
            state_dir: state_dir.map(PathBuf::from),
            max_audio_bytes: env_parse("JESSE_SPEECH_MAX_AUDIO_BYTES", DEFAULT_MAX_AUDIO_BYTES)
                .max(1),
            tier,
            second_reading: !env_off("JESSE_SPEECH_SECOND_READING"),
            threads: env_parse("JESSE_SPEECH_THREADS", 8i32).clamp(1, 64),
            result_ttl_secs: env_parse("JESSE_SPEECH_RESULT_TTL_SECS", DEFAULT_RESULT_TTL_SECS)
                .max(60),
            engine: match env_string("JESSE_SPEECH_ENGINE") {
                None => EngineChoice::Local,
                Some(raw) => EngineChoice::parse(&raw).unwrap_or_else(|e| {
                    eprintln!("jesse-bridge: WARNING — JESSE_SPEECH_ENGINE: {e}; using local");
                    EngineChoice::Local
                }),
            },
            second_engine: match env_string("JESSE_SPEECH_SECOND_ENGINE") {
                None => None,
                Some(raw) => parse_second_engine(&raw).unwrap_or_else(|e| {
                    eprintln!(
                        "jesse-bridge: WARNING — JESSE_SPEECH_SECOND_ENGINE: {e}; using local"
                    );
                    None
                }),
            },
        }
    }

    /// Off, with no state dir: the test fixture's value, and what a deploy that sets
    /// `JESSE_SPEECH=off` resolves to.
    pub fn disabled() -> Self {
        SpeechConfig {
            enabled: false,
            state_dir: None,
            max_audio_bytes: DEFAULT_MAX_AUDIO_BYTES,
            tier: SpeechTier::Accurate,
            second_reading: true,
            threads: 4,
            result_ttl_secs: DEFAULT_RESULT_TTL_SECS,
            engine: EngineChoice::Local,
            second_engine: None,
        }
    }

    /// On, rooted at `state_dir`, with every other setting at its default.
    pub fn at(state_dir: &Path) -> Self {
        SpeechConfig {
            enabled: true,
            state_dir: Some(state_dir.to_path_buf()),
            ..SpeechConfig::disabled()
        }
    }

    fn root(&self) -> Option<&Path> {
        if self.enabled {
            self.state_dir.as_deref()
        } else {
            None
        }
    }

    pub fn intake_dir(&self) -> Option<PathBuf> {
        self.root().map(|d| d.join(INTAKE_DIR_NAME))
    }

    pub fn models_dir(&self) -> Option<PathBuf> {
        self.root().map(|d| d.join(MODELS_DIR_NAME))
    }

    /// The tiers this deploy keeps installed: the primary, and the other one when a second
    /// reading is on.
    pub fn tiers(&self) -> Vec<SpeechTier> {
        let mut t = vec![self.tier];
        if self.second_reading {
            t.push(self.tier.other());
        }
        t
    }
}

/// What the caller asked for, per recording.
#[derive(Debug, Clone, PartialEq)]
pub struct JobOptions {
    /// A Whisper language code, or `None` to detect.
    pub language: Option<String>,
    pub conditioning: ConditioningRequest,
    /// `None` follows the config.
    pub second_reading: Option<bool>,
    /// Which engines read this recording. Resolved by the HTTP boundary (the only speech file
    /// that sees the registry) from the run's `engine` or the configured default; the parser
    /// leaves it local.
    pub plan: EnginePlan,
}

/// Which engine reads a recording, as configured or asked for: the engine ids the wire uses.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EngineChoice {
    /// The Studio's own whisper engines. Nothing leaves the machine.
    #[default]
    Local,
    /// `hosted:<id>`: the registry entry `<id>`'s transcription capability, and nothing local.
    Hosted(String),
    /// `local,hosted:<id>`: local first; when the local reading fails, that hosted engine.
    LocalThenHosted(String),
}

fn hosted_id(raw: &str) -> Result<String, String> {
    let id = raw
        .trim()
        .strip_prefix(HOSTED_PREFIX)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("{raw:?} is not \"local\" or \"hosted:<model id>\""))?;
    Ok(id.to_string())
}

impl EngineChoice {
    /// `local`, `hosted:<id>`, or `local,hosted:<id>`.
    pub fn parse(raw: &str) -> Result<EngineChoice, String> {
        let parts: Vec<&str> = raw
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        match parts.as_slice() {
            [] => Ok(EngineChoice::Local),
            [one] if one.eq_ignore_ascii_case("local") => Ok(EngineChoice::Local),
            [one] => Ok(EngineChoice::Hosted(hosted_id(one)?)),
            [first, then] if first.eq_ignore_ascii_case("local") => {
                Ok(EngineChoice::LocalThenHosted(hosted_id(then)?))
            }
            _ => Err(format!(
                "{raw:?} is not \"local\", \"hosted:<model id>\" or \"local,hosted:<model id>\""
            )),
        }
    }

    /// The wire form, which [`EngineChoice::parse`] reads back.
    pub fn label(&self) -> String {
        match self {
            EngineChoice::Local => "local".to_string(),
            EngineChoice::Hosted(id) => format!("{HOSTED_PREFIX}{id}"),
            EngineChoice::LocalThenHosted(id) => format!("local,{HOSTED_PREFIX}{id}"),
        }
    }

    /// The hosted id the choice names, if any.
    pub fn hosted(&self) -> Option<&str> {
        match self {
            EngineChoice::Local => None,
            EngineChoice::Hosted(id) | EngineChoice::LocalThenHosted(id) => Some(id),
        }
    }
}

/// `local` (`None`) or `hosted:<id>` (`Some(id)`): the second reading's engine.
pub fn parse_second_engine(raw: &str) -> Result<Option<String>, String> {
    let t = raw.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("local") {
        return Ok(None);
    }
    hosted_id(t).map(Some)
}

/// Where one reading comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineSource {
    /// The local engine for the configured tier (primary) or the other tier (second).
    Local,
    Hosted(HostedTarget),
}

/// A run's engines, resolved. The default is today's: local, and a local second reading.
#[derive(Debug, Clone, PartialEq)]
pub struct EnginePlan {
    pub primary: EngineSource,
    /// Read with this when a LOCAL primary fails. Only ever set from the owner's
    /// configuration or the run's own `engine`.
    pub fallback: Option<HostedTarget>,
    /// The second reading's engine. `Local` is the other local tier.
    pub second: EngineSource,
    /// What the plan was asked as, for the log and the status.
    pub choice: EngineChoice,
}

impl Default for EnginePlan {
    fn default() -> Self {
        EnginePlan {
            primary: EngineSource::Local,
            fallback: None,
            second: EngineSource::Local,
            choice: EngineChoice::Local,
        }
    }
}

impl EnginePlan {
    /// Whether any reading in this plan may send audio off the Studio.
    pub fn leaves_the_studio(&self) -> bool {
        matches!(self.primary, EngineSource::Hosted(_))
            || self.fallback.is_some()
            || matches!(self.second, EngineSource::Hosted(_))
    }
}

impl JobOptions {
    pub fn parse(
        language: Option<&str>,
        conditioning: Option<&str>,
        second_reading: Option<&str>,
    ) -> Result<JobOptions, String> {
        Ok(JobOptions {
            language: match language {
                Some(raw) => whisper_language(raw)?,
                None => None,
            },
            conditioning: ConditioningRequest::parse(conditioning)?,
            second_reading: match second_reading
                .map(|s| s.trim().to_ascii_lowercase())
                .as_deref()
            {
                None | Some("") | Some("auto") => None,
                Some("on") | Some("true") | Some("1") => Some(true),
                Some("off") | Some("false") | Some("0") => Some(false),
                Some(other) => {
                    return Err(format!(
                        "second_reading must be \"auto\", \"on\" or \"off\", got {other:?}"
                    ))
                }
            },
            plan: EnginePlan::default(),
        })
    }
}

/// A language tag (`it`, `it-IT`, `pt_BR`) as the code Whisper knows it by, or `None` for
/// `auto`. The engine refuses a code it does not know, by name; this only refuses what is not
/// a language tag at all.
pub fn whisper_language(raw: &str) -> Result<Option<String>, String> {
    let t = raw.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    let primary = t.split(['-', '_']).next().unwrap_or("");
    let well_formed = (2..=3).contains(&primary.len())
        && primary.chars().all(|c| c.is_ascii_alphabetic())
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !well_formed {
        return Err(format!(
            "language must be a language tag such as \"it\" or \"it-IT\", got {raw:?}"
        ));
    }
    let code = primary.to_ascii_lowercase();
    // The handful of tags whose ISO code Whisper spells differently.
    Ok(Some(
        match code.as_str() {
            "nb" | "nn" => "no",
            "iw" => "he",
            "in" => "id",
            "ji" => "yi",
            "fil" => "tl",
            other => other,
        }
        .to_string(),
    ))
}

// ---- A run's state ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Queued,
    DownloadingModel,
    Preparing,
    Conditioning,
    Transcribing,
    SecondReading,
    Reconciling,
    Done,
    Failed,
    Cancelled,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Queued => "queued",
            Phase::DownloadingModel => "downloading_model",
            Phase::Preparing => "preparing",
            Phase::Conditioning => "conditioning",
            Phase::Transcribing => "transcribing",
            Phase::SecondReading => "second_reading",
            Phase::Reconciling => "reconciling",
            Phase::Done => "done",
            Phase::Failed => "failed",
            Phase::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Phase::Done | Phase::Failed | Phase::Cancelled)
    }

    fn state(self) -> &'static str {
        match self {
            Phase::Done => "done",
            Phase::Failed => "failed",
            Phase::Cancelled => "cancelled",
            _ => "running",
        }
    }
}

/// Every way a run can end without a transcript, each with its own sentence. The kinds are
/// the wire vocabulary the app maps onto its own failure taxonomy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechFailure {
    Unreadable(String),
    NoDecoder(String),
    NoSpeech,
    UnknownLanguage(String),
    ModelUnavailable(String),
    EngineFailed(String),
    /// A hosted engine the owner selected refused or failed the request.
    HostedFailed(String),
}

impl SpeechFailure {
    pub fn kind(&self) -> &'static str {
        match self {
            SpeechFailure::Unreadable(_) => "unreadable_file",
            SpeechFailure::NoDecoder(_) => "no_decoder",
            SpeechFailure::NoSpeech => "no_speech",
            SpeechFailure::UnknownLanguage(_) => "unknown_language",
            SpeechFailure::ModelUnavailable(_) => "model_unavailable",
            SpeechFailure::EngineFailed(_) => "engine_failed",
            SpeechFailure::HostedFailed(_) => "hosted_failed",
        }
    }

    pub fn message(&self) -> String {
        match self {
            SpeechFailure::Unreadable(d) => {
                format!("The Studio could not read the recording: {d}.")
            }
            SpeechFailure::NoDecoder(d) => {
                format!("The Studio has no decoder for this recording: {d}.")
            }
            SpeechFailure::NoSpeech => "No speech was recognized in the recording.".to_string(),
            SpeechFailure::UnknownLanguage(l) => {
                format!("The Studio's speech engine does not know the language {l:?}.")
            }
            SpeechFailure::ModelUnavailable(d) => {
                format!("The Studio has no speech model ready: {d}.")
            }
            SpeechFailure::EngineFailed(d) => format!("The Studio's speech engine failed: {d}."),
            SpeechFailure::HostedFailed(d) => format!("The hosted speech engine failed: {d}."),
        }
    }
}

#[derive(Debug, Clone)]
struct EngineUse {
    id: String,
    label: String,
    role: &'static str,
    /// The host the audio went to, for a hosted reading. `None` is the Studio itself.
    host: Option<String>,
}

#[derive(Debug, Clone)]
struct JobStatus {
    phase: Phase,
    fraction: f64,
    engine: Option<String>,
    mime: &'static str,
    bytes: u64,
    language: Option<String>,
    duration_secs: Option<f64>,
    conditioning: Option<ConditioningDecision>,
    transcript: Option<String>,
    engines: Vec<EngineUse>,
    disagreements: Vec<Disagreement>,
    agreement: Option<f32>,
    notes: Vec<String>,
    failure: Option<SpeechFailure>,
    cancel_requested: bool,
    finished_ms: Option<u64>,
    /// The engine choice the run was started with (`local`, `hosted:<id>`, ...).
    choice: String,
}

/// One run, as the HTTP side sees it.
pub struct Job {
    id: String,
    status: Mutex<JobStatus>,
    cancel: CancellationToken,
}

impl Job {
    fn new(id: &str, upload: &SniffedUpload, opts: &JobOptions) -> Job {
        Job {
            id: id.to_string(),
            status: Mutex::new(JobStatus {
                phase: Phase::Queued,
                fraction: 0.0,
                engine: None,
                mime: upload.mime,
                bytes: upload.bytes,
                language: opts.language.clone(),
                duration_secs: None,
                conditioning: None,
                transcript: None,
                engines: Vec::new(),
                disagreements: Vec::new(),
                agreement: None,
                notes: Vec::new(),
                failure: None,
                cancel_requested: false,
                finished_ms: None,
                choice: opts.plan.choice.label(),
            }),
            cancel: CancellationToken::new(),
        }
    }

    /// Update a running job. A no-op once it has ended, so a late progress callback from an
    /// engine that is still unwinding can never un-finish a run.
    fn set(&self, f: impl FnOnce(&mut JobStatus)) {
        let mut s = self.status.lock_ok();
        if !s.phase.is_terminal() {
            f(&mut s);
        }
    }

    fn note(&self, note: String) {
        self.set(|s| s.notes.push(note));
    }

    fn enter(&self, phase: Phase, engine: Option<String>) {
        self.set(|s| {
            s.phase = phase;
            s.fraction = 0.0;
            s.engine = engine;
        });
    }

    fn progress_in(self: &Arc<Self>, phase: Phase) -> ProgressFn {
        let job = self.clone();
        Arc::new(move |f| {
            job.set(|s| {
                if s.phase == phase {
                    s.fraction = f.clamp(0.0, 1.0);
                }
            })
        })
    }

    fn finish(&self, phase: Phase, failure: Option<SpeechFailure>) {
        let mut s = self.status.lock_ok();
        if s.phase.is_terminal() {
            return;
        }
        s.phase = phase;
        s.failure = failure;
        s.engine = None;
        if phase == Phase::Done {
            s.fraction = 1.0;
        }
        s.finished_ms = Some(system_time_to_ms(SystemTime::now()));
    }

    fn finished_ms(&self) -> Option<u64> {
        self.status.lock_ok().finished_ms
    }

    fn is_running(&self) -> bool {
        !self.status.lock_ok().phase.is_terminal()
    }

    /// The wire form. Never carries audio: the only content in it is the transcript.
    pub fn to_json(&self) -> Value {
        let s = self.status.lock_ok();
        json!({
            "id": self.id,
            "state": s.phase.state(),
            "phase": s.phase.label(),
            "fraction": (s.fraction * 1000.0).round() / 1000.0,
            "engine": s.engine,
            "type": s.mime,
            "bytes": s.bytes,
            "language": s.language,
            "duration_secs": s.duration_secs,
            "conditioning": s.conditioning.map(|d| json!({
                "applied": d.applies(),
                "reason": d.reason().label(),
            })),
            "transcript": s.transcript,
            // Which engine produced what, and WHERE: a hosted reading names its host, so a
            // transcript read in the cloud is never taken for one read on the Studio.
            "engines": s.engines.iter().map(|e| json!({
                "id": e.id, "label": e.label, "role": e.role,
                "hosted": e.host.is_some(), "host": e.host,
            })).collect::<Vec<_>>(),
            "hosted": s.engines.iter().any(|e| e.host.is_some()),
            "engine_choice": s.choice,
            "disagreements": s.disagreements,
            "agreement": s.agreement,
            "notes": s.notes,
            "error": s.failure.as_ref().map(|f| json!({
                "kind": f.kind(), "message": f.message(),
            })),
            "cancel_requested": s.cancel_requested,
        })
    }
}

// ---- The service --------------------------------------------------------------------

/// Transcription on this machine: the models, the decoder, the one slot, and the runs.
pub struct SpeechService {
    pub config: SpeechConfig,
    models: Arc<ModelManager>,
    decoder: Arc<dyn AudioDecoder>,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    run_lock: Arc<Semaphore>,
}

impl SpeechService {
    /// The production service: whisper.cpp engines, weights from the known-good list, and the
    /// system decoder.
    pub fn from_config(config: SpeechConfig) -> Self {
        let loader: Arc<dyn EngineLoader> = Arc::new(WhisperLoader {
            threads: config.threads,
        });
        Self::with_parts(
            config,
            models::known_good(),
            Arc::new(HttpFetcher::default()),
            loader,
            Arc::new(SystemDecoder::default()),
        )
    }

    /// The service over any parts — how the tests run the whole pipeline with no model, no
    /// microphone and no network.
    pub fn with_parts(
        config: SpeechConfig,
        catalog: Vec<CatalogEntry>,
        fetcher: Arc<dyn ModelFetcher>,
        loader: Arc<dyn EngineLoader>,
        decoder: Arc<dyn AudioDecoder>,
    ) -> Self {
        let models = Arc::new(ModelManager::new(
            config.models_dir(),
            catalog,
            fetcher,
            loader,
        ));
        SpeechService {
            config,
            models,
            decoder,
            jobs: Mutex::new(HashMap::new()),
            run_lock: Arc::new(Semaphore::new(1)),
        }
    }

    /// Whether this bridge takes recordings at all, and if not, the sentence that says why.
    pub fn availability(&self) -> Result<(), String> {
        if !self.config.enabled {
            return Err(
                "speech transcription is turned off on this bridge (JESSE_SPEECH=off)".to_string(),
            );
        }
        if self.config.state_dir.is_none() {
            return Err(
                "this bridge has no state dir, so there is nowhere to keep recordings or \
                        speech models (set JESSE_STATE_DIR)"
                    .to_string(),
            );
        }
        Ok(())
    }

    pub fn intake_dir(&self) -> Option<PathBuf> {
        self.config.intake_dir()
    }

    /// Boot: delete every recording a killed process left behind.
    pub fn purge_abandoned(&self) -> usize {
        self.intake_dir()
            .map(|d| intake::purge_intake(&d))
            .unwrap_or(0)
    }

    /// Take custody of an accepted upload and start its run. Returns the run's first status.
    ///
    /// `on_finish`, when given, is called once as the run ends, with its id and its wire
    /// state (`done`, `failed` or `cancelled`), after the audio is deleted and the result is
    /// readable. It is how the HTTP side sends the completion push without this file ever
    /// seeing the application state.
    pub fn start(
        &self,
        custody: AudioCustody,
        upload: PathBuf,
        sniffed: SniffedUpload,
        opts: JobOptions,
        on_finish: Option<FinishHook>,
    ) -> Value {
        self.evict();
        let id = format!("tr-{}", random_hex());
        let job = Arc::new(Job::new(&id, &sniffed, &opts));
        self.jobs.lock_ok().insert(id.clone(), job.clone());
        // Metadata only. Never a filename, never a byte, never a word of transcript.
        eprintln!(
            "jesse-bridge: speech ACCEPTED id={id} type={} bytes={} language={} \
             conditioning={:?} engine={}",
            sniffed.mime,
            sniffed.bytes,
            opts.language.as_deref().unwrap_or("auto"),
            opts.conditioning,
            opts.plan.choice.label(),
        );
        // A hosted primary reads alone unless the plan names a hosted second reading: the
        // owner chose the cloud because the local engines were not serving, and a silent local
        // second reading would spend the minutes and the GPU they chose to avoid.
        let default_second = match (&opts.plan.primary, &opts.plan.second) {
            (EngineSource::Hosted(_), EngineSource::Local) => false,
            _ => self.config.second_reading,
        };
        let parts = RunParts {
            models: self.models.clone(),
            decoder: self.decoder.clone(),
            run_lock: self.run_lock.clone(),
            tier: self.config.tier,
            second_reading: opts.second_reading.unwrap_or(default_second),
        };
        let status = job.to_json();
        tokio::spawn(run(parts, job, custody, upload, opts, on_finish));
        status
    }

    pub fn status(&self, id: &str) -> Option<Value> {
        self.evict();
        self.jobs.lock_ok().get(id).map(|j| j.to_json())
    }

    /// Ask a run to stop. The run notices at its next checkpoint (between stages, or inside
    /// the engine's own abort poll) and deletes the audio the same way every other ending
    /// does; the status says `cancel_requested` until it has.
    pub fn cancel(&self, id: &str) -> Option<Value> {
        let job = self.jobs.lock_ok().get(id).cloned()?;
        job.cancel.cancel();
        job.set(|s| s.cancel_requested = true);
        Some(job.to_json())
    }

    /// The weekly check, for the scheduler.
    pub async fn check_models(&self) -> CheckReport {
        self.models.check(&self.config.tiers()).await
    }

    /// `GET /jesse/speech`.
    pub fn overview(&self) -> Value {
        let available = self.availability();
        let running = self
            .jobs
            .lock_ok()
            .values()
            .filter(|j| j.is_running())
            .count();
        let mut v = json!({
            "available": available.is_ok(),
            "reason": available.err(),
            "tier": self.config.tier.label(),
            "second_reading": self.config.second_reading,
            "default_engine": self.config.engine.label(),
            "second_engine": self
                .config
                .second_engine
                .as_deref()
                .map(|id| format!("{HOSTED_PREFIX}{id}"))
                .unwrap_or_else(|| "local".to_string()),
            "max_audio_bytes": self.config.max_audio_bytes,
            "running": running,
        });
        if let (Some(obj), Value::Object(models)) = (
            v.as_object_mut(),
            self.models
                .overview(self.config.tier, self.config.second_reading),
        ) {
            obj.extend(models);
        }
        v
    }

    /// Move a finished run's ending `by_ms` into the past, so a test can walk a result
    /// through its retention without waiting a day.
    #[cfg(test)]
    pub(crate) fn backdate_finish(&self, id: &str, by_ms: u64) {
        if let Some(job) = self.jobs.lock_ok().get(id) {
            let mut s = job.status.lock_ok();
            s.finished_ms = s.finished_ms.map(|f| f.saturating_sub(by_ms));
        }
    }

    fn evict(&self) {
        let now = system_time_to_ms(SystemTime::now());
        let ttl_ms = self.config.result_ttl_secs.saturating_mul(1000);
        let mut jobs = self.jobs.lock_ok();
        jobs.retain(|_, j| {
            j.finished_ms()
                .map(|f| now.saturating_sub(f) < ttl_ms)
                .unwrap_or(true)
        });
        let mut finished: Vec<(u64, String)> = jobs
            .iter()
            .filter_map(|(id, j)| j.finished_ms().map(|f| (f, id.clone())))
            .collect();
        if finished.len() > MAX_KEPT_RESULTS {
            finished.sort();
            for (_, id) in &finished[..finished.len() - MAX_KEPT_RESULTS] {
                jobs.remove(id);
            }
        }
    }
}

// ---- The run ------------------------------------------------------------------------

/// Everything a run may touch. See the module docs for why this is all of it.
struct RunParts {
    models: Arc<ModelManager>,
    decoder: Arc<dyn AudioDecoder>,
    run_lock: Arc<Semaphore>,
    tier: SpeechTier,
    second_reading: bool,
}

/// Told once that a run has ended: its id and its wire state. See [`SpeechService::start`].
pub type FinishHook = Box<dyn FnOnce(&str, &'static str) + Send>;

enum Stop {
    Cancelled,
    Failed(SpeechFailure),
}

async fn run(
    parts: RunParts,
    job: Arc<Job>,
    custody: AudioCustody,
    upload: PathBuf,
    opts: JobOptions,
    // Beside the parts rather than among them: a hook is `Send` but not `Sync`, and the
    // parts are borrowed across the pipeline's awaits.
    on_finish: Option<FinishHook>,
) {
    let started = Instant::now();
    let outcome = pipeline(&parts, &job, &custody, &upload, &opts).await;
    // THE AUDIO IS DELETED HERE, on every path: the upload and the decoded working copy go
    // with the custody directory before the run reports its ending. (A panic above unwinds
    // through the same drop.)
    drop(custody);
    let took = started.elapsed().as_secs();
    match outcome {
        Ok(summary) => {
            job.finish(Phase::Done, None);
            eprintln!(
                "jesse-bridge: speech DONE id={} {summary} took={took}s",
                job.id
            );
        }
        Err(Stop::Cancelled) => {
            job.finish(Phase::Cancelled, None);
            eprintln!("jesse-bridge: speech CANCELLED id={} took={took}s", job.id);
        }
        Err(Stop::Failed(f)) => {
            eprintln!(
                "jesse-bridge: speech FAILED id={} kind={} took={took}s",
                job.id,
                f.kind()
            );
            job.finish(Phase::Failed, Some(f));
        }
    }
    // After `finish`, so whoever the hook tells can already read the result.
    if let Some(hook) = on_finish {
        hook(&job.id, job.status.lock_ok().phase.state());
    }
}

async fn pipeline(
    parts: &RunParts,
    job: &Arc<Job>,
    custody: &AudioCustody,
    upload: &Path,
    opts: &JobOptions,
) -> Result<String, Stop> {
    let _slot = tokio::select! {
        biased;
        _ = job.cancel.cancelled() => return Err(Stop::Cancelled),
        permit = parts.run_lock.clone().acquire_owned() => permit.map_err(|_| {
            Stop::Failed(SpeechFailure::EngineFailed("the transcription queue closed".into()))
        })?,
    };

    // The primary engine. A local one is obtained (its model installed on first need) before
    // the audio is decoded, as it always was; a hosted one has nothing to install. A LOCAL
    // failure moves to the hosted fallback only when the plan carries one, which only the
    // owner's configuration or the run's own `engine` can put there.
    let mut fallback = opts.plan.fallback.clone();
    let mut primary: Option<(EngineUse, Arc<dyn SpeechEngine>)> = match &opts.plan.primary {
        EngineSource::Hosted(t) => Some(hosted_engine(t, "primary")),
        EngineSource::Local => match obtain_engine(parts, job, parts.tier).await {
            Ok((entry, engine)) => Some((local_use(&entry, &engine, "primary"), engine)),
            Err(Stop::Failed(f)) if fallback.is_some() => {
                note_local_failure(job, &f, fallback.as_ref());
                None
            }
            Err(e) => return Err(e),
        },
    };

    job.enter(Phase::Preparing, None);
    let decoder = parts.decoder.clone();
    let (input, work) = (upload.to_path_buf(), custody.dir().to_path_buf());
    let samples = tokio::task::spawn_blocking(move || decoder.decode(&input, &work))
        .await
        .map_err(|e| {
            Stop::Failed(SpeechFailure::EngineFailed(format!(
                "the decoder stopped ({e})"
            )))
        })?
        .map_err(|e| {
            Stop::Failed(match e {
                DecodeError::Unreadable(d) => SpeechFailure::Unreadable(d),
                DecodeError::NoDecoder(d) => SpeechFailure::NoDecoder(d),
            })
        })?;
    if job.cancel.is_cancelled() {
        return Err(Stop::Cancelled);
    }
    let duration = samples.len() as f64 / ENGINE_SAMPLE_RATE as f64;
    job.set(|s| s.duration_secs = Some((duration * 10.0).round() / 10.0));

    let decision = condition::decide(opts.conditioning, &condition::measure(&samples));
    job.set(|s| s.conditioning = Some(decision));
    let samples = if decision.applies() {
        job.enter(Phase::Conditioning, None);
        let token = job.cancel.clone();
        tokio::task::spawn_blocking(move || {
            condition::condition(&samples, ENGINE_SAMPLE_RATE, &|| token.is_cancelled())
        })
        .await
        .map_err(|e| {
            Stop::Failed(SpeechFailure::EngineFailed(format!(
                "conditioning stopped ({e})"
            )))
        })?
        .ok_or(Stop::Cancelled)?
    } else {
        samples
    };
    let samples = Arc::new(samples);

    let scratch = custody.dir();

    let mut first: Option<(Cleaned, EngineUse)> = None;
    if let Some((used, engine)) = primary.take() {
        match read_clean(job, &engine, &samples, opts, scratch, Phase::Transcribing).await {
            Ok(cleaned) => first = Some((cleaned, used)),
            Err(Stop::Failed(f)) if fallback.is_some() => {
                note_local_failure(job, &f, fallback.as_ref())
            }
            Err(e) => return Err(e),
        }
    }
    let (first, first_use) = match first {
        Some(read) => read,
        None => {
            // Only reachable when a local stage failed AND the plan carries a fallback.
            let target = fallback.take().ok_or_else(|| {
                Stop::Failed(SpeechFailure::EngineFailed(
                    "no engine read the recording".into(),
                ))
            })?;
            let (used, engine) = hosted_engine(&target, "primary");
            let cleaned =
                read_clean(job, &engine, &samples, opts, scratch, Phase::Transcribing).await?;
            (cleaned, used)
        }
    };
    let mut engines = vec![first_use];

    let mut second: Option<Cleaned> = None;
    if parts.second_reading {
        let obtained = match &opts.plan.second {
            EngineSource::Hosted(t) => Ok(hosted_engine(t, "second")),
            EngineSource::Local => {
                // The other tier beside a local primary; the configured tier beside a hosted
                // one, which used no local tier at all.
                let tier = match opts.plan.primary {
                    EngineSource::Hosted(_) => parts.tier,
                    EngineSource::Local => parts.tier.other(),
                };
                obtain_engine(parts, job, tier)
                    .await
                    .map(|(entry, engine)| (local_use(&entry, &engine, "second"), engine))
            }
        };
        match obtained {
            Ok((used, engine)) => {
                match read(job, &engine, &samples, opts, scratch, Phase::SecondReading).await {
                    Ok(segments) => {
                        engines.push(used);
                        second = Some(clean_segments(segments));
                    }
                    Err(Stop::Cancelled) => return Err(Stop::Cancelled),
                    Err(Stop::Failed(f)) => job.note(format!(
                        "The second reading failed ({}), so nothing in this transcript was \
                     cross-checked.",
                        f.message().trim_end_matches('.')
                    )),
                }
            }
            Err(Stop::Cancelled) => return Err(Stop::Cancelled),
            Err(Stop::Failed(f)) => job.note(format!(
                "There was no second reading ({}), so nothing in this transcript was \
                 cross-checked.",
                f.message().trim_end_matches('.')
            )),
        }
    }

    job.enter(Phase::Reconciling, None);
    let transcript = transcript_text(&first.segments);
    if first.non_speech_dropped > 0 {
        job.note(format!(
            "{} stretch(es) of silence or music that the engine wrote down anyway were dropped.",
            first.non_speech_dropped
        ));
    }
    if first.loops_collapsed > 0 {
        job.note(format!(
            "The engine looped {} time(s); each loop is collapsed and marked in the text.",
            first.loops_collapsed
        ));
    }
    let reconciled = second
        .as_ref()
        .map(|alt| reconcile(&first.segments, &alt.segments));
    if let Some(note) = reconciled.as_ref().and_then(|r| r.note.clone()) {
        job.note(note);
    }
    // Where the last new text begins. Far short of `audio=` means the recording's tail came
    // back as nothing, or as one collapsed loop, which `loops=` alone does not show.
    let last_text_secs = first.segments.last().map_or(0, |s| s.start_ms / 1000);
    let summary = format!(
        "audio={duration:.0}s last_text={last_text_secs}s engines={} conditioned={} \
         disagreements={} dropped={} loops={}",
        engines
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>()
            .join("+"),
        if decision.applies() {
            decision.reason().label()
        } else {
            "no"
        },
        reconciled
            .as_ref()
            .map(|r| r.disagreements.len())
            .unwrap_or(0),
        first.non_speech_dropped,
        first.loops_collapsed,
    );
    job.set(|s| {
        s.transcript = Some(transcript);
        s.engines = engines;
        if let Some(r) = reconciled {
            s.agreement = Some((r.agreement * 1000.0).round() / 1000.0);
            s.disagreements = r.disagreements;
        }
    });
    Ok(summary)
}

/// The engine for a tier, installing its model first when this is the first time it is
/// needed. The install runs on its own task: a cancelled recording must not abandon a
/// half-downloaded model the next recording will want.
async fn obtain_engine(
    parts: &RunParts,
    job: &Arc<Job>,
    tier: SpeechTier,
) -> Result<(CatalogEntry, Arc<dyn SpeechEngine>), Stop> {
    let readiness = parts
        .models
        .readiness(tier)
        .map_err(|e| Stop::Failed(SpeechFailure::ModelUnavailable(e)))?;
    let entry = match readiness {
        Readiness::Ready(e) => e,
        Readiness::NeedsInstall { target, fallback } => {
            job.enter(Phase::DownloadingModel, Some(target.label.clone()));
            let models = parts.models.clone();
            let progress = job.progress_in(Phase::DownloadingModel);
            let t = target.clone();
            let install = tokio::spawn(async move { models.install(&t, progress).await });
            let result = tokio::select! {
                biased;
                _ = job.cancel.cancelled() => return Err(Stop::Cancelled),
                r = install => r.unwrap_or_else(|e| Err(format!("the install stopped ({e})"))),
            };
            match (result, fallback) {
                (Ok(()), _) => target,
                (Err(e), Some(fb)) => {
                    job.note(format!(
                        "{} could not be installed ({e}); this reading used {} instead.",
                        target.label, fb.label
                    ));
                    fb
                }
                (Err(e), None) => {
                    return Err(Stop::Failed(SpeechFailure::ModelUnavailable(format!(
                        "{}: {e}",
                        target.label
                    ))))
                }
            }
        }
    };
    let engine = parts
        .models
        .engine(&entry)
        .await
        .map_err(|e| Stop::Failed(SpeechFailure::ModelUnavailable(e)))?;
    Ok((entry, engine))
}

/// One engine over the samples, on a blocking thread, reporting progress under `phase` and
/// polling the run's cancel.
async fn read(
    job: &Arc<Job>,
    engine: &Arc<dyn SpeechEngine>,
    samples: &Arc<Vec<f32>>,
    opts: &JobOptions,
    scratch: &Path,
    phase: Phase,
) -> Result<Vec<Segment>, Stop> {
    job.enter(phase, Some(engine.label().to_string()));
    let progress = job.progress_in(phase);
    let token = job.cancel.clone();
    let cancelled: CancelFn = Arc::new(move || token.is_cancelled());
    let hosted = engine.id().starts_with(HOSTED_PREFIX);
    let (engine, samples, language, c, scratch) = (
        engine.clone(),
        samples.clone(),
        opts.language.clone(),
        cancelled.clone(),
        scratch.to_path_buf(),
    );
    let result = tokio::task::spawn_blocking(move || {
        engine.transcribe(EngineRun {
            samples: &samples,
            language: language.as_deref(),
            progress,
            cancelled: c,
            scratch: Some(&scratch),
        })
    })
    .await;
    let failed = |e: String| {
        Stop::Failed(if hosted {
            SpeechFailure::HostedFailed(e)
        } else {
            SpeechFailure::EngineFailed(e)
        })
    };
    match result {
        Err(e) => Err(failed(format!("the engine stopped unexpectedly ({e})"))),
        Ok(Err(EngineError::Cancelled)) => Err(Stop::Cancelled),
        Ok(Err(EngineError::UnknownLanguage(l))) => {
            Err(Stop::Failed(SpeechFailure::UnknownLanguage(l)))
        }
        Ok(Err(EngineError::Failed(e))) => Err(failed(e)),
        Ok(Ok(_)) if cancelled() => Err(Stop::Cancelled),
        Ok(Ok(segments)) => Ok(segments),
    }
}

/// A reading, cleaned, where no speech at all is a failure (as it always was for the primary).
async fn read_clean(
    job: &Arc<Job>,
    engine: &Arc<dyn SpeechEngine>,
    samples: &Arc<Vec<f32>>,
    opts: &JobOptions,
    scratch: &Path,
    phase: Phase,
) -> Result<Cleaned, Stop> {
    let cleaned = clean_segments(read(job, engine, samples, opts, scratch, phase).await?);
    if cleaned.segments.is_empty() {
        return Err(Stop::Failed(SpeechFailure::NoSpeech));
    }
    Ok(cleaned)
}

fn local_use(
    entry: &CatalogEntry,
    engine: &Arc<dyn SpeechEngine>,
    role: &'static str,
) -> EngineUse {
    EngineUse {
        id: entry.id.clone(),
        label: engine.label().to_string(),
        role,
        host: None,
    }
}

fn hosted_engine(target: &HostedTarget, role: &'static str) -> (EngineUse, Arc<dyn SpeechEngine>) {
    let engine: Arc<dyn SpeechEngine> = Arc::new(HostedEngine::new(target.clone()));
    let used = EngineUse {
        id: target.engine_id(),
        label: engine.label().to_string(),
        role,
        host: Some(target.host()),
    };
    (used, engine)
}

/// Say, in the result, that the local engine failed and which hosted engine read it instead.
fn note_local_failure(job: &Job, failure: &SpeechFailure, fallback: Option<&HostedTarget>) {
    if let Some(t) = fallback {
        job.note(format!(
            "The Studio's local engine failed ({}), so this transcript was read by {} at {}, \
             the hosted fallback configured for this bridge.",
            failure.message().trim_end_matches('.'),
            t.engine_label(),
            t.host()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_tags_become_whisper_codes_and_nonsense_is_refused() {
        assert_eq!(whisper_language("it-IT"), Ok(Some("it".to_string())));
        assert_eq!(whisper_language("pt_BR"), Ok(Some("pt".to_string())));
        assert_eq!(whisper_language("EN"), Ok(Some("en".to_string())));
        assert_eq!(whisper_language("nb-NO"), Ok(Some("no".to_string())));
        assert_eq!(whisper_language("auto"), Ok(None));
        assert_eq!(whisper_language(" "), Ok(None));
        assert!(whisper_language("italiano").is_err());
        assert!(whisper_language("it;rm -rf").is_err());
    }

    #[test]
    fn options_parse_strictly() {
        let o = JobOptions::parse(Some("it"), Some("on"), Some("off")).unwrap();
        assert_eq!(o.language.as_deref(), Some("it"));
        assert_eq!(o.conditioning, ConditioningRequest::On);
        assert_eq!(o.second_reading, Some(false));
        assert!(JobOptions::parse(None, Some("sometimes"), None).is_err());
        assert!(JobOptions::parse(None, None, Some("twice")).is_err());
    }

    #[test]
    fn the_config_places_both_directories_under_the_state_dir_or_nowhere() {
        let c = SpeechConfig::at(Path::new("/var/jesse"));
        assert_eq!(
            c.intake_dir(),
            Some(PathBuf::from("/var/jesse/speech-intake"))
        );
        assert_eq!(
            c.models_dir(),
            Some(PathBuf::from("/var/jesse/speech-models"))
        );
        assert_eq!(c.tiers(), vec![SpeechTier::Accurate, SpeechTier::Fast]);
        let off = SpeechConfig {
            enabled: false,
            ..c.clone()
        };
        assert_eq!(
            off.intake_dir(),
            None,
            "off means no directory is ever touched"
        );
        let one = SpeechConfig {
            second_reading: false,
            tier: SpeechTier::Fast,
            ..c
        };
        assert_eq!(one.tiers(), vec![SpeechTier::Fast]);
    }

    #[test]
    fn every_failure_has_its_own_kind_and_sentence() {
        let all = [
            SpeechFailure::Unreadable("x".into()),
            SpeechFailure::NoDecoder("x".into()),
            SpeechFailure::NoSpeech,
            SpeechFailure::UnknownLanguage("xx".into()),
            SpeechFailure::ModelUnavailable("x".into()),
            SpeechFailure::EngineFailed("x".into()),
            SpeechFailure::HostedFailed("x".into()),
        ];
        let mut kinds: Vec<&str> = all.iter().map(|f| f.kind()).collect();
        kinds.sort();
        kinds.dedup();
        assert_eq!(kinds.len(), all.len());
        for f in &all {
            assert!(f.message().ends_with('.'), "{}", f.message());
        }
    }

    #[test]
    fn engine_choices_parse_strictly_and_default_to_local() {
        assert_eq!(EngineChoice::parse("local"), Ok(EngineChoice::Local));
        assert_eq!(EngineChoice::parse(" "), Ok(EngineChoice::Local));
        assert_eq!(
            EngineChoice::parse("hosted:gemini-flash"),
            Ok(EngineChoice::Hosted("gemini-flash".into()))
        );
        assert_eq!(
            EngineChoice::parse("local, hosted:glm"),
            Ok(EngineChoice::LocalThenHosted("glm".into()))
        );
        for bad in ["cloud", "hosted:", "hosted:a,local", "local,local", "a,b,c"] {
            assert!(EngineChoice::parse(bad).is_err(), "{bad}");
        }
        for c in [
            EngineChoice::Local,
            EngineChoice::Hosted("x".into()),
            EngineChoice::LocalThenHosted("y".into()),
        ] {
            assert_eq!(EngineChoice::parse(&c.label()), Ok(c.clone()));
        }
        assert_eq!(parse_second_engine("local"), Ok(None));
        assert_eq!(parse_second_engine("hosted:k"), Ok(Some("k".into())));
        assert!(parse_second_engine("k").is_err());
        assert_eq!(SpeechConfig::disabled().engine, EngineChoice::Local);
        assert!(!EnginePlan::default().leaves_the_studio());
    }
}
