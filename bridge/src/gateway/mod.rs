//! **THE TOOL GATEWAY** — the bridge brokers every MCP call, and the agent holds no MCP
//! credential.
//!
//! ---- WHAT CHANGES WHEN IT IS ON ------------------------------------------------
//!
//! With `[gateway] enabled = false` (the default) nothing here runs and every child is spawned
//! exactly as before. With it on:
//!
//!   * **The child's MCP config names a stub for every `gateway` server**, under the server's
//!     own name, so every tool keeps its exact `mcp__<server>__<tool>` name. The stub,
//!     `jesse-gateway-mcp`, holds nothing: it is told a socket path (or a URL and a token
//!     file) and relays four messages. `qmd` (placement `child`) is started by the child as
//!     before: it has no credential and reads a local index.
//!   * **The child's environment is cleared** to a named list ([`CHILD_ENV_PASSTHROUGH`]) plus
//!     what the harness sets explicitly, so neither the child nor anything it runs (a shell, a
//!     hook, a stub) can read a vendor credential the bridge holds.
//!   * **The bridge starts or calls each upstream itself**, lazily, on the turn's first
//!     `tools/list` or `tools/call` for it: a `bridge_stdio` server with `env_clear()` plus
//!     [`crate::mcpservers::BASE_ENV`] plus exactly the names its row lists, in its own process
//!     group; an `http` server with its bearer, named headers or client certificate read when
//!     the call is made; an `in_process` server through [`InProcessServer`].
//!   * **`tools/list` returns the granted intersection only**, and a `tools/call` for anything
//!     else is refused by exact name without reaching the upstream.
//!   * **Every call is a line in the activity ledger** ([`crate::activity`]), content-free.
//!
//! ---- THE PIECES ----------------------------------------------------------------
//!
//! [`Gateway`] is one per process: the token key, the live turns, the in-process servers, the
//! ledger. [`Gateway::open_turn`] admits a turn's servers, binds one unix socket per server
//! under `<state_dir>/gw/<job_id>/` (mode 0700) and returns a [`TurnGuard`]; dropping the guard
//! kills every upstream the turn started BY PROCESS GROUP, revokes the token and removes the
//! directory. The optional TCP listener ([`listen::serve_tcp`]) routes `POST /mcp/<server>` by
//! the turn token in `Authorization`, verified on every request.
//!
//! **NO GATEWAY CODE PATH READS THE MODEL OR THE HARNESS.** A turn is admitted on its job id,
//! its capability and its granted tools; the one per-harness value a server needs (`inbound`'s
//! harness id) is handed in by the caller as an opaque string. Asserted by
//! [`tests::no_gateway_source_reads_the_model_or_the_harness`].

pub mod direct;
pub mod listen;
pub mod session;
pub mod token;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::activity::ActivityLedger;
use crate::mcpservers::{row as table_row, HttpAuthSpec, McpServerRow, Placement, Transport};
use crate::Capability;

pub use session::{InProcessServer, TurnSession};
pub use token::{Claims, TokenError, TokenKey, TOKEN_GRACE_SECS};

/// The stub's file name: the one program a gateway-on child's MCP config may start for a
/// `gateway` server.
pub const STUB_BIN: &str = "jesse-gateway-mcp";

/// The longest unix socket path this platform binds (`sun_path` is 104 bytes on macOS,
/// including the terminating NUL).
pub const MAX_SOCKET_PATH: usize = 103;

/// What a gateway-on child inherits from the bridge's environment, BY NAME, before the
/// harness layers its own explicit variables on top. Nothing on this list names a vendor
/// credential; [`tests::the_child_passthrough_names_no_mcp_credential`] holds that against
/// every row of the table.
///
/// `USER` and `LOGNAME` are what the Claude Code CLI's Keychain login is looked up under;
/// `SHELL` is what its Bash tool runs.
pub const CHILD_ENV_PASSTHROUGH: &[&str] = &[
    "PATH", "HOME", "TMPDIR", "LANG", "TZ", "USER", "LOGNAME", "SHELL",
];

/// The Claude Code CLI's batch size for connecting LOCAL MCP servers, set on a gateway-on
/// child. Its default is 3 (remote servers: 20), and with the gateway every server is a local
/// stub; the CLI does not hold the turn for servers still connecting, so a small batch drops
/// the ones queued behind a slow upstream from the turn's toolset. Not a credential.
pub const CLAUDE_CONNECT_BATCH_ENV: &str = "MCP_SERVER_CONNECTION_BATCH_SIZE";
/// Above the main set's server count, so every stub connects in one batch.
pub const CLAUDE_CONNECT_BATCH: &str = "64";

