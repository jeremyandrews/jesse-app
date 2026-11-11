//! **THE MODEL PROXY AND THE EGRESS PROXY** — the bridge as the only route to a model and to
//! the open web.
//!
//! ---- WHAT CHANGES WHEN THEY ARE ON -----------------------------------------------
//!
//! Both are OFF by default (`[proxy] model = false`, `[proxy] egress = false`), and with both
//! off nothing here runs and every child is spawned exactly as before. Both need the tool
//! gateway (`[gateway] enabled = true`): the turn token they validate is the gateway's.
//!
//!   * **`[proxy] model = true`** ([`model`]): every child's model base URL points at
//!     `http://<model_listen>/model/<route>` and its credential is the turn token. The proxy
//!     validates the token (signature, expiry, route), swaps the child's auth headers for the
//!     real credential, forwards the body unchanged, streams the response back unchanged, and
//!     meters the provider's usage block into the activity ledger as a `model_call` line.
//!   * **`[proxy] egress = true`** ([`egress`]): every child's `HTTPS_PROXY`/`HTTP_PROXY` is
//!     `http://<job id>:<turn token>@<egress_listen>`, and every client the measurements found
//!     is told to trust the per-install CA ([`ca`]). The proxy enforces the turn's tier
//!     ([`tier`]), terminates TLS on `allowlist` and `open-logged` turns, classes each request
//!     `read` or `send`, checks outbound content against canaries and vault fingerprints
//!     ([`content`]), and writes one `egress` line per request: host, never URL.
//!
//! On the Studio both are VOLUNTARY: a child that ignores its base URL or its proxy variables
//! goes around them. They become the only route when the child runs in a pod whose network
//! admits only the bridge. SECURITY.md says so.

pub mod ca;
pub mod content;
pub mod egress;
pub mod model;
pub mod tier;
pub mod wiring;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

pub use tier::Egress;

/// The model proxy's default loopback listener on the Studio.
pub const DEFAULT_MODEL_LISTEN: &str = "127.0.0.1:8767";
/// The egress proxy's default loopback listener on the Studio.
pub const DEFAULT_EGRESS_LISTEN: &str = "127.0.0.1:8768";
/// The local model gateway today.
pub const DEFAULT_LOCAL_URL: &str = "http://127.0.0.1:9100";

// ---- SETTINGS ------------------------------------------------------------------------

/// `[proxy]` in the bridge TOML.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyToml {
    #[serde(default)]
    pub model: bool,
    #[serde(default)]
    pub egress: bool,
    /// `host:port` the model proxy listens on. Default [`DEFAULT_MODEL_LISTEN`].
    pub model_listen: Option<String>,
    /// `host:port` the egress proxy listens on. Default [`DEFAULT_EGRESS_LISTEN`].
    pub egress_listen: Option<String>,
    /// The base a CHILD reaches the model proxy at (the cluster's service name). Default
    /// `http://<model_listen>`.
    pub model_child_url: Option<String>,
    /// The `host:port` a CHILD reaches the egress proxy at. Default `egress_listen`.
    pub egress_child_addr: Option<String>,
    /// The subscription credential: a file holding the long-lived token `claude
    /// setup-token` prints. Read at call time.
    pub subscription_token_file: Option<PathBuf>,
    /// ...or the NAME of a variable holding it.
    pub subscription_token_env: Option<String>,
    /// The NAME of the variable holding an Anthropic API key for `anthropic-api`.
    pub anthropic_api_key_env: Option<String>,
    /// The local model endpoint. Default [`DEFAULT_LOCAL_URL`]; an `https://` URL with
    /// `local_client_cert` is the later remote shape.
    pub local_url: Option<String>,
    /// A `[gateway.client_certs]` entry the `local` route presents.
    pub local_client_cert: Option<String>,
    /// Codex's canonical `auth.json` (the ChatGPT login), read at call time by the
    /// `openai-chatgpt` route. Default `$CODEX_HOME/auth.json`, else `~/.codex/auth.json`.
    pub codex_auth_file: Option<PathBuf>,
}

/// `[egress]` in the bridge TOML.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressToml {
    #[serde(default)]
    pub send_hosts: Vec<String>,
    pub read_url_max: Option<usize>,
    pub read_query_max: Option<usize>,
    pub index_interval_secs: Option<u64>,
    pub fingerprint_threshold: Option<usize>,
}

/// `[sandbox]` in the bridge TOML.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxToml {
    pub egress_phone: Option<String>,
}

