//! **THE MODEL PROXY** — `/model/<route>/<rest...>`: validate the turn token, swap the child's
//! credential for the real one, forward the body unchanged, stream the response back
//! unchanged, and meter the provider's usage block into the activity ledger.
//!
//! ---- ROUTES -----------------------------------------------------------------------------
//!
//! | route                    | upstream                                  | credential set          |
//! |--------------------------|-------------------------------------------|-------------------------|
//! | `anthropic-subscription` | `https://api.anthropic.com/<rest>`        | `authorization: Bearer <oauth>` + `oauth-2025-04-20` beta |
//! | `anthropic-api`          | `https://api.anthropic.com/<rest>`        | `x-api-key: <$anthropic_api_key_env>` |
//! | `fireworks`              | `https://api.fireworks.ai/inference/<rest>` | `authorization: Bearer <the model's own key>` |
//! | `local`                  | `<local_url>/<rest>`                      | `authorization: Bearer <the model's key>`, if it has one |
//! | `openai-chatgpt`         | `https://chatgpt.com/backend-api/codex/<rest>` | `authorization: Bearer <access JWT>` + `chatgpt-account-id` |
//!
//! **Child base URLs.** Claude Code appends `/v1/messages` to its base URL, so a Claude Code
//! child's `ANTHROPIC_BASE_URL` is `<proxy>/model/<route>`. Codex appends `/responses` (and
//! `/models`), so a Codex child's provider `base_url` is `<proxy>/model/fireworks/v1` for
//! Fireworks and `<proxy>/model/openai-chatgpt` for the ChatGPT login (whose real base,
//! `https://chatgpt.com/backend-api/codex`, has no `/v1`). With `/model/fireworks/<rest>` mapped
//! onto `https://api.fireworks.ai/inference/<rest>`, both of Fireworks' surfaces work from one
//! route: Claude Code reaches `/inference/v1/messages`, Codex `/inference/v1/responses`.
//!
//! ---- WHAT IS CHANGED ON THE WAY THROUGH, AND WHAT IS NOT ---------------------------------
//!
//! Removed from the child's request: every [`CREDENTIAL_HEADERS`] entry (`authorization`,
//! `x-api-key`, `chatgpt-account-id`, `cookie`, `proxy-authorization`), the hop-by-hop headers
//! (and any header the child's `connection` names), `host` and `content-length` (the client
//! sets both for the upstream, the latter to the same value), and `accept-encoding`. That last
//! is deliberate: the meter has to read the usage block, so the upstream is asked for an
//! identity-encoded body. The child still receives exactly the bytes the upstream sent.
//!
//! Added: the route's credential (table above). On `anthropic-subscription` the
//! `oauth-2025-04-20` beta is appended to `anthropic-beta` when absent, because Claude Code
//! omits it whenever it is handed a token rather than its own login (measured, CLI 2.1.322) and
//! the OAuth credential is refused without it. `extended-cache-ttl-2025-04-11`, which the CLI
//! also drops in that mode, is NOT added: whether it applies depends on the body, and the body
//! is the child's.
//!
//! Everything else, and the body, byte for byte. The response comes back with its status, its
//! headers minus hop-by-hop, and its body bytes as they arrive: a server-sent event stream is
//! relayed chunk by chunk, never buffered. Prompts are never pooled, shared or rewritten; the
//! only thing shared across turns is the HTTP client's connection pool to each upstream.
//!
//! `HEAD /model/<route>/api/hello` (Claude Code's connectivity check) is answered here with an
//! empty 200, before the token is looked at: it carries no prompt, reaches no upstream and
//! spends no credential, and answering it locally keeps a probe from ever holding a real one.
//!
//! ---- REFUSALS ------------------------------------------------------------------------------
//!
//! Every refusal is Anthropic's error shape, `{"type":"error","error":{"type":..,"message":..}}`:
//!   * 401 `authentication_error` — no bearer (`authorization: Bearer` or `x-api-key`), or one
//!     the [`TurnAuthority`] does not admit for THIS route. A token minted for one route is
//!     refused on every other.
//!   * 400 `unpriced_model` — a METERED call for a model with no complete four-rate
//!     [`PriceDeck`] (input, output, cache write, cache read). Nothing is sent: a call the proxy
//!     cannot price is never forwarded and never counted as zero.
//!   * 502 `credential_unavailable` — the route's real credential could not be read. The
//!     message names the source, never a value.
//!   * 502 `upstream_unreachable`, 404 `unknown_route`, 400 `invalid_request` (a `.`/`..` path
//!     segment, or an unreadable body).
//!
//! ---- METERING -----------------------------------------------------------------------------
//!
//! A METERED call is a `POST` whose path ends in `messages`, `responses` or `completions`
//! (Anthropic Messages, OpenAI Responses, OpenAI Chat Completions). `count_tokens`, `GET
//! /models`, `HEAD` and every other call are forwarded but write NO ledger line and need no
//! price: they bill nothing.
//!
//! The response body is teed to a [`UsageMeter`] as it streams: an event stream is parsed line
//! by line as it arrives (one line at most [`METER_LINE_MAX`] is held), any other body is held
//! for parsing up to [`METER_JSON_MAX`] bytes, past which it is relayed but not parsed. When the
//! response ends (or fails, or the child goes away) ONE `model_call` line is written: job,
//! route, registry model id, timing, upstream status (0 when it was never reached) and the four
//! token counts with their cost in micro-dollars. Identifiers and counts only: never a prompt,
//! a URL or a header value.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde_json::Value;
use tokio::sync::mpsc;

use super::{EnvMap, ModelTurn, ProxySettings, TurnAuthority, CREDENTIAL_HEADERS};
use crate::activity::{ActivityLedger, ActivityLine, ModelCallLine, Token};
use crate::config::{ModelKind, PriceDeck};
use crate::gateway::ClientCertToml;

// ---- ROUTES ----------------------------------------------------------------------------------

/// The Claude Code CLI's own subscription login.
pub const ROUTE_ANTHROPIC_SUBSCRIPTION: &str = "anthropic-subscription";
/// An Anthropic API key.
pub const ROUTE_ANTHROPIC_API: &str = "anthropic-api";
/// Fireworks, both its Anthropic-compatible and its OpenAI-compatible surface.
pub const ROUTE_FIREWORKS: &str = "fireworks";
/// The local model gateway.
pub const ROUTE_LOCAL: &str = "local";
/// Codex's ChatGPT login.
pub const ROUTE_OPENAI_CHATGPT: &str = "openai-chatgpt";

/// Every route the proxy serves.
pub const ROUTES: &[&str] = &[
    ROUTE_ANTHROPIC_SUBSCRIPTION,
    ROUTE_ANTHROPIC_API,
    ROUTE_FIREWORKS,
    ROUTE_LOCAL,
    ROUTE_OPENAI_CHATGPT,
];

/// The production upstream for Anthropic, both routes.
pub const ANTHROPIC_UPSTREAM: &str = "https://api.anthropic.com";
/// The production upstream for Fireworks (its `/inference` prefix included).
pub const FIREWORKS_UPSTREAM: &str = "https://api.fireworks.ai/inference";
/// The production upstream for Codex's ChatGPT login.
pub const CHATGPT_UPSTREAM: &str = "https://chatgpt.com/backend-api/codex";

/// The beta a subscription OAuth credential is refused without.
pub const OAUTH_BETA: &str = "oauth-2025-04-20";

/// The longest single event-stream line the meter holds while it waits for its newline.
pub const METER_LINE_MAX: usize = 8 * 1024 * 1024;
/// The most of a non-stream body the meter holds for parsing.
pub const METER_JSON_MAX: usize = 8 * 1024 * 1024;
/// The largest request body the proxy accepts from a child.
pub const REQUEST_BODY_MAX: usize = 128 * 1024 * 1024;

/// The proxy route a registry entry's model calls take, or `None` when the proxy has no route
/// for it (the child is then spawned as it would be with the proxy off).
///
///   * a base URL at `api.fireworks.ai` → `fireworks` (Hosted or OpenAi kind alike);
///   * `Local` kind, or a base URL at `127.0.0.1`, `localhost` or `::1` → `local`;
///   * a base URL at `api.anthropic.com` → `anthropic-api`;
///   * a base URL at `chatgpt.com` → `openai-chatgpt`;
///   * no base URL, on the `codex` harness, any kind but `OpenAi`/`Local` → `openai-chatgpt`
///     (Codex's own ChatGPT login);
///   * no base URL, `Ambient` or `Subscription`, on `claude-code` → `anthropic-subscription`;
///   * anything else (another host, the `direct` harness on the login) → `None`.
pub fn route_for(kind: &ModelKind, base_url: Option<&str>, harness: &str) -> Option<&'static str> {
    // ONE decision, made in `wiring`, so the route a child is pointed at and the route its
    // token is signed for can never disagree.
    super::wiring::route_for(*kind, base_url, harness)
}

// ---- SECRETS AND WHERE THEY COME FROM --------------------------------------------------------

/// A credential value. Its `Debug` prints a marker, never the value; [`Secret::expose`] is the
/// one way to read it, and the only caller puts it in an outbound header marked sensitive.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Where a single-valued credential comes from (the subscription OAuth token, the Anthropic API
/// key). Read on every call, so a refreshed login is picked up without a restart. An `Err` names
/// the source and why it failed, never a value.
pub trait CredentialSource: Send + Sync {
    fn credential(&self) -> Result<Secret, String>;
}

/// Codex's ChatGPT login: the access JWT and the account it belongs to.
#[derive(Debug, Clone)]
pub struct ChatGptCredential {
    pub access_token: Secret,
    pub account_id: Secret,
}

/// Where the ChatGPT login comes from. Read on every call (Codex refreshes it in place).
pub trait ChatGptSource: Send + Sync {
    fn credential(&self) -> Result<ChatGptCredential, String>;
}

/// The per-model key (`fireworks`, `local`): each registry entry's own
/// `JESSE_MODEL_*_AUTH_TOKEN`.
pub trait ModelKeys: Send + Sync {
    fn key_for(&self, model_id: &str) -> Option<Secret>;
}

/// The registry's price decks, by model id.
pub trait PriceBook: Send + Sync {
    fn deck(&self, model_id: &str) -> Option<PriceDeck>;
}

fn lookup_var(vars: &Option<EnvMap>, name: &str) -> Option<String> {
    match vars {
        Some(m) => m.get(name).cloned(),
        None => std::env::var(name).ok(),
    }
}

/// A credential held in an environment variable, by NAME (the `anthropic-api` key).
#[derive(Debug, Clone)]
pub struct EnvCredential {
    pub name: String,
    /// `None` reads the process environment; tests pass a map.
    pub vars: Option<EnvMap>,
}

impl CredentialSource for EnvCredential {
    fn credential(&self) -> Result<Secret, String> {
        match lookup_var(&self.vars, &self.name).map(|v| v.trim().to_string()) {
            Some(v) if !v.is_empty() => Ok(Secret(v)),
            _ => Err(format!("${} is not set", self.name)),
        }
    }
}

