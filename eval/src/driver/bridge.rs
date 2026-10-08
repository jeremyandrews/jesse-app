//! **The `bridge` driver**: a task runs as a real phone turn through `jesse-bridge`.
//!
//! The other two drivers stop short of the thing the phone actually talks to. `claude-cli`
//! spawns `claude -p` with an EMPTY strict MCP config, and `direct` calls the agent loop in
//! process; neither exercises the bridge's spawn path, its hooks, its skills, its shell
//! grants or a single MCP server. This one submits the prompt through `POST /jesse` with the
//! bearer token, exactly as the app does, watches `GET /jesse/stream/{job}` for latency, waits
//! on `GET /jesse/result/{job}`, and grades the vault the turn left behind.
//!
//! ---- THE TARGET IS A TRAIT ---------------------------------------------------
//!
//! WHERE the bridge runs is a [`BridgeTarget`]. Today there is one, [`Spawned`]: a scratch
//! `jesse-bridge` process per task run, on a free loopback port, with its own state
//! directory, its own config file and the task's fixture vault, stopped by the PID recorded
//! at spawn. A later target can point at a URL with a fixture reset route instead; nothing
//! above the trait changes when it does.
//!
//! ---- WHAT [`Spawned`] ISOLATES, AND WHAT IT DELIBERATELY DOES NOT ------------
//!
//! Every path the bridge OWNS is redirected, by the variables the bridge itself reads
//! (`bridge/src/config.rs`, `Config::from_env`, and `persona::local_config_path`):
//!
//! | What | Variable | Scratch value |
//! |---|---|---|
//! | state dir (jobs, conversations, codex homes, direct threads, turn trace) | `JESSE_STATE_DIR` | `<scratch>/state` |
//! | config overlay (persona, `[[models]]`, schedule) | `JESSE_CONFIG` | `<scratch>/jesse.local.toml`, written here |
//! | vault | `JESSE_VAULT` | the task's fixture workspace |
//! | bearer token | `JESSE_TOKEN` | 32 random bytes, hex, per run |
//! | listen address | `JESSE_BIND`, `JESSE_PORT` | `127.0.0.1`, a free port |
//!
//! The process runs with its cwd in the scratch dir, so the `./jesse.local.toml` fallback
//! cannot find a stray file either. EVERY other inherited `JESSE_*` variable is removed (a
//! `JESSE_STATE_DIR` or `JESSE_CONFIG` in the caller's shell would otherwise win), as are
//! `ANTHROPIC_*`, `CLAUDE_CODE_*`, `CLAUDECODE` and `CODEX_HOME`, which would change how the
//! children authenticate or make a nested Claude Code session think it is inside another.
//! A variable the run NEEDS (a model's key, `JESSE_CLAUDE_BIN`) is named with `--pass-env`.
//!
//! `HOME` IS KEPT, ON PURPOSE. The `claude` child authenticates from the login the CLI keeps
//! for the user (`~/.claude.json` and the keychain entry it points at) and the `codex` harness
//! copies `~/.codex/auth.json` into a per-turn `CODEX_HOME` under the scratch state dir. A
//! redirected HOME has no login, so every turn would fail auth, and the cell would measure
//! nothing. What HOME then reaches, and why each is acceptable:
//!
//! * `~/.claude/projects/<cwd key>/`: Claude Code's session files, keyed by the escaped
//!   VAULT path (every non-alphanumeric character becomes `-`). The vault is a fresh temp
//!   dir, so the key is new and never the live vault's. The driver deletes that one
//!   directory when the run ends (`--keep-sessions` keeps it), and only when its name carries
//!   the temp-root marker.
//! * `~/.claude/settings.json`, `~/.claude/CLAUDE.md`, user skills: loaded because the bridge
//!   passes `--setting-sources user,project`. That is the production configuration, which is
//!   what a baseline should measure.
//! * the MCP servers' own credential files under HOME (a token cache, an env file): reached
//!   exactly as in production. A server whose credential is a plist variable starts
//!   unauthenticated unless the variable is passed with `--pass-env`.
//!
//! The MCP config and the tool allowlist are the bridge DEFAULTS (no `JESSE_MAIN_MCP_CONFIG`,
//! no `JESSE_ALLOWED_TOOLS`), so the child sees the real servers and the real grants.
//!
//! ---- THE MODEL OVERLAY -------------------------------------------------------
//!
//! `--model <id>` names a registry id; it rides every request as the per-turn `model`
//! field. Built-in ids (`opus`, `glm`, `kimi`, `qwen`, …) need only their key passed through.
//! An id the built-in registry lacks is declared by `--bridge-config <file.toml>`, whose text
//! becomes the scratch config file verbatim (ready-made ones live in `eval/bridge-overlays/`).
//! Before any task runs, a PREFLIGHT bridge reports the model's harness and whether it is
//! available; a model that is unconfigured or never turns healthy makes the whole cell NOT
//! RUN, with the reason, rather than a column of failures.

use super::{BoxFuture, Driver, Latency, PreparedWorkspace, TaskRun};
use crate::suite::{Task, Workspace};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// The temp-dir prefix every scratch bridge's directory carries. The session cleanup
/// refuses to touch a `~/.claude/projects/` entry whose name does not contain it.
pub const SCRATCH_PREFIX: &str = "jesse-eval-bridge-";

/// Variable-name prefixes stripped from the inherited environment (unless passed through).
const STRIPPED_PREFIXES: &[&str] = &["JESSE_", "ANTHROPIC_", "CLAUDE_CODE_"];
/// Exact names stripped from the inherited environment (unless passed through).
const STRIPPED_NAMES: &[&str] = &["CLAUDECODE", "CODEX_HOME"];

// ---- the target ------------------------------------------------------------------

