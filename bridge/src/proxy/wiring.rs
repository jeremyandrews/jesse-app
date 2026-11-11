//! **CHILD WIRING** — what a turn's child is told about the proxies, and the startup checks.
//!
//! The proxies need the turn token, and the token exists only once the gateway has admitted
//! the turn ([`crate::gateway::open_for_child`]), which happens after the harness has built
//! the child's `Command`. So the model base URL that does not depend on the token is decided
//! when the command is built (Codex's provider `-c` args, [`codex_proxy_provider_args`]), and
//! everything that carries the token is layered onto the built `Command` by
//! [`apply_child_env`] after the turn is admitted and before the child is spawned.
//!
//! ---- WHAT FACT 2 AND FACT 3 MEASURED, AND WHAT IT FORCES ----------------------------
//!
//! Claude Code 2.1.322, on its own subscription login with `ANTHROPIC_BASE_URL` set, sends its
//! request to that base URL with `Authorization: Bearer <OAuth token>` and `oauth-2025-04-20`
//! in `anthropic-beta`. Given `ANTHROPIC_AUTH_TOKEN`, it sends THAT as the bearer instead, does
//! not use its stored login, and drops the OAuth beta. So the subscription CAN be proxied:
//! the child gets the turn token, and the proxy puts the OAuth bearer and the beta back.
//!
//! Codex 0.153.4 with a custom provider (`env_key`, no `requires_openai_auth`) sends the same
//! request shape it sends on its ChatGPT login, to `<base_url>/responses`, with the turn token
//! as its bearer and no `chatgpt-account-id`, and needs no `auth.json` in its home. So the
//! ChatGPT login is proxied the same way: the child's home gets no `auth.json`, and the proxy
//! adds the bearer and the account id from the bridge's canonical copy.

use std::collections::BTreeMap;

use tokio::process::Command;

use crate::config::{ActiveModel, ModelKind, RegistryModel};
use crate::proxy::Egress;

/// The model proxy's routes.
pub const ROUTE_SUBSCRIPTION: &str = "anthropic-subscription";
pub const ROUTE_ANTHROPIC_API: &str = "anthropic-api";
pub const ROUTE_FIREWORKS: &str = "fireworks";
pub const ROUTE_LOCAL: &str = "local";
pub const ROUTE_CHATGPT: &str = "openai-chatgpt";

/// Every trust setting the fact-6 measurements found a client honouring for an added CA, set
/// to the CA certificate's path on every child when the egress proxy is on. `SSL_CERT_FILE`
/// REPLACES a client's bundle rather than adding to it, which is right here: every HTTPS
/// request a proxied child makes is terminated by the proxy, so the proxy's CA is the only
/// one it needs.
///
/// | client (measured 2026-11-11)            | honours                                   |
/// |-----------------------------------------|-------------------------------------------|
/// | Claude Code CLI 2.1.322 (node)          | `NODE_EXTRA_CA_CERTS`                     |
/// | Codex 0.153.4 (rust)                    | `CODEX_CA_CERTIFICATE`, `SSL_CERT_FILE`   |
/// | `/usr/bin/curl` 8.7.1                   | `SSL_CERT_FILE`, `CURL_CA_BUNDLE`         |
/// | `/usr/bin/git` 2.54                     | `GIT_SSL_CAINFO` (not `SSL_CERT_FILE`)    |
/// | `pip3` (Xcode python 3.9)               | `PIP_CERT`, `REQUESTS_CA_BUNDLE`          |
/// | `npm` 10 (node 22)                      | `NODE_EXTRA_CA_CERTS`, `NPM_CONFIG_CAFILE`|
/// | `cargo` 1.99                            | `CARGO_HTTP_CAINFO` (not `SSL_CERT_FILE`) |
pub const CA_TRUST_VARS: &[&str] = &[
    "NODE_EXTRA_CA_CERTS",
    "CODEX_CA_CERTIFICATE",
    "SSL_CERT_FILE",
    "CURL_CA_BUNDLE",
    "GIT_SSL_CAINFO",
    "PIP_CERT",
    "REQUESTS_CA_BUNDLE",
    "NPM_CONFIG_CAFILE",
    "CARGO_HTTP_CAINFO",
];

/// The proxy variables a child is given, both spellings (curl reads only the lower-case
/// `http_proxy`; most tools read either).
pub const PROXY_VARS: &[&str] = &["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"];

/// The `NO_PROXY` variables.
pub const NO_PROXY_VARS: &[&str] = &["NO_PROXY", "no_proxy"];

/// Credential-bearing variables a proxied child must never inherit.
pub const PROVIDER_CREDENTIAL_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "FIREWORKS_API_KEY",
];

fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
}

fn is_loopback_host(h: &str) -> bool {
    h == "127.0.0.1" || h == "localhost" || h == "::1" || h == "[::1]"
}