/// The subscription OAuth token, in this order: `subscription_token_file` (trimmed), the
/// variable `subscription_token_env` names, else the Claude Code CLI's own stored login — on
/// macOS the `Claude Code-credentials` Keychain item (through `/usr/bin/security`), elsewhere
/// `~/.claude/.credentials.json` — whose JSON holds `claudeAiOauth.accessToken`. All read at
/// call time. The CLI refreshes its own login; the proxy only ever reads it.
#[derive(Debug, Clone, Default)]
pub struct SubscriptionLogin {
    pub file: Option<PathBuf>,
    pub env_name: Option<String>,
    /// `None` reads the process environment; tests pass a map.
    pub vars: Option<EnvMap>,
    /// Whether to fall back to the CLI's stored login. Tests turn it off.
    pub cli_login: bool,
}

impl SubscriptionLogin {
    pub fn from_settings(s: &ProxySettings) -> SubscriptionLogin {
        SubscriptionLogin {
            file: s.subscription_token_file.clone(),
            env_name: s.subscription_token_env.clone(),
            vars: None,
            cli_login: true,
        }
    }

    fn cli_stored_login(&self) -> Result<Secret, String> {
        if cfg!(target_os = "macos") {
            // THE ACCOUNT FIRST, as the CLI itself asks. Measured on the Studio (2026-11-11):
            // the Keychain holds TWO `Claude Code-credentials` items, the live login under the
            // user's account and a stale one under `unknown` whose access token is empty, and an
            // account-less lookup returns the stale one. The account-less lookup stays as the
            // fallback for a machine with only one item.
            let user = lookup_var(&self.vars, "USER").or_else(|| lookup_var(&self.vars, "LOGNAME"));
            let mut last = Err("the Claude Code login is not in the Keychain".to_string());
            let mut attempts: Vec<Vec<String>> = Vec::new();
            if let Some(u) = user.filter(|u| !u.is_empty()) {
                attempts.push(vec!["-a".into(), u]);
            }
            attempts.push(Vec::new());
            for extra in attempts {
                let out = std::process::Command::new("/usr/bin/security")
                    .arg("find-generic-password")
                    .args(&extra)
                    .args(["-s", "Claude Code-credentials", "-w"])
                    .stdin(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .output()
                    .map_err(|e| format!("could not run /usr/bin/security: {e}"))?;
                if !out.status.success() {
                    continue;
                }
                match parse_claude_credentials(&out.stdout) {
                    Ok(s) => return Ok(s),
                    Err(e) => last = Err(e),
                }
            }
            last
        } else {
            let home = lookup_var(&self.vars, "HOME").ok_or("$HOME is not set")?;
            let path = PathBuf::from(home).join(".claude/.credentials.json");
            let bytes = std::fs::read(&path)
                .map_err(|e| format!("could not read {}: {}", path.display(), e.kind()))?;
            parse_claude_credentials(&bytes)
        }
    }
}

impl CredentialSource for SubscriptionLogin {
    fn credential(&self) -> Result<Secret, String> {
        if let Some(f) = &self.file {
            let text = std::fs::read_to_string(f)
                .map_err(|e| format!("could not read {}: {}", f.display(), e.kind()))?;
            let t = text.trim();
            if t.is_empty() {
                return Err(format!("{} is empty", f.display()));
            }
            return Ok(Secret(t.to_string()));
        }
        if let Some(n) = &self.env_name {
            return EnvCredential {
                name: n.clone(),
                vars: self.vars.clone(),
            }
            .credential();
        }
        if self.cli_login {
            return self.cli_stored_login();
        }
        Err("no subscription credential is configured".into())
    }
}

/// `claudeAiOauth.accessToken` out of the CLI's stored-login JSON. The error never quotes it.
pub fn parse_claude_credentials(bytes: &[u8]) -> Result<Secret, String> {
    let v: Value = serde_json::from_slice(bytes)
        .map_err(|_| "the Claude Code login is not JSON".to_string())?;
    v.pointer("/claudeAiOauth/accessToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| Secret(t.to_string()))
        .ok_or_else(|| "the Claude Code login holds no claudeAiOauth.accessToken".into())
}

/// Codex's `auth.json`, read on every call: `tokens.access_token` and `tokens.account_id`.
#[derive(Debug, Clone)]
pub struct CodexAuthFile {
    pub path: PathBuf,
}

impl CodexAuthFile {
    /// `codex_auth_file`, else `$CODEX_HOME/auth.json`, else `~/.codex/auth.json`. `None` when
    /// none of those can be named.
    pub fn from_settings(s: &ProxySettings, vars: Option<&EnvMap>) -> Option<CodexAuthFile> {
        let vars = vars.cloned();
        let path = s
            .codex_auth_file
            .clone()
            .or_else(|| {
                lookup_var(&vars, "CODEX_HOME")
                    .filter(|h| !h.is_empty())
                    .map(|h| PathBuf::from(h).join("auth.json"))
            })
            .or_else(|| {
                lookup_var(&vars, "HOME")
                    .filter(|h| !h.is_empty())
                    .map(|h| PathBuf::from(h).join(".codex/auth.json"))
            })?;
        Some(CodexAuthFile { path })
    }
}

impl ChatGptSource for CodexAuthFile {
    fn credential(&self) -> Result<ChatGptCredential, String> {
        let bytes = std::fs::read(&self.path)
            .map_err(|e| format!("could not read {}: {}", self.path.display(), e.kind()))?;
        parse_codex_auth(&bytes)
    }
}

/// The ChatGPT login out of Codex's `auth.json`. Refuses an API-key login (`auth_mode` other
/// than `chatgpt`): that is not this route's credential. The error never quotes a value.
pub fn parse_codex_auth(bytes: &[u8]) -> Result<ChatGptCredential, String> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|_| "Codex's auth.json is not JSON".to_string())?;
    if let Some(mode) = v.get("auth_mode").and_then(Value::as_str) {
        if !mode.eq_ignore_ascii_case("chatgpt") {
            return Err("Codex's auth.json is not a ChatGPT login".into());
        }
    }
    let field = |name: &str| {
        v.pointer(&format!("/tokens/{name}"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| Secret(t.to_string()))
            .ok_or_else(|| format!("Codex's auth.json holds no tokens.{name}"))
    };
    Ok(ChatGptCredential {
        access_token: field("access_token")?,
        account_id: field("account_id")?,
    })
}

/// A source that never has a credential: a route that is not deployed.
pub struct NoCredential;

impl CredentialSource for NoCredential {
    fn credential(&self) -> Result<Secret, String> {
        Err("this route has no credential configured".into())
    }
}

impl ChatGptSource for NoCredential {
    fn credential(&self) -> Result<ChatGptCredential, String> {
        Err("this route has no credential configured".into())
    }
}

// ---- THE PROXY --------------------------------------------------------------------------------

/// Each route's upstream base, without a trailing slash. Production values from
/// [`Upstreams::from_settings`]; tests point them at a local fake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstreams {
    pub anthropic: String,
    pub fireworks: String,
    pub chatgpt: String,
    pub local: String,
}

impl Upstreams {
    pub fn from_settings(s: &ProxySettings) -> Upstreams {
        Upstreams {
            anthropic: ANTHROPIC_UPSTREAM.into(),
            fireworks: FIREWORKS_UPSTREAM.into(),
            chatgpt: CHATGPT_UPSTREAM.into(),
            local: s.local_url.trim_end_matches('/').to_string(),
        }
    }

    fn base(&self, route: &str) -> &str {
        match route {
            ROUTE_FIREWORKS => &self.fireworks,
            ROUTE_LOCAL => &self.local,
            ROUTE_OPENAI_CHATGPT => &self.chatgpt,
            _ => &self.anthropic,
        }
    }
}

/// What the model proxy is built from.
pub struct ModelDeps {
    pub authority: Arc<dyn TurnAuthority>,
    /// `None` meters nothing (the calls are still forwarded).
    pub ledger: Option<Arc<ActivityLedger>>,
    pub prices: Arc<dyn PriceBook>,
    pub keys: Arc<dyn ModelKeys>,
    /// `anthropic-subscription`'s OAuth token ([`SubscriptionLogin`]).
    pub subscription: Arc<dyn CredentialSource>,
    /// `anthropic-api`'s key ([`EnvCredential`] on `anthropic_api_key_env`).
    pub api_key: Arc<dyn CredentialSource>,
    /// `openai-chatgpt`'s login ([`CodexAuthFile`]).
    pub chatgpt: Arc<dyn ChatGptSource>,
    pub upstreams: Upstreams,
    /// The `[gateway.client_certs]` entry `local_client_cert` names, resolved by the caller:
    /// presented to an `https` `local_url`, whose certificate must chain to its pinned CA.
    pub local_tls: Option<ClientCertToml>,
}

/// The model proxy. Holds no credential: every one is read from its source per call.
pub struct ModelProxy {
    deps: ModelDeps,
    client: reqwest::Client,
    local_client: reqwest::Client,
}

impl std::fmt::Debug for ModelProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelProxy")
            .field("upstreams", &self.deps.upstreams)
            .field("local_tls", &self.deps.local_tls.is_some())
            .field("ledger", &self.deps.ledger.is_some())
            .finish_non_exhaustive()
    }
}

fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .use_rustls_tls()
        // The proxy is the route out; it never goes through another one the environment names.
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
}

impl ModelProxy {
    /// Build the proxy and its HTTP clients. Fails when the local route's client certificate,
    /// key or pinned CA cannot be read (the message names a path, never key material).
    pub fn new(deps: ModelDeps) -> Result<ModelProxy, String> {
        let client = client_builder()
            .build()
            .map_err(|e| format!("model proxy: could not build its HTTP client: {e}"))?;
        let local_client = match &deps.local_tls {
            None => client.clone(),
            Some(c) => {
                let read = |what: &str, p: &PathBuf| {
                    std::fs::read(p).map_err(|e| {
                        format!(
                            "model proxy: could not read the local route's {what} ({}): {}",
                            p.display(),
                            e.kind()
                        )
                    })
                };
                let mut pem = read("client certificate", &c.cert_file)?;
                pem.push(b'\n');
                pem.extend_from_slice(&read("client key", &c.key_file)?);
                let identity = reqwest::Identity::from_pem(&pem).map_err(|_| {
                    "model proxy: the local route's client certificate and key are not usable PEM"
                        .to_string()
                })?;
                let ca = reqwest::Certificate::from_pem(&read("pinned CA", &c.ca_file)?).map_err(
                    |_| {
                        "model proxy: the local route's pinned CA is not a PEM certificate"
                            .to_string()
                    },
                )?;
                client_builder()
                    .identity(identity)
                    .tls_built_in_root_certs(false)
                    .add_root_certificate(ca)
                    .build()
                    .map_err(|e| format!("model proxy: could not build the local client: {e}"))?
            }
        };
        Ok(ModelProxy {
            deps,
            client,
            local_client,
        })
    }

    /// The proxy as an axum router: every path is handled by the one handler, which answers
    /// `/model/<route>/...` and 404s everything else.
    pub fn router(self: Arc<Self>) -> axum::Router {
        axum::Router::new().fallback(handle).with_state(self)
    }

    /// Serve on `listener` until the task is dropped.
    pub async fn serve(self: Arc<Self>, listener: tokio::net::TcpListener) -> std::io::Result<()> {
        axum::serve(listener, self.router()).await
    }
}