/// A bridge serving one fixture vault, however it came to be.
pub trait RunningBridge {
    /// `http://host:port`, no trailing slash.
    fn base_url(&self) -> &str;
    /// The bearer token. Never printed, never written to a results file.
    fn token(&self) -> &str;
    /// The PID, when this target owns a process.
    fn pid(&self) -> Option<u32>;
    /// The last part of the bridge's log, for a harness error.
    fn log_tail(&self) -> String;
    /// Tear it down. For [`Spawned`], by the PID recorded at spawn.
    fn stop(self: Box<Self>) -> Result<(), String>;
}

/// Where a bridge comes from. One implementation today ([`Spawned`]); a remote target that
/// resets a fixture on a long-running bridge is the planned second.
pub trait BridgeTarget {
    /// `spawned`, for the results file.
    fn kind(&self) -> &'static str;
    /// Bring up a bridge whose vault is `vault` (an absolute, canonical path).
    fn start(&self, vault: &Path) -> Result<Box<dyn RunningBridge>, String>;
}

/// A scratch `jesse-bridge` process per task run.
pub struct Spawned {
    /// The built `jesse-bridge` binary. `jesse-hook` should sit beside it, as in a deploy;
    /// without it the bridge disarms its vault write lock and says so.
    pub bin: PathBuf,
    /// The scratch config file's content: the `--bridge-config` overlay, or empty.
    pub config: String,
    /// Inherited variables to KEEP despite the strip list (a model key, `JESSE_CLAUDE_BIN`).
    pub pass_env: Vec<String>,
    /// Keep `~/.claude/projects/<cwd key>/` after the run instead of deleting it.
    pub keep_sessions: bool,
    /// Every vault a bridge was started on, so [`Drop`] can sweep their session directories
    /// once more: a `claude` child that outlives its bridge by a moment can write its session
    /// file AFTER the per-run cleanup, and that file would otherwise stay behind.
    pub started: std::cell::RefCell<Vec<PathBuf>>,
}

/// The environment a scratch bridge runs with: the caller's, minus the strip list, plus the
/// redirections. Pure, so a test can assert that nothing live leaks through.
pub fn bridge_env(
    inherited: impl IntoIterator<Item = (String, String)>,
    pass_env: &[String],
    vault: &Path,
    state_dir: &Path,
    config: &Path,
    port: u16,
    token: &str,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = inherited
        .into_iter()
        .filter(|(k, _)| {
            pass_env.iter().any(|p| p == k)
                || !(STRIPPED_PREFIXES.iter().any(|p| k.starts_with(p))
                    || STRIPPED_NAMES.contains(&k.as_str()))
        })
        .collect();
    // The redirections win over anything passed through: a `--pass-env JESSE_STATE_DIR`
    // would otherwise point the scratch bridge at a real state directory.
    let ours = [
        ("JESSE_TOKEN", token.to_string()),
        ("JESSE_VAULT", vault.to_string_lossy().into_owned()),
        ("JESSE_STATE_DIR", state_dir.to_string_lossy().into_owned()),
        ("JESSE_CONFIG", config.to_string_lossy().into_owned()),
        ("JESSE_BIND", "127.0.0.1".to_string()),
        ("JESSE_PORT", port.to_string()),
        ("JESSE_SHOW_QR", "0".to_string()),
    ];
    env.retain(|(k, _)| !ours.iter().any(|(o, _)| o == k));
    env.extend(ours.iter().map(|(k, v)| (k.to_string(), v.clone())));
    env
}

/// 32 random bytes as lowercase hex.
fn random_hex(n: usize) -> Result<String, String> {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).map_err(|e| format!("no randomness: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// A random RFC 4122 v4 UUID, lowercase, as the app mints a conversation id.
pub fn uuid_v4() -> Result<String, String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| format!("no randomness: {e}"))?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

/// Claude Code's `~/.claude/projects/` directory name for a working directory: every
/// character that is not ASCII alphanumeric becomes `-` (the bridge's own
/// `sessions::escape_project_path`, verified there against the CLI).
pub fn claude_project_key(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// A loopback port nothing is listening on right now.
fn free_port() -> Result<u16, String> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| format!("could not find a free port: {e}"))?;
    l.local_addr()
        .map(|a| a.port())
        .map_err(|e| format!("could not read the port: {e}"))
}

/// The live state directory, which a scratch bridge must never be pointed at.
fn live_state_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".jesse-bridge"))
}

struct SpawnedBridge {
    child: Child,
    pid: u32,
    base_url: String,
    token: String,
    scratch: tempfile::TempDir,
    vault: PathBuf,
    keep_sessions: bool,
}

impl BridgeTarget for Spawned {
    fn kind(&self) -> &'static str {
        "spawned"
    }

    fn start(&self, vault: &Path) -> Result<Box<dyn RunningBridge>, String> {
        if !self.bin.is_file() {
            return Err(format!(
                "jesse-bridge binary not found at {} (build it: cargo build --release in bridge/)",
                self.bin.display()
            ));
        }
        let scratch = tempfile::Builder::new()
            .prefix(SCRATCH_PREFIX)
            .tempdir()
            .map_err(|e| format!("could not create the scratch dir: {e}"))?;
        let state = scratch.path().join("state");
        std::fs::create_dir_all(&state)
            .map_err(|e| format!("could not create the state dir: {e}"))?;
        // THE GUARD THAT MAKES THE HARD CONSTRAINT A CHECK, not only a construction.
        if let Some(live) = live_state_dir() {
            if state.starts_with(&live) {
                return Err(format!(
                    "refusing to start: the scratch state dir {} is inside the live one",
                    state.display()
                ));
            }
        }
        let config = scratch.path().join("jesse.local.toml");
        let header = "# jesse-eval scratch bridge config. Generated per run; never the live \
                      jesse.local.toml.\n";
        std::fs::write(&config, format!("{header}{}", self.config))
            .map_err(|e| format!("could not write the scratch config: {e}"))?;
        let token = random_hex(32)?;
        let port = free_port()?;
        let env = bridge_env(
            std::env::vars(),
            &self.pass_env,
            vault,
            &state,
            &config,
            port,
            &token,
        );
        let log = std::fs::File::create(scratch.path().join("bridge.log"))
            .map_err(|e| format!("could not create the bridge log: {e}"))?;
        let log2 = log
            .try_clone()
            .map_err(|e| format!("could not open the bridge log twice: {e}"))?;
        // Absolute: the child starts in the scratch dir, where a relative path means nothing.
        let bin = self
            .bin
            .canonicalize()
            .map_err(|e| format!("could not resolve {}: {e}", self.bin.display()))?;
        let child = Command::new(&bin)
            .current_dir(scratch.path())
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log2)
            .spawn()
            .map_err(|e| format!("could not spawn {}: {e}", self.bin.display()))?;
        let pid = child.id();
        self.started.borrow_mut().push(vault.to_path_buf());
        Ok(Box::new(SpawnedBridge {
            child,
            pid,
            base_url: format!("http://127.0.0.1:{port}"),
            token,
            scratch,
            vault: vault.to_path_buf(),
            keep_sessions: self.keep_sessions,
        }))
    }
}

