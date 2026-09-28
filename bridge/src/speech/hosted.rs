//! A HOSTED speech engine: the one place recorded audio may leave the Studio, and only to the
//! engine the owner selected.
//!
//! # Why this exists
//!
//! Until Bridge 0.159.0 the rule was that audio never left the Studio. When the local whisper
//! engines went wrong (on 2026-09-28 they looped on every hour-long recording and returned a
//! minute of text), the owner was left with no transcript at all and no other engine to ask.
//! The rule is now: audio leaves the Studio only to a hosted speech engine the owner explicitly
//! chose (the Studio default in `JESSE_SPEECH_ENGINE`, or one run's `engine`), only as that
//! engine's transcription request, and only from this file. See `speech/mod.rs` for the whole
//! invariant and the guards that hold it.
//!
//! # What an engine is here
//!
//! A [`HostedTarget`]: plain data (an id, a label, a base URL, a token and a
//! [`TranscriptionCapability`]) resolved from the model registry by the HTTP boundary, which
//! is the only speech file that may see the registry. This file never sees the registry, the
//! application state, a turn or a vision helper; it is handed one target and speaks to it.
//!
//! # The wire shapes, verified live on 2026-09-28 with a synthetic clip
//!
//! * [`TranscriptionWire::AudioTranscriptions`]: `POST <base>/audio/transcriptions`, a
//!   multipart upload (`file`, `model`, `language`, `response_format`). OpenAI and every host
//!   that copies it; Fireworks serves it for `whisper-v3-turbo` on its own audio host and
//!   answers `verbose_json` with timed segments.
//! * [`TranscriptionWire::ChatInputAudio`]: `POST <base>/chat/completions` with an
//!   `input_audio` content part (base64 WAV). Gemini's OpenAI-compatible surface has no
//!   `/audio/transcriptions` (it answers 404) and takes audio this way instead; it returns
//!   text only, so each chunk becomes one segment spanning the chunk.
//!
//! # Chunks
//!
//! The decoded 16 kHz signal is cut into chunks under the target's size and duration caps,
//! each overlapping the last by [`OVERLAP_MS`], encoded as 16-bit WAV into the run's custody
//! directory (0700, deleted with the run on every ending), sent, and removed. The readings are
//! stitched back by time, and the words the overlap read twice are dropped.

use super::engine::{EngineError, EngineRun, Segment, SpeechEngine};
use super::reconcile::norm_word;
use super::wav::{encode_wav16, ENGINE_SAMPLE_RATE};
use crate::*;

/// The prefix an engine id carries when it names a hosted engine: `hosted:gemini-flash`.
pub const HOSTED_PREFIX: &str = "hosted:";
/// How much each chunk overlaps the one before it, so a word cut at a boundary is heard whole
/// in one of the two.
pub const OVERLAP_MS: u64 = 3_000;
/// One request's budget. An upload of a ten-minute chunk and its reading fit well inside it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
/// Attempts per chunk on a transient failure (429, 5xx, a dropped connection).
const ATTEMPTS: u32 = 3;
/// The most words a junction is searched for a repeated run.
const MAX_JUNCTION_WORDS: usize = 40;

/// How a hosted engine takes audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptionWire {
    /// `POST <base>/audio/transcriptions`, multipart.
    AudioTranscriptions,
    /// `POST <base>/chat/completions` with an `input_audio` part.
    ChatInputAudio,
}

impl TranscriptionWire {
    pub fn parse(raw: &str) -> Option<TranscriptionWire> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "audio_transcriptions" | "transcriptions" => {
                Some(TranscriptionWire::AudioTranscriptions)
            }
            "chat_input_audio" | "input_audio" => Some(TranscriptionWire::ChatInputAudio),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TranscriptionWire::AudioTranscriptions => "audio_transcriptions",
            TranscriptionWire::ChatInputAudio => "chat_input_audio",
        }
    }
}