// ---- SETTINGS ------------------------------------------------------------------------

/// `[gateway]` in the bridge TOML.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayToml {
    #[serde(default)]
    pub enabled: bool,
    /// `host:port` for the token-authenticated TCP listener. Off when absent, and the Studio
    /// never sets it in this step.
    pub listen: Option<String>,
    /// The base URL a CHILD reaches the TCP listener at. When set, children are handed
    /// `--url <child_url> --token-file <path>` instead of a socket (the cluster's shape).
    pub child_url: Option<String>,
    /// The stub's path, when it is not beside the bridge binary or on `PATH`.
    pub stub: Option<PathBuf>,
    /// Where turn directories (and their sockets) go, when `<state_dir>/gw` would make a socket
    /// path longer than a unix socket can name.
    pub socket_dir: Option<PathBuf>,
    /// Client certificates an `http` row may present, by name.
    #[serde(default)]
    pub client_certs: BTreeMap<String, ClientCertToml>,
}

/// One client certificate: PATHS only.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientCertToml {
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
    /// The CA the server's certificate must chain to, pinned in place of the bundled roots.
    pub ca_file: PathBuf,
}

/// `[mcp.<name>]` in the bridge TOML: a per-deployment override of one table row. Applied
/// only while the gateway is on: with it off every child starts its servers exactly as the
/// table spells them, so the shipped posture cannot be edited from a config file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpOverrideToml {
    /// `bridge_stdio` or `http`.
    pub transport: Option<String>,
    /// `gateway` or `child`.
    pub placement: Option<String>,
    pub url: Option<String>,
    /// The NAME of a variable carrying a bearer token.
    pub bearer_env: Option<String>,
    /// Header name to variable NAME.
    #[serde(default)]
    pub header_env: BTreeMap<String, String>,
    /// The name of a `[gateway.client_certs]` entry.
    pub client_cert: Option<String>,
}

/// Everything the gateway reads from the deployment, resolved.
#[derive(Debug, Clone, Default)]
pub struct GatewaySettings {
    pub enabled: bool,
    pub listen: Option<String>,
    pub child_url: Option<String>,
    pub stub: Option<PathBuf>,
    pub socket_dir: Option<PathBuf>,
    pub client_certs: BTreeMap<String, ClientCertToml>,
    pub overrides: BTreeMap<String, McpOverrideToml>,
    /// The one [`Gateway`] these settings build, shared by every clone of the `Config` that
    /// carries them. Built on first use ([`global`]).
    pub runtime: GatewayRuntime,
    /// `in_process` servers, by row name, registered by whatever builds the config (a test, or
    /// the cluster's wiring). None ships on the Studio.
    pub in_process: InProcessRegistry,
}

/// `in_process` servers by row name. Cloning shares them.
#[derive(Clone, Default)]
pub struct InProcessRegistry(pub BTreeMap<String, Arc<dyn InProcessServer>>);

impl std::fmt::Debug for InProcessRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

/// The slot the built [`Gateway`] lives in. Cloning shares it.
#[derive(Clone, Default)]
pub struct GatewayRuntime(Arc<OnceLock<Arc<Gateway>>>);

impl std::fmt::Debug for GatewayRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GatewayRuntime(built: {})", self.0.get().is_some())
    }
}

impl GatewaySettings {
    pub fn from_toml(
        gateway: Option<GatewayToml>,
        overrides: BTreeMap<String, McpOverrideToml>,
    ) -> GatewaySettings {
        let g = gateway.unwrap_or_default();
        GatewaySettings {
            enabled: g.enabled,
            listen: g.listen,
            child_url: g.child_url,
            stub: g.stub,
            socket_dir: g.socket_dir,
            client_certs: g.client_certs,
            overrides,
            runtime: GatewayRuntime::default(),
            in_process: InProcessRegistry::default(),
        }
    }

    /// Problems an operator must fix before the bridge boots with this table. Empty when the
    /// gateway is off: nothing here is read then.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.enabled {
            return out;
        }
        for (name, o) in &self.overrides {
            if let Err(e) = resolve_row(name, self) {
                out.push(e);
                continue;
            }
            if let Some(c) = &o.client_cert {
                if !self.client_certs.contains_key(c) {
                    out.push(format!(
                        "[mcp.{name}] names client_cert '{c}', which [gateway.client_certs] \
                         does not define"
                    ));
                }
            }
        }
        out
    }
}

// ---- ROW RESOLUTION -------------------------------------------------------------------

/// How a resolved HTTP server authenticates. NAMES and PATHS; never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedAuth {
    None,
    BearerEnv(String),
    HeadersEnv(Vec<(String, String)>),
    ClientCert(String),
}