/// Anthropic's error shape, which both Claude Code and Codex surface legibly.
fn refuse(status: StatusCode, kind: &str, message: &str) -> Response {
    let body = serde_json::json!({
        "type": "error",
        "error": { "type": kind, "message": message },
    });
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// The message an `unpriced_model` refusal carries.
pub fn unpriced_message(model: &str) -> String {
    format!(
        "model {model} has no complete four-rate price deck (input, output, cache write, cache \
         read); the model proxy refuses to forward a call it cannot meter"
    )
}

/// The child's turn token: `authorization: Bearer <t>`, else `x-api-key: <t>`.
fn child_token(h: &HeaderMap) -> Option<String> {
    if let Some(v) = h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        if v.len() > 7 && v[..7].eq_ignore_ascii_case("bearer ") {
            let t = v[7..].trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    h.get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Remove the hop-by-hop headers, and any the `connection` header names.
fn strip_hop_by_hop(h: &mut HeaderMap) {
    let named: Vec<String> = h
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    for n in HOP_BY_HOP
        .iter()
        .copied()
        .chain(named.iter().map(String::as_str))
    {
        h.remove(n);
    }
}

/// The child's headers as they go upstream, before the route's credential is added.
pub fn outbound_headers(child: &HeaderMap) -> HeaderMap {
    let mut h = child.clone();
    strip_hop_by_hop(&mut h);
    for n in CREDENTIAL_HEADERS {
        h.remove(*n);
    }
    h.remove(header::HOST);
    h.remove(header::CONTENT_LENGTH);
    h.remove(header::ACCEPT_ENCODING);
    h
}

fn sensitive(value: &str) -> Result<HeaderValue, ()> {
    let mut v = HeaderValue::from_str(value).map_err(|_| ())?;
    v.set_sensitive(true);
    Ok(v)
}

/// Make sure `anthropic-beta` lists [`OAUTH_BETA`]: untouched when it does, appended to the one
/// header when there is one, a header of its own otherwise.
pub fn ensure_oauth_beta(h: &mut HeaderMap) {
    let values: Vec<String> = h
        .get_all("anthropic-beta")
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_string))
        .collect();
    if values
        .iter()
        .flat_map(|v| v.split(','))
        .any(|b| b.trim() == OAUTH_BETA)
    {
        return;
    }
    let name = HeaderName::from_static("anthropic-beta");
    match values.as_slice() {
        [one] if !one.trim().is_empty() => {
            let joined = format!("{},{OAUTH_BETA}", one.trim_end().trim_end_matches(','));
            if let Ok(v) = HeaderValue::from_str(&joined) {
                h.insert(name, v);
            }
        }
        [] => {
            h.insert(name, HeaderValue::from_static(OAUTH_BETA));
        }
        _ => {
            h.append(name, HeaderValue::from_static(OAUTH_BETA));
        }
    }
}

/// Whether a call bills: a `POST` to a Messages, Responses or Chat Completions endpoint.
pub fn is_metered(method: &Method, rest: &str) -> bool {
    let p = rest.trim_end_matches('/');
    method == Method::POST
        && (p.ends_with("messages") || p.ends_with("responses") || p.ends_with("completions"))
}

/// A complete four-rate deck, or `None`.
fn complete(deck: Option<PriceDeck>) -> Option<PriceDeck> {
    deck.filter(|d| d.cache_write_per_m.is_some())
}

fn bad_segment(rest: &str) -> bool {
    rest.split('/').any(|s| {
        let s = s.to_ascii_lowercase().replace("%2e", ".");
        s == "." || s == ".."
    })
}

/// A credential read, off the async threads (the Keychain read runs a process).
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|_| Err("the credential read did not finish".into()))
}

async fn handle(State(proxy): State<Arc<ModelProxy>>, req: Request) -> Response {
    let at_ms = crate::activity::now_ms();
    let started = Instant::now();
    let (parts, body) = req.into_parts();
    let path = parts.uri.path();
    let Some(after) = path.strip_prefix("/model/") else {
        return refuse(StatusCode::NOT_FOUND, "not_found", "no such endpoint");
    };
    let (route, rest) = after.split_once('/').unwrap_or((after, ""));
    let Some(route) = ROUTES.iter().copied().find(|r| *r == route) else {
        return refuse(
            StatusCode::NOT_FOUND,
            "unknown_route",
            "no such model route",
        );
    };
    if parts.method == Method::HEAD && rest.trim_end_matches('/') == "api/hello" {
        return StatusCode::OK.into_response();
    }
    if bad_segment(rest) {
        return refuse(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "a model proxy path may not hold a '.' or '..' segment",
        );
    }

    let Some(token) = child_token(&parts.headers) else {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "the model proxy needs the turn token as a bearer",
        );
    };
    let Some(turn) = proxy.deps.authority.model_turn(&token, route) else {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "the model proxy does not accept this token on this route",
        );
    };

    let metered = is_metered(&parts.method, rest);
    let deck = if metered {
        match complete(proxy.deps.prices.deck(&turn.model)) {
            Some(d) => Some(d),
            None => {
                return refuse(
                    StatusCode::BAD_REQUEST,
                    "unpriced_model",
                    &unpriced_message(&turn.model),
                )
            }
        }
    } else {
        None
    };
    let call = Call {
        proxy: proxy.clone(),
        turn,
        route,
        at_ms,
        started,
        deck,
    };

    let body = match axum::body::to_bytes(body, REQUEST_BODY_MAX).await {
        Ok(b) => b,
        Err(_) => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "the request body could not be read",
            )
        }
    };

    let asked_for_events = parts
        .headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("text/event-stream"));
    let mut headers = outbound_headers(&parts.headers);
    if let Err(e) = call.add_credential(&mut headers).await {
        call.record(0, Usage::default());
        return refuse(StatusCode::BAD_GATEWAY, "credential_unavailable", &e);
    }

    let mut url = format!(
        "{}/{}",
        proxy.deps.upstreams.base(route).trim_end_matches('/'),
        rest
    );
    if let Some(q) = parts.uri.query() {
        url.push('?');
        url.push_str(q);
    }
    let client = if route == ROUTE_LOCAL {
        &proxy.local_client
    } else {
        &proxy.client
    };
    let upstream = client
        .request(parts.method.clone(), &url)
        .headers(headers)
        .body(body)
        .send()
        .await;
    let mut upstream = match upstream {
        Ok(r) => r,
        Err(_) => {
            call.record(0, Usage::default());
            return refuse(
                StatusCode::BAD_GATEWAY,
                "upstream_unreachable",
                &format!("the model proxy could not reach the {route} upstream"),
            );
        }
    };

    let status = upstream.status();
    let mut out_headers = upstream.headers().clone();
    strip_hop_by_hop(&mut out_headers);
    // AN EVENT STREAM BY ITS CONTENT TYPE, or, when the upstream names none, by what the
    // child asked for. Measured 2026-11-11: the ChatGPT backend answers Codex's
    // `accept: text/event-stream` request with an SSE body and NO `content-type` at all, and a
    // meter that read that as a JSON body recorded the call as zero tokens.
    let content_type = out_headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_ascii_lowercase);
    let event_stream = match content_type {
        Some(ct) => ct.contains("text/event-stream"),
        None => asked_for_events,
    };

    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    tokio::spawn(async move {
        let mut meter = call.deck.map(|_| UsageMeter::new(event_stream));
        loop {
            match upstream.chunk().await {
                Ok(Some(chunk)) => {
                    if let Some(m) = meter.as_mut() {
                        m.feed(&chunk);
                    }
                    if tx.send(Ok(chunk)).await.is_err() {
                        // The child went away; what was metered so far is what is recorded.
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    let _ = tx
                        .send(Err(std::io::Error::other("the upstream stream failed")))
                        .await;
                    break;
                }
            }
        }
        if let Some(m) = meter {
            call.record(status.as_u16(), m.finish());
        }
        // `tx` drops here, AFTER the line is written: the child sees the end of the body only
        // once its call is on the ledger.
        drop(tx);
    });

    let mut resp = Response::new(Body::from_stream(ChannelBody(rx)));
    *resp.status_mut() = status;
    *resp.headers_mut() = out_headers;
    resp
}

/// One admitted call, carried to the end of its response.
struct Call {
    proxy: Arc<ModelProxy>,
    turn: ModelTurn,
    route: &'static str,
    at_ms: u64,
    started: Instant,
    /// `Some` exactly when the call is metered.
    deck: Option<PriceDeck>,
}

impl Call {
    /// Set the route's real credential on `h`. The error names a source, never a value.
    async fn add_credential(&self, h: &mut HeaderMap) -> Result<(), String> {
        let d = &self.proxy.deps;
        let bad = |what: &str| format!("the {what} for route {} is not a valid header", self.route);
        match self.route {
            ROUTE_ANTHROPIC_SUBSCRIPTION => {
                let src = d.subscription.clone();
                let s = blocking(move || src.credential()).await?;
                let v = sensitive(&format!("Bearer {}", s.expose()))
                    .map_err(|_| bad("subscription credential"))?;
                h.insert(header::AUTHORIZATION, v);
                ensure_oauth_beta(h);
            }
            ROUTE_ANTHROPIC_API => {
                let src = d.api_key.clone();
                let s = blocking(move || src.credential()).await?;
                h.insert(
                    "x-api-key",
                    sensitive(s.expose()).map_err(|_| bad("API key"))?,
                );
            }
            ROUTE_FIREWORKS | ROUTE_LOCAL => match d.keys.key_for(&self.turn.model) {
                Some(k) => {
                    let v = sensitive(&format!("Bearer {}", k.expose()))
                        .map_err(|_| bad("model key"))?;
                    h.insert(header::AUTHORIZATION, v);
                }
                None if self.route == ROUTE_LOCAL => {}
                None => {
                    return Err(format!(
                        "model {} has no key configured for route {}",
                        self.turn.model, self.route
                    ))
                }
            },
            ROUTE_OPENAI_CHATGPT => {
                let src = d.chatgpt.clone();
                let c = blocking(move || src.credential()).await?;
                let v = sensitive(&format!("Bearer {}", c.access_token.expose()))
                    .map_err(|_| bad("ChatGPT access token"))?;
                h.insert(header::AUTHORIZATION, v);
                h.insert(
                    "chatgpt-account-id",
                    sensitive(c.account_id.expose()).map_err(|_| bad("ChatGPT account id"))?,
                );
            }
            _ => return Err("unknown route".into()),
        }
        Ok(())
    }

    /// Write the call's one ledger line. Unmetered calls write none.
    fn record(&self, status: u16, usage: Usage) {
        let (Some(deck), Some(ledger)) = (self.deck, self.proxy.deps.ledger.as_ref()) else {
            return;
        };
        ledger.record(&ActivityLine::model_call(self.line(status, usage, &deck)));
    }

    fn line(&self, status: u16, u: Usage, deck: &PriceDeck) -> ModelCallLine {
        ModelCallLine {
            job_id: Token::new(&self.turn.job_id),
            route: Token::new(self.route),
            model: Token::new(&self.turn.model),
            at_ms: self.at_ms,
            duration_ms: self.started.elapsed().as_millis() as u64,
            status: status as u64,
            input_tokens: u.input,
            output_tokens: u.output,
            cache_write_tokens: u.cache_write,
            cache_read_tokens: u.cache_read,
            cost_micros: cost_micros(&u, deck),
        }
    }
}