/// What a registry entry declares when its provider takes audio. Nothing here is a secret:
/// the token is the entry's own.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptionCapability {
    pub wire: TranscriptionWire,
    /// The model slug the speech request names, which may differ from the entry's chat slug
    /// (`whisper-v3-turbo` beside `glm-5p3`).
    pub model: String,
    /// A base URL for the speech request when the provider serves audio on another host than
    /// the entry's (Fireworks). `None` uses the entry's own base URL.
    pub endpoint: Option<String>,
    /// The largest request body the provider takes, in bytes.
    pub max_chunk_bytes: u64,
    /// The longest chunk sent, in seconds.
    pub max_chunk_secs: u32,
    /// Ask for timed segments (`verbose_json`). Off for a model that answers text only.
    pub timestamps: bool,
}

/// One selectable hosted engine, resolved from an ARMED registry entry. Plain data.
#[derive(Clone, PartialEq)]
pub struct HostedTarget {
    /// The registry entry's id (`gemini-flash`).
    pub id: String,
    /// The registry entry's label (`Gemini 3.8 Flash`).
    pub label: String,
    /// The base URL the request goes to: the capability's endpoint, or the entry's own.
    pub base_url: String,
    pub token: String,
    pub cap: TranscriptionCapability,
}

impl std::fmt::Debug for HostedTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostedTarget")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .field("cap", &self.cap)
            .finish()
    }
}

impl HostedTarget {
    /// The engine id the wire uses: `hosted:<entry id>`.
    pub fn engine_id(&self) -> String {
        format!("{HOSTED_PREFIX}{}", self.id)
    }

    /// The host the audio goes to, for the provenance line.
    pub fn host(&self) -> String {
        let rest = self
            .base_url
            .split_once("://")
            .map(|(_, r)| r)
            .unwrap_or(&self.base_url);
        rest.split(['/', '?']).next().unwrap_or(rest).to_string()
    }

    /// What the app and the log show while it runs.
    pub fn engine_label(&self) -> String {
        format!("{} via {}", self.cap.model, self.label)
    }

    /// The one URL audio is sent to.
    pub fn url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        match self.cap.wire {
            TranscriptionWire::AudioTranscriptions => format!("{base}/audio/transcriptions"),
            TranscriptionWire::ChatInputAudio => format!("{base}/chat/completions"),
        }
    }

    /// The overview's row for this engine. Never the token.
    pub fn overview(&self) -> Value {
        json!({
            "id": self.engine_id(),
            "label": self.engine_label(),
            "hosted": true,
            "host": self.host(),
            "model": self.cap.model,
            "wire": self.cap.wire.label(),
            "timestamps": self.cap.timestamps,
        })
    }
}

// ---- Chunks ---------------------------------------------------------------------------

/// The WAV header [`encode_wav16`] writes.
const WAV_HEADER_BYTES: u64 = 44;
/// Room left in a chat request for everything that is not the audio.
const CHAT_ENVELOPE_BYTES: u64 = 4_096;

/// The most samples one chunk may hold under the target's caps.
pub fn max_chunk_samples(cap: &TranscriptionCapability) -> usize {
    let by_secs = cap.max_chunk_secs.max(1) as u64 * ENGINE_SAMPLE_RATE as u64;
    // What the audio may take of the body: all of it for a multipart upload less a little
    // for the form fields; three quarters for base64 inside a JSON body.
    let audio_bytes = match cap.wire {
        TranscriptionWire::AudioTranscriptions => {
            cap.max_chunk_bytes.saturating_sub(CHAT_ENVELOPE_BYTES)
        }
        TranscriptionWire::ChatInputAudio => {
            cap.max_chunk_bytes.saturating_sub(CHAT_ENVELOPE_BYTES) / 4 * 3
        }
    };
    let by_bytes = audio_bytes.saturating_sub(WAV_HEADER_BYTES) / 2;
    by_secs.min(by_bytes).max(ENGINE_SAMPLE_RATE as u64) as usize
}