/// One table row with the deployment's `[mcp.<name>]` override applied.
#[derive(Debug, Clone)]
pub struct ResolvedServer {
    pub name: String,
    /// The table row, for classes, the environment column and the result cap.
    pub row: &'static McpServerRow,
    pub transport: Transport,
    pub placement: Placement,
    pub command: String,
    pub args: Vec<String>,
    pub url: String,
    pub auth: ResolvedAuth,
}

/// Resolve `name` against the table and the deployment's override.
pub fn resolve_row(name: &str, settings: &GatewaySettings) -> Result<ResolvedServer, String> {
    let row = table_row(name).ok_or_else(|| format!("no MCP_SERVERS row named '{name}'"))?;
    let mut r = ResolvedServer {
        name: name.to_string(),
        row,
        transport: row.transport,
        placement: row.placement,
        command: row.command.to_string(),
        args: row.args.iter().map(|s| s.to_string()).collect(),
        url: row.url.to_string(),
        auth: match row.auth {
            HttpAuthSpec::None => ResolvedAuth::None,
            HttpAuthSpec::Bearer(v) => ResolvedAuth::BearerEnv(v.to_string()),
            HttpAuthSpec::Headers(p) => ResolvedAuth::HeadersEnv(
                p.iter()
                    .map(|(h, v)| (h.to_string(), v.to_string()))
                    .collect(),
            ),
            HttpAuthSpec::ClientCert(c) => ResolvedAuth::ClientCert(c.to_string()),
        },
    };
    let Some(o) = settings.overrides.get(name) else {
        return Ok(r);
    };
    if let Some(t) = &o.transport {
        r.transport = match t.as_str() {
            "bridge_stdio" => Transport::BridgeStdio,
            "http" => Transport::Http,
            other => {
                return Err(format!(
                    "[mcp.{name}] transport = \"{other}\": only \"bridge_stdio\" and \"http\" \
                     can be configured"
                ))
            }
        };
    }
    if let Some(p) = &o.placement {
        r.placement = match p.as_str() {
            "gateway" => Placement::Gateway,
            "child" => Placement::Child,
            other => {
                return Err(format!(
                    "[mcp.{name}] placement = \"{other}\": only \"gateway\" and \"child\""
                ))
            }
        };
    }
    if r.placement == Placement::Child && name != "qmd" {
        return Err(format!(
            "[mcp.{name}] placement = \"child\": only qmd, which holds no credential, may be \
             started by the child"
        ));
    }
    if let Some(u) = &o.url {
        r.url = u.clone();
    }
    let named_auth = [
        o.bearer_env.is_some(),
        !o.header_env.is_empty(),
        o.client_cert.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if named_auth > 1 {
        return Err(format!(
            "[mcp.{name}] sets more than one of bearer_env, header_env and client_cert"
        ));
    }
    if let Some(b) = &o.bearer_env {
        r.auth = ResolvedAuth::BearerEnv(b.clone());
    } else if !o.header_env.is_empty() {
        r.auth = ResolvedAuth::HeadersEnv(
            o.header_env
                .iter()
                .map(|(h, v)| (h.clone(), v.clone()))
                .collect(),
        );
    } else if let Some(c) = &o.client_cert {
        r.auth = ResolvedAuth::ClientCert(c.clone());
    }
    if r.transport == Transport::Http && r.url.is_empty() {
        return Err(format!("[mcp.{name}] is http with no url"));
    }
    if r.transport == Transport::BridgeStdio && r.command.is_empty() {
        return Err(format!(
            "[mcp.{name}] is bridge_stdio but the row has no command"
        ));
    }
    Ok(r)
}

// ---- WHAT A TURN IS ADMITTED TO ---------------------------------------------------------

/// One server a turn may reach through the gateway, and the tools it may call there.
#[derive(Debug, Clone)]
pub struct Admitted {
    pub server: ResolvedServer,
    /// Tool names ON THE SERVER, from the turn's allowlist. May be empty: the server is then
    /// listed with no tools and never started.
    pub tools: Vec<String>,
}

/// The servers in `mcp_config` (the bridge's `{"mcpServers":{…}}`) that the gateway serves for
/// a turn at `capability` under `allowed_tools`. `basic` admits none; a server placed in the
/// child is not admitted (the child starts it).
pub fn admitted_servers(
    settings: &GatewaySettings,
    mcp_config: &str,
    capability: Capability,
    allowed_tools: &str,
) -> Result<Vec<Admitted>, String> {
    if capability == Capability::Basic {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for name in config_server_names(mcp_config) {
        let server = resolve_row(&name, settings)?;
        if server.placement != Placement::Gateway {
            continue;
        }
        let tools = server.row.granted_tools(allowed_tools);
        out.push(Admitted { server, tools });
    }
    Ok(out)
}

/// The server names in a `{"mcpServers":{…}}` config, in the config's order.
pub fn config_server_names(mcp_config: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(mcp_config) else {
        return Vec::new();
    };
    v.get("mcpServers")
        .and_then(|m| m.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// What a turn hands [`Gateway::open_turn`].
#[derive(Debug, Clone)]
pub struct TurnSpec {
    pub job_id: String,
    pub capability: Capability,
    pub servers: Vec<Admitted>,
    /// The value for a row's `harness_env` variable (`inbound`'s). OPAQUE to the gateway: the
    /// caller decides it, nothing here interprets it.
    pub harness_env_value: Option<String>,
    /// How long the turn may run; the token expires [`TOKEN_GRACE_SECS`] after it.
    pub deadline: Duration,
    /// Bind one unix socket per server for a child to reach. `false` for an in-process turn
    /// (`direct`), which calls the session directly.
    pub bind_sockets: bool,
    /// Values for named variables that THIS turn supplies, consulted before the deployment's
    /// and the environment's. It changes which value a name has, never which names an
    /// upstream gets: that is still exactly its row's environment column. For a caller that
    /// runs several turns at once with different values (the containment battery's rows).
    pub env_overlay: BTreeMap<String, String>,
}

/// Where a child reaches one server.
pub fn socket_path(turn_dir: &Path, server: &str) -> PathBuf {
    turn_dir.join(format!("{server}.sock"))
}

/// The directory every turn directory lives under: `[gateway] socket_dir`, else
/// `<state_dir>/gw`.
pub fn gw_root(settings: &GatewaySettings, state_dir: &Path) -> PathBuf {
    settings
        .socket_dir
        .clone()
        .unwrap_or_else(|| state_dir.join("gw"))
}

/// `<root>/<job_id>`, where `root` is [`gw_root`].
pub fn turn_dir(root: &Path, job_id: &str) -> PathBuf {
    root.join(job_id)
}

/// `basic`, `read` or `write`.
pub fn capability_label(c: Capability) -> &'static str {
    match c {
        Capability::Basic => "basic",
        Capability::Read => "read",
        Capability::Write => "write",
    }
}

/// A job id is a directory name and a token claim: identifier characters only.
fn usable_job_id(job: &str) -> bool {
    !job.is_empty()
        && job.len() <= 64
        && job
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !job.starts_with('.')
}

// ---- THE GATEWAY --------------------------------------------------------------------

/// The process's gateway.
pub struct Gateway {
    pub(crate) key: TokenKey,
    pub(crate) state_dir: PathBuf,
    pub(crate) settings: GatewaySettings,
    pub(crate) sessions: Mutex<HashMap<String, Arc<TurnSession>>>,
    pub(crate) in_process: HashMap<String, Arc<dyn InProcessServer>>,
    pub(crate) ledger: Option<Arc<ActivityLedger>>,
    /// Values the DEPLOYMENT supplies for a variable name, read from the bridge's own config
    /// rather than its environment (`JESSE_HOUSE_TOKEN` from `[house] token_file`, the
    /// `healthdb` paths). Consulted before the environment. Never logged or printed.
    pub(crate) deployment_env: BTreeMap<String, String>,
    /// URLs the deployment supplies for a row (`house`'s, from `[house]`).
    pub(crate) deployment_url: BTreeMap<String, String>,
    /// An extra CA, PEM, per row (`house`'s `ca_file`).
    pub(crate) deployment_ca: BTreeMap<String, Vec<u8>>,
    pub(crate) call_timeout: Duration,
    pub(crate) connect_timeout: Duration,
}

impl std::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gateway")
            .field("state_dir", &self.state_dir)
            .field("enabled", &self.settings.enabled)
            .field("listen", &self.settings.listen)
            .field(
                "deployment_env",
                &self.deployment_env.keys().collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// The deployment's gateway, built on first use from `cfg` (its key generated then, and held
/// only in memory), or `None` when it is off or there is no state directory to put sockets in.
pub fn global(cfg: &crate::Config) -> Option<Arc<Gateway>> {
    if !cfg.gateway.enabled {
        return None;
    }
    let state = cfg.state_dir.as_deref()?;
    Some(
        cfg.gateway
            .runtime
            .0
            .get_or_init(|| Arc::new(Gateway::from_config(cfg, Path::new(state))))
            .clone(),
    )
}

impl Gateway {
    /// A gateway with no deployment values, for tests and for the cluster's own wiring.
    pub fn new(
        state_dir: &Path,
        settings: GatewaySettings,
        in_process: HashMap<String, Arc<dyn InProcessServer>>,
        ledger: Option<Arc<ActivityLedger>>,
    ) -> Gateway {
        // The settings are kept WITHOUT the runtime slot that will hold this gateway, so the
        // two never form a reference cycle.
        let settings = GatewaySettings {
            runtime: GatewayRuntime::default(),
            ..settings
        };
        Gateway {
            key: TokenKey::generate(),
            state_dir: state_dir.to_path_buf(),
            settings,
            sessions: Mutex::new(HashMap::new()),
            in_process,
            ledger,
            deployment_env: BTreeMap::new(),
            deployment_url: BTreeMap::new(),
            deployment_ca: BTreeMap::new(),
            call_timeout: jesse_agent::mcp::DEFAULT_CALL_TIMEOUT,
            connect_timeout: jesse_agent::mcp::DEFAULT_CONNECT_TIMEOUT,
        }
    }

    /// The deployment's gateway: its settings, its ledger, and the values its own config
    /// supplies for the `house` and `healthdb` rows.
    pub fn from_config(cfg: &crate::Config, state_dir: &Path) -> Gateway {
        let mut g = Gateway::new(
            state_dir,
            cfg.gateway.clone(),
            cfg.gateway
                .in_process
                .0
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            crate::activity::global(cfg),
        );
        if let Some(h) = cfg.house.ready() {
            g.deployment_env.insert(
                crate::HOUSE_TOKEN_ENV.to_string(),
                h.token.expose().to_string(),
            );
            g.deployment_url
                .insert(crate::HOUSE_SERVER.to_string(), h.url.clone());
            if let Some(ca) = &h.ca_file {
                if let Ok(pem) = std::fs::read(ca) {
                    g.deployment_ca.insert(crate::HOUSE_SERVER.to_string(), pem);
                }
            }
        }
        if let Some(env) = crate::healthdb_child_env(cfg) {
            for (k, v) in env {
                g.deployment_env.insert(k.to_string(), v);
            }
        }
        g
    }

    /// Replace the call and connect deadlines. Tests use short ones.
    pub fn with_timeouts(mut self, call: Duration, connect: Duration) -> Gateway {
        self.call_timeout = call;
        self.connect_timeout = connect;
        self
    }

    /// Sign arbitrary claims with this gateway's key. ONLY in a `containment-probe` build, for
    /// the test that an expired token is refused: a deployed bridge has no way to mint a token
    /// for anything but a turn it admitted.
    #[cfg(feature = "containment-probe")]
    #[doc(hidden)]
    pub fn sign_claims_for_tests(&self, claims: &Claims) -> String {
        self.key.sign(claims)
    }

    pub fn settings(&self) -> &GatewaySettings {
        &self.settings
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn ledger(&self) -> Option<&Arc<ActivityLedger>> {
        self.ledger.as_ref()
    }

    /// The value of variable `name` for an upstream: the deployment's own value first, then
    /// the bridge's environment. `None` when neither has it, and the name is then simply not
    /// passed.
    pub(crate) fn lookup(&self, name: &str) -> Option<String> {
        self.deployment_env
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok())
    }

    /// Admit a turn: sign its token, register its session, and (for a spawned child) bind one
    /// unix socket per admitted server under a fresh `0700` directory.
    pub async fn open_turn(self: &Arc<Self>, spec: TurnSpec) -> Result<TurnGuard, String> {
        if !usable_job_id(&spec.job_id) {
            return Err(format!(
                "the job id {:?} cannot name a gateway turn directory",
                spec.job_id
            ));
        }
        let mut servers: Vec<String> = spec.servers.iter().map(|a| a.server.name.clone()).collect();
        servers.sort();
        let claims = Claims {
            job: spec.job_id.clone(),
            cap: capability_label(spec.capability).to_string(),
            servers,
            tier: String::new(),
            exp: token::now_secs() + spec.deadline.as_secs() + TOKEN_GRACE_SECS,
        };
        let token = self.key.sign(&claims);
        let dir = turn_dir(&gw_root(&self.settings, &self.state_dir), &spec.job_id);
        let session = Arc::new(TurnSession::new(
            self.clone(),
            spec.job_id.clone(),
            token,
            claims,
            spec.servers,
            spec.harness_env_value,
            spec.env_overlay,
            dir.clone(),
        ));
        {
            let mut live = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            if live.contains_key(&spec.job_id) {
                return Err(format!(
                    "a gateway turn for job {} is already open",
                    spec.job_id
                ));
            }
            live.insert(spec.job_id.clone(), session.clone());
        }
        let guard = TurnGuard {
            gateway: self.clone(),
            session: session.clone(),
        };
        if spec.bind_sockets || self.settings.child_url.is_some() {
            make_private_dir(&dir).map_err(|e| {
                format!(
                    "could not create the gateway turn directory {}: {e}",
                    dir.display()
                )
            })?;
        }
        if spec.bind_sockets {
            for name in session.server_names() {
                let path = socket_path(&dir, &name);
                if path.as_os_str().len() > MAX_SOCKET_PATH {
                    return Err(format!(
                        "the gateway socket path {} is longer than the {MAX_SOCKET_PATH} bytes \
                         a unix socket can name; use a shorter state directory",
                        path.display()
                    ));
                }
                let task = listen::serve_unix(session.clone(), name.clone(), &path)
                    .map_err(|e| self.bind_error(&path, &e))?;
                session.add_task(task);
            }
        }
        if self.settings.child_url.is_some() {
            write_private_file(&dir.join("token"), session.token().as_bytes())
                .map_err(|e| format!("could not write the turn token file: {e}"))?;
        }
        Ok(guard)
    }

    /// Why a turn's socket did not bind. Running out of descriptors is named as that, with the
    /// soft limit and the turns open (this one included), so the log says what to raise rather
    /// than only `os error 24`.
    fn bind_error(&self, path: &Path, e: &std::io::Error) -> String {
        if !crate::nofile::is_out_of_files(e) {
            return format!("could not bind {}: {e}", path.display());
        }
        let open = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len();
        let soft = crate::nofile::current()
            .map_or("unknown".to_string(), |l| crate::nofile::label(l.soft));
        format!(
            "could not bind {}: the bridge ran out of open files ({e}); open files soft limit \
             {soft}, {open} gateway turn(s) open including this one",
            path.display()
        )
    }

    /// The live session for `job`, if its turn is open.
    pub(crate) fn session(&self, job: &str) -> Option<Arc<TurnSession>> {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(job)
            .cloned()
    }

    /// End a turn: forget its session (which REVOKES its token: a token whose session is gone
    /// is refused), kill everything it started, remove its directory.
    fn close_turn(&self, session: &Arc<TurnSession>) {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(session.job_id());
        session.shutdown();
        let _ = std::fs::remove_dir_all(session.dir());
    }

    /// Remove every turn directory a previous process left under `root` ([`gw_root`]). Run
    /// once at startup, before any turn is admitted.
    pub fn sweep(root: &Path) -> usize {
        let Ok(entries) = std::fs::read_dir(root) else {
            return 0;
        };
        let mut n = 0;
        for e in entries.flatten() {
            if std::fs::remove_dir_all(e.path()).is_ok() {
                n += 1;
            }
        }
        n
    }

    /// What `/health` reports per upstream: name, transport, placement and whether it can be
    /// reached (a `bridge_stdio` command resolves on `PATH`; an `http` URL parses; an
    /// `in_process` server is registered).
    pub fn upstream_health(&self, servers: &[String]) -> Vec<Value> {
        servers
            .iter()
            .filter_map(|n| resolve_row(n, &self.settings).ok())
            .map(|r| {
                let reachable = match r.transport {
                    Transport::BridgeStdio => resolve_on_path(&r.command).is_some(),
                    Transport::Http => {
                        let url = self.deployment_url.get(&r.name).unwrap_or(&r.url);
                        reqwest::Url::parse(url).is_ok()
                    }
                    Transport::InProcess => self.in_process.contains_key(&r.name),
                };
                serde_json::json!({
                    "server": r.name,
                    "transport": r.transport.label(),
                    "placement": r.placement.label(),
                    "reachable": reachable,
                })
            })
            .collect()
    }
}

/// An open gateway turn. Dropping it closes the turn.
pub struct TurnGuard {
    gateway: Arc<Gateway>,
    session: Arc<TurnSession>,
}

impl TurnGuard {
    pub fn session(&self) -> &Arc<TurnSession> {
        &self.session
    }

    /// The turn's signed token. For a child reached over HTTP, and for tests.
    pub fn token(&self) -> &str {
        self.session.token()
    }

    pub fn dir(&self) -> &Path {
        self.session.dir()
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.gateway.close_turn(&self.session);
    }
}

fn make_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if let Some(parent) = dir.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    std::fs::DirBuilder::new().mode(0o700).create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)
}

/// `command` resolved on this process's `PATH`, or as given when it is a path.
pub fn resolve_on_path(command: &str) -> Option<PathBuf> {
    if command.contains('/') {
        let p = PathBuf::from(command);
        return p.is_file().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(command))
        .find(|p| p.is_file())
}

/// Where the stub is: the configured path, else beside the running bridge binary, else on
/// `PATH`. `None` is fatal at startup when the gateway is on.
pub fn stub_path(settings: &GatewaySettings) -> Option<PathBuf> {
    if let Some(p) = &settings.stub {
        return p.is_file().then(|| p.clone());
    }
    if let Some(beside) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(STUB_BIN)))
        .filter(|p| p.is_file())
    {
        return Some(beside);
    }
    resolve_on_path(STUB_BIN)
}