/// The egress proxy's policy knobs, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressPolicy {
    /// Hosts a `send` may go to (exact or `.suffix`), from `[egress] send_hosts`. The
    /// bridge-owned list under the state directory is read alongside it at request time.
    pub send_hosts: Vec<String>,
    /// Longest path-plus-query a `read` may have, in bytes.
    pub read_url_max: usize,
    /// Longest query string a `read` may have, in bytes.
    pub read_query_max: usize,
    /// How often the vault fingerprint index is rebuilt, besides after each commit.
    pub index_interval_secs: u64,
    /// How many fingerprint windows a request must match to be refused.
    pub fingerprint_threshold: usize,
}

impl Default for EgressPolicy {
    fn default() -> Self {
        EgressPolicy {
            send_hosts: Vec::new(),
            read_url_max: 512,
            read_query_max: 256,
            index_interval_secs: 900,
            fingerprint_threshold: 3,
        }
    }
}

/// Everything the proxies read from the deployment, resolved. NAMES and PATHS; never a value.
#[derive(Debug, Clone)]
pub struct ProxySettings {
    pub model: bool,
    pub egress: bool,
    pub model_listen: String,
    pub egress_listen: String,
    pub model_child_url: String,
    pub egress_child_addr: String,
    pub subscription_token_file: Option<PathBuf>,
    pub subscription_token_env: Option<String>,
    pub anthropic_api_key_env: String,
    pub local_url: String,
    pub local_client_cert: Option<String>,
    pub codex_auth_file: Option<PathBuf>,
    pub policy: EgressPolicy,
    /// `[sandbox] egress_phone`, parsed. A bad value is a startup problem
    /// ([`ProxySettings::problems`]) and is `open-logged` until then.
    pub egress_phone: Egress,
    pub egress_phone_raw: Option<String>,
}

impl Default for ProxySettings {
    fn default() -> Self {
        ProxySettings::from_toml(None, None, None)
    }
}

impl ProxySettings {
    pub fn from_toml(
        proxy: Option<ProxyToml>,
        egress: Option<EgressToml>,
        sandbox: Option<SandboxToml>,
    ) -> ProxySettings {
        let p = proxy.unwrap_or_default();
        let e = egress.unwrap_or_default();
        let s = sandbox.unwrap_or_default();
        let d = EgressPolicy::default();
        let model_listen = p
            .model_listen
            .unwrap_or_else(|| DEFAULT_MODEL_LISTEN.to_string());
        let egress_listen = p
            .egress_listen
            .unwrap_or_else(|| DEFAULT_EGRESS_LISTEN.to_string());
        ProxySettings {
            model: p.model,
            egress: p.egress,
            model_child_url: p
                .model_child_url
                .unwrap_or_else(|| format!("http://{model_listen}")),
            egress_child_addr: p.egress_child_addr.unwrap_or_else(|| egress_listen.clone()),
            model_listen,
            egress_listen,
            subscription_token_file: p.subscription_token_file,
            subscription_token_env: p.subscription_token_env,
            anthropic_api_key_env: p
                .anthropic_api_key_env
                .unwrap_or_else(|| "ANTHROPIC_API_KEY".to_string()),
            local_url: p.local_url.unwrap_or_else(|| DEFAULT_LOCAL_URL.to_string()),
            local_client_cert: p.local_client_cert,
            codex_auth_file: p.codex_auth_file,
            policy: EgressPolicy {
                send_hosts: e
                    .send_hosts
                    .iter()
                    .map(|h| h.trim().to_ascii_lowercase())
                    .collect(),
                read_url_max: e.read_url_max.unwrap_or(d.read_url_max),
                read_query_max: e.read_query_max.unwrap_or(d.read_query_max),
                index_interval_secs: e.index_interval_secs.unwrap_or(d.index_interval_secs),
                fingerprint_threshold: e.fingerprint_threshold.unwrap_or(d.fingerprint_threshold),
            },
            egress_phone: s
                .egress_phone
                .as_deref()
                .map(|v| Egress::from_phone_setting(v).unwrap_or(Egress::OpenLogged))
                .unwrap_or(Egress::OpenLogged),
            egress_phone_raw: s.egress_phone,
        }
    }

    /// Whether either proxy is on.
    pub fn any(&self) -> bool {
        self.model || self.egress
    }

    /// Problems an operator must fix before the bridge boots with these settings.
    pub fn problems(&self, gateway_enabled: bool) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(raw) = &self.egress_phone_raw {
            if let Err(e) = Egress::from_phone_setting(raw) {
                out.push(e);
            }
        }
        if !self.any() {
            return out;
        }
        if !gateway_enabled {
            out.push(
                "[proxy] model or egress is on, but [gateway] enabled is false: the proxies \
                 validate the gateway's turn token"
                    .into(),
            );
        }
        for (name, addr) in [
            ("model_listen", &self.model_listen),
            ("egress_listen", &self.egress_listen),
            ("egress_child_addr", &self.egress_child_addr),
        ] {
            if addr.parse::<std::net::SocketAddr>().is_err() && !addr.contains(':') {
                out.push(format!(
                    "[proxy] {name} = \"{addr}\" is not a host:port address"
                ));
            }
        }
        if self.policy.fingerprint_threshold == 0 {
            out.push("[egress] fingerprint_threshold must be at least 1".into());
        }
        for h in &self.policy.send_hosts {
            if h.is_empty() || h.contains('/') || h.contains(' ') {
                out.push(format!(
                    "[egress] send_hosts entry {h:?} is not a host name"
                ));
            }
        }
        out
    }
}