/// The model proxy route a model takes on a harness, or `None` when the proxy does not carry
/// it (the child then keeps today's wiring).
pub fn route_for(kind: ModelKind, base_url: Option<&str>, harness: &str) -> Option<&'static str> {
    let host = base_url.and_then(host_of);
    if harness == crate::harness::CODEX_ID {
        return match kind {
            ModelKind::OpenAi => match host.as_deref() {
                Some(h) if h.ends_with("fireworks.ai") => Some(ROUTE_FIREWORKS),
                Some(h) if is_loopback_host(h) => Some(ROUTE_LOCAL),
                _ => None,
            },
            // Every other Codex model runs on the ChatGPT login (fact 3).
            _ => Some(ROUTE_CHATGPT),
        };
    }
    match kind {
        // The subscription is the Claude Code CLI's login: no other harness speaks it.
        ModelKind::Ambient | ModelKind::Subscription
            if harness == crate::harness::CLAUDE_CODE_ID =>
        {
            Some(ROUTE_SUBSCRIPTION)
        }
        ModelKind::Ambient | ModelKind::Subscription => None,
        ModelKind::Local => Some(ROUTE_LOCAL),
        _ => match host.as_deref() {
            Some(h) if h.ends_with("fireworks.ai") => Some(ROUTE_FIREWORKS),
            Some(h) if is_loopback_host(h) => Some(ROUTE_LOCAL),
            Some("api.anthropic.com") => Some(ROUTE_ANTHROPIC_API),
            _ => None,
        },
    }
}

/// The route for a turn's active model.
pub fn route_for_active(active: &ActiveModel) -> Option<&'static str> {
    route_for(
        active.kind,
        active.env.as_ref().map(|(u, _, _)| u.as_str()),
        &active.harness,
    )
}

/// The upstream base each route forwards to: the prefix a model's own base URL is measured
/// against when it is rewritten onto the proxy.
pub fn route_upstream_base(cfg: &crate::Config, route: &str) -> String {
    match route {
        ROUTE_FIREWORKS => "https://api.fireworks.ai/inference".into(),
        ROUTE_LOCAL => cfg.proxy.local_url.trim_end_matches('/').to_string(),
        ROUTE_CHATGPT => "https://chatgpt.com/backend-api/codex".into(),
        _ => "https://api.anthropic.com".into(),
    }
}

/// The base URL a child is given for `route`: the proxy route plus whatever path the model's
/// own base URL has beyond the route's upstream base. `https://api.fireworks.ai/inference`
/// becomes `<proxy>/model/fireworks`; `https://api.fireworks.ai/inference/v1` becomes
/// `<proxy>/model/fireworks/v1`.
pub fn proxied_base_url(cfg: &crate::Config, route: &str, original: Option<&str>) -> String {
    let base = format!(
        "{}/model/{route}",
        cfg.proxy.model_child_url.trim_end_matches('/')
    );
    let upstream = route_upstream_base(cfg, route);
    let suffix = original
        .map(|o| o.trim_end_matches('/'))
        .and_then(|o| o.strip_prefix(upstream.as_str()))
        .unwrap_or("");
    format!("{base}{suffix}")
}

/// A model that may run a scheduled job only on a `closed` tier: any `local` kind, or any
/// entry served by Fireworks.
pub fn non_frontier(m: &RegistryModel) -> bool {
    if matches!(m.kind, ModelKind::Local) {
        return true;
    }
    m.backend
        .as_ref()
        .and_then(|(u, _, _)| host_of(u))
        .is_some_and(|h| h.ends_with("fireworks.ai") || is_loopback_host(&h))
}

/// The startup gate's proxy half: the settings' own problems and the tier/model rule for every
/// scheduled job. The tier/model rule applies whatever the flags say: it is a statement about
/// the schedule, and a job written today must not become an open non-frontier job the day the
/// flag is flipped.
pub fn validate_proxy(cfg: &crate::Config) -> Vec<crate::ConfigError> {
    let mut out = cfg.proxy.problems(cfg.gateway.enabled);
    out.extend(crate::schedule::tier_model_problems(
        &cfg.schedule.jobs,
        |id| {
            cfg.model_registry
                .models
                .iter()
                .find(|m| m.id == id || m.aliases.iter().any(|a| a == id))
                .is_some_and(non_frontier)
        },
    ));
    out.into_iter()
        .map(|message| crate::ConfigError {
            model: None,
            message,
        })
        .collect()
}

/// The startup table: every scheduled job with its tier (and hosts), and a warning line per
/// `open` job. Printed at boot whatever the flags say, so the posture the schedule asks for is
/// visible before the flag that enforces it is flipped.
pub fn tier_table(cfg: &crate::Config) -> Vec<String> {
    let mut lines = vec![format!(
        "jesse-bridge: egress tiers (enforced: {}; phone turns: {})",
        if cfg.proxy.egress {
            "yes, [proxy] egress = true"
        } else {
            "no, [proxy] egress = false"
        },
        cfg.proxy.egress_phone.label()
    )];
    for j in &cfg.schedule.jobs {
        if j.builtin.is_some() || j.is_devjob() {
            continue;
        }
        let hosts = match &j.egress {
            Egress::Allowlist(h) => format!(" {}", h.join(",")),
            _ => String::new(),
        };
        lines.push(format!(
            "  {:<32} {}{hosts}{}",
            j.id,
            j.egress.label(),
            j.model
                .as_deref()
                .map(|m| format!("  model={m}"))
                .unwrap_or_default()
        ));
    }
    for j in &cfg.schedule.jobs {
        if j.egress == Egress::OpenLogged {
            lines.push(format!(
                "jesse-bridge: WARNING — scheduled job '{}' runs with egress = \"open\": an \
                 unattended turn that can read any public site",
                j.id
            ));
        }
    }
    lines
}

