//! One admitted turn: the four-message MCP subset it is served, and the upstreams it starts.
//!
//! ---- THE SUBSET ------------------------------------------------------------------
//!
//! `initialize`, `notifications/initialized`, `tools/list`, `tools/call`. Nothing else: any
//! other request is answered with a protocol error and the session that sent it is CLOSED,
//! because a stub that sends something outside the subset is not the stub this bridge ships.
//! The gateway declares only the `tools` capability, so a conforming client has no reason to
//! send anything more.
//!
//! `tools/list` returns the GRANTED INTERSECTION: the upstream's tools whose names the turn's
//! allowlist grants, each relayed as the upstream described it. A server with no granted tool
//! answers an empty list and is never started. `tools/call` checks the name against the grant
//! by exact match BEFORE anything is sent upstream; an ungranted call is refused with an error
//! and recorded as `refused`, and the upstream never sees it.
//!
//! ---- UPSTREAMS ---------------------------------------------------------------------
//!
//! Started on first need and kept for the turn. A failure to start is remembered for the turn
//! (the same "a server that will not start costs one attempt" rule the agent crate applies),
//! so a broken server is not re-spawned on every call.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use jesse_agent::mcp::{HttpAuth, McpClient, MutualTls};
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use super::{Admitted, Claims, Gateway, ResolvedAuth};
use crate::activity::{now_ms, ActivityLine, Token, ToolCallLine, ToolOutcome};
use crate::mcpservers::{Transport, BASE_ENV};

/// A server implemented inside the bridge: no process, no socket, no credential of its own
/// beyond what the bridge already holds.
pub trait InProcessServer: Send + Sync {
    /// Every tool, as an MCP `tools/list` entry (`name`, `description`, `inputSchema`).
    fn tools(&self) -> Vec<Value>;
    /// One call. `Ok` is an MCP `CallToolResult` object; `Err` is a failure to answer.
    fn call<'a>(
        &'a self,
        tool: &'a str,
        arguments: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send + 'a>>;
}

enum Upstream {
    Client(Arc<McpClient>),
    InProcess(Arc<dyn InProcessServer>),
    /// It would not start this turn; the reason is for the operator log only.
    Failed,
}

impl Clone for Upstream {
    fn clone(&self) -> Self {
        match self {
            Upstream::Client(c) => Upstream::Client(c.clone()),
            Upstream::InProcess(s) => Upstream::InProcess(s.clone()),
            Upstream::Failed => Upstream::Failed,
        }
    }
}

/// What the subset handler decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Handled {
    /// Send this reply.
    Reply(Value),
    /// A notification: nothing to send.
    NoReply,
    /// Send this error reply, then close the session that sent the message.
    Close(Value),
}

/// One admitted turn.
pub struct TurnSession {
    gateway: Arc<Gateway>,
    job_id: String,
    token: String,
    claims: Claims,
    servers: BTreeMap<String, Admitted>,
    harness_env_value: Option<String>,
    env_overlay: BTreeMap<String, String>,
    dir: PathBuf,
    /// One cell per server, so servers start CONCURRENTLY (a child lists every server at
    /// once) while each starts at most once.
    upstreams: Mutex<HashMap<String, Arc<tokio::sync::OnceCell<Upstream>>>>,
    /// Process-group ids of every stdio upstream started, for the kill at turn end.
    pgids: Mutex<Vec<u32>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// Live HTTP session ids handed out on `initialize`, per server.
    http_sessions: Mutex<HashMap<String, String>>,
}