/// The startup gate's gateway half. Empty when the gateway is off.
///
/// **A MISSING STUB IS FATAL** when the gateway is on: every gateway-placed server of every
/// child would otherwise be a command that does not exist, and the turn would run with none of
/// them while looking healthy.
pub fn validate_gateway(cfg: &crate::Config) -> Vec<crate::ConfigError> {
    let s = &cfg.gateway;
    if !s.enabled {
        return Vec::new();
    }
    let mut problems = s.problems();
    if cfg.state_dir.is_none() {
        problems.push(
            "[gateway] enabled needs a state directory for its turn sockets (JESSE_STATE_DIR \
             or HOME)"
                .into(),
        );
    }
    if stub_path(s).is_none() {
        problems.push(format!(
            "[gateway] enabled but the stub {STUB_BIN} is neither configured ([gateway] stub), \
             beside the bridge binary, nor on PATH"
        ));
    }
    if let Some(l) = &s.listen {
        if l.parse::<std::net::SocketAddr>().is_err() {
            problems.push(format!(
                "[gateway] listen = \"{l}\" is not a host:port address"
            ));
        }
    }
    problems
        .into_iter()
        .map(|message| crate::ConfigError {
            model: None,
            message,
        })
        .collect()
}

// ---- THE CHILD'S SIDE -----------------------------------------------------------------