/// The tier for a turn from `origin` (`phone`, `schedule:<id>`, ...).
pub fn tier_for_origin(cfg: &crate::Config, origin: &str) -> Egress {
    let kind = crate::proxy::tier::kind_from_origin(origin);
    let job = match kind {
        crate::proxy::tier::TurnKind::Scheduled(id) => cfg
            .schedule
            .jobs
            .iter()
            .find(|j| j.id == id)
            .map(|j| &j.egress),
        _ => None,
    };
    crate::proxy::tier::select(kind, job, &cfg.proxy.egress_phone)
}

/// The proxy claims for a turn: its filed tier, and the route and registry id of its model
/// when the model proxy is on and carries it.
pub fn proxy_claims(
    cfg: &crate::Config,
    active: &ActiveModel,
    job_id: &str,
) -> crate::gateway::ProxyClaims {
    let route = if cfg.proxy.model {
        route_for_active(active).unwrap_or_default()
    } else {
        ""
    };
    crate::gateway::ProxyClaims {
        tier: Some(crate::proxy::tier::tier_for(job_id)),
        route: route.to_string(),
        metered: if route.is_empty() {
            String::new()
        } else {
            active.id.clone()
        },
        filter_all_egress: cfg.proxy.egress,
        required: needs_token(cfg, active),
    }
}

/// Whether a turn on `active` needs a gateway turn even with no gateway server to admit: the
/// proxies validate its token.
pub fn needs_token(cfg: &crate::Config, active: &ActiveModel) -> bool {
    cfg.gateway.enabled
        && (cfg.proxy.egress || (cfg.proxy.model && route_for_active(active).is_some()))
}

/// Where the per-install CA's CERTIFICATE (never its key) is put for children to read.
pub fn ca_child_cert_path(state_dir: &std::path::Path) -> std::path::PathBuf {
    state_dir.join("proxy-ca-cert.pem")
}

/// The variables [`apply_child_env`] sets, as a map (for the tests and the containment probe).
/// `ca_path` is the per-install CA certificate's child-readable path.
pub fn child_env(
    cfg: &crate::Config,
    active: &ActiveModel,
    job_id: &str,
    token: &str,
    ca_path: Option<&std::path::Path>,
) -> (BTreeMap<String, String>, Vec<&'static str>) {
    let mut set = BTreeMap::new();
    let mut remove: Vec<&'static str> = Vec::new();
    if cfg.proxy.model {
        if let Some(route) = route_for_active(active) {
            remove.extend(PROVIDER_CREDENTIAL_VARS);
            let original = active.env.as_ref().map(|(u, _, _)| u.as_str());
            let base = proxied_base_url(cfg, route, original);
            if active.harness == crate::harness::CODEX_ID {
                set.insert(
                    crate::harness::CODEX_PROVIDER_KEY_ENV.to_string(),
                    token.to_string(),
                );
            } else {
                set.insert("ANTHROPIC_BASE_URL".into(), base);
                set.insert("ANTHROPIC_AUTH_TOKEN".into(), token.to_string());
            }
        }
    }
    if cfg.proxy.egress {
        let url = format!("http://{job_id}:{token}@{}", cfg.proxy.egress_child_addr);
        for v in PROXY_VARS {
            set.insert((*v).to_string(), url.clone());
        }
        let mut no_proxy: Vec<String> = Vec::new();
        if let Some(h) = host_of(&cfg.proxy.model_child_url) {
            no_proxy.push(h);
        }
        if let Some(h) = cfg
            .gateway
            .child_url
            .as_deref()
            .and_then(host_of)
            .filter(|h| !no_proxy.contains(h))
        {
            no_proxy.push(h);
        }
        for v in NO_PROXY_VARS {
            set.insert((*v).to_string(), no_proxy.join(","));
        }
        // The Claude Code CLI's own telemetry, error reporting and registry calls go to
        // api.anthropic.com, Datadog and the MCP registry through the same proxy (measured: a
        // `-p` turn made eleven such requests). On a `closed` turn each would be a refusal
        // line in the ledger and noise in the digest; the CLI's documented switch turns them
        // off. It changes nothing a turn does.
        set.insert(
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
            "1".into(),
        );
        if let Some(ca) = ca_path {
            for v in CA_TRUST_VARS {
                set.insert((*v).to_string(), ca.display().to_string());
            }
        }
    }
    (set, remove)
}

/// What the egress proxy running in THIS process wrote where, set once by
/// [`start_egress_proxy_with`]. The child-readable CA certificate is the one a child must be
/// told about, and it lives under the state directory the PROXY was started with, which is not
/// always the one a child's config names: the containment battery starts one proxy from a
/// battery-owned state directory and runs every row with a state directory of its own.
pub struct RunningEgress {
    /// The child-readable CA certificate the running proxy wrote ([`ca_child_cert_path`]).
    pub ca_cert: std::path::PathBuf,
    /// The running proxy's content check (canaries and the vault fingerprint index).
    pub content: std::sync::Arc<crate::proxy::content::ContentCheck>,
    /// Notify to rebuild the fingerprint index now.
    pub rebuild: std::sync::Arc<tokio::sync::Notify>,
}

static RUNNING_EGRESS: std::sync::OnceLock<RunningEgress> = std::sync::OnceLock::new();

/// The egress proxy this process started, if any.
pub fn running_egress() -> Option<&'static RunningEgress> {
    RUNNING_EGRESS.get()
}