/// Cut `total` samples into `(start, end)` ranges of at most `max` samples, each starting
/// [`OVERLAP_MS`] before the previous one ended.
pub fn plan_chunks(total: usize, max: usize) -> Vec<(usize, usize)> {
    let overlap = (OVERLAP_MS as usize * ENGINE_SAMPLE_RATE as usize / 1_000).min(max / 2);
    let mut out = Vec::new();
    let mut start = 0;
    while start < total {
        let end = (start + max).min(total);
        out.push((start, end));
        if end == total {
            break;
        }
        start = end - overlap;
    }
    out
}

fn samples_to_ms(n: usize) -> u64 {
    n as u64 * 1_000 / ENGINE_SAMPLE_RATE as u64
}

/// One chunk's reading, with times already absolute.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkReading {
    pub start_ms: u64,
    pub end_ms: u64,
    pub segments: Vec<Segment>,
    /// Whether the provider timed the segments itself, rather than one span per chunk.
    pub timed: bool,
}

/// Stitch chunk readings into one reading. Where two chunks overlap, a timed reading is cut at
/// the middle of the overlap (each segment kept by the chunk its midpoint falls in); then, for
/// every reading, a run of words that ends one chunk and starts the next is kept once.
pub fn stitch(chunks: Vec<ChunkReading>) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut previous_end: Option<u64> = None;
    for chunk in chunks {
        let mut segs = chunk.segments;
        if let Some(prev_end) = previous_end {
            // The middle of THIS overlap, which is shorter than `OVERLAP_MS` when the chunks
            // themselves are short.
            let cut = (chunk.start_ms + prev_end.max(chunk.start_ms)) / 2;
            let mid = |s: &Segment| (s.start_ms + s.end_ms) / 2;
            if chunk.timed {
                out.retain(|s| mid(s) < cut);
                segs.retain(|s| mid(s) >= cut);
            }
            drop_repeated_junction(&out, &mut segs);
        }
        previous_end = Some(chunk.end_ms);
        out.extend(segs.into_iter().filter(|s| !s.text.trim().is_empty()));
    }
    out
}

/// Remove from the head of `next` the longest run of words (two or more) that the tail of
/// `before` already ends with.
fn drop_repeated_junction(before: &[Segment], next: &mut [Segment]) {
    let tail: Vec<String> = before
        .iter()
        .rev()
        .flat_map(|s| s.text.split_whitespace().rev().map(norm_word))
        .filter(|w| !w.is_empty())
        .take(MAX_JUNCTION_WORDS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let Some(first) = next.first_mut() else {
        return;
    };
    let head: Vec<&str> = first.text.split_whitespace().collect();
    let head_norm: Vec<String> = head.iter().map(|w| norm_word(w)).collect();
    let most = tail.len().min(head_norm.len()).min(MAX_JUNCTION_WORDS);
    for k in (2..=most).rev() {
        if tail[tail.len() - k..] == head_norm[..k] {
            first.text = head[k..].join(" ");
            return;
        }
    }
}

// ---- The engine -------------------------------------------------------------------------

/// A hosted engine over one [`HostedTarget`].
pub struct HostedEngine {
    target: HostedTarget,
    id: String,
    label: String,
    client: reqwest::Client,
}

impl HostedEngine {
    pub fn new(target: HostedTarget) -> HostedEngine {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        HostedEngine {
            id: target.engine_id(),
            label: target.engine_label(),
            target,
            client,
        }
    }

    /// Read one WAV chunk. Times in the result are relative to the chunk.
    async fn read_chunk(
        &self,
        wav: &[u8],
        chunk_ms: u64,
        language: Option<&str>,
    ) -> Result<(Vec<Segment>, bool), Attempt> {
        let url = self.target.url();
        let request = match self.target.cap.wire {
            TranscriptionWire::AudioTranscriptions => {
                let mut fields: Vec<(&str, &str)> = vec![
                    ("model", self.target.cap.model.as_str()),
                    (
                        "response_format",
                        if self.target.cap.timestamps {
                            "verbose_json"
                        } else {
                            "json"
                        },
                    ),
                    ("temperature", "0"),
                ];
                if let Some(l) = language {
                    fields.push(("language", l));
                }
                let (content_type, body) = multipart(&fields, "chunk.wav", "audio/wav", wav);
                self.client
                    .post(&url)
                    .bearer_auth(&self.target.token)
                    .header("content-type", content_type)
                    .body(body)
            }
            TranscriptionWire::ChatInputAudio => {
                let body = chat_body(&self.target.cap.model, wav, language);
                self.client
                    .post(&url)
                    .bearer_auth(&self.target.token)
                    .header("content-type", "application/json")
                    .body(body.to_string())
            }
        };
        let resp = request.send().await.map_err(|e| {
            Attempt::Retry(format!(
                "{} could not be reached: {}",
                self.target.host(),
                e
            ))
        })?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| Attempt::Retry(format!("the answer was cut off: {e}")))?;
        if !status.is_success() {
            let why = provider_error(&text);
            let msg = format!("{} answered {}{why}", self.target.host(), status.as_u16());
            return Err(if status.as_u16() == 429 || status.is_server_error() {
                Attempt::Retry(msg)
            } else {
                Attempt::Fatal(msg)
            });
        }
        let v: Value = serde_json::from_str(&text).map_err(|_| {
            Attempt::Fatal(format!(
                "{} answered something that is not JSON",
                self.target.host()
            ))
        })?;
        Ok(match self.target.cap.wire {
            TranscriptionWire::AudioTranscriptions => parse_transcription(&v, chunk_ms),
            TranscriptionWire::ChatInputAudio => (parse_chat(&v, chunk_ms), false),
        })
    }
}