// ---- THE SEAMS THE PROXIES ARE BUILT AGAINST -----------------------------------------
//
// The proxies do not reach into the gateway, the registry or APNs directly: each is handed
// these, so its tests can supply fakes and the cluster can supply its own.

/// A turn the egress proxy admitted a request for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressTurn {
    pub job_id: String,
    pub tier: Egress,
}

/// A turn the model proxy admitted a request for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelTurn {
    pub job_id: String,
    /// The registry id the token names.
    pub model: String,
}

/// Who decides whether a turn token is live. The gateway, in the bridge.
pub trait TurnAuthority: Send + Sync {
    /// `Proxy-Authorization: Basic job:token` → the turn, when the token's signature and expiry
    /// hold, its job is `job`, its turn is still open, and it carries a tier.
    fn egress_turn(&self, job: &str, token: &str) -> Option<EgressTurn>;
    /// A model proxy bearer for `route` → the turn, when the token's signature, expiry and
    /// route hold and its turn is still open.
    fn model_turn(&self, token: &str, route: &str) -> Option<ModelTurn>;
}

/// Who is told when the egress proxy refuses a request on content grounds. The turn, host and
/// reason; NEVER the content.
pub trait RefusalNotifier: Send + Sync {
    fn refused(&self, job_id: &str, host: &str, reason: crate::activity::EgressReason);
}

/// A notifier that tells nobody (tests, and a deployment with no APNs).
pub struct NoNotifier;

impl RefusalNotifier for NoNotifier {
    fn refused(&self, _: &str, _: &str, _: crate::activity::EgressReason) {}
}

/// One inspected exchange, handed to the [`ExchangeRecorder`]. The FULL request is here (it
/// belongs to the turn record, a later step); the ledger never sees any of it.
pub struct InspectedExchange<'a> {
    pub job_id: &'a str,
    pub host: &'a str,
    pub port: u16,
    pub method: &'a str,
    /// Path and query.
    pub target: &'a str,
    pub request_headers: &'a [(String, Vec<u8>)],
    pub request_body: &'a [u8],
    /// `None` when the request was refused before it left.
    pub response_status: Option<u16>,
    pub response_headers: &'a [(String, Vec<u8>)],
}

/// THE SINGLE HOOK where each inspected request and response can be handed to a recorder. The
/// turn record (a later step) implements it; today nothing does.
pub trait ExchangeRecorder: Send + Sync {
    fn record(&self, exchange: &InspectedExchange<'_>);
}

/// The recorder that records nothing: today's.
pub struct NoRecorder;

impl ExchangeRecorder for NoRecorder {
    fn record(&self, _: &InspectedExchange<'_>) {}
}

/// Header names whose VALUES are credentials. Never logged, never copied into an error.
pub const CREDENTIAL_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "cookie",
    "chatgpt-account-id",
];

/// Values by name for tests that need a deterministic environment.
pub type EnvMap = BTreeMap<String, String>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_proxies_are_off_by_default_and_need_the_gateway() {
        let s = ProxySettings::default();
        assert!(!s.model && !s.egress);
        assert!(s.problems(false).is_empty());
        assert_eq!(s.policy, EgressPolicy::default());
        assert_eq!(s.egress_phone, Egress::OpenLogged);
        let on = ProxySettings::from_toml(
            Some(ProxyToml {
                model: true,
                ..Default::default()
            }),
            None,
            None,
        );
        assert!(on.problems(false).iter().any(|p| p.contains("[gateway]")));
        assert!(on.problems(true).is_empty());
    }

    #[test]
    fn the_defaults_are_the_documented_ones() {
        let p = EgressPolicy::default();
        assert_eq!(
            (
                p.read_url_max,
                p.read_query_max,
                p.index_interval_secs,
                p.fingerprint_threshold
            ),
            (512, 256, 900, 3)
        );
    }

    #[test]
    fn a_bad_phone_tier_is_a_startup_problem() {
        let s = ProxySettings::from_toml(
            None,
            None,
            Some(SandboxToml {
                egress_phone: Some("allowlist".into()),
            }),
        );
        assert_eq!(s.problems(false).len(), 1);
    }
}