/// The CA certificate path a child is told to trust: the RUNNING proxy's when this process
/// started one, else the one under the child's own state directory (a child of a bridge whose
/// proxy runs elsewhere, e.g. the cluster's).
pub fn child_ca_path(
    running: Option<&std::path::Path>,
    state_dir: Option<&str>,
) -> Option<std::path::PathBuf> {
    running
        .map(std::path::Path::to_path_buf)
        .or_else(|| state_dir.map(|d| ca_child_cert_path(std::path::Path::new(d))))
}

/// Layer the proxies' variables onto a built child `Command`, once its turn token exists.
/// A no-op with both proxies off.
pub fn apply_child_env(
    cmd: &mut Command,
    cfg: &crate::Config,
    active: &ActiveModel,
    job_id: &str,
    token: &str,
) {
    if !cfg.proxy.any() {
        return;
    }
    let ca = child_ca_path(
        running_egress().map(|r| r.ca_cert.as_path()),
        cfg.state_dir.as_deref(),
    );
    let (set, remove) = child_env(cfg, active, job_id, token, ca.as_deref());
    for v in remove {
        cmd.env_remove(v);
    }
    for (k, v) in set {
        cmd.env(k, v);
    }
}

/// The Codex `-c` provider overrides for a proxied turn, or `None` when the model proxy does
/// not carry this turn (the harness then builds today's argv). The key is NOT here: only the
/// NAME of the variable [`apply_child_env`] fills with the turn token.
pub fn codex_proxy_provider_args(cfg: &crate::Config, active: &ActiveModel) -> Option<Vec<String>> {
    if !(cfg.proxy.model && cfg.gateway.enabled) {
        return None;
    }
    let route = route_for_active(active)?;
    let original = active.env.as_ref().map(|(u, _, _)| u.as_str());
    let base = proxied_base_url(cfg, route, original);
    // The slug rides here only for an OpenAI-kind entry, whose slug `codex_model_args` leaves
    // to the provider path; every other Codex model's slug is emitted there as before.
    let model = matches!(active.kind, ModelKind::OpenAi)
        .then(|| active.env.as_ref().map(|(_, _, m)| m.as_str()))
        .flatten();
    Some(crate::harness::codex_provider_args_for(&base, model))
}

// ---- STARTING THE PROXIES -------------------------------------------------------------

/// The registry as the model proxy's price book and key ring: the deck and the backend key of
/// each entry, by id or alias. Values are read once at startup, as every registry value is.
pub struct RegistryBook {
    models: Vec<RegistryModel>,
}

impl RegistryBook {
    pub fn new(models: &[RegistryModel]) -> RegistryBook {
        RegistryBook {
            models: models.to_vec(),
        }
    }

    fn find(&self, id: &str) -> Option<&RegistryModel> {
        self.models
            .iter()
            .find(|m| m.id == id || m.aliases.iter().any(|a| a == id))
    }
}

impl std::fmt::Debug for RegistryBook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.models.iter().map(|m| &m.id))
            .finish()
    }
}

impl crate::proxy::model::PriceBook for RegistryBook {
    fn deck(&self, model_id: &str) -> Option<crate::config::PriceDeck> {
        self.find(model_id).map(|m| m.price)
    }
}

impl crate::proxy::model::ModelKeys for RegistryBook {
    fn key_for(&self, model_id: &str) -> Option<crate::proxy::model::Secret> {
        self.find(model_id)
            .and_then(|m| m.backend.as_ref())
            .map(|(_, token, _)| token.trim())
            .filter(|t| !t.is_empty())
            .map(|t| crate::proxy::model::Secret::new(t.to_string()))
    }
}

/// Start the model proxy on `[proxy] model_listen` when `[proxy] model` is on. Returns the
/// bound address, `None` when off. A bind failure is fatal to the caller: a bridge that
/// points its children at a proxy it is not serving would fail every turn.
pub async fn start_model_proxy(
    cfg: &crate::Config,
    gw: std::sync::Arc<crate::gateway::Gateway>,
) -> Result<Option<std::net::SocketAddr>, String> {
    use crate::proxy::model::{
        CodexAuthFile, EnvCredential, ModelDeps, ModelProxy, NoCredential, SubscriptionLogin,
        Upstreams,
    };
    use std::sync::Arc;
    if !cfg.proxy.model {
        return Ok(None);
    }
    let book = Arc::new(RegistryBook::new(&cfg.model_registry.models));
    let chatgpt: Arc<dyn crate::proxy::model::ChatGptSource> =
        match CodexAuthFile::from_settings(&cfg.proxy, None) {
            Some(c) => Arc::new(c),
            None => Arc::new(NoCredential),
        };
    let local_tls = cfg
        .proxy
        .local_client_cert
        .as_ref()
        .and_then(|n| cfg.gateway.client_certs.get(n).cloned());
    let proxy = ModelProxy::new(ModelDeps {
        authority: gw.clone(),
        ledger: gw.ledger().cloned(),
        prices: book.clone(),
        keys: book,
        subscription: Arc::new(SubscriptionLogin::from_settings(&cfg.proxy)),
        api_key: Arc::new(EnvCredential {
            name: cfg.proxy.anthropic_api_key_env.clone(),
            vars: None,
        }),
        chatgpt,
        upstreams: Upstreams::from_settings(&cfg.proxy),
        local_tls,
    })?;
    let listener = tokio::net::TcpListener::bind(&cfg.proxy.model_listen)
        .await
        .map_err(|e| format!("[proxy] model_listen {}: {e}", cfg.proxy.model_listen))?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?;
    let proxy = Arc::new(proxy);
    tokio::spawn(async move {
        if let Err(e) = proxy.serve(listener).await {
            eprintln!("jesse-bridge: model proxy stopped: {e}");
        }
    });
    Ok(Some(bound))
}