/// The response body the child reads: whatever the pump task sends.
struct ChannelBody(mpsc::Receiver<Result<Bytes, std::io::Error>>);

impl futures_core::Stream for ChannelBody {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

// ---- METERING ---------------------------------------------------------------------------------

/// The four counts a call is priced on. `input` EXCLUDES cached tokens on every provider (the
/// OpenAI shapes, whose input includes them, are converted).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

/// Millionths of a dollar: `Σ tokens × $/Mtok`, rounded. (Dollars are that over 10^6; micros
/// multiply it back.) A deck with no cache-write rate is never passed here.
pub fn cost_micros(u: &Usage, d: &PriceDeck) -> u64 {
    let micros = u.input as f64 * d.in_per_m
        + u.output as f64 * d.out_per_m
        + u.cache_write as f64 * d.cache_write_per_m.unwrap_or(d.in_per_m)
        + u.cache_read as f64 * d.cached_per_m;
    if micros.is_finite() && micros > 0.0 {
        micros.round() as u64
    } else {
        0
    }
}

fn num(v: &Value, ptr: &str) -> Option<u64> {
    v.pointer(ptr).and_then(Value::as_u64)
}

/// Reads a provider's usage out of a response body fed to it chunk by chunk.
#[derive(Debug)]
pub struct UsageMeter {
    event_stream: bool,
    /// The partial event-stream line, or the held non-stream body.
    buf: Vec<u8>,
    /// A line (or body) overran its cap; skip until the next newline (or for good).
    overflow: bool,
    usage: Usage,
}

impl UsageMeter {
    pub fn new(event_stream: bool) -> UsageMeter {
        UsageMeter {
            event_stream,
            buf: Vec::new(),
            overflow: false,
            usage: Usage::default(),
        }
    }

    pub fn feed(&mut self, mut chunk: &[u8]) {
        if !self.event_stream {
            if !self.overflow {
                if self.buf.len() + chunk.len() > METER_JSON_MAX {
                    self.overflow = true;
                    self.buf = Vec::new();
                } else {
                    self.buf.extend_from_slice(chunk);
                }
            }
            return;
        }
        while let Some(nl) = chunk.iter().position(|b| *b == b'\n') {
            if !self.overflow {
                self.buf.extend_from_slice(&chunk[..nl]);
                let line = std::mem::take(&mut self.buf);
                self.sse_line(&line);
            }
            self.buf.clear();
            self.overflow = false;
            chunk = &chunk[nl + 1..];
        }
        if !self.overflow {
            if self.buf.len() + chunk.len() > METER_LINE_MAX {
                self.overflow = true;
                self.buf = Vec::new();
            } else {
                self.buf.extend_from_slice(chunk);
            }
        }
    }

    /// The usage, once the body has ended.
    pub fn finish(mut self) -> Usage {
        if self.event_stream {
            if !self.overflow && !self.buf.is_empty() {
                let line = std::mem::take(&mut self.buf);
                self.sse_line(&line);
            }
        } else if !self.overflow {
            if let Ok(v) = serde_json::from_slice::<Value>(&self.buf) {
                self.json_body(&v);
            }
        }
        self.usage
    }