impl RunningBridge for SpawnedBridge {
    fn base_url(&self) -> &str {
        &self.base_url
    }
    fn token(&self) -> &str {
        &self.token
    }
    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }
    fn log_tail(&self) -> String {
        let body =
            std::fs::read_to_string(self.scratch.path().join("bridge.log")).unwrap_or_default();
        let lines: Vec<&str> = body.lines().collect();
        redact_home(&lines[lines.len().saturating_sub(15)..].join("\n"))
    }
    fn stop(mut self: Box<Self>) -> Result<(), String> {
        // By the RECORDED PID: SIGTERM first so the bridge can reap its own children, then
        // SIGKILL if it has not exited within ten seconds. `kill` with a constant argv.
        let _ = Command::new("kill")
            .args(["-TERM", &self.pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        if !self.keep_sessions {
            cleanup_sessions(&self.vault);
        }
        Ok(())
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        if self.keep_sessions {
            return;
        }
        if self.started.borrow().is_empty() {
            return;
        }
        // A moment for a straggling child to finish writing before the last sweep.
        std::thread::sleep(Duration::from_secs(2));
        for v in self.started.borrow().iter() {
            cleanup_sessions(v);
        }
    }
}

/// Delete `~/.claude/projects/<key of vault>`, the session files the run's `claude`
/// children wrote, and only when the key carries the eval temp-root marker.
fn cleanup_sessions(vault: &Path) {
    let key = claude_project_key(vault);
    if !key.contains("jesse-eval-") {
        return;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let dir = PathBuf::from(home)
            .join(".claude")
            .join("projects")
            .join(&key);
        if dir.is_dir() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

// ---- the driver ------------------------------------------------------------------

/// What a preflight learned about the model.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub harness: String,
    pub bridge_version: String,
}

/// The `bridge` driver.
pub struct BridgeDriver {
    pub target: Box<dyn BridgeTarget>,
    /// The registry id every turn names.
    pub model: String,
    /// Learned at preflight.
    pub info: ModelInfo,
    /// Per-turn wall-clock limit.
    pub timeout: Duration,
    /// How long a non-ambient model may take to pass its first health probe.
    pub model_wait: Duration,
}

/// Why a cell could not run at all.
#[derive(Debug, Clone, PartialEq)]
pub struct NotRun(pub String);

impl BridgeDriver {
    /// Bring up a bridge on an empty scratch vault, learn the model's harness and whether it
    /// is available, and tear it down. A model that is unconfigured (its credential absent)
    /// or never turns healthy is [`NotRun`], with the reason.
    pub fn preflight(
        target: Box<dyn BridgeTarget>,
        model: String,
        timeout: Duration,
        model_wait: Duration,
    ) -> Result<BridgeDriver, NotRun> {
        let vault = tempfile::Builder::new()
            .prefix("jesse-eval-preflight-")
            .tempdir()
            .map_err(|e| NotRun(format!("could not create a preflight vault: {e}")))?;
        let vault_path = vault
            .path()
            .canonicalize()
            .map_err(|e| NotRun(format!("could not resolve the preflight vault: {e}")))?;
        let running = target.start(&vault_path).map_err(NotRun)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| NotRun(format!("could not start a runtime: {e}")))?;
        let outcome = rt.block_on(async {
            let client = http_client()?;
            let version = wait_ready(&client, &*running, Duration::from_secs(60)).await?;
            let harness = wait_model(&client, &*running, &model, model_wait).await?;
            Ok::<ModelInfo, String>(ModelInfo {
                harness,
                bridge_version: version,
            })
        });
        let tail = running.log_tail();
        let _ = running.stop();
        match outcome {
            Ok(info) => Ok(BridgeDriver {
                target,
                model,
                info,
                timeout,
                model_wait,
            }),
            Err(e) => Err(NotRun(if tail.is_empty() {
                e
            } else {
                format!("{e}\n--- bridge log tail ---\n{tail}")
            })),
        }
    }
}

fn http_client() -> Result<reqwest::Client, String> {
    // NO PROXY: the target is loopback, and a proxy variable in the caller's environment
    // would otherwise route the bearer token through it.
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| format!("could not build the HTTP client: {e}"))
}