/// The tree the fingerprint index and the canaries live in: `<vault>/vault` when the vault
/// root holds one (the Studio's layout), else the vault root itself.
pub fn vault_text_root(vault: &str) -> std::path::PathBuf {
    let inner = std::path::Path::new(vault).join("vault");
    if inner.is_dir() {
        inner
    } else {
        std::path::PathBuf::from(vault)
    }
}

/// `jesse-bridge canary new --path <vault-relative path> [--vault <dir>] [--state-dir <dir>]`:
/// write one canary note and record its marker. The operator runs it; it prints the path and
/// NEVER the marker (the marker is in the note and in `<state_dir>/canaries.json`, 0600).
pub fn run_canary_cli(args: &[String]) -> Result<String, String> {
    let mut path = None;
    let mut vault = std::env::var("JESSE_VAULT").ok();
    let mut state = std::env::var("JESSE_STATE_DIR").ok().or_else(|| {
        std::env::var("HOME")
            .ok()
            .map(|h| format!("{h}/.jesse-bridge"))
    });
    let mut it = args.iter();
    match it.next().map(String::as_str) {
        Some("new") => {}
        _ => return Err(CANARY_USAGE.into()),
    }
    while let Some(a) = it.next() {
        let (k, inline) = match a.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (a.as_str(), None),
        };
        let mut val = || inline.clone().or_else(|| it.next().cloned());
        match k {
            "--path" => path = val(),
            "--vault" => vault = val(),
            "--state-dir" => state = val(),
            other => return Err(format!("unknown argument {other:?}\n{CANARY_USAGE}")),
        }
    }
    let path = path.ok_or(CANARY_USAGE)?;
    let vault = vault.ok_or("no vault: set JESSE_VAULT or pass --vault")?;
    let state = state.ok_or("no state directory: set JESSE_STATE_DIR or pass --state-dir")?;
    let rec = crate::proxy::content::canary_new(
        std::path::Path::new(&state),
        &vault_text_root(&vault),
        &path,
    )?;
    Ok(format!(
        "canary written to {} and recorded under {}; the egress proxy refuses any request \
         carrying it\n",
        rec.path, state
    ))
}

pub const CANARY_USAGE: &str = "usage: jesse-bridge canary new --path <vault-relative path> \
                                [--vault <dir>] [--state-dir <dir>]";

/// The phone push on a content refusal: the turn, the host and the reason, NEVER the content.
pub struct ApnsRefusalNotifier {
    apns: Option<std::sync::Arc<crate::ApnsClient>>,
    devices: std::sync::Arc<crate::DeviceStore>,
}

impl crate::proxy::RefusalNotifier for ApnsRefusalNotifier {
    fn refused(&self, job_id: &str, host: &str, reason: crate::activity::EgressReason) {
        let why = match reason {
            crate::activity::EgressReason::Canary => "it carried a canary",
            crate::activity::EgressReason::ContentMatch => "it carried text from the vault",
            _ => "it was refused",
        };
        eprintln!("jesse-bridge: egress: refused a request from turn {job_id} to {host}: {why}");
        let Some(apns) = self.apns.clone() else {
            return;
        };
        let devices = self.devices.clone();
        let payload = serde_json::json!({
            "aps": {
                "alert": {
                    "title": "Outbound request blocked",
                    "body": format!(
                        "Turn {job_id} tried to send a request to {host}, and the egress proxy \
                         refused it because {why}. Nothing was sent."
                    ),
                },
                "sound": "default",
            },
            "job_id": job_id,
            "kind": "egress_refused",
        });
        let id = job_id.to_string();
        tokio::spawn(async move {
            crate::push_to_devices(
                &apns,
                &devices,
                "egress_refused",
                &id,
                payload.to_string().into_bytes(),
                None,
            )
            .await;
        });
    }
}

/// Start the egress proxy on `[proxy] egress_listen` when `[proxy] egress` is on: the
/// per-install CA (created on first start), the content check with its vault index (built at
/// boot, rebuilt every `[egress] index_interval_secs` and after each commit), and the listener.
pub async fn start_egress_proxy(
    state: &crate::AppState,
    gw: std::sync::Arc<crate::gateway::Gateway>,
) -> Result<Option<std::net::SocketAddr>, String> {
    start_egress_proxy_with(
        &state.cfg,
        gw,
        std::sync::Arc::new(ApnsRefusalNotifier {
            apns: state.apns.clone(),
            devices: state.devices.clone(),
        }),
    )
    .await
}