enum Attempt {
    Retry(String),
    Fatal(String),
}

/// The provider's own error sentence, when its body carries one: `: <message>`.
fn provider_error(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let msg = v["error"]["message"]
        .as_str()
        .or_else(|| v["message"].as_str())
        .or_else(|| v["error"].as_str())
        .unwrap_or("");
    let msg: String = msg.chars().take(300).collect();
    if msg.is_empty() {
        String::new()
    } else {
        format!(": {msg}")
    }
}

/// An `/audio/transcriptions` answer: its segments when it timed them, else its text as one
/// segment spanning the chunk. The provider's no-speech figure is not trusted (one host
/// answers values above 1), so every segment is taken as speech and the cleanup that runs
/// over every engine decides.
pub fn parse_transcription(v: &Value, chunk_ms: u64) -> (Vec<Segment>, bool) {
    if let Some(segs) = v["segments"].as_array().filter(|s| !s.is_empty()) {
        let secs = |x: &Value| (x.as_f64().unwrap_or(0.0).max(0.0) * 1_000.0).round() as u64;
        let out = segs
            .iter()
            .map(|s| {
                let start = secs(&s["start"]).min(chunk_ms);
                let end = secs(&s["end"]).clamp(start, chunk_ms);
                Segment::new(start, end, s["text"].as_str().unwrap_or("").trim())
            })
            .filter(|s| !s.text.is_empty())
            .collect();
        return (out, true);
    }
    (
        whole_chunk(v["text"].as_str().unwrap_or(""), chunk_ms),
        false,
    )
}

/// A chat answer's text, as one segment spanning the chunk.
pub fn parse_chat(v: &Value, chunk_ms: u64) -> Vec<Segment> {
    let content = &v["choices"][0]["message"]["content"];
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    };
    whole_chunk(&text, chunk_ms)
}

fn whole_chunk(text: &str, chunk_ms: u64) -> Vec<Segment> {
    let t = text.trim();
    if t.is_empty() {
        Vec::new()
    } else {
        vec![Segment::new(0, chunk_ms, t)]
    }
}