impl std::fmt::Debug for TurnSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnSession")
            .field("job_id", &self.job_id)
            .field("servers", &self.servers.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl TurnSession {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        gateway: Arc<Gateway>,
        job_id: String,
        token: String,
        claims: Claims,
        servers: Vec<Admitted>,
        harness_env_value: Option<String>,
        env_overlay: BTreeMap<String, String>,
        dir: PathBuf,
    ) -> TurnSession {
        TurnSession {
            gateway,
            job_id,
            token,
            claims,
            servers: servers
                .into_iter()
                .map(|a| (a.server.name.clone(), a))
                .collect(),
            harness_env_value,
            env_overlay,
            dir,
            upstreams: Mutex::new(HashMap::new()),
            pgids: Mutex::new(Vec::new()),
            tasks: Mutex::new(Vec::new()),
            http_sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn claims(&self) -> &Claims {
        &self.claims
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The admitted servers, sorted.
    pub fn server_names(&self) -> Vec<String> {
        self.servers.keys().cloned().collect()
    }

    /// The tools granted on `server`, by their names on the server.
    pub fn granted(&self, server: &str) -> &[String] {
        self.servers
            .get(server)
            .map(|a| a.tools.as_slice())
            .unwrap_or(&[])
    }

    /// The admitted server's resolved row.
    pub fn admitted(&self, server: &str) -> Option<&Admitted> {
        self.servers.get(server)
    }

    pub(super) fn add_task(&self, t: JoinHandle<()>) {
        self.tasks.lock().unwrap_or_else(|p| p.into_inner()).push(t);
    }

    pub(super) fn open_http_session(&self, server: &str) -> String {
        let id = uuid::Uuid::new_v4().simple().to_string();
        self.http_sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.clone(), server.to_string());
        id
    }

    pub(super) fn http_session_is(&self, id: &str, server: &str) -> bool {
        self.http_sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
            .is_some_and(|s| s == server)
    }

    pub(super) fn close_http_session(&self, id: &str) {
        self.http_sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(id);
    }

    /// Serve one message for `server`. The whole subset, transport-free: the unix listener,
    /// the TCP listener and the in-process tool set all come through here.
    pub async fn handle(&self, server: &str, msg: &Value) -> Handled {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        if !self.servers.contains_key(server) {
            return Handled::Close(error(id, -32600, "this server is not admitted to the turn"));
        }
        let Some(id) = id else {
            // A notification. `notifications/initialized` is the only one the subset has, and
            // it needs no answer; any other is outside the subset.
            return if method == "notifications/initialized" {
                Handled::NoReply
            } else {
                Handled::Close(error(
                    None,
                    -32601,
                    "the gateway serves initialize, tools/list and tools/call only",
                ))
            };
        };
        match method {
            "initialize" => {
                // The client's requested version is echoed: the subset's three requests have
                // the same shape in every published version.
                let version = msg["params"]["protocolVersion"]
                    .as_str()
                    .unwrap_or(jesse_agent::mcp::PROTOCOL_VERSION);
                Handled::Reply(reply(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {}},
                        "serverInfo": {
                            "name": format!("jesse-gateway/{server}"),
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                    }),
                ))
            }
            "tools/list" => match self.list(server).await {
                Ok(tools) => Handled::Reply(reply(id, json!({ "tools": tools }))),
                Err(e) => Handled::Reply(error(Some(id), -32603, &e)),
            },
            "tools/call" => {
                let tool = msg["params"]["name"].as_str().unwrap_or("").to_string();
                let args = match msg["params"].get("arguments") {
                    None | Some(Value::Null) => json!({}),
                    Some(a) => a.clone(),
                };
                match self.call(server, &tool, args).await {
                    Ok(result) => Handled::Reply(reply(id, result)),
                    Err(e) => Handled::Reply(error(Some(id), e.0, &e.1)),
                }
            }
            _ => Handled::Close(error(
                Some(id),
                -32601,
                "the gateway serves initialize, tools/list and tools/call only",
            )),
        }
    }

    /// The granted intersection of the upstream's tools, each as the upstream described it.
    pub async fn list(&self, server: &str) -> Result<Vec<Value>, String> {
        let granted = self.granted(server);
        if granted.is_empty() {
            return Ok(Vec::new());
        }
        let advertised: Vec<Value> = match self.upstream(server).await {
            Upstream::Client(c) => c
                .list_tools()
                .await
                .map_err(|e| format!("upstream '{server}' would not list its tools: {e}"))?
                .into_iter()
                .map(|t| t.raw)
                .collect(),
            Upstream::InProcess(s) => s.tools(),
            Upstream::Failed => return Err(format!("upstream '{server}' did not start")),
        };
        Ok(advertised
            .into_iter()
            .filter(|t| {
                t.get("name")
                    .and_then(|n| n.as_str())
                    .is_some_and(|n| granted.iter().any(|g| g == n))
            })
            .collect())
    }

    /// One call, checked against the grant by exact name before anything goes upstream.
    /// `Err` is `(JSON-RPC code, message)`.
    pub async fn call(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, (i64, String)> {
        let started = Instant::now();
        let at = now_ms();
        let request_bytes = serde_json::to_vec(&arguments)
            .map(|b| b.len() as u64)
            .unwrap_or(0);
        let ledger = |outcome: ToolOutcome, response_bytes: u64| {
            if let Some(l) = self.gateway.ledger.as_ref() {
                l.record(&ActivityLine::tool_call(ToolCallLine {
                    job_id: Token::new(&self.job_id),
                    server: Token::new(server),
                    tool: Token::new(tool),
                    at_ms: at,
                    duration_ms: started.elapsed().as_millis() as u64,
                    outcome,
                    request_bytes,
                    response_bytes,
                }));
            }
        };
        if !self.granted(server).iter().any(|g| g == tool) {
            ledger(ToolOutcome::Refused, 0);
            return Err((
                -32602,
                format!("the tool '{tool}' is not granted to this turn"),
            ));
        }
        if !arguments.is_object() {
            ledger(ToolOutcome::Refused, 0);
            return Err((-32602, "tool arguments must be an object".into()));
        }
        let result = match self.upstream(server).await {
            Upstream::Client(c) => c
                .call_tool(tool, arguments)
                .await
                .map(|o| o.raw)
                .map_err(|e| format!("{e}")),
            Upstream::InProcess(s) => s.call(tool, arguments).await,
            Upstream::Failed => Err(format!("upstream '{server}' did not start")),
        };
        match result {
            Err(e) => {
                ledger(ToolOutcome::Failed, 0);
                eprintln!(
                    "gateway: {server}::{tool} failed for job {}: {e}",
                    self.job_id
                );
                Err((-32603, format!("the upstream '{server}' did not answer")))
            }
            Ok(v) => {
                let bytes = serde_json::to_vec(&v).map(|b| b.len()).unwrap_or(0);
                let cap = self
                    .servers
                    .get(server)
                    .map(|a| a.server.row.result_cap)
                    .unwrap_or(crate::mcpservers::DEFAULT_RESULT_CAP);
                if bytes > cap {
                    ledger(ToolOutcome::Truncated, bytes as u64);
                    return Ok(json!({
                        "content": [{"type": "text", "text": format!(
                            "The result was {bytes} bytes, more than the {cap} bytes the \
                             gateway passes from '{server}', and was withheld."
                        )}],
                        "isError": true,
                    }));
                }
                ledger(ToolOutcome::Ok, bytes as u64);
                Ok(v)
            }
        }
    }

    /// The upstream for `server`, started on first need.
    async fn upstream(&self, server: &str) -> Upstream {
        let cell = self
            .upstreams
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(server.to_string())
            .or_default()
            .clone();
        cell.get_or_init(|| async {
            match self.start(server).await {
                Ok(u) => u,
                Err(e) => {
                    eprintln!(
                        "gateway: upstream '{server}' did not start for job {}: {e}",
                        self.job_id
                    );
                    Upstream::Failed
                }
            }
        })
        .await
        .clone()
    }

    async fn start(&self, server: &str) -> Result<Upstream, String> {
        let a = self
            .servers
            .get(server)
            .ok_or_else(|| format!("'{server}' is not admitted"))?;
        let r = &a.server;
        let gw = &self.gateway;
        match r.transport {
            Transport::InProcess => gw
                .in_process
                .get(server)
                .cloned()
                .map(Upstream::InProcess)
                .ok_or_else(|| format!("no in-process server is registered as '{server}'")),
            Transport::BridgeStdio => {
                let mut env = BTreeMap::new();
                for name in BASE_ENV.iter().chain(r.row.env.iter()) {
                    if let Some(v) = self
                        .env_overlay
                        .get(*name)
                        .cloned()
                        .or_else(|| gw.lookup(name))
                    {
                        env.insert(name.to_string(), v);
                    }
                }
                if let (Some(var), Some(v)) = (r.row.harness_env, &self.harness_env_value) {
                    env.insert(var.to_string(), v.clone());
                }
                let c = McpClient::connect(
                    server,
                    &r.command,
                    &r.args,
                    &env,
                    gw.call_timeout,
                    gw.connect_timeout,
                )
                .await
                .map_err(|e| e.to_string())?;
                if let Some(pid) = c.pid() {
                    self.pgids
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push(pid);
                }
                Ok(Upstream::Client(Arc::new(c)))
            }
            Transport::Http => {
                let url = gw.deployment_url.get(server).unwrap_or(&r.url).clone();
                let mut auth = HttpAuth {
                    ca_pem: gw.deployment_ca.get(server).cloned(),
                    ..HttpAuth::default()
                };
                match &r.auth {
                    ResolvedAuth::None => {}
                    // A value the DEPLOYMENT holds (the house token) is read now and held as a
                    // `Bearer`, whose Debug prints a marker; one in the environment is passed
                    // by name and read on every message.
                    ResolvedAuth::BearerEnv(var) => match gw.deployment_env.get(var) {
                        Some(v) => auth.bearer = Some(jesse_agent::mcp::Bearer::new(v.clone())),
                        None => auth.bearer_env = Some(var.clone()),
                    },
                    ResolvedAuth::HeadersEnv(pairs) => auth.header_env = pairs.clone(),
                    ResolvedAuth::ClientCert(name) => {
                        let c = gw.settings.client_certs.get(name).ok_or_else(|| {
                            format!("no [gateway.client_certs.{name}] for '{server}'")
                        })?;
                        auth.mtls = Some(MutualTls {
                            cert_file: c.cert_file.clone(),
                            key_file: c.key_file.clone(),
                            ca_file: c.ca_file.clone(),
                        });
                    }
                }
                let c = McpClient::connect_http_with(
                    server,
                    &url,
                    &auth,
                    gw.call_timeout,
                    gw.connect_timeout,
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(Upstream::Client(Arc::new(c)))
            }
        }
    }

    /// The process-group ids of the stdio upstreams started so far.
    pub fn upstream_pgids(&self) -> Vec<u32> {
        self.pgids.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Kill every upstream by PROCESS GROUP, stop the listeners, and drop the clients.
    /// Synchronous so a guard's `Drop` can call it.
    pub(super) fn shutdown(&self) {
        for t in self
            .tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
        {
            t.abort();
        }
        for pgid in self
            .pgids
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
        {
            kill_group(pgid);
        }
        self.upstreams
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        self.http_sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }
}

/// SIGKILL a process group. The upstream was started as its group's leader, so this takes the
/// server and everything it started (`npx`'s real server, a Python worker).
pub fn kill_group(pgid: u32) {
    if pgid <= 1 {
        return;
    }
    // SAFETY: `killpg` takes a pid and a signal number and touches no memory.
    unsafe {
        libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
    }
}

fn reply(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {"code": code, "message": message},
    })
}