/// [`start_egress_proxy`] with the refusal notifier given: the containment battery's (which
/// has no app state and records the notifications it would have sent).
pub async fn start_egress_proxy_with(
    cfg: &crate::Config,
    gw: std::sync::Arc<crate::gateway::Gateway>,
    notifier: std::sync::Arc<dyn crate::proxy::RefusalNotifier>,
) -> Result<Option<std::net::SocketAddr>, String> {
    use std::sync::Arc;
    if !cfg.proxy.egress {
        return Ok(None);
    }
    let state_dir = cfg
        .state_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .ok_or("[proxy] egress needs a state directory for its CA")?;
    let ca = Arc::new(
        crate::proxy::ca::ProxyCa::load_or_create(&state_dir)
            .map_err(|e| format!("the egress CA under {}: {e}", state_dir.display()))?,
    );
    let content = Arc::new(crate::proxy::content::ContentCheck::new(
        &state_dir,
        cfg.proxy.policy.fingerprint_threshold,
    ));
    let (_task, rebuild) = crate::proxy::content::spawn_index_task(
        content.clone(),
        vault_text_root(&cfg.vault),
        std::time::Duration::from_secs(cfg.proxy.policy.index_interval_secs.max(60)),
    );
    // The first proxy a process starts is the one its children are pointed at.
    let _ = RUNNING_EGRESS.set(RunningEgress {
        ca_cert: ca.cert_path().to_path_buf(),
        content: content.clone(),
        rebuild: rebuild.clone(),
    });
    // Rebuild the vault index after every commit the bridge records.
    let commits = crate::activity::commit_notify();
    tokio::spawn(async move {
        loop {
            commits.notified().await;
            rebuild.notify_one();
        }
    });
    let proxy = Arc::new(crate::proxy::egress::EgressProxy::new(
        crate::proxy::egress::EgressDeps {
            authority: gw.clone(),
            ledger: gw.ledger().cloned(),
            notifier,
            recorder: Arc::new(crate::proxy::NoRecorder),
            ca,
            content,
            policy: cfg.proxy.policy.clone(),
            state_dir,
            resolver: Arc::new(crate::proxy::egress::SystemResolver),
            upstream_tls: crate::proxy::egress::default_upstream_tls(),
            owner_name: cfg.persona.owner_name.clone(),
        },
    ));
    let listener = tokio::net::TcpListener::bind(&cfg.proxy.egress_listen)
        .await
        .map_err(|e| format!("[proxy] egress_listen {}: {e}", cfg.proxy.egress_listen))?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?;
    tokio::spawn(async move { proxy.serve(listener).await });
    Ok(Some(bound))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(model: bool, egress: bool) -> crate::Config {
        let mut cfg = crate::testutil::test_config();
        cfg.gateway.enabled = true;
        cfg.proxy = crate::proxy::ProxySettings::from_toml(
            Some(crate::proxy::ProxyToml {
                model,
                egress,
                ..Default::default()
            }),
            None,
            None,
        );
        cfg
    }

    #[test]
    fn routes_per_kind_and_harness() {
        let cc = crate::harness::CLAUDE_CODE_ID;
        let cx = crate::harness::CODEX_ID;
        assert_eq!(
            route_for(ModelKind::Ambient, None, cc),
            Some(ROUTE_SUBSCRIPTION)
        );
        assert_eq!(
            route_for(ModelKind::Subscription, None, cc),
            Some(ROUTE_SUBSCRIPTION)
        );
        assert_eq!(
            route_for(
                ModelKind::Hosted,
                Some("https://api.fireworks.ai/inference"),
                cc
            ),
            Some(ROUTE_FIREWORKS)
        );
        assert_eq!(
            route_for(ModelKind::Local, Some("http://127.0.0.1:9100"), cc),
            Some(ROUTE_LOCAL)
        );
        assert_eq!(
            route_for(ModelKind::Hosted, Some("https://api.anthropic.com"), cc),
            Some(ROUTE_ANTHROPIC_API)
        );
        assert_eq!(
            route_for(
                ModelKind::Hosted,
                Some("https://generativelanguage.googleapis.com/v1beta/openai"),
                cc
            ),
            None
        );
        assert_eq!(
            route_for(ModelKind::Hosted, Some("https://x.example"), cx),
            Some(ROUTE_CHATGPT)
        );
        assert_eq!(
            route_for(
                ModelKind::OpenAi,
                Some("https://api.fireworks.ai/inference/v1"),
                cx
            ),
            Some(ROUTE_FIREWORKS)
        );
    }

    #[test]
    fn base_urls_are_rewritten_onto_the_proxy_keeping_their_suffix() {
        let cfg = cfg_with(true, false);
        assert_eq!(
            proxied_base_url(
                &cfg,
                ROUTE_FIREWORKS,
                Some("https://api.fireworks.ai/inference")
            ),
            "http://127.0.0.1:8767/model/fireworks"
        );
        assert_eq!(
            proxied_base_url(
                &cfg,
                ROUTE_FIREWORKS,
                Some("https://api.fireworks.ai/inference/v1/")
            ),
            "http://127.0.0.1:8767/model/fireworks/v1"
        );
        assert_eq!(
            proxied_base_url(&cfg, ROUTE_SUBSCRIPTION, None),
            "http://127.0.0.1:8767/model/anthropic-subscription"
        );
        assert_eq!(
            proxied_base_url(&cfg, ROUTE_LOCAL, Some("http://127.0.0.1:9100")),
            "http://127.0.0.1:8767/model/local"
        );
    }

    #[test]
    fn with_both_proxies_off_the_child_env_is_untouched() {
        let mut cfg = cfg_with(false, false);
        cfg.gateway.enabled = true;
        let active = ActiveModel::ambient();
        let (set, remove) = child_env(&cfg, &active, "job-1", "v1.tok", None);
        assert!(set.is_empty() && remove.is_empty());
        assert!(!needs_token(&cfg, &active));
    }

    #[test]
    fn the_model_proxy_gives_the_ambient_child_the_turn_token_and_no_provider_credential() {
        let cfg = cfg_with(true, false);
        let active = ActiveModel::ambient();
        let (set, remove) = child_env(&cfg, &active, "job-1", "v1.tok", None);
        assert_eq!(set["ANTHROPIC_AUTH_TOKEN"], "v1.tok");
        assert_eq!(
            set["ANTHROPIC_BASE_URL"],
            "http://127.0.0.1:8767/model/anthropic-subscription"
        );
        assert!(remove.contains(&"ANTHROPIC_API_KEY"));
        assert!(!set.contains_key("HTTPS_PROXY"));
        assert!(needs_token(&cfg, &active));
    }

    #[test]
    fn the_egress_proxy_gives_every_child_its_proxy_and_the_ca_in_every_trust_variable() {
        let cfg = cfg_with(false, true);
        let active = ActiveModel::ambient();
        let ca = std::path::Path::new("/state/proxy-ca-cert.pem");
        let (set, _) = child_env(&cfg, &active, "job-1", "v1.tok", Some(ca));
        for v in PROXY_VARS {
            assert_eq!(set[*v], "http://job-1:v1.tok@127.0.0.1:8768");
        }
        for v in CA_TRUST_VARS {
            assert_eq!(set[*v], "/state/proxy-ca-cert.pem");
        }
        assert_eq!(set["NO_PROXY"], "127.0.0.1");
        assert!(!set.contains_key("ANTHROPIC_AUTH_TOKEN"));
    }

    #[test]
    fn a_child_is_told_the_running_proxys_ca_before_its_own_state_dir() {
        let running = std::path::Path::new("/battery/proxy-state/proxy-ca-cert.pem");
        assert_eq!(
            child_ca_path(Some(running), Some("/row/state")).as_deref(),
            Some(running)
        );
        assert_eq!(
            child_ca_path(None, Some("/row/state")),
            Some(std::path::PathBuf::from("/row/state/proxy-ca-cert.pem"))
        );
        assert_eq!(child_ca_path(None, None), None);
    }

    #[test]
    fn origins_select_tiers() {
        let cfg = cfg_with(false, true);
        assert_eq!(tier_for_origin(&cfg, "phone"), Egress::OpenLogged);
        assert_eq!(tier_for_origin(&cfg, "schedule:not-a-job"), Egress::Closed);
    }
}