/// Poll `/health` until it answers 200; return the bridge's version.
async fn wait_ready(
    client: &reqwest::Client,
    b: &dyn RunningBridge,
    limit: Duration,
) -> Result<String, String> {
    let deadline = Instant::now() + limit;
    let url = format!("{}/health", b.base_url());
    loop {
        if let Ok(r) = client.get(&url).bearer_auth(b.token()).send().await {
            if r.status().is_success() {
                let v: Value = r.json().await.unwrap_or(Value::Null);
                return Ok(v
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the bridge did not answer /health within {}s",
                limit.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The model's row from `/jesse/models`, by id or alias.
fn find_model<'a>(models: &'a Value, id: &str) -> Option<&'a Value> {
    models.get("models")?.as_array()?.iter().find(|m| {
        m.get("id").and_then(|v| v.as_str()) == Some(id)
            || m.get("aliases")
                .and_then(|a| a.as_array())
                .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(id)))
    })
}

/// Wait until the model is available; return its harness.
async fn wait_model(
    client: &reqwest::Client,
    b: &dyn RunningBridge,
    model: &str,
    limit: Duration,
) -> Result<String, String> {
    let deadline = Instant::now() + limit;
    let url = format!("{}/jesse/models", b.base_url());
    loop {
        let v: Value = client
            .get(&url)
            .bearer_auth(b.token())
            .send()
            .await
            .map_err(|e| format!("GET /jesse/models failed: {e}"))?
            .json()
            .await
            .map_err(|e| format!("GET /jesse/models was not JSON: {e}"))?;
        let row = find_model(&v, model).ok_or_else(|| {
            format!("model '{model}' is not in the registry (declare it with --bridge-config)")
        })?;
        let flag = |k: &str| row.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
        let harness = row
            .get("harness")
            .and_then(|x| x.as_str())
            .unwrap_or("claude-code")
            .to_string();
        if !flag("configured") {
            return Err(format!(
                "model '{model}' is not configured: its credential is absent from the scratch \
                 bridge's environment (pass it with --pass-env)"
            ));
        }
        if flag("available") {
            return Ok(harness);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "model '{model}' is configured but did not pass a health probe within {}s \
                 (its backend is unreachable or rejects the credential)",
                limit.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// What the stream showed, relative to submit.
#[derive(Debug, Default, Clone)]
pub struct StreamRecord {
    /// Every frame but text deltas, as `(event, data, ms since submit)`.
    pub frames: Vec<(String, Value, u64)>,
    /// How many delta frames arrived (their text is the answer, recorded once at the end).
    pub deltas: usize,
    pub latency: Latency,
}

impl StreamRecord {
    /// Account for one SSE frame observed `t` ms after submit.
    pub fn observe(&mut self, event: &str, data: Value, t: u64) {
        let text_of = |d: &Value| {
            d.get("text")
                .and_then(|x| x.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false)
        };
        let (is_event, is_token, terminal) = match event {
            // The subscription's opening snapshot. Empty means nothing had happened yet,
            // which is not an event; non-empty means text already streamed before we
            // subscribed, observed now (an upper bound on both times).
            "reset" => (text_of(&data), text_of(&data), false),
            "delta" | "narration" => (true, text_of(&data), false),
            "activity" => (true, false, false),
            "done" | "error" | "cancelled" => (true, false, true),
            _ => (false, false, false),
        };
        if is_event && self.latency.first_event_ms.is_none() {
            self.latency.first_event_ms = Some(t);
        }
        if is_token && self.latency.first_token_ms.is_none() {
            self.latency.first_token_ms = Some(t);
        }
        if terminal && self.latency.result_ms.is_none() {
            self.latency.result_ms = Some(t);
            // A harness that does not stream text shows its first model token in the
            // result itself.
            if self.latency.first_token_ms.is_none()
                && event == "done"
                && data
                    .get("response")
                    .and_then(|x| x.as_str())
                    .is_some_and(|s| !s.is_empty())
            {
                self.latency.first_token_ms = Some(t);
            }
        }
        if event == "delta" {
            self.deltas += 1;
        } else {
            self.frames.push((event.to_string(), data, t));
        }
    }

    pub fn terminal(&self) -> bool {
        self.latency.result_ms.is_some()
    }
}

/// Split complete SSE frames off the front of `buf`; return `(event, data)` pairs.
pub fn drain_sse(buf: &mut String) -> Vec<(String, String)> {
    let mut out = Vec::new();
    loop {
        let normalized = buf.replace("\r\n", "\n");
        if normalized.len() != buf.len() {
            *buf = normalized;
        }
        let Some(end) = buf.find("\n\n") else { break };
        let block: String = buf.drain(..end + 2).collect();
        let mut event = "message".to_string();
        let mut data = Vec::new();
        for line in block.lines() {
            if let Some(v) = line.strip_prefix("event:") {
                event = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("data:") {
                data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
            }
        }
        if !data.is_empty() {
            out.push((event, data.join("\n")));
        }
    }
    out
}

/// Read one job's stream until its terminal frame (or the connection ends).
async fn read_stream(
    client: reqwest::Client,
    url: String,
    token: String,
    t0: Instant,
) -> Result<StreamRecord, String> {
    let mut rec = StreamRecord::default();
    let mut resp = client
        .get(&url)
        .bearer_auth(&token)
        .header("accept", "text/event-stream")
        .send()
        .await
        .map_err(|e| format!("GET stream failed: {e}"))?;
    let mut buf = String::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("stream read failed: {e}"))?
    {
        let t = t0.elapsed().as_millis() as u64;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        for (event, data) in drain_sse(&mut buf) {
            let v: Value = serde_json::from_str(&data).unwrap_or(Value::String(data));
            rec.observe(&event, v, t);
        }
        if rec.terminal() {
            break;
        }
    }
    Ok(rec)
}

/// One turn's outcome.
struct TurnOutcome {
    stream: StreamRecord,
    result: Value,
    result_ms: u64,
}

impl BridgeDriver {
    /// Submit one turn and wait for its result.
    async fn turn(
        &self,
        client: &reqwest::Client,
        b: &dyn RunningBridge,
        body: Value,
    ) -> Result<TurnOutcome, String> {
        let t0 = Instant::now();
        let resp = client
            .post(format!("{}/jesse", b.base_url()))
            .bearer_auth(b.token())
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("POST /jesse failed: {e}"))?;
        let status = resp.status();
        let accepted: Value = resp.json().await.unwrap_or(Value::Null);
        if status.as_u16() != 202 {
            return Err(format!("POST /jesse answered {status}: {accepted}"));
        }
        let job = accepted
            .get("job_id")
            .and_then(|v| v.as_str())
            .ok_or("POST /jesse returned no job_id")?
            .to_string();
        let stream = tokio::spawn(read_stream(
            client.clone(),
            format!("{}/jesse/stream/{job}", b.base_url()),
            b.token().to_string(),
            t0,
        ));
        let deadline = t0 + self.timeout;
        let result_url = format!("{}/jesse/result/{job}", b.base_url());
        let (result, polled_ms) = loop {
            let v: Value = client
                .get(&result_url)
                .bearer_auth(b.token())
                .send()
                .await
                .map_err(|e| format!("GET /jesse/result failed: {e}"))?
                .json()
                .await
                .map_err(|e| format!("GET /jesse/result was not JSON: {e}"))?;
            if v.get("status").and_then(|s| s.as_str()) != Some("running") {
                break (v, t0.elapsed().as_millis() as u64);
            }
            if Instant::now() >= deadline {
                let _ = client
                    .post(format!("{}/jesse/cancel/{job}", b.base_url()))
                    .bearer_auth(b.token())
                    .send()
                    .await;
                stream.abort();
                return Err(format!(
                    "the turn did not finish within {}s (cancelled)",
                    self.timeout.as_secs()
                ));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        let stream = match tokio::time::timeout(Duration::from_secs(10), stream).await {
            Ok(Ok(Ok(rec))) => rec,
            // The stream is for latency only; losing it never loses the result.
            _ => StreamRecord::default(),
        };
        let result_ms = stream.latency.result_ms.unwrap_or(polled_ms).min(polled_ms);
        Ok(TurnOutcome {
            stream,
            result,
            result_ms,
        })
    }

    async fn drive(&self, task: &Task, b: &dyn RunningBridge) -> Result<TaskRun, String> {
        let client = http_client()?;
        wait_ready(&client, b, Duration::from_secs(60)).await?;
        wait_model(&client, b, &self.model, self.model_wait).await?;

        let conversation_id = uuid_v4()?;
        let mode = task.mode.clone().unwrap_or_else(|| "ask".to_string());
        let mut lines: Vec<String> = Vec::new();
        let mut session_id: Option<String> = None;
        let mut first_latency: Option<Latency> = None;
        let mut total_ms = 0u64;
        let texts: Vec<&String> = std::iter::once(&task.prompt)
            .chain(task.followups.iter())
            .collect();
        for (i, text) in texts.iter().enumerate() {
            let mut body = json!({
                "mode": mode,
                "text": text,
                "model": self.model,
                "conversation_id": conversation_id,
                "request_id": random_hex(16)?,
            });
            if let Some(s) = &session_id {
                body["session_id"] = json!(s);
            }
            if i == 0 {
                if let Some(h) = &task.health_context {
                    body["health_context"] = json!(h);
                }
            }
            let out = self.turn(&client, b, body).await?;
            total_ms += out.result_ms;
            let mut lat = out.stream.latency;
            lat.result_ms = Some(out.result_ms);
            first_latency.get_or_insert(lat);
            session_id = out
                .result
                .get("session_id")
                .and_then(|s| s.as_str())
                .map(str::to_string)
                .or(session_id);
            lines.extend(turn_lines(i, &out));
            let status = out.result.get("status").and_then(|s| s.as_str());
            if status != Some("done") {
                return Ok(finish(
                    lines,
                    total_ms,
                    first_latency.unwrap_or_default(),
                    Some(format!(
                        "turn {} ended {}: {}",
                        i + 1,
                        status.unwrap_or("without a status"),
                        out.result
                            .get("error")
                            .map(|e| e.to_string())
                            .unwrap_or_default()
                    )),
                ));
            }
        }
        Ok(finish(
            lines,
            total_ms,
            first_latency.unwrap_or_default(),
            None,
        ))
    }
}

/// The transcript lines for one turn, in the stream-json shape the parser reads plus
/// `bridge_*` lines it ignores:
///
/// * `bridge_turn`: which turn, the job's status and the three latencies;
/// * `bridge_sse`: every non-delta frame with its time (deltas are counted, not stored,
///   since their text is the answer);
/// * one `assistant` line whose `tool_use` blocks are the turn's tool calls, by name, from
///   the bridge's own content-free trace (`timing.tools` on the result);
/// * one `result` line with the answer and the usage, the terminal line the parser keys on.
fn turn_lines(i: usize, out: &TurnOutcome) -> Vec<String> {
    let r = &out.result;
    let status = r.get("status").and_then(|s| s.as_str()).unwrap_or("");
    let mut lines = vec![json!({
        "type": "bridge_turn",
        "turn": i + 1,
        "status": status,
        "deltas": out.stream.deltas,
        "latency": out.stream.latency,
        "result_ms": out.result_ms,
        "timing": r.get("timing"),
        "provenance": r.get("provenance"),
    })
    .to_string()];
    for (event, data, t) in &out.stream.frames {
        lines.push(
            json!({"type": "bridge_sse", "event": event, "t_ms": t, "data": data}).to_string(),
        );
    }
    let tools: Vec<Value> = r
        .pointer("/timing/tools")
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|t| t.get("tool").and_then(|n| n.as_str()))
                .map(|n| json!({"type": "tool_use", "name": n}))
                .collect()
        })
        .unwrap_or_default();
    if !tools.is_empty() {
        lines.push(json!({"type": "assistant", "message": {"content": tools}}).to_string());
    }
    if status == "done" {
        let usage = r
            .get("usage")
            .filter(|u| !u.is_null())
            .cloned()
            .unwrap_or(json!({}));
        lines.push(
            json!({
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "result": r.get("response").and_then(|s| s.as_str()).unwrap_or(""),
                "usage": usage,
            })
            .to_string(),
        );
    }
    lines
}

fn finish(lines: Vec<String>, wall_ms: u64, latency: Latency, error: Option<String>) -> TaskRun {
    let mut run = TaskRun::from_lines(lines, wall_ms, latency.first_token_ms);
    run.latency = latency;
    run.error = error;
    run
}

impl Driver for BridgeDriver {
    fn id(&self) -> &'static str {
        "bridge"
    }

    fn endpoint(&self) -> Option<String> {
        Some(format!(
            "{} jesse-bridge {}",
            self.target.kind(),
            self.info.bridge_version
        ))
    }

    fn model(&self) -> Option<String> {
        Some(self.model.clone())
    }

    fn is_mock(&self) -> bool {
        false
    }

    fn harness(&self) -> Option<String> {
        Some(self.info.harness.clone())
    }

    fn holds_conversation(&self) -> bool {
        true
    }

    fn run_task<'a>(
        &'a self,
        task: &'a Task,
        workspace: &'a PreparedWorkspace,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, TaskRun> {
        Box::pin(async move {
            if workspace.kind != Workspace::Fixture {
                return TaskRun::failed(
                    "the bridge driver runs fixture workspaces only; it never points a \
                     bridge at a real vault",
                );
            }
            let vault = match workspace.dir.canonicalize() {
                Ok(v) => v,
                Err(e) => return TaskRun::failed(format!("could not resolve the workspace: {e}")),
            };
            let running = match self.target.start(&vault) {
                Ok(r) => r,
                Err(e) => return TaskRun::failed(e),
            };
            let pid = running.pid();
            let outcome = self.drive(task, &*running).await.map(|mut run| {
                // Which process served it, for a reader matching a transcript to a log.
                run.lines.insert(
                    0,
                    json!({"type": "bridge_spawn", "target": self.target.kind(), "pid": pid})
                        .to_string(),
                );
                run
            });
            let tail = running.log_tail();
            let stopped = running.stop();
            match (outcome, stopped) {
                (Ok(run), Ok(())) => run,
                (Ok(mut run), Err(e)) => {
                    run.error
                        .get_or_insert(format!("the bridge did not stop cleanly: {e}"));
                    run
                }
                (Err(e), _) => TaskRun::failed(format!("{e}\n--- bridge log tail ---\n{tail}")),
            }
        })
    }
}