/// The instruction a chat model is given with the audio. It asks for the words and nothing
/// else, so the answer can be used as a transcript without parsing.
pub const CHAT_INSTRUCTION: &str = "You are a speech transcription engine. Transcribe the \
speech in the audio verbatim, in the language it is spoken in. Output only the transcript: no \
commentary, no labels, no timestamps, no quotation marks. If there is no speech, output \
nothing.";

/// The chat request for one chunk.
pub fn chat_body(model: &str, wav: &[u8], language: Option<&str>) -> Value {
    let ask = match language {
        Some(l) => format!("Transcribe this audio. The expected language code is \"{l}\"."),
        None => "Transcribe this audio.".to_string(),
    };
    json!({
        "model": model,
        "temperature": 0,
        "messages": [
            {"role": "system", "content": CHAT_INSTRUCTION},
            {"role": "user", "content": [
                {"type": "text", "text": ask},
                {"type": "input_audio", "input_audio": {
                    "data": base64_encode(wav),
                    "format": "wav",
                }},
            ]},
        ],
    })
}

/// A `multipart/form-data` body: the text fields, then the file. Returns the content type
/// (with its boundary) and the body.
pub fn multipart(
    fields: &[(&str, &str)],
    file_name: &str,
    file_mime: &str,
    file: &[u8],
) -> (String, Vec<u8>) {
    let boundary = format!("jesse-{}", random_hex());
    let mut body = Vec::with_capacity(file.len() + 1_024);
    for (name, value) in fields {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"{file_name}\"\r\nContent-Type: {file_mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// Wait until the run is cancelled, polling as the local engine's abort callback does.
async fn until_cancelled(cancelled: &super::engine::CancelFn) {
    loop {
        if cancelled() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

impl SpeechEngine for HostedEngine {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> &str {
        &self.label
    }

    /// Runs on the pipeline's blocking thread, which is inside the runtime, so each request
    /// is driven with `block_on` and raced against the run's cancel.
    fn transcribe(&self, run: EngineRun<'_>) -> Result<Vec<Segment>, EngineError> {
        let scratch = run.scratch.ok_or_else(|| {
            EngineError::Failed("a hosted reading needs the run's custody directory".into())
        })?;
        let rt = tokio::runtime::Handle::try_current()
            .map_err(|_| EngineError::Failed("no runtime to send the audio from".into()))?;
        let plan = plan_chunks(run.samples.len(), max_chunk_samples(&self.target.cap));
        let total = plan.len().max(1);
        let mut readings = Vec::with_capacity(plan.len());
        for (i, &(start, end)) in plan.iter().enumerate() {
            if (run.cancelled)() {
                return Err(EngineError::Cancelled);
            }
            let path = scratch.join(format!("hosted-chunk-{i:04}.wav"));
            write_private(
                &path,
                &encode_wav16(&run.samples[start..end], ENGINE_SAMPLE_RATE),
            )
            .map_err(|e| EngineError::Failed(format!("could not stage a chunk: {e}")))?;
            let wav = std::fs::read(&path)
                .map_err(|e| EngineError::Failed(format!("could not read a chunk back: {e}")));
            let chunk_ms = samples_to_ms(end - start);
            let result = wav.and_then(|wav| {
                rt.block_on(async {
                    let mut last = String::new();
                    for attempt in 0..ATTEMPTS {
                        if attempt > 0 {
                            let wait = Duration::from_secs(2 * 3u64.pow(attempt - 1));
                            tokio::select! {
                                _ = until_cancelled(&run.cancelled) => return Err(EngineError::Cancelled),
                                _ = tokio::time::sleep(wait) => {}
                            }
                        }
                        let outcome = tokio::select! {
                            _ = until_cancelled(&run.cancelled) => return Err(EngineError::Cancelled),
                            r = self.read_chunk(&wav, chunk_ms, run.language) => r,
                        };
                        match outcome {
                            Ok(r) => return Ok(r),
                            Err(Attempt::Fatal(m)) => return Err(EngineError::Failed(m)),
                            Err(Attempt::Retry(m)) => last = m,
                        }
                    }
                    Err(EngineError::Failed(format!("{last} ({ATTEMPTS} attempts)")))
                })
            });
            let _ = std::fs::remove_file(&path);
            let (segments, timed) = result?;
            let offset = samples_to_ms(start);
            readings.push(ChunkReading {
                start_ms: offset,
                end_ms: offset + chunk_ms,
                segments: segments
                    .into_iter()
                    .map(|s| Segment {
                        start_ms: s.start_ms + offset,
                        end_ms: s.end_ms + offset,
                        ..s
                    })
                    .collect(),
                timed,
            });
            (run.progress)((i + 1) as f64 / total as f64);
        }
        Ok(stitch(readings))
    }
}

/// Write a file only its owner can read, as every other file in custody is.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)
}

#[cfg(test)]
pub(crate) mod fakes {
    //! A hosted speech provider on loopback, for the tests: it records every request and
    //! answers with a script.
    use super::*;

    #[derive(Default)]
    pub struct Recorded {
        pub requests: Mutex<Vec<(String, Vec<u8>)>>,
    }

    /// Start a fake provider that answers every POST with `answer(path, body)`.
    pub async fn provider(
        answer: impl Fn(&str, &[u8]) -> (u16, String) + Send + Sync + 'static,
    ) -> (String, Arc<Recorded>) {
        let rec = Arc::new(Recorded::default());
        let answer = Arc::new(answer);
        let r = rec.clone();
        let router =
            Router::new().fallback(move |uri: axum::http::Uri, body: axum::body::Bytes| {
                let r = r.clone();
                let answer = answer.clone();
                async move {
                    let path = uri.path().to_string();
                    r.requests.lock_ok().push((path.clone(), body.to_vec()));
                    let (code, text) = answer(&path, &body);
                    (
                        StatusCode::from_u16(code).unwrap(),
                        [("content-type", "application/json")],
                        text,
                    )
                        .into_response()
                }
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router.into_make_service()).await;
        });
        (url, rec)
    }

    pub fn target(id: &str, base_url: &str, wire: TranscriptionWire) -> HostedTarget {
        HostedTarget {
            id: id.to_string(),
            label: format!("Fake {id}"),
            base_url: base_url.to_string(),
            token: "hosted-test-token".to_string(),
            cap: TranscriptionCapability {
                wire,
                model: "fake-speech".to_string(),
                endpoint: None,
                max_chunk_bytes: 25 * 1024 * 1024,
                max_chunk_secs: 600,
                timestamps: wire == TranscriptionWire::AudioTranscriptions,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::*;
    use super::*;

    fn cap(wire: TranscriptionWire, bytes: u64, secs: u32) -> TranscriptionCapability {
        TranscriptionCapability {
            wire,
            model: "m".into(),
            endpoint: None,
            max_chunk_bytes: bytes,
            max_chunk_secs: secs,
            timestamps: true,
        }
    }

    #[test]
    fn chunks_respect_both_caps_and_overlap() {
        let c = cap(
            TranscriptionWire::AudioTranscriptions,
            25 * 1024 * 1024,
            600,
        );
        assert_eq!(
            max_chunk_samples(&c),
            600 * 16_000,
            "ten minutes fit in 25 MB"
        );
        let small = cap(TranscriptionWire::AudioTranscriptions, 1_000_000, 600);
        let n = max_chunk_samples(&small);
        assert!(44 + 2 * n as u64 + CHAT_ENVELOPE_BYTES <= 1_000_000, "{n}");
        // Base64 costs a third more inside a chat body.
        let chat = cap(TranscriptionWire::ChatInputAudio, 1_000_000, 600);
        assert!(max_chunk_samples(&chat) < n);
        assert!((2 * max_chunk_samples(&chat) as u64 + 44) * 4 / 3 <= 1_000_000);

        let plan = plan_chunks(25 * 16_000, 10 * 16_000);
        assert_eq!(
            plan,
            vec![
                (0, 160_000),
                (160_000 - 48_000, 272_000),
                (272_000 - 48_000, 384_000),
                (384_000 - 48_000, 400_000),
            ]
        );
        assert_eq!(plan_chunks(5, 100), vec![(0, 5)]);
        assert!(plan_chunks(0, 100).is_empty());
    }

    #[test]
    fn timed_chunks_are_cut_at_the_overlap_midpoint() {
        let a = ChunkReading {
            start_ms: 0,
            end_ms: 10_000,
            segments: vec![
                Segment::new(0, 6_000, "The collection is on"),
                Segment::new(6_000, 9_000, "Thursday the fourteenth."),
                Segment::new(9_000, 10_000, "It"),
            ],
            timed: true,
        };
        let b = ChunkReading {
            start_ms: 7_000,
            end_ms: 17_000,
            segments: vec![
                Segment::new(7_000, 9_000, "the fourteenth."),
                Segment::new(9_000, 12_000, "It starts at nine."),
            ],
            timed: true,
        };
        let out = stitch(vec![a, b]);
        let text: Vec<&str> = out.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            text,
            vec![
                "The collection is on",
                "Thursday the fourteenth.",
                "It starts at nine."
            ]
        );
    }

    #[test]
    fn text_only_chunks_drop_the_words_the_overlap_read_twice() {
        let a = ChunkReading {
            start_ms: 0,
            end_ms: 10_000,
            segments: vec![Segment::new(0, 10_000, "We meet on Thursday at nine")],
            timed: false,
        };
        let b = ChunkReading {
            start_ms: 7_000,
            end_ms: 14_000,
            segments: vec![Segment::new(
                7_000,
                14_000,
                "Thursday at nine, in the hall.",
            )],
            timed: false,
        };
        let out = stitch(vec![a, b]);
        assert_eq!(out[1].text, "in the hall.");
        // One shared word is not enough to call it a repeat.
        let c = ChunkReading {
            start_ms: 0,
            end_ms: 10_000,
            segments: vec![Segment::new(0, 10_000, "and then yes")],
            timed: false,
        };
        let d = ChunkReading {
            start_ms: 7_000,
            end_ms: 14_000,
            segments: vec![Segment::new(7_000, 14_000, "yes we did")],
            timed: false,
        };
        assert_eq!(stitch(vec![c, d])[1].text, "yes we did");
    }

    #[test]
    fn answers_parse_into_segments() {
        let v = json!({"text": "x", "segments": [
            {"start": 0.0, "end": 3.046, "text": " The plumber comes.", "no_speech_prob": 4.41},
            {"start": 3.1, "end": 99.0, "text": "Nine."},
        ]});
        let (segs, timed) = parse_transcription(&v, 5_000);
        assert!(timed);
        assert_eq!(segs[0], Segment::new(0, 3_046, "The plumber comes."));
        assert_eq!(segs[1].end_ms, 5_000, "clamped to the chunk");
        let (segs, timed) = parse_transcription(&json!({"text": " hello "}), 4_000);
        assert!(!timed);
        assert_eq!(segs, vec![Segment::new(0, 4_000, "hello")]);
        let chat = json!({"choices": [{"message": {"content": "Buonasera."}}]});
        assert_eq!(
            parse_chat(&chat, 2_000),
            vec![Segment::new(0, 2_000, "Buonasera.")]
        );
        assert!(parse_chat(&json!({"choices": [{"message": {"content": "  "}}]}), 1).is_empty());
    }

    #[test]
    fn the_target_names_its_host_and_never_prints_its_token() {
        let t = target(
            "x",
            "https://api.example.com/v1/",
            TranscriptionWire::ChatInputAudio,
        );
        assert_eq!(t.url(), "https://api.example.com/v1/chat/completions");
        assert_eq!(t.host(), "api.example.com");
        assert!(!format!("{t:?}").contains("hosted-test-token"));
        assert!(!t.overview().to_string().contains("hosted-test-token"));
    }

    fn tone(secs: f32) -> Vec<f32> {
        (0..(16_000.0 * secs) as usize)
            .map(|i| 0.2 * (i as f32 / 20.0).sin())
            .collect()
    }

    fn run_engine(
        engine: &HostedEngine,
        samples: &[f32],
        scratch: &Path,
    ) -> Result<Vec<Segment>, EngineError> {
        engine.transcribe(EngineRun {
            samples,
            language: Some("en"),
            progress: Arc::new(|_| {}),
            cancelled: Arc::new(|| false),
            scratch: Some(scratch),
        })
    }

    fn scratch_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("jesse-hosted-{}", random_hex()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_multipart_upload_carries_the_model_language_and_audio_and_leaves_no_chunk() {
        let (url, rec) = provider(|_, _| {
            (
                200,
                json!({"text": "one two", "segments": [
                    {"start": 0.0, "end": 1.0, "text": "one"},
                    {"start": 2.5, "end": 4.0, "text": "two"},
                ]})
                .to_string(),
            )
        })
        .await;
        let mut t = target("fw", &url, TranscriptionWire::AudioTranscriptions);
        t.cap.max_chunk_secs = 4;
        let engine = HostedEngine::new(t);
        let dir = scratch_dir();
        let d = dir.clone();
        let samples = tone(10.0);
        let out = tokio::task::spawn_blocking(move || run_engine(&engine, &samples, &d))
            .await
            .unwrap()
            .unwrap();
        let reqs = rec.requests.lock_ok().clone();
        assert_eq!(
            reqs.len(),
            4,
            "10 s in 4 s chunks, each overlapping the last by 2 s"
        );
        let (path, body) = &reqs[0];
        assert_eq!(path, "/v1/audio/transcriptions");
        let body = String::from_utf8_lossy(body);
        for want in [
            "name=\"model\"",
            "fake-speech",
            "verbose_json",
            "name=\"language\"",
            "RIFF",
        ] {
            assert!(body.contains(want), "{want}");
        }
        // Chunks start at 0, 2, 4 and 6 s. Each later chunk's "one" and each earlier
        // chunk's "two" fall in an overlap on the far side of its midpoint, so they go.
        let text: Vec<&str> = out.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, vec!["one", "two"]);
        assert_eq!(out[1].start_ms, 6_000 + 2_500, "times are absolute");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "chunks are removed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_chat_request_carries_base64_wav_and_a_refusal_is_named() {
        let (url, rec) = provider(|_, _| {
            (
                200,
                json!({"choices": [{"message": {"content": "Buonasera a tutti."}}]}).to_string(),
            )
        })
        .await;
        let engine = HostedEngine::new(target("gem", &url, TranscriptionWire::ChatInputAudio));
        let dir = scratch_dir();
        let d = dir.clone();
        let out = tokio::task::spawn_blocking(move || run_engine(&engine, &tone(1.0), &d))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out, vec![Segment::new(0, 1_000, "Buonasera a tutti.")]);
        let (path, body) = rec.requests.lock_ok()[0].clone();
        assert_eq!(path, "/v1/chat/completions");
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["model"], "fake-speech");
        assert_eq!(
            v["messages"][1]["content"][1]["input_audio"]["format"],
            "wav"
        );
        assert!(v["messages"][1]["content"][1]["input_audio"]["data"]
            .as_str()
            .unwrap()
            .starts_with("UklGR"));

        let (url, _) =
            provider(|_, _| (401, json!({"error": {"message": "bad key"}}).to_string())).await;
        let engine = HostedEngine::new(target("gem", &url, TranscriptionWire::ChatInputAudio));
        let d = dir.clone();
        let err = tokio::task::spawn_blocking(move || run_engine(&engine, &tone(1.0), &d))
            .await
            .unwrap()
            .unwrap_err();
        match err {
            EngineError::Failed(m) => assert!(m.contains("401") && m.contains("bad key"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