#[cfg(test)]
mod price_tests {
    /// EVERY BUILT-IN MODEL HAS ALL FOUR RATES. The model proxy refuses a model whose deck is
    /// incomplete (`unpriced_model`) rather than counting it as free, so a built-in entry
    /// without a cache-write rate would be a model that stops working the day the proxy is
    /// switched on. A model declared in the TOML states its own deck and is held to the same
    /// rule at the proxy.
    #[test]
    fn every_built_in_model_has_a_complete_four_rate_deck() {
        let reg = crate::config::ModelRegistry::from_env("");
        assert!(!reg.models.is_empty());
        for m in &reg.models {
            assert!(
                m.price.is_complete(),
                "model '{}' has an incomplete price deck: {:?}",
                m.id,
                m.price
            );
        }
    }

    #[test]
    fn a_deck_without_a_cache_write_rate_is_incomplete() {
        let mut d = crate::config::PriceDeck::ZERO;
        assert!(d.is_complete());
        d.cache_write_per_m = None;
        assert!(!d.is_complete());
        d.cache_write_per_m = Some(f64::NAN);
        assert!(!d.is_complete());
    }
}

#[cfg(test)]
mod canary_cli_tests {
    use super::*;

    /// The operator's command, on a fixture vault: it writes the note under `<vault>/vault`,
    /// records the marker 0600, never prints it, and the content check then refuses a request
    /// carrying it.
    #[test]
    fn canary_new_writes_a_note_the_content_check_then_refuses() {
        let root = std::env::temp_dir().join(format!("k04-canary-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("vault/Notes")).unwrap();
        let state = root.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let args: Vec<String> = [
            "new",
            "--path",
            "Notes/gate-codes.md",
            "--vault",
            root.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let out = run_canary_cli(&args).unwrap();
        let note = std::fs::read_to_string(root.join("vault/Notes/gate-codes.md")).unwrap();
        let recs = crate::proxy::content::canaries_load(&state).unwrap();
        assert_eq!(recs.len(), 1);
        let marker = &recs[0].marker;
        assert!(note.contains(marker.as_str()));
        assert!(
            !out.contains(marker.as_str()),
            "the CLI never prints the marker"
        );
        let check = crate::proxy::content::ContentCheck::new(&state, 3);
        assert_eq!(
            check.check(&format!("/search?q={marker}"), &[], b""),
            crate::proxy::content::ContentVerdict::Canary
        );
        assert!(
            run_canary_cli(&args).is_err(),
            "an existing file is refused"
        );
        assert!(run_canary_cli(&["new".to_string()]).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod false_positive_measure {
    /// THE CONTENT CHECK'S FALSE POSITIVE RATE, measured live. Ignored: it reads a real vault
    /// and fetches real public pages.
    ///
    /// ```text
    /// K04_FP_VAULT=~/jesse/vault K04_FP_URLS=/tmp/urls.txt \
    ///   cargo test --lib false_positive_rate -- --ignored --nocapture
    /// ```
    ///
    /// For each URL (pages the vault itself links to): (a) the READ an agent makes for it, the
    /// request target plus ordinary headers, through `ContentCheck::check`; (b) the page's own
    /// text as the BODY of a send, which is what an agent posting a public page's words
    /// somewhere looks like. (a) is the rate that matters for `read`s; (b) is the harsher case,
    /// since a vault note often quotes the page it links to.
    #[test]
    #[ignore]
    fn false_positive_rate() {
        let vault = std::env::var("K04_FP_VAULT").expect("K04_FP_VAULT");
        let urls =
            std::fs::read_to_string(std::env::var("K04_FP_URLS").expect("K04_FP_URLS")).unwrap();
        let threshold: usize = std::env::var("K04_FP_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        let want: usize = std::env::var("K04_FP_PAGES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(200);
        let t0 = std::time::Instant::now();
        let index = crate::proxy::content::FingerprintIndex::build(&super::vault_text_root(&vault));
        eprintln!(
            "index: {} windows over {} files in {:.1}s",
            index.len(),
            index.files(),
            t0.elapsed().as_secs_f64()
        );
        let state = std::env::temp_dir().join(format!("k04-fp-{}", std::process::id()));
        std::fs::create_dir_all(&state).unwrap();
        let check = crate::proxy::content::ContentCheck::new(&state, threshold);
        check.set_index(index);
        // REPLAY MODE: the request targets alone, no network. For a log of real fetches (a
        // week of turns' WebFetch URLs): each is checked exactly as the read it was.
        if std::env::var("K04_FP_REPLAY").is_ok() {
            let (mut n, mut hits) = (0usize, 0usize);
            for url in urls.lines().map(str::trim).filter(|u| !u.is_empty()) {
                let Ok(parsed) = reqwest::Url::parse(url) else {
                    continue;
                };
                n += 1;
                let target = match parsed.query() {
                    Some(q) => format!("{}?{q}", parsed.path()),
                    None => parsed.path().to_string(),
                };
                let headers = vec![("accept".to_string(), b"text/html,*/*".to_vec())];
                if check.check(&target, &headers, b"")
                    != crate::proxy::content::ContentVerdict::Clean
                {
                    hits += 1;
                    eprintln!("REPLAY FP: {}", parsed.host_str().unwrap_or(""));
                }
            }
            eprintln!(
                "replayed {n} real fetches: {hits} refused by the content check ({:.2}%), \
                 threshold {threshold}",
                100.0 * hits as f64 / n.max(1) as f64
            );
            let _ = std::fs::remove_dir_all(&state);
            return;
        }
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("Mozilla/5.0 (compatible; jesse-fp-measure)")
            .build()
            .unwrap();
        let (mut pages, mut read_hits, mut send_hits) = (0usize, 0usize, 0usize);
        for url in urls.lines().map(str::trim).filter(|u| !u.is_empty()) {
            if pages >= want {
                break;
            }
            let Ok(parsed) = reqwest::Url::parse(url) else {
                continue;
            };
            let body = rt.block_on(async {
                let r = client.get(url).send().await.ok()?;
                if !r.status().is_success() {
                    return None;
                }
                let ct = r
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                if !ct.contains("html") && !ct.contains("text") {
                    return None;
                }
                r.bytes().await.ok()
            });
            let Some(body) = body else { continue };
            pages += 1;
            let target = match parsed.query() {
                Some(q) => format!("{}?{q}", parsed.path()),
                None => parsed.path().to_string(),
            };
            let headers = vec![
                ("accept".to_string(), b"text/html,*/*".to_vec()),
                ("user-agent".to_string(), b"claude-code/2.1.322".to_vec()),
            ];
            if check.check(&target, &headers, b"") != crate::proxy::content::ContentVerdict::Clean {
                read_hits += 1;
                eprintln!("READ FP: {}", parsed.host_str().unwrap_or(""));
            }
            let text = strip_tags(&String::from_utf8_lossy(&body));
            if check.check("/submit", &headers, text.as_bytes())
                != crate::proxy::content::ContentVerdict::Clean
            {
                send_hits += 1;
                eprintln!(
                    "SEND-OF-PAGE-TEXT match: {}",
                    parsed.host_str().unwrap_or("")
                );
            }
        }
        eprintln!(
            "pages: {pages}; read false positives: {read_hits} ({:.2}%); page text as a send \
             body matching the vault: {send_hits} ({:.2}%); threshold {threshold}",
            100.0 * read_hits as f64 / pages.max(1) as f64,
            100.0 * send_hits as f64 / pages.max(1) as f64
        );
        let _ = std::fs::remove_dir_all(&state);
    }

    fn strip_tags(html: &str) -> String {
        let bytes = html.as_bytes();
        let lower = bytes.to_ascii_lowercase();
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len() / 2);
        let (mut i, mut in_tag) = (0usize, false);
        let mut skip_until: Option<&[u8]> = None;
        while i < bytes.len() {
            if let Some(end) = skip_until {
                if lower[i..].starts_with(end) {
                    i += end.len();
                    skip_until = None;
                } else {
                    i += 1;
                }
                continue;
            }
            if lower[i..].starts_with(b"<script") {
                skip_until = Some(b"</script>");
                continue;
            }
            if lower[i..].starts_with(b"<style") {
                skip_until = Some(b"</style>");
                continue;
            }
            match bytes[i] {
                b'<' => in_tag = true,
                b'>' => {
                    in_tag = false;
                    out.push(b' ');
                }
                c if !in_tag => out.push(c),
                _ => {}
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}