/// The MCP config a gateway-on child is given: every admitted server becomes a stub entry
/// under its own name; every other server (`qmd`, placed in the child) is kept as the table
/// renders it. `rendered` is the config after the deployment's own rendering (house URL,
/// healthdb); its server set is what the turn may load.
pub fn child_mcp_config(
    settings: &GatewaySettings,
    rendered: &str,
    state_dir: &Path,
    job_id: &str,
    stub: &Path,
) -> Result<String, String> {
    let v: Value = serde_json::from_str(rendered).map_err(|e| format!("MCP config: {e}"))?;
    let Some(servers) = v.get("mcpServers").and_then(|m| m.as_object()) else {
        return Err("MCP config has no mcpServers object".into());
    };
    let dir = turn_dir(&gw_root(settings, state_dir), job_id);
    let mut out = serde_json::Map::new();
    for (name, spec) in servers {
        let r = resolve_row(name, settings)?;
        if r.placement == Placement::Child {
            out.insert(name.clone(), spec.clone());
            continue;
        }
        let args: Vec<String> = match &settings.child_url {
            Some(url) => vec![
                "--upstream".into(),
                name.clone(),
                "--url".into(),
                url.clone(),
                "--token-file".into(),
                dir.join("token").display().to_string(),
            ],
            None => vec![
                "--upstream".into(),
                name.clone(),
                "--socket".into(),
                socket_path(&dir, name).display().to_string(),
            ],
        };
        out.insert(
            name.clone(),
            serde_json::json!({
                "type": "stdio",
                "command": stub.display().to_string(),
                "args": args,
            }),
        );
    }
    Ok(serde_json::json!({ "mcpServers": out }).to_string())
}