/// The bridge log tail lands in `results.json`, which is committed under `eval-runs/`. The
/// bridge logs absolute paths (its hook helper, the scratch vault), so the operator's home
/// directory would otherwise ride into a tracked file. Rewrite it to `~`.
fn redact_home(text: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if home.len() > 1 => text.replace(home.trim_end_matches('/'), "~"),
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_tail_never_carries_the_home_directory() {
        let home = std::env::var("HOME").expect("HOME is set under cargo test");
        let line = format!("helper {home}/devel/bridge/target/release/jesse-hook");
        let out = redact_home(&line);
        assert!(!out.contains(&home), "{out}");
        assert!(
            out.contains("~/devel/bridge/target/release/jesse-hook"),
            "{out}"
        );
    }

    fn env_of(v: &[(String, String)], k: &str) -> Option<String> {
        v.iter().find(|(n, _)| n == k).map(|(_, x)| x.clone())
    }

    #[test]
    fn the_scratch_env_strips_live_settings_and_redirects_every_owned_path() {
        let inherited = vec![
            ("HOME".to_string(), "/home/someone".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
            (
                "JESSE_STATE_DIR".to_string(),
                "/home/someone/.jesse-bridge".to_string(),
            ),
            (
                "JESSE_CONFIG".to_string(),
                "/home/someone/.jesse-bridge/jesse.local.toml".to_string(),
            ),
            (
                "JESSE_MAIN_MCP_CONFIG".to_string(),
                "/tmp/other.json".to_string(),
            ),
            ("JESSE_ALLOWED_TOOLS".to_string(), "Bash".to_string()),
            ("JESSE_MODEL_GLM_AUTH_TOKEN".to_string(), "k".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "k".to_string()),
            ("CLAUDECODE".to_string(), "1".to_string()),
            ("CLAUDE_CODE_ENTRYPOINT".to_string(), "cli".to_string()),
            ("CODEX_HOME".to_string(), "/x".to_string()),
        ];
        let env = bridge_env(
            inherited,
            &[
                "JESSE_MODEL_GLM_AUTH_TOKEN".to_string(),
                "JESSE_STATE_DIR".to_string(),
            ],
            Path::new("/tmp/jesse-eval-x/task"),
            Path::new("/tmp/jesse-eval-bridge-y/state"),
            Path::new("/tmp/jesse-eval-bridge-y/jesse.local.toml"),
            40123,
            "tok",
        );
        // Kept: HOME (the CLI login), PATH, and the one named key.
        assert_eq!(env_of(&env, "HOME").as_deref(), Some("/home/someone"));
        assert_eq!(env_of(&env, "PATH").as_deref(), Some("/usr/bin"));
        assert_eq!(
            env_of(&env, "JESSE_MODEL_GLM_AUTH_TOKEN").as_deref(),
            Some("k")
        );
        // Gone: every live setting that would change what the bridge reads or grants.
        for k in [
            "JESSE_MAIN_MCP_CONFIG",
            "JESSE_ALLOWED_TOOLS",
            "ANTHROPIC_API_KEY",
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CODEX_HOME",
        ] {
            assert_eq!(env_of(&env, k), None, "{k} leaked");
        }
        // Redirected, and the redirection beats a pass-through of the same name.
        assert_eq!(
            env_of(&env, "JESSE_STATE_DIR").as_deref(),
            Some("/tmp/jesse-eval-bridge-y/state")
        );
        assert_eq!(
            env.iter().filter(|(k, _)| k == "JESSE_STATE_DIR").count(),
            1
        );
        assert_eq!(
            env_of(&env, "JESSE_CONFIG").as_deref(),
            Some("/tmp/jesse-eval-bridge-y/jesse.local.toml")
        );
        assert_eq!(
            env_of(&env, "JESSE_VAULT").as_deref(),
            Some("/tmp/jesse-eval-x/task")
        );
        assert_eq!(env_of(&env, "JESSE_BIND").as_deref(), Some("127.0.0.1"));
        assert_eq!(env_of(&env, "JESSE_PORT").as_deref(), Some("40123"));
        assert_eq!(env_of(&env, "JESSE_TOKEN").as_deref(), Some("tok"));
        for (k, v) in &env {
            assert!(
                !v.contains(".jesse-bridge"),
                "{k}={v} points at the live state directory"
            );
        }
    }

    #[test]
    fn sse_frames_split_on_blank_lines_and_keep_partial_ones() {
        let mut buf = String::from(
            "event: reset\ndata: {\"text\":\"\"}\n\nevent: delta\ndata: {\"text\":\"Hi\"}\n\n: keepalive\n\nevent: done\ndata: {\"respo",
        );
        let frames = drain_sse(&mut buf);
        assert_eq!(
            frames,
            vec![
                ("reset".to_string(), "{\"text\":\"\"}".to_string()),
                ("delta".to_string(), "{\"text\":\"Hi\"}".to_string()),
            ]
        );
        assert!(
            buf.starts_with("event: done"),
            "the partial frame stays: {buf}"
        );
        buf.push_str("nse\":\"Hi\"}\r\n\r\n");
        let frames = drain_sse(&mut buf);
        assert_eq!(frames[0].0, "done");
        assert!(buf.is_empty());
    }

    #[test]
    fn latency_ignores_the_empty_opening_snapshot() {
        let mut r = StreamRecord::default();
        r.observe("reset", json!({"text": ""}), 5);
        assert_eq!(r.latency.first_event_ms, None);
        r.observe("activity", json!({"name": "Read"}), 900);
        r.observe("delta", json!({"text": "The"}), 1500);
        r.observe("delta", json!({"text": " answer"}), 1600);
        r.observe("done", json!({"response": "The answer"}), 2000);
        assert_eq!(
            r.latency,
            Latency {
                first_event_ms: Some(900),
                first_token_ms: Some(1500),
                result_ms: Some(2000),
            }
        );
        assert_eq!(r.deltas, 2);
        assert!(r.frames.iter().all(|(e, _, _)| e != "delta"));
    }

    #[test]
    fn a_harness_that_does_not_stream_text_gets_its_first_token_at_the_result() {
        let mut r = StreamRecord::default();
        r.observe("activity", json!({"name": "shell"}), 300);
        r.observe("done", json!({"response": "Logged."}), 4000);
        assert_eq!(r.latency.first_event_ms, Some(300));
        assert_eq!(r.latency.first_token_ms, Some(4000));
        assert_eq!(r.latency.result_ms, Some(4000));
    }

    #[test]
    fn turn_lines_parse_into_the_answer_the_tools_and_the_usage() {
        let out = TurnOutcome {
            stream: StreamRecord::default(),
            result: json!({
                "status": "done",
                "response": "Logged the banana.",
                "session_id": "s1",
                "timing": {"tools": [{"tool": "Read", "ms": 3}, {"tool": "Edit", "ms": 9}]},
                "usage": {"input_tokens": 100, "output_tokens": 20},
            }),
            result_ms: 1234,
        };
        let lines = turn_lines(0, &out);
        let t = crate::transcript::parse(&lines);
        assert!(t.completed);
        assert_eq!(t.final_answer.as_deref(), Some("Logged the banana."));
        assert_eq!(t.tool_names, ["Read", "Edit"]);
        assert_eq!(t.usage.unwrap().input_tokens, 100);
    }

    #[test]
    fn a_failed_turn_has_no_result_line() {
        let out = TurnOutcome {
            stream: StreamRecord::default(),
            result: json!({"status": "failed", "error": "boom"}),
            result_ms: 10,
        };
        let t = crate::transcript::parse(&turn_lines(0, &out));
        assert!(!t.completed);
    }

    #[test]
    fn the_project_key_matches_the_cli_escape() {
        assert_eq!(
            claude_project_key(Path::new("/private/var/folders/x_y/T/jesse-eval-Ab.c/t1")),
            "-private-var-folders-x-y-T-jesse-eval-Ab-c-t1"
        );
    }

    #[test]
    fn a_v4_uuid_has_the_version_and_variant_bits() {
        let u = uuid_v4().unwrap();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"));
        assert_eq!(u, u.to_lowercase());
    }

    #[test]
    fn find_model_matches_an_id_or_an_alias() {
        let v = json!({"models": [{"id": "glm", "aliases": ["glm-5.2"]}, {"id": "opus"}]});
        assert!(find_model(&v, "opus").is_some());
        assert_eq!(find_model(&v, "glm-5.2").unwrap()["id"], "glm");
        assert!(find_model(&v, "nope").is_none());
    }

    // ---- the whole client path, against an in-process stand-in for the bridge ----------

    /// A test-only target: an axum-free, hand-rolled HTTP server on a loopback port that
    /// answers the five routes the driver uses. It exists to prove the client side (submit,
    /// stream, poll, transcript, latency) without a model or a bridge binary.
    struct FakeTarget;

    struct FakeBridge {
        base: String,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl RunningBridge for FakeBridge {
        fn base_url(&self) -> &str {
            &self.base
        }
        fn token(&self) -> &str {
            "t"
        }
        fn pid(&self) -> Option<u32> {
            None
        }
        fn log_tail(&self) -> String {
            String::new()
        }
        fn stop(self: Box<Self>) -> Result<(), String> {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    impl BridgeTarget for FakeTarget {
        fn kind(&self) -> &'static str {
            "fake"
        }
        fn start(&self, vault: &Path) -> Result<Box<dyn RunningBridge>, String> {
            use std::io::{BufRead, BufReader, Read, Write};
            let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let base = format!("http://{}", l.local_addr().unwrap());
            l.set_nonblocking(true).unwrap();
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop2 = stop.clone();
            let vault = vault.to_path_buf();
            std::thread::spawn(move || {
                let mut polls = 0u32;
                let mut posts: Vec<Value> = Vec::new();
                while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
                    let Ok((mut s, _)) = l.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    s.set_nonblocking(false).unwrap();
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    let mut first = String::new();
                    r.read_line(&mut first).unwrap();
                    let mut len = 0usize;
                    let mut auth = String::new();
                    loop {
                        let mut h = String::new();
                        r.read_line(&mut h).unwrap();
                        if h == "\r\n" || h.is_empty() {
                            break;
                        }
                        let lower = h.to_ascii_lowercase();
                        if let Some(v) = lower.strip_prefix("content-length:") {
                            len = v.trim().parse().unwrap();
                        }
                        if lower.starts_with("authorization:") {
                            auth = h.trim().to_string();
                        }
                    }
                    let mut body = vec![0u8; len];
                    r.read_exact(&mut body).unwrap();
                    let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
                    let reply = |s: &mut std::net::TcpStream, code: &str, ct: &str, b: &str| {
                        let _ = write!(
                            s,
                            "HTTP/1.1 {code}\r\ncontent-type: {ct}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}",
                            b.len()
                        );
                    };
                    if !auth.ends_with("Bearer t") {
                        reply(&mut s, "401 Unauthorized", "text/plain", "no");
                        continue;
                    }
                    match path.as_str() {
                        "/health" => reply(
                            &mut s,
                            "200 OK",
                            "application/json",
                            r#"{"ok":true,"version":"9.9.9"}"#,
                        ),
                        "/jesse/models" => reply(
                            &mut s,
                            "200 OK",
                            "application/json",
                            r#"{"models":[{"id":"opus","harness":"claude-code","configured":true,"available":true}]}"#,
                        ),
                        "/jesse" => {
                            posts.push(serde_json::from_slice(&body).unwrap());
                            // The turn writes into the vault, as a real one would.
                            std::fs::write(vault.join(format!("turn{}.txt", posts.len())), "x")
                                .unwrap();
                            reply(
                                &mut s,
                                "202 Accepted",
                                "application/json",
                                &format!(r#"{{"job_id":"j{}","status":"running"}}"#, posts.len()),
                            );
                        }
                        p if p.starts_with("/jesse/stream/") => {
                            let frames = "event: reset\ndata: {\"text\":\"\"}\n\n\
                                          event: activity\ndata: {\"name\":\"Read\"}\n\n\
                                          event: delta\ndata: {\"text\":\"ok\"}\n\n\
                                          event: done\ndata: {\"response\":\"ok\"}\n\n";
                            reply(&mut s, "200 OK", "text/event-stream", frames);
                        }
                        p if p.starts_with("/jesse/result/") => {
                            polls += 1;
                            let n = posts.len();
                            let last = posts.last().cloned().unwrap_or(Value::Null);
                            // Running on the first poll of each job, done after.
                            if polls % 2 == 1 {
                                reply(
                                    &mut s,
                                    "200 OK",
                                    "application/json",
                                    r#"{"status":"running"}"#,
                                );
                            } else {
                                let answer = if n == 2 {
                                    // The follow-up must carry the first turn's session.
                                    format!(
                                        "second turn, session {}",
                                        last["session_id"].as_str().unwrap_or("none")
                                    )
                                } else {
                                    format!(
                                        "first turn, model {}",
                                        last["model"].as_str().unwrap_or("")
                                    )
                                };
                                let v = json!({
                                    "status": "done", "response": answer, "session_id": "sess-1",
                                    "timing": {"tools": [{"tool": "Read", "ms": 1}]},
                                    "usage": {"input_tokens": 7, "output_tokens": 3}
                                });
                                reply(&mut s, "200 OK", "application/json", &v.to_string());
                            }
                        }
                        _ => reply(&mut s, "404 Not Found", "text/plain", "?"),
                    }
                }
            });
            Ok(Box::new(FakeBridge { base, stop }))
        }
    }

    #[test]
    fn a_two_turn_task_runs_end_to_end_through_the_client() {
        let driver = BridgeDriver::preflight(
            Box::new(FakeTarget),
            "opus".into(),
            Duration::from_secs(20),
            Duration::from_secs(5),
        )
        .expect("preflight");
        assert_eq!(driver.harness().as_deref(), Some("claude-code"));
        assert_eq!(
            driver.endpoint().as_deref(),
            Some("fake jesse-bridge 9.9.9")
        );
        let ws = tempfile::tempdir().unwrap();
        let task: Task = serde_json::from_value(json!({
            "id": "t", "class": "c", "workspace": "fixture",
            "prompt": "remember C-417", "followups": ["what was it?"],
            "assertions": []
        }))
        .unwrap();
        let prepared = PreparedWorkspace {
            kind: Workspace::Fixture,
            dir: ws.path().to_path_buf(),
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let run = rt.block_on(driver.run_task(&task, &prepared, CancellationToken::new()));
        assert_eq!(run.error, None, "{:?}", run.error);
        assert!(run.completed);
        assert_eq!(run.answer, "second turn, session sess-1");
        assert_eq!(run.tool_names, ["Read", "Read"]);
        assert!(ws.path().join("turn2.txt").is_file());
        let l = run.latency;
        assert!(l.first_event_ms.is_some() && l.first_token_ms.is_some() && l.result_ms.is_some());
        assert!(l.first_event_ms <= l.first_token_ms && l.first_token_ms <= l.result_ms);
        // The persisted lines reparse to what was scored.
        assert_eq!(
            crate::transcript::parse(&run.lines).final_answer.as_deref(),
            Some("second turn, session sess-1")
        );
    }

    #[test]
    fn an_unconfigured_model_is_not_run_with_the_reason() {
        struct NoModel;
        impl BridgeTarget for NoModel {
            fn kind(&self) -> &'static str {
                "fake"
            }
            fn start(&self, _vault: &Path) -> Result<Box<dyn RunningBridge>, String> {
                Err("could not spawn: no such binary".into())
            }
        }
        let err = BridgeDriver::preflight(
            Box::new(NoModel),
            "glm".into(),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .err()
        .expect("not run");
        assert!(err.0.contains("no such binary"));
    }
}