    fn sse_line(&mut self, line: &[u8]) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(data) = line.strip_prefix(b"data:") else {
            return;
        };
        let data = data.strip_prefix(b" ").unwrap_or(data);
        if data.is_empty() || data == b"[DONE]" {
            return;
        }
        let Ok(v) = serde_json::from_slice::<Value>(data) else {
            return;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(u) = v.pointer("/message/usage") {
                    self.anthropic(u, false);
                }
            }
            Some("message_delta") => {
                if let Some(u) = v.get("usage") {
                    self.anthropic(u, true);
                }
            }
            Some("response.completed" | "response.incomplete" | "response.failed") => {
                if let Some(u) = v.pointer("/response/usage").filter(|u| u.is_object()) {
                    self.openai_responses(u);
                }
            }
            _ => {
                // A chat-completions chunk carries `usage` on its last chunk only.
                if let Some(u) = v.get("usage").filter(|u| u.get("prompt_tokens").is_some()) {
                    self.openai_chat(u);
                }
            }
        }
    }

    fn json_body(&mut self, v: &Value) {
        let Some(u) = v.get("usage").filter(|u| u.is_object()) else {
            return;
        };
        if u.get("prompt_tokens").is_some() {
            self.openai_chat(u);
        } else if v.get("object").and_then(Value::as_str) == Some("response")
            || u.get("input_tokens_details").is_some()
        {
            self.openai_responses(u);
        } else {
            self.anthropic(u, false);
        }
    }

    /// Anthropic's usage block. On `message_start` (and a whole JSON body) every field is
    /// taken. On `message_delta`, `output_tokens` is cumulative and always taken; the input
    /// and cache fields only when present and non-zero.
    fn anthropic(&mut self, u: &Value, delta: bool) {
        let take = |slot: &mut u64, key: &str| {
            if let Some(n) = num(u, &format!("/{key}")) {
                if !delta || n > 0 {
                    *slot = n;
                }
            }
        };
        take(&mut self.usage.input, "input_tokens");
        take(&mut self.usage.cache_write, "cache_creation_input_tokens");
        take(&mut self.usage.cache_read, "cache_read_input_tokens");
        if let Some(n) = num(u, "/output_tokens") {
            self.usage.output = n;
        }
    }

    /// OpenAI Responses: input includes the cached tokens, which are a cache read.
    fn openai_responses(&mut self, u: &Value) {
        let input = num(u, "/input_tokens").unwrap_or(0);
        let cached = num(u, "/input_tokens_details/cached_tokens").unwrap_or(0);
        self.usage = Usage {
            input: input.saturating_sub(cached),
            output: num(u, "/output_tokens").unwrap_or(0),
            cache_write: 0,
            cache_read: cached,
        };
    }

    /// OpenAI Chat Completions: the same, with the older names.
    fn openai_chat(&mut self, u: &Value) {
        let input = num(u, "/prompt_tokens").unwrap_or(0);
        let cached = num(u, "/prompt_tokens_details/cached_tokens").unwrap_or(0);
        self.usage = Usage {
            input: input.saturating_sub(cached),
            output: num(u, "/completion_tokens").unwrap_or(0),
            cache_write: 0,
            cache_read: cached,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{Claims, TokenKey};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    const FIX_ANTHROPIC_SSE: &[u8] =
        include_bytes!("../../tests/fixtures/model-proxy/anthropic-messages.sse");
    const FIX_ANTHROPIC_JSON: &[u8] =
        include_bytes!("../../tests/fixtures/model-proxy/anthropic-messages.json");
    const FIX_RESPONSES_SSE: &[u8] =
        include_bytes!("../../tests/fixtures/model-proxy/openai-responses.sse");
    const FIX_RESPONSES_JSON: &[u8] =
        include_bytes!("../../tests/fixtures/model-proxy/openai-responses.json");
    const FIX_FIREWORKS_SSE: &[u8] =
        include_bytes!("../../tests/fixtures/model-proxy/fireworks-messages.sse");
    const FIX_CHAT_JSON: &[u8] =
        include_bytes!("../../tests/fixtures/model-proxy/openai-chat.json");

    /// Words from the fixtures' prompts and completions; none may reach a ledger line.
    const MARKER: &str = "PROMPT-ECHO-MARKER";

    const OPUS: PriceDeck = PriceDeck {
        in_per_m: 15.0,
        cached_per_m: 1.5,
        cache_write_per_m: Some(18.75),
        out_per_m: 75.0,
    };
    const CODEX: PriceDeck = PriceDeck {
        in_per_m: 1.25,
        cached_per_m: 0.125,
        cache_write_per_m: Some(0.0),
        out_per_m: 10.0,
    };
    const GLM: PriceDeck = PriceDeck {
        in_per_m: 0.6,
        cached_per_m: 0.3,
        cache_write_per_m: Some(0.6),
        out_per_m: 2.2,
    };

    // ---- fakes -------------------------------------------------------------------------

    /// The real gateway key, behind the seam the gateway will implement.
    struct KeyAuthority(TokenKey);

    impl TurnAuthority for KeyAuthority {
        fn egress_turn(&self, _: &str, _: &str) -> Option<super::super::EgressTurn> {
            None
        }
        fn model_turn(&self, token: &str, route: &str) -> Option<ModelTurn> {
            let c = self
                .0
                .verify_route(token, route, crate::gateway::token::now_secs())
                .ok()?;
            Some(ModelTurn {
                job_id: c.job,
                model: c.metered,
            })
        }
    }

    fn mint(key: &TokenKey, route: &str, model: &str) -> String {
        key.sign(&Claims {
            job: "job-42".into(),
            cap: "basic".into(),
            servers: Vec::new(),
            tier: String::new(),
            route: route.into(),
            metered: model.into(),
            exp: crate::gateway::token::now_secs() + 600,
        })
    }

    struct Prices(BTreeMap<&'static str, PriceDeck>);
    impl PriceBook for Prices {
        fn deck(&self, m: &str) -> Option<PriceDeck> {
            self.0.get(m).copied()
        }
    }

    struct Keys;
    impl ModelKeys for Keys {
        fn key_for(&self, m: &str) -> Option<Secret> {
            match m {
                "glm" => Some(Secret::new("fw-key-glm")),
                "qwen-local" => Some(Secret::new("local-key")),
                _ => None,
            }
        }
    }

    struct Fixed(&'static str);
    impl CredentialSource for Fixed {
        fn credential(&self) -> Result<Secret, String> {
            Ok(Secret::new(self.0))
        }
    }

    struct FixedChatGpt;
    impl ChatGptSource for FixedChatGpt {
        fn credential(&self) -> Result<ChatGptCredential, String> {
            Ok(ChatGptCredential {
                access_token: Secret::new("chatgpt-jwt"),
                account_id: Secret::new("acct-123"),
            })
        }
    }

    #[derive(Debug, Clone)]
    struct Seen {
        method: String,
        target: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    struct Reply {
        status: u16,
        content_type: &'static str,
        body: Vec<u8>,
        /// Send `body[..split]`, wait for the gate, then the rest.
        gate: Option<(usize, Arc<tokio::sync::Notify>)>,
    }

    #[derive(Default)]
    struct Fake {
        seen: Mutex<Vec<Seen>>,
        reply: Mutex<Option<Reply>>,
    }

    async fn fake_handler(State(f): State<Arc<Fake>>, req: Request) -> Response {
        let (parts, body) = req.into_parts();
        let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let mut headers: Vec<(String, String)> = parts
            .headers
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string()))
            .collect();
        headers.sort();
        f.seen.lock().unwrap().push(Seen {
            method: parts.method.to_string(),
            target: parts.uri.to_string(),
            headers,
            body: body.to_vec(),
        });
        let r = f.reply.lock().unwrap().take().unwrap_or(Reply {
            status: 200,
            content_type: "application/json",
            body: b"{}".to_vec(),
            gate: None,
        });
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            match r.gate {
                None => {
                    let _ = tx.send(Ok(Bytes::from(r.body))).await;
                }
                Some((split, gate)) => {
                    let _ = tx.send(Ok(Bytes::copy_from_slice(&r.body[..split]))).await;
                    gate.notified().await;
                    let _ = tx.send(Ok(Bytes::copy_from_slice(&r.body[split..]))).await;
                }
            }
        });
        let mut resp = Response::new(Body::from_stream(ChannelBody(rx)));
        *resp.status_mut() = StatusCode::from_u16(r.status).unwrap();
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(r.content_type),
        );
        resp.headers_mut()
            .insert("request-id", HeaderValue::from_static("req_fake_1"));
        resp
    }

    struct Rig {
        proxy_url: String,
        fake: Arc<Fake>,
        key: TokenKey,
        state: PathBuf,
        http: reqwest::Client,
    }

    impl Rig {
        async fn new() -> Rig {
            let fake = Arc::new(Fake::default());
            let fl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let fake_addr = fl.local_addr().unwrap();
            let app = axum::Router::new()
                .fallback(fake_handler)
                .with_state(fake.clone());
            tokio::spawn(async move { axum::serve(fl, app).await });
            let base = format!("http://{fake_addr}");

            // Two handles on one key: the authority's and the test's minter.
            let key_bytes: [u8; 32] = rand_bytes();
            let key = TokenKey::from_bytes(&key_bytes);
            let state = std::env::temp_dir().join(format!(
                "model-proxy-test-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let prices = Prices(BTreeMap::from([
                ("opus", OPUS),
                ("codex-chatgpt", CODEX),
                ("glm", GLM),
                ("qwen-local", GLM),
                ("sonnet-api", OPUS),
                (
                    "half-priced",
                    PriceDeck {
                        cache_write_per_m: None,
                        ..OPUS
                    },
                ),
            ]));
            let proxy = ModelProxy::new(ModelDeps {
                authority: Arc::new(KeyAuthority(TokenKey::from_bytes(&key_bytes))),
                ledger: Some(Arc::new(ActivityLedger::new(&state))),
                prices: Arc::new(prices),
                keys: Arc::new(Keys),
                subscription: Arc::new(Fixed("oauth-real")),
                api_key: Arc::new(Fixed("sk-ant-real")),
                chatgpt: Arc::new(FixedChatGpt),
                upstreams: Upstreams {
                    anthropic: format!("{base}/anthropic"),
                    fireworks: format!("{base}/inference"),
                    chatgpt: format!("{base}/backend-api/codex"),
                    local: format!("{base}/local"),
                },
                local_tls: None,
            })
            .unwrap();
            let pl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy_url = format!("http://{}", pl.local_addr().unwrap());
            tokio::spawn(Arc::new(proxy).serve(pl));
            Rig {
                proxy_url,
                fake,
                key,
                state,
                http: reqwest::Client::builder().no_proxy().build().unwrap(),
            }
        }

        fn reply(&self, status: u16, content_type: &'static str, body: &[u8]) {
            *self.fake.reply.lock().unwrap() = Some(Reply {
                status,
                content_type,
                body: body.to_vec(),
                gate: None,
            });
        }

        fn seen(&self) -> Vec<Seen> {
            self.fake.seen.lock().unwrap().clone()
        }

        fn ledger(&self) -> Vec<Value> {
            let dir = self.state.join("activity");
            let mut out = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    for l in std::fs::read_to_string(e.path()).unwrap().lines() {
                        out.push(serde_json::from_str(l).unwrap());
                    }
                }
            }
            out
        }

        async fn post(
            &self,
            path: &str,
            headers: &[(&str, &str)],
            body: &[u8],
        ) -> (u16, Vec<(String, String)>, Vec<u8>) {
            let mut rb = self
                .http
                .post(format!("{}{path}", self.proxy_url))
                .body(body.to_vec());
            for (k, v) in headers {
                rb = rb.header(*k, *v);
            }
            let r = rb.send().await.unwrap();
            let status = r.status().as_u16();
            let hs = r
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            let b = r.bytes().await.unwrap().to_vec();
            (status, hs, b)
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.state);
        }
    }

    fn rand_bytes() -> [u8; 32] {
        let mut b = [0u8; 32];
        let u = uuid::Uuid::new_v4();
        b[..16].copy_from_slice(u.as_bytes());
        b[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        b
    }

    /// Claude Code 2.1.322 handed `ANTHROPIC_AUTH_TOKEN=<turn token>` (measured): no OAuth beta,
    /// no extended-cache-ttl beta.
    fn claude_code_headers(token: &str) -> Vec<(String, String)> {
        vec![
            ("accept".into(), "application/json".into()),
            ("anthropic-beta".into(), "claude-code-20250219,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14".into()),
            ("anthropic-dangerous-direct-browser-access".into(), "true".into()),
            ("anthropic-version".into(), "2023-06-01".into()),
            ("authorization".into(), format!("Bearer {token}")),
            ("content-type".into(), "application/json".into()),
            ("user-agent".into(), "claude-cli/2.1.322 (external, cli)".into()),
            ("x-app".into(), "cli".into()),
            ("x-claude-code-session-id".into(), "5b0c2d4e-6f70-4a8b-9c1d-2e3f4a5b6c7d".into()),
            ("x-stainless-arch".into(), "arm64".into()),
            ("x-stainless-lang".into(), "js".into()),
            ("x-stainless-os".into(), "MacOS".into()),
            ("x-stainless-package-version".into(), "0.60.0".into()),
            ("x-stainless-retry-count".into(), "0".into()),
            ("x-stainless-runtime".into(), "node".into()),
            ("x-stainless-runtime-version".into(), "v24.3.0".into()),
            ("x-stainless-timeout".into(), "600".into()),
        ]
    }

    /// codex-cli 0.153.4 as a custom provider with `env_key` (measured): the turn token as the
    /// bearer, no `chatgpt-account-id`.
    fn codex_headers(token: &str) -> Vec<(String, String)> {
        vec![
            ("accept".into(), "text/event-stream".into()),
            ("authorization".into(), format!("Bearer {token}")),
            ("content-type".into(), "application/json".into()),
            ("originator".into(), "codex_exec".into()),
            (
                "session_id".into(),
                "0199aa11-2233-7445-8899-aabbccddeeff".into(),
            ),
            (
                "thread_id".into(),
                "0199aa11-2233-7445-8899-aabbccddee00".into(),
            ),
            (
                "user-agent".into(),
                "codex_exec/0.153.4 (Mac OS 26.1.0; arm64)".into(),
            ),
            ("version".into(), "0.153.4".into()),
            ("x-codex-turn-metadata".into(), "{\"turn\":1}".into()),
        ]
    }

    fn as_refs(v: &[(String, String)]) -> Vec<(&str, &str)> {
        v.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect()
    }

    /// What the upstream should see: the child's set minus its credential, with `edit` applied,
    /// plus `host` and `content-length`, sorted.
    fn expected(
        child: &[(String, String)],
        host: &str,
        len: usize,
        edit: impl FnOnce(&mut Vec<(String, String)>),
    ) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = child
            .iter()
            .filter(|(k, _)| k != "authorization" && k != "x-api-key")
            .cloned()
            .collect();
        edit(&mut v);
        v.push(("host".into(), host.into()));
        v.push(("content-length".into(), len.to_string()));
        v.sort();
        v
    }

    fn host_of(url: &str) -> String {
        url.trim_start_matches("http://").to_string()
    }

    fn fake_host(rig: &Rig) -> String {
        // Every route's upstream is the one fake.
        let s = rig.seen();
        s.last()
            .and_then(|x| x.headers.iter().find(|(k, _)| k == "host"))
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| host_of(&rig.proxy_url))
    }

    const BODY: &[u8] = br#"{"model":"claude-opus-4-1","max_tokens":64,"stream":true,"messages":[{"role":"user","content":"PROMPT-ECHO-MARKER what goes in pancakes?"}]}"#;

    // ---- header replacement, per route -----------------------------------------------

    #[tokio::test]
    async fn subscription_route_swaps_the_bearer_and_adds_the_oauth_beta() {
        let rig = Rig::new().await;
        let t = mint(&rig.key, ROUTE_ANTHROPIC_SUBSCRIPTION, "opus");
        let child = claude_code_headers(&t);
        rig.reply(200, "text/event-stream", FIX_ANTHROPIC_SSE);
        let (status, _, body) = rig
            .post(
                "/model/anthropic-subscription/v1/messages?beta=true",
                &as_refs(&child),
                BODY,
            )
            .await;
        assert_eq!(status, 200);
        assert_eq!(
            body, FIX_ANTHROPIC_SSE,
            "the stream comes back byte-identical"
        );
        let seen = rig.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].target, "/anthropic/v1/messages?beta=true");
        assert_eq!(seen[0].body, BODY, "the body goes up byte-identical");
        let want = expected(&child, &fake_host(&rig), BODY.len(), |v| {
            for (k, val) in v.iter_mut() {
                if k == "anthropic-beta" {
                    val.push_str(",oauth-2025-04-20");
                }
            }
            v.push(("authorization".into(), "Bearer oauth-real".into()));
        });
        assert_eq!(seen[0].headers, want);
        assert!(!seen[0].headers.iter().any(|(_, v)| v.contains(&t)));
    }

    #[tokio::test]
    async fn anthropic_api_route_sends_the_api_key_and_no_bearer() {
        let rig = Rig::new().await;
        let t = mint(&rig.key, ROUTE_ANTHROPIC_API, "sonnet-api");
        let mut child = claude_code_headers(&t);
        // Given ANTHROPIC_API_KEY instead, the CLI sends x-api-key; both must go.
        child.push(("x-api-key".into(), t.clone()));
        rig.reply(200, "application/json", FIX_ANTHROPIC_JSON);
        let (status, _, body) = rig
            .post("/model/anthropic-api/v1/messages", &as_refs(&child), BODY)
            .await;
        assert_eq!(status, 200);
        assert_eq!(body, FIX_ANTHROPIC_JSON);
        let seen = rig.seen();
        assert_eq!(seen[0].target, "/anthropic/v1/messages");
        let want = expected(&child, &fake_host(&rig), BODY.len(), |v| {
            v.push(("x-api-key".into(), "sk-ant-real".into()));
        });
        assert_eq!(seen[0].headers, want);
        assert_eq!(seen[0].body, BODY);
    }

    #[tokio::test]
    async fn fireworks_route_maps_both_surfaces_and_uses_the_models_own_key() {
        let rig = Rig::new().await;
        let t = mint(&rig.key, ROUTE_FIREWORKS, "glm");
        // Claude Code: base `<proxy>/model/fireworks`, appends /v1/messages.
        let child = claude_code_headers(&t);
        rig.reply(200, "text/event-stream", FIX_FIREWORKS_SSE);
        let (s, _, b) = rig
            .post("/model/fireworks/v1/messages", &as_refs(&child), BODY)
            .await;
        assert_eq!((s, b.as_slice()), (200, FIX_FIREWORKS_SSE));
        // Codex: base `<proxy>/model/fireworks/v1`, appends /responses.
        let cx = codex_headers(&t);
        rig.reply(200, "application/json", FIX_RESPONSES_JSON);
        let (s, _, b) = rig
            .post("/model/fireworks/v1/responses", &as_refs(&cx), BODY)
            .await;
        assert_eq!((s, b.as_slice()), (200, FIX_RESPONSES_JSON));
        let seen = rig.seen();
        assert_eq!(seen[0].target, "/inference/v1/messages");
        assert_eq!(seen[1].target, "/inference/v1/responses");
        let host = fake_host(&rig);
        assert_eq!(
            seen[0].headers,
            expected(&child, &host, BODY.len(), |v| v
                .push(("authorization".into(), "Bearer fw-key-glm".into())))
        );
        assert_eq!(
            seen[1].headers,
            expected(&cx, &host, BODY.len(), |v| v
                .push(("authorization".into(), "Bearer fw-key-glm".into())))
        );
        let lines = rig.ledger();
        assert_eq!(lines.len(), 2);
        // Fireworks' Anthropic surface: input from message_start, cache read and output from
        // message_delta (its zero input_tokens there is not taken).
        let l = &lines[0];
        assert_eq!(l["route"], "fireworks");
        assert_eq!(l["input_tokens"], 4321);
        assert_eq!(l["cache_read_tokens"], 1200);
        assert_eq!(l["output_tokens"], 77);
        let want = cost_micros(
            &Usage {
                input: 4321,
                output: 77,
                cache_write: 0,
                cache_read: 1200,
            },
            &GLM,
        );
        assert_eq!(l["cost_micros"], want);
        // Its Responses surface, non-streamed: input excludes the cached tokens.
        assert_eq!(lines[1]["input_tokens"], 800);
        assert_eq!(lines[1]["cache_read_tokens"], 100);
        assert_eq!(lines[1]["output_tokens"], 42);
    }

    #[tokio::test]
    async fn local_route_sends_the_models_key_when_it_has_one() {
        let rig = Rig::new().await;
        let t = mint(&rig.key, ROUTE_LOCAL, "qwen-local");
        let child = claude_code_headers(&t);
        rig.reply(200, "application/json", FIX_ANTHROPIC_JSON);
        let (s, _, b) = rig
            .post("/model/local/v1/messages", &as_refs(&child), BODY)
            .await;
        assert_eq!((s, b.as_slice()), (200, FIX_ANTHROPIC_JSON));
        let seen = rig.seen();
        assert_eq!(seen[0].target, "/local/v1/messages");
        assert_eq!(
            seen[0].headers,
            expected(&child, &fake_host(&rig), BODY.len(), |v| v
                .push(("authorization".into(), "Bearer local-key".into())))
        );
    }

    #[tokio::test]
    async fn chatgpt_route_sets_the_login_and_its_account() {
        let rig = Rig::new().await;
        let t = mint(&rig.key, ROUTE_OPENAI_CHATGPT, "codex-chatgpt");
        let mut child = codex_headers(&t);
        // A stray account id from the child is replaced, never forwarded.
        child.push(("chatgpt-account-id".into(), "child-acct".into()));
        rig.reply(200, "text/event-stream", FIX_RESPONSES_SSE);
        let (s, _, b) = rig
            .post("/model/openai-chatgpt/responses", &as_refs(&child), BODY)
            .await;
        assert_eq!((s, b.as_slice()), (200, FIX_RESPONSES_SSE));
        let seen = rig.seen();
        assert_eq!(seen[0].target, "/backend-api/codex/responses");
        let want = expected(&child, &fake_host(&rig), BODY.len(), |v| {
            v.retain(|(k, _)| k != "chatgpt-account-id");
            v.push(("authorization".into(), "Bearer chatgpt-jwt".into()));
            v.push(("chatgpt-account-id".into(), "acct-123".into()));
        });
        assert_eq!(seen[0].headers, want);
        let l = &rig.ledger()[0];
        assert_eq!(l["kind"], "model_call");
        assert_eq!(l["route"], "openai-chatgpt");
        assert_eq!(l["model"], "codex-chatgpt");
        assert_eq!(l["job_id"], "job-42");
        assert_eq!(l["status"], 200);
        assert_eq!(l["input_tokens"], 1104);
        assert_eq!(l["cache_read_tokens"], 4096);
        assert_eq!(l["cache_write_tokens"], 0);
        assert_eq!(l["output_tokens"], 350);
        // 1104×1.25 + 4096×0.125 + 350×10
        assert_eq!(l["cost_micros"], 1380 + 512 + 3500);
    }

    #[tokio::test]
    async fn hop_by_hop_and_encoding_headers_are_not_forwarded_but_response_headers_are() {
        let rig = Rig::new().await;
        let t = mint(&rig.key, ROUTE_ANTHROPIC_SUBSCRIPTION, "opus");
        let mut child = claude_code_headers(&t);
        child.push(("connection".into(), "keep-alive, x-drop-me".into()));
        child.push(("x-drop-me".into(), "1".into()));
        child.push(("accept-encoding".into(), "gzip, br".into()));
        child.push(("cookie".into(), "a=b".into()));
        rig.reply(200, "application/json", FIX_ANTHROPIC_JSON);
        let (_, rh, _) = rig
            .post(
                "/model/anthropic-subscription/v1/messages",
                &as_refs(&child),
                BODY,
            )
            .await;
        let names: Vec<String> = rig.seen()[0]
            .headers
            .iter()
            .map(|(k, _)| k.clone())
            .collect();
        for gone in ["x-drop-me", "accept-encoding", "cookie"] {
            assert!(!names.iter().any(|n| n == gone), "{gone} was forwarded");
        }
        assert!(rh
            .iter()
            .any(|(k, v)| k == "request-id" && v == "req_fake_1"));
    }

    // ---- scoping and refusals ------------------------------------------------------

    #[tokio::test]
    async fn a_token_is_admitted_on_its_own_route_only() {
        let rig = Rig::new().await;
        let fw = mint(&rig.key, ROUTE_FIREWORKS, "glm");
        for (path, ok) in [
            ("/model/local/v1/messages", false),
            ("/model/anthropic-subscription/v1/messages", false),
            ("/model/openai-chatgpt/responses", false),
            ("/model/fireworks/v1/messages", true),
        ] {
            rig.reply(200, "application/json", FIX_ANTHROPIC_JSON);
            let (s, _, b) = rig
                .post(path, &[("authorization", &format!("Bearer {fw}"))], BODY)
                .await;
            if ok {
                assert_eq!(s, 200, "{path}");
            } else {
                assert_eq!(s, 401, "{path}");
                let v: Value = serde_json::from_slice(&b).unwrap();
                assert_eq!(v["error"]["type"], "authentication_error");
            }
        }
        assert_eq!(
            rig.seen().len(),
            1,
            "only the right route reached the upstream"
        );
        // A token with no route at all (proxy off when minted) is refused everywhere.
        let none = mint(&rig.key, "", "glm");
        let (s, _, _) = rig
            .post(
                "/model/fireworks/v1/messages",
                &[("authorization", &format!("Bearer {none}"))],
                BODY,
            )
            .await;
        assert_eq!(s, 401);
    }

    #[tokio::test]
    async fn a_missing_garbage_or_forged_token_is_401_and_reaches_nothing() {
        let rig = Rig::new().await;
        let forged = mint(&TokenKey::generate(), ROUTE_FIREWORKS, "glm");
        let cases: Vec<Vec<(&str, String)>> = vec![
            vec![],
            vec![("authorization", "Bearer garbage".into())],
            vec![("authorization", "Basic Zm9vOmJhcg==".into())],
            vec![("x-api-key", "v1.zz.zz".into())],
            vec![("authorization", format!("Bearer {forged}"))],
        ];
        for h in cases {
            let refs: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let (s, _, b) = rig.post("/model/fireworks/v1/messages", &refs, BODY).await;
            assert_eq!(s, 401);
            let v: Value = serde_json::from_slice(&b).unwrap();
            assert_eq!(v["type"], "error");
        }
        assert!(rig.seen().is_empty());
        assert!(rig.ledger().is_empty());
    }

    #[tokio::test]
    async fn an_unpriced_model_is_refused_before_anything_is_sent() {
        let rig = Rig::new().await;
        for model in ["no-deck", "half-priced"] {
            let t = mint(&rig.key, ROUTE_FIREWORKS, model);
            let (s, _, b) = rig
                .post(
                    "/model/fireworks/v1/messages",
                    &[("authorization", &format!("Bearer {t}"))],
                    BODY,
                )
                .await;
            assert_eq!(s, 400, "{model}");
            let v: Value = serde_json::from_slice(&b).unwrap();
            assert_eq!(v["error"]["type"], "unpriced_model");
            assert_eq!(v["error"]["message"], unpriced_message(model));
        }
        assert!(rig.seen().is_empty(), "no upstream call");
        assert!(rig.ledger().is_empty(), "never counted as zero");
    }

    #[tokio::test]
    async fn the_connectivity_check_is_answered_here() {
        let rig = Rig::new().await;
        let r = rig
            .http
            .head(format!(
                "{}/model/anthropic-subscription/api/hello",
                rig.proxy_url
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert!(rig.seen().is_empty());
    }

    #[tokio::test]
    async fn count_tokens_and_gets_are_forwarded_but_not_metered() {
        let rig = Rig::new().await;
        let t = mint(&rig.key, ROUTE_OPENAI_CHATGPT, "unpriced-but-listing");
        let r = rig
            .http
            .get(format!(
                "{}/model/openai-chatgpt/models?client_version=0.153.4",
                rig.proxy_url
            ))
            .bearer_auth(&t)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let t = mint(&rig.key, ROUTE_ANTHROPIC_SUBSCRIPTION, "opus");
        let (s, _, _) = rig
            .post(
                "/model/anthropic-subscription/v1/messages/count_tokens",
                &[("authorization", &format!("Bearer {t}"))],
                BODY,
            )
            .await;
        assert_eq!(s, 200);
        let seen = rig.seen();
        assert_eq!(
            seen[0].target,
            "/backend-api/codex/models?client_version=0.153.4"
        );
        assert_eq!(seen[1].target, "/anthropic/v1/messages/count_tokens");
        assert!(rig.ledger().is_empty());
    }

    #[tokio::test]
    async fn dot_segments_and_unknown_routes_are_refused() {
        let rig = Rig::new().await;
        let t = mint(&rig.key, ROUTE_OPENAI_CHATGPT, "codex-chatgpt");
        let auth = format!("Bearer {t}");
        // Raw, because an HTTP client resolves dot segments before it sends.
        for target in [
            "/model/openai-chatgpt/../../x",
            "/model/openai-chatgpt/%2e%2E/x",
        ] {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut c = tokio::net::TcpStream::connect(host_of(&rig.proxy_url))
                .await
                .unwrap();
            let req = format!(
                "POST {target} HTTP/1.1\r\nhost: x\r\nauthorization: {auth}\r\n\
                 content-length: 0\r\nconnection: close\r\n\r\n"
            );
            c.write_all(req.as_bytes()).await.unwrap();
            let mut out = String::new();
            c.read_to_string(&mut out).await.unwrap();
            assert!(out.starts_with("HTTP/1.1 400"), "{target}: {out}");
        }
        let (s, _, _) = rig
            .post(
                "/model/elsewhere/v1/messages",
                &[("authorization", &auth)],
                BODY,
            )
            .await;
        assert_eq!(s, 404);
        assert!(rig.seen().is_empty());
    }

    #[tokio::test]
    async fn an_unreadable_credential_is_502_and_names_no_value() {
        let rig = Rig::new().await;
        // `opus` is priced, but `Keys` holds no Fireworks key for it.
        let t = mint(&rig.key, ROUTE_FIREWORKS, "opus");
        let (s, _, b) = rig
            .post(
                "/model/fireworks/v1/messages",
                &[("authorization", &format!("Bearer {t}"))],
                BODY,
            )
            .await;
        assert_eq!(s, 502);
        let v: Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v["error"]["type"], "credential_unavailable");
        assert!(rig.seen().is_empty());
        let lines = rig.ledger();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["status"], 0);
        assert_eq!(lines[0]["cost_micros"], 0);
    }

    // ---- the local route over mutual TLS ------------------------------------------------

    /// A CA, a server leaf for `localhost`, and a client leaf, all written as PEM files.
    struct Pki {
        dir: PathBuf,
        ca: rcgen::Certificate,
        ca_key: rcgen::KeyPair,
    }

    impl Pki {
        fn new() -> Pki {
            let dir = std::env::temp_dir().join(format!("mp-mtls-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
            p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            let ca_key = rcgen::KeyPair::generate().unwrap();
            let ca = p.self_signed(&ca_key).unwrap();
            std::fs::write(dir.join("ca.pem"), ca.pem()).unwrap();
            Pki { dir, ca, ca_key }
        }

        fn issue(&self, name: &str, file: &str) -> (rcgen::Certificate, rcgen::KeyPair) {
            let p = rcgen::CertificateParams::new(vec![name.to_string()]).unwrap();
            let key = rcgen::KeyPair::generate().unwrap();
            let cert = p.signed_by(&key, &self.ca, &self.ca_key).unwrap();
            std::fs::write(self.dir.join(format!("{file}.pem")), cert.pem()).unwrap();
            std::fs::write(self.dir.join(format!("{file}.key")), key.serialize_pem()).unwrap();
            (cert, key)
        }

        fn client_cert(&self) -> ClientCertToml {
            ClientCertToml {
                cert_file: self.dir.join("client.pem"),
                key_file: self.dir.join("client.key"),
                ca_file: self.dir.join("ca.pem"),
            }
        }
    }

    impl Drop for Pki {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// An HTTPS upstream that demands a client certificate from `pki`'s CA, answers every
    /// request with the Anthropic JSON fixture, and keeps each request's `authorization`.
    async fn mtls_upstream(pki: &Pki) -> (String, Arc<Mutex<Vec<Option<String>>>>) {
        use rustls::pki_types::PrivateKeyDer;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let (cert, key) = pki.issue("localhost", "server");
        let mut roots = rustls::RootCertStore::empty();
        roots.add(pki.ca.der().clone()).unwrap();
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots),
            provider.clone(),
        )
        .build()
        .unwrap();
        let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                vec![cert.der().clone()],
                PrivateKeyDer::try_from(key.serialize_der()).unwrap(),
            )
            .unwrap();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let (acceptor, s) = (acceptor.clone(), s.clone());
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(sock).await else {
                        return;
                    };
                    let svc = hyper::service::service_fn(
                        move |req: hyper::Request<hyper::body::Incoming>| {
                            let s = s.clone();
                            async move {
                                s.lock().unwrap().push(
                                    req.headers()
                                        .get("authorization")
                                        .map(|v| v.to_str().unwrap().to_string()),
                                );
                                let mut r = hyper::Response::new(http_body_util::Full::new(
                                    Bytes::from_static(FIX_ANTHROPIC_JSON),
                                ));
                                r.headers_mut().insert(
                                    header::CONTENT_TYPE,
                                    HeaderValue::from_static("application/json"),
                                );
                                Ok::<_, std::convert::Infallible>(r)
                            }
                        },
                    );
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(tls), svc)
                        .await;
                });
            }
        });
        (url, seen)
    }

    async fn local_proxy(
        key: &[u8; 32],
        local: String,
        tls: Option<ClientCertToml>,
        state: &std::path::Path,
    ) -> String {
        let proxy = ModelProxy::new(ModelDeps {
            authority: Arc::new(KeyAuthority(TokenKey::from_bytes(key))),
            ledger: Some(Arc::new(ActivityLedger::new(state))),
            prices: Arc::new(Prices(BTreeMap::from([("qwen-local", GLM)]))),
            keys: Arc::new(Keys),
            subscription: Arc::new(NoCredential),
            api_key: Arc::new(NoCredential),
            chatgpt: Arc::new(NoCredential),
            upstreams: Upstreams {
                local,
                ..Upstreams::from_settings(&ProxySettings::default())
            },
            local_tls: tls,
        })
        .unwrap();
        let pl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", pl.local_addr().unwrap());
        tokio::spawn(Arc::new(proxy).serve(pl));
        url
    }

    #[tokio::test]
    async fn the_local_route_presents_its_client_certificate_to_an_https_endpoint() {
        let pki = Pki::new();
        pki.issue("jesse-bridge", "client");
        let (upstream, seen) = mtls_upstream(&pki).await;
        let key = rand_bytes();
        let state = pki.dir.join("state");
        let token = mint(&TokenKey::from_bytes(&key), ROUTE_LOCAL, "qwen-local");
        let http = reqwest::Client::builder().no_proxy().build().unwrap();

        let with = local_proxy(&key, upstream.clone(), Some(pki.client_cert()), &state).await;
        let r = http
            .post(format!("{with}/model/local/v1/messages"))
            .bearer_auth(&token)
            .body(BODY.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.bytes().await.unwrap(), FIX_ANTHROPIC_JSON);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [Some("Bearer local-key".to_string())]
        );

        // Without the certificate (and so without the pinned CA) it never gets through.
        let without = local_proxy(&key, upstream, None, &state).await;
        let r = http
            .post(format!("{without}/model/local/v1/messages"))
            .bearer_auth(&token)
            .body(BODY.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 502);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn unreadable_client_certificate_files_fail_construction_naming_only_paths() {
        let deps = ModelDeps {
            authority: Arc::new(KeyAuthority(TokenKey::from_bytes(&[1; 32]))),
            ledger: None,
            prices: Arc::new(Prices(BTreeMap::new())),
            keys: Arc::new(Keys),
            subscription: Arc::new(NoCredential),
            api_key: Arc::new(NoCredential),
            chatgpt: Arc::new(NoCredential),
            upstreams: Upstreams::from_settings(&ProxySettings::default()),
            local_tls: Some(ClientCertToml {
                cert_file: "/nonexistent/client.pem".into(),
                key_file: "/nonexistent/client.key".into(),
                ca_file: "/nonexistent/ca.pem".into(),
            }),
        };
        let e = ModelProxy::new(deps).unwrap_err();
        assert!(e.contains("/nonexistent/client.pem"), "{e}");
    }

    // ---- streaming and metering ------------------------------------------------------

    #[tokio::test]
    async fn an_event_stream_is_relayed_as_it_arrives_not_buffered() {
        let rig = Rig::new().await;
        let t = mint(&rig.key, ROUTE_ANTHROPIC_SUBSCRIPTION, "opus");
        let gate = Arc::new(tokio::sync::Notify::new());
        let split = FIX_ANTHROPIC_SSE.len() / 3;
        *rig.fake.reply.lock().unwrap() = Some(Reply {
            status: 200,
            content_type: "text/event-stream",
            body: FIX_ANTHROPIC_SSE.to_vec(),
            gate: Some((split, gate.clone())),
        });
        let mut r = rig
            .http
            .post(format!(
                "{}/model/anthropic-subscription/v1/messages",
                rig.proxy_url
            ))
            .bearer_auth(&t)
            .body(BODY.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(
            r.headers().get("content-type").unwrap(),
            "text/event-stream"
        );
        // The first third arrives while the upstream is still holding the rest.
        let mut got = Vec::new();
        while got.len() < split {
            let c = tokio::time::timeout(Duration::from_secs(5), r.chunk())
                .await
                .expect("the first part was buffered, not streamed")
                .unwrap()
                .unwrap();
            got.extend_from_slice(&c);
        }
        assert_eq!(got, &FIX_ANTHROPIC_SSE[..split]);
        assert!(rig.ledger().is_empty(), "no line before the stream ends");
        gate.notify_one();
        while let Some(c) = r.chunk().await.unwrap() {
            got.extend_from_slice(&c);
        }
        assert_eq!(got, FIX_ANTHROPIC_SSE);
        let lines = rig.ledger();
        assert_eq!(lines.len(), 1);
        let l = &lines[0];
        assert_eq!(l["input_tokens"], 12);
        assert_eq!(l["cache_write_tokens"], 2048);
        assert_eq!(l["cache_read_tokens"], 30000);
        assert_eq!(l["output_tokens"], 187);
        // 12×15 + 187×75 + 2048×18.75 + 30000×1.5
        assert_eq!(l["cost_micros"], 180 + 14025 + 38400 + 45000);
    }

    #[tokio::test]
    async fn an_upstream_error_status_is_relayed_and_recorded_with_its_status() {
        let rig = Rig::new().await;
        let t = mint(&rig.key, ROUTE_ANTHROPIC_SUBSCRIPTION, "opus");
        let err = br#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        rig.reply(529, "application/json", err);
        let (s, _, b) = rig
            .post(
                "/model/anthropic-subscription/v1/messages",
                &[("authorization", &format!("Bearer {t}"))],
                BODY,
            )
            .await;
        assert_eq!((s, b.as_slice()), (529, err.as_slice()));
        let l = &rig.ledger()[0];
        assert_eq!(l["status"], 529);
        assert_eq!(l["input_tokens"], 0);
    }

    #[tokio::test]
    async fn a_ledger_line_holds_identifiers_and_counts_only() {
        let rig = Rig::new().await;
        let t = mint(&rig.key, ROUTE_ANTHROPIC_SUBSCRIPTION, "opus");
        rig.reply(200, "text/event-stream", FIX_ANTHROPIC_SSE);
        rig.post(
            "/model/anthropic-subscription/v1/messages",
            &[("authorization", &format!("Bearer {t}"))],
            BODY,
        )
        .await;
        let lines = rig.ledger();
        assert_eq!(lines.len(), 1);
        let obj = lines[0].as_object().unwrap();
        let strings: BTreeMap<&str, &str> = obj
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.as_str(), s)))
            .collect();
        assert_eq!(
            strings,
            BTreeMap::from([
                ("kind", "model_call"),
                ("job_id", "job-42"),
                ("route", "anthropic-subscription"),
                ("model", "opus"),
            ])
        );
        for (k, v) in obj {
            assert!(
                v.is_string() || v.is_u64(),
                "{k} is neither a token nor a count"
            );
        }
        let text = lines[0].to_string();
        for leak in [MARKER, "pancake", "oauth-real", &t, "/v1/messages"] {
            assert!(!text.contains(leak), "the ledger line holds {leak}");
        }
    }

    fn meter_all(event_stream: bool, bytes: &[u8], step: usize) -> Usage {
        let mut m = UsageMeter::new(event_stream);
        for c in bytes.chunks(step) {
            m.feed(c);
        }
        m.finish()
    }

    #[test]
    fn the_meter_reads_every_fixture_however_it_is_chunked() {
        let cases: [(bool, &[u8], Usage); 6] = [
            (
                true,
                FIX_ANTHROPIC_SSE,
                Usage {
                    input: 12,
                    output: 187,
                    cache_write: 2048,
                    cache_read: 30000,
                },
            ),
            (
                false,
                FIX_ANTHROPIC_JSON,
                Usage {
                    input: 2095,
                    output: 503,
                    cache_write: 0,
                    cache_read: 1024,
                },
            ),
            (
                true,
                FIX_RESPONSES_SSE,
                Usage {
                    input: 1104,
                    output: 350,
                    cache_write: 0,
                    cache_read: 4096,
                },
            ),
            (
                false,
                FIX_RESPONSES_JSON,
                Usage {
                    input: 800,
                    output: 42,
                    cache_write: 0,
                    cache_read: 100,
                },
            ),
            (
                true,
                FIX_FIREWORKS_SSE,
                Usage {
                    input: 4321,
                    output: 77,
                    cache_write: 0,
                    cache_read: 1200,
                },
            ),
            (
                false,
                FIX_CHAT_JSON,
                Usage {
                    input: 100,
                    output: 20,
                    cache_write: 0,
                    cache_read: 200,
                },
            ),
        ];
        for (i, (sse, bytes, want)) in cases.iter().enumerate() {
            for step in [1, 7, 64, bytes.len()] {
                assert_eq!(
                    meter_all(*sse, bytes, step),
                    *want,
                    "fixture {i}, step {step}"
                );
            }
        }
        // CRLF line endings read the same.
        let crlf = String::from_utf8_lossy(FIX_ANTHROPIC_SSE).replace('\n', "\r\n");
        assert_eq!(meter_all(true, crlf.as_bytes(), 5), cases[0].2);
    }

    #[test]
    fn a_streamed_chat_completion_reads_its_final_usage_chunk() {
        let s = b"data: {\"id\":\"c\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}],\"usage\":null}\n\n\
data: {\"id\":\"c\",\"choices\":[],\"usage\":{\"prompt_tokens\":50,\"completion_tokens\":5,\"prompt_tokens_details\":{\"cached_tokens\":10}}}\n\n\
data: [DONE]\n\n";
        assert_eq!(
            meter_all(true, s, 3),
            Usage {
                input: 40,
                output: 5,
                cache_write: 0,
                cache_read: 10
            }
        );
    }

    #[test]
    fn an_oversized_body_or_line_is_not_held() {
        let mut m = UsageMeter::new(false);
        m.feed(&vec![b' '; METER_JSON_MAX + 1]);
        assert!(m.buf.is_empty());
        assert_eq!(m.finish(), Usage::default());
        let mut m = UsageMeter::new(true);
        m.feed(&vec![b'x'; METER_LINE_MAX + 1]);
        assert!(m.buf.is_empty());
        m.feed(b"\n");
        m.feed(FIX_ANTHROPIC_SSE);
        assert_eq!(
            m.finish().output,
            187,
            "the meter recovers at the next line"
        );
    }

    #[test]
    fn cost_is_the_four_rate_sum_in_micros() {
        let u = Usage {
            input: 1_000_000,
            output: 1_000_000,
            cache_write: 1_000_000,
            cache_read: 1_000_000,
        };
        assert_eq!(cost_micros(&u, &OPUS), 110_250_000); // (15 + 75 + 18.75 + 1.5) × 10^6
        assert_eq!(cost_micros(&Usage::default(), &OPUS), 0);
    }

    // ---- pieces ----------------------------------------------------------------------

    #[test]
    fn the_oauth_beta_is_appended_created_or_left_alone() {
        let mut h = HeaderMap::new();
        ensure_oauth_beta(&mut h);
        assert_eq!(h.get("anthropic-beta").unwrap(), OAUTH_BETA);
        let mut h = HeaderMap::new();
        h.insert("anthropic-beta", HeaderValue::from_static("a,b"));
        ensure_oauth_beta(&mut h);
        assert_eq!(h.get("anthropic-beta").unwrap(), "a,b,oauth-2025-04-20");
        let mut h = HeaderMap::new();
        h.insert(
            "anthropic-beta",
            HeaderValue::from_static("oauth-2025-04-20,extended-cache-ttl-2025-04-11"),
        );
        ensure_oauth_beta(&mut h);
        assert_eq!(
            h.get("anthropic-beta").unwrap(),
            "oauth-2025-04-20,extended-cache-ttl-2025-04-11"
        );
    }

    #[test]
    fn metered_calls_are_the_billing_endpoints_posted() {
        assert!(is_metered(&Method::POST, "v1/messages"));
        assert!(is_metered(&Method::POST, "v1/responses"));
        assert!(is_metered(&Method::POST, "responses"));
        assert!(is_metered(&Method::POST, "v1/chat/completions"));
        assert!(!is_metered(&Method::POST, "v1/messages/count_tokens"));
        assert!(!is_metered(&Method::GET, "models"));
        assert!(!is_metered(&Method::HEAD, "api/hello"));
    }

    #[test]
    fn registry_entries_map_to_their_routes() {
        use crate::harness::CLAUDE_CODE_ID;
        use crate::harness::CODEX_ID;
        use ModelKind::*;
        let fw = Some("https://api.fireworks.ai/inference");
        let cases: Vec<(ModelKind, Option<&str>, &str, Option<&str>)> = vec![
            (
                Ambient,
                None,
                CLAUDE_CODE_ID,
                Some(ROUTE_ANTHROPIC_SUBSCRIPTION),
            ),
            (
                Subscription,
                None,
                CLAUDE_CODE_ID,
                Some(ROUTE_ANTHROPIC_SUBSCRIPTION),
            ),
            (Hosted, fw, CLAUDE_CODE_ID, Some(ROUTE_FIREWORKS)),
            (
                OpenAi,
                Some("https://api.fireworks.ai/inference/v1"),
                CODEX_ID,
                Some(ROUTE_FIREWORKS),
            ),
            (
                Local,
                Some("http://10.0.0.5:9100"),
                CLAUDE_CODE_ID,
                Some(ROUTE_LOCAL),
            ),
            (
                Hosted,
                Some("http://127.0.0.1:9100"),
                CLAUDE_CODE_ID,
                Some(ROUTE_LOCAL),
            ),
            (
                Hosted,
                Some("http://localhost:9100/v1"),
                CODEX_ID,
                // A non-OpenAI-kind Codex entry runs on the ChatGPT login whatever its probe
                // URL says (see `codex_provider_args`): the deployed `codex-write` is exactly
                // this shape.
                Some(ROUTE_OPENAI_CHATGPT),
            ),
            (
                Hosted,
                Some("https://api.anthropic.com"),
                CLAUDE_CODE_ID,
                Some(ROUTE_ANTHROPIC_API),
            ),
            (Ambient, None, CODEX_ID, Some(ROUTE_OPENAI_CHATGPT)),
            (Hosted, None, CODEX_ID, Some(ROUTE_OPENAI_CHATGPT)),
            (OpenAi, None, CODEX_ID, None),
            (
                Hosted,
                Some("https://api.moonshot.ai/anthropic"),
                CLAUDE_CODE_ID,
                None,
            ),
            (Ambient, None, crate::harness::DIRECT_ID, None),
        ];
        for (kind, base, harness, want) in cases {
            assert_eq!(
                route_for(&kind, base, harness),
                want,
                "{kind:?} {base:?} {harness}"
            );
        }
    }

    #[test]
    fn credentials_parse_and_never_print() {
        let s = parse_claude_credentials(
            br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-SECRET","refreshToken":"r","expiresAt":1}}"#,
        )
        .unwrap();
        assert_eq!(s.expose(), "sk-ant-oat01-SECRET");
        assert!(!format!("{s:?}").contains("SECRET"));
        assert!(parse_claude_credentials(b"{}").is_err());
        let e = parse_claude_credentials(b"not json SECRET").unwrap_err();
        assert!(!e.contains("SECRET"));

        let c = parse_codex_auth(
            br#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{"id_token":"i","access_token":"JWT-SECRET","refresh_token":"r","account_id":"ACCT-SECRET"},"last_refresh":"2026-11-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(c.access_token.expose(), "JWT-SECRET");
        assert_eq!(c.account_id.expose(), "ACCT-SECRET");
        assert!(!format!("{c:?}").contains("SECRET"));
        assert!(parse_codex_auth(br#"{"auth_mode":"apikey","OPENAI_API_KEY":"k"}"#).is_err());
        assert!(parse_codex_auth(br#"{"tokens":{"access_token":"a"}}"#).is_err());
    }

    #[test]
    fn the_subscription_login_reads_file_then_variable() {
        let dir = std::env::temp_dir().join(format!("mp-sub-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("token");
        std::fs::write(&f, "  from-file\n").unwrap();
        let vars = EnvMap::from([("SUB".to_string(), "from-env".to_string())]);
        let both = SubscriptionLogin {
            file: Some(f.clone()),
            env_name: Some("SUB".into()),
            vars: Some(vars.clone()),
            cli_login: false,
        };
        assert_eq!(both.credential().unwrap().expose(), "from-file");
        let env = SubscriptionLogin {
            file: None,
            ..both.clone()
        };
        assert_eq!(env.credential().unwrap().expose(), "from-env");
        let none = SubscriptionLogin {
            file: None,
            env_name: None,
            vars: Some(vars),
            cli_login: false,
        };
        assert!(none.credential().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_codex_auth_file_defaults_follow_codex_home() {
        let s = ProxySettings::default();
        let v = EnvMap::from([
            ("CODEX_HOME".to_string(), "/x/codex".to_string()),
            ("HOME".to_string(), "/home/u".to_string()),
        ]);
        assert_eq!(
            CodexAuthFile::from_settings(&s, Some(&v)).unwrap().path,
            PathBuf::from("/x/codex/auth.json")
        );
        let v = EnvMap::from([("HOME".to_string(), "/home/u".to_string())]);
        assert_eq!(
            CodexAuthFile::from_settings(&s, Some(&v)).unwrap().path,
            PathBuf::from("/home/u/.codex/auth.json")
        );
    }

    #[test]
    fn production_upstreams_are_the_documented_ones() {
        let u = Upstreams::from_settings(&ProxySettings::default());
        assert_eq!(
            u.base(ROUTE_ANTHROPIC_SUBSCRIPTION),
            "https://api.anthropic.com"
        );
        assert_eq!(u.base(ROUTE_ANTHROPIC_API), "https://api.anthropic.com");
        assert_eq!(
            u.base(ROUTE_FIREWORKS),
            "https://api.fireworks.ai/inference"
        );
        assert_eq!(
            u.base(ROUTE_OPENAI_CHATGPT),
            "https://chatgpt.com/backend-api/codex"
        );
        assert_eq!(u.base(ROUTE_LOCAL), "http://127.0.0.1:9100");
    }
}