/// `config` with its `in_process` rows rendered in, for a deployment whose gateway is ON and has
/// each such server registered ([`GatewaySettings::in_process`]). With the gateway off, or for a
/// set that names no registered `in_process` row, `config` unchanged, byte for byte: every
/// gateway-off config, argv fixture and containment record is exactly what it was before
/// `in_process` rows could be named in a set.
///
/// The rendered entry ([`crate::mcpservers::render_in_process_entry`]) is never handed to a child:
/// [`admitted_servers`] admits it and [`child_mcp_config`] swaps it for the stub, as for every
/// other gateway-placed server. A row that is named but NOT registered stays out, so a child is
/// never given a stub for a server nothing in the bridge would answer.
pub fn render_in_process_mcp<'a>(
    cfg: &crate::Config,
    config: &'a str,
) -> std::borrow::Cow<'a, str> {
    if !cfg.gateway.enabled {
        return std::borrow::Cow::Borrowed(config);
    }
    match crate::mcpservers::gateway_config_json(config, |name| {
        cfg.gateway.in_process.0.contains_key(name)
    }) {
        Some(c) => std::borrow::Cow::Owned(c),
        None => std::borrow::Cow::Borrowed(config),
    }
}

/// The `--mcp-config` a child gets, given the config the deployment rendered. With the gateway
/// off, `rendered` unchanged. With it on, [`child_mcp_config`] for this turn, or, if that
/// cannot be built, NO servers at all: a child is never given the direct-spawn config while
/// its environment is scrubbed, and never a broken one.
pub fn child_config_for_turn(cfg: &crate::Config, rendered: &str, job_id: &str) -> String {
    if !cfg.gateway.enabled {
        return rendered.to_string();
    }
    let built = cfg
        .state_dir
        .as_deref()
        .ok_or_else(|| "no state directory".to_string())
        .and_then(|dir| {
            let stub = stub_path(&cfg.gateway).ok_or_else(|| format!("no {STUB_BIN} to start"))?;
            child_mcp_config(&cfg.gateway, rendered, Path::new(dir), job_id, &stub)
        });
    match built {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gateway: job {job_id} gets no MCP servers: {e}");
            crate::EMPTY_MCP_CONFIG.to_string()
        }
    }
}

/// Open the gateway turn a SPAWNED child needs: its servers admitted from the same config and
/// allowlist its argv was built from, one socket each. `None` when the gateway is off or the
/// child loads no gateway server. `harness_env_value` is opaque here (see [`TurnSpec`]).
pub async fn open_for_child(
    cfg: &crate::Config,
    req: &crate::TurnRequest<'_>,
    harness_env_value: &str,
    deadline: Duration,
) -> Result<Option<TurnGuard>, String> {
    let Some(gw) = global(cfg) else {
        return Ok(None);
    };
    let rendered = crate::render_child_mcp(cfg, req.mcp_config);
    let set = crate::McpSet::from_config(req.mcp_config).unwrap_or(crate::McpSet::None);
    let allowed = crate::row_allowed_tools(cfg, req.capability, set);
    let servers = admitted_servers(&cfg.gateway, &rendered, req.capability, allowed)?;
    if servers.is_empty() {
        return Ok(None);
    }
    gw.open_turn(TurnSpec {
        job_id: req.turn_id.to_string(),
        capability: req.capability,
        servers,
        harness_env_value: Some(harness_env_value.to_string()),
        deadline,
        bind_sockets: true,
        env_overlay: BTreeMap::new(),
    })
    .await
    .map(Some)
}

/// [`open_for_child`] for the containment battery's gateway probes: the probe upstream is
/// granted `extra_grants` on top of the row's allowlist (the child's own argv is unchanged),
/// and `env_overlay` supplies this row's log and dump paths. Only in a `containment-probe`
/// build.
#[cfg(feature = "containment-probe")]
pub async fn open_for_probe_child(
    cfg: &crate::Config,
    req: &crate::TurnRequest<'_>,
    harness_env_value: &str,
    deadline: Duration,
    extra_grants: &str,
    env_overlay: BTreeMap<String, String>,
) -> Result<Option<TurnGuard>, String> {
    let Some(gw) = global(cfg) else {
        return Ok(None);
    };
    let rendered = crate::render_child_mcp(cfg, req.mcp_config);
    let set = crate::McpSet::from_config(req.mcp_config).unwrap_or(crate::McpSet::None);
    let allowed = format!(
        "{},{extra_grants}",
        crate::row_allowed_tools(cfg, req.capability, set)
    );
    let servers = admitted_servers(&cfg.gateway, &rendered, req.capability, &allowed)?;
    if servers.is_empty() {
        return Ok(None);
    }
    gw.open_turn(TurnSpec {
        job_id: req.turn_id.to_string(),
        capability: req.capability,
        servers,
        harness_env_value: Some(harness_env_value.to_string()),
        deadline,
        bind_sockets: true,
        env_overlay,
    })
    .await
    .map(Some)
}

/// Whether `command` is the stub. Codex's translation uses this to give a stub entry no
/// environment at all.
pub fn is_stub_command(command: &str) -> bool {
    Path::new(command)
        .file_name()
        .is_some_and(|n| n == STUB_BIN)
}

/// Clear a gateway-on child's environment to [`CHILD_ENV_PASSTHROUGH`]. Called FIRST, before
/// the harness layers its own explicit variables, because `env_clear` also clears anything
/// set before it.
pub fn scrub_child_env(cmd: &mut tokio::process::Command) {
    cmd.env_clear();
    for name in CHILD_ENV_PASSTHROUGH {
        if let Some(v) = std::env::var_os(name) {
            cmd.env(name, v);
        }
    }
}

#[cfg(test)]
mod tests;
