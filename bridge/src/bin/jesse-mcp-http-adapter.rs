//! `jesse-mcp-http-adapter` — run ONE third-party stdio MCP server and serve it over Streamable
//! HTTP, behind a bearer and an allowlist.
//!
//! ```text
//! jesse-mcp-http-adapter --listen 0.0.0.0:8080 --name slack --bearer-env ADAPTER_TOKEN \
//!     --allow-tool channels_list --allow-tool conversations_history \
//!     --pass-env PATH --pass-env HOME --pass-env SLACK_MCP_XOXP_TOKEN \
//!     [--max-result-bytes N] -- npx -y slack-mcp-server@1.3.0 --transport stdio
//! ```
//!
//! This is how the cluster runs a vendor's stdio server as its own pod with only its own
//! secret: the pod's environment holds that one credential, the adapter hands the server
//! exactly the names given by `--pass-env` (nothing else, not even `PATH` unless named), and
//! the bridge's gateway reaches the pod as an `http` row.
//!
//!   * **EVERY REQUEST CARRIES THE BEARER** read once at startup from the variable `--bearer-env`
//!     names; a request without it is a 401 and reaches nothing.
//!   * **ONLY `--allow-tool` TOOLS EXIST.** `tools/list` returns the allowed intersection;
//!     a `tools/call` for anything else is refused by exact name and never reaches the server.
//!   * **THE SAME FOUR MESSAGES** the gateway serves, and nothing else.
//!   * **RESULTS ARE CAPPED** (`--max-result-bytes`, default 8 MiB) and the server's stderr is
//!     drained to this process's own stderr, line by line, prefixed with the server's name.
//!
//! The child is the agent crate's MCP client (`jesse_agent::mcp::McpClient`), with its deadlines
//! and drop rules: a server that breaks protocol or times out is restarted on the next
//! `initialize`, never on a call.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use jesse_agent::mcp::{McpClient, StderrLog, DEFAULT_CALL_TIMEOUT, DEFAULT_CONNECT_TIMEOUT};
use serde_json::{json, Value};

const DEFAULT_MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Args {
    listen: String,
    name: String,
    bearer_env: String,
    allow: Vec<String>,
    pass_env: Vec<String>,
    max_result_bytes: usize,
    command: String,
    args: Vec<String>,
}

const USAGE: &str = "usage: jesse-mcp-http-adapter --listen <addr> --name <server> \
                     --bearer-env <VAR> [--allow-tool <tool>]... [--pass-env <NAME>]... \
                     [--max-result-bytes <n>] -- <command> [args...]";

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut listen = None;
    let mut name = None;
    let mut bearer_env = None;
    let mut allow = Vec::new();
    let mut pass_env = Vec::new();
    let mut max = DEFAULT_MAX_RESULT_BYTES;
    let mut it = argv.iter();
    let mut rest = Vec::new();
    while let Some(flag) = it.next() {
        if flag == "--" {
            rest = it.cloned().collect();
            break;
        }
        let v = it
            .next()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{flag} needs a value"))?
            .clone();
        match flag.as_str() {
            "--listen" => listen = Some(v),
            "--name" => name = Some(v),
            "--bearer-env" => bearer_env = Some(v),
            "--allow-tool" => allow.push(v),
            "--pass-env" => pass_env.push(v),
            "--max-result-bytes" => {
                max = v
                    .parse()
                    .map_err(|_| format!("--max-result-bytes {v:?} is not a number"))?
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let mut rest = rest.into_iter();
    let command = rest.next().ok_or("the server command goes after --")?;
    Ok(Args {
        listen: listen.ok_or("--listen is required")?,
        name: name.ok_or("--name is required")?,
        bearer_env: bearer_env.ok_or("--bearer-env is required")?,
        allow,
        pass_env,
        max_result_bytes: max,
        command,
        args: rest.collect(),
    })
}

struct Adapter {
    args: Args,
    bearer: String,
    client: tokio::sync::Mutex<Option<Arc<McpClient>>>,
    sessions: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl Adapter {
    /// The child's whole environment: the `--pass-env` names this process has, nothing else.
    fn child_env(&self) -> BTreeMap<String, String> {
        self.args
            .pass_env
            .iter()
            .filter_map(|n| std::env::var(n).ok().map(|v| (n.clone(), v)))
            .collect()
    }

    async fn client(&self, restart: bool) -> Result<Arc<McpClient>, String> {
        let mut slot = self.client.lock().await;
        if let Some(c) = slot.as_ref() {
            if !(restart && c.death_reason().is_some()) {
                return Ok(c.clone());
            }
        }
        let name = self.args.name.clone();
        let log: StderrLog = Arc::new(move |line: &str| eprintln!("[{name}] {line}"));
        let c = McpClient::connect_logging(
            &self.args.name,
            &self.args.command,
            &self.args.args,
            &self.child_env(),
            DEFAULT_CALL_TIMEOUT,
            DEFAULT_CONNECT_TIMEOUT,
            Some(log),
        )
        .await
        .map_err(|e| e.to_string())?;
        let c = Arc::new(c);
        *slot = Some(c.clone());
        Ok(c)
    }

    fn allowed(&self, tool: &str) -> bool {
        self.args.allow.iter().any(|a| a == tool)
    }

    async fn handle(&self, msg: &Value) -> (Option<Value>, bool) {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let Some(id) = id else {
            return (None, method != "notifications/initialized");
        };
        match method {
            "initialize" => {
                if let Err(e) = self.client(true).await {
                    eprintln!("[{}] the server did not start: {e}", self.args.name);
                    return (Some(err(id, -32603, "the server did not start")), false);
                }
                let version = msg["params"]["protocolVersion"]
                    .as_str()
                    .unwrap_or(jesse_agent::mcp::PROTOCOL_VERSION);
                (
                    Some(ok(
                        id,
                        json!({
                            "protocolVersion": version,
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": format!("jesse-mcp-http-adapter/{}", self.args.name),
                                           "version": env!("CARGO_PKG_VERSION")},
                        }),
                    )),
                    false,
                )
            }
            "tools/list" => match self.client(false).await {
                Err(_) => (Some(err(id, -32603, "the server did not start")), false),
                Ok(c) => match c.list_tools().await {
                    Err(e) => {
                        eprintln!("[{}] tools/list failed: {e}", self.args.name);
                        (
                            Some(err(id, -32603, "the server would not list its tools")),
                            false,
                        )
                    }
                    Ok(tools) => {
                        let tools: Vec<Value> = tools
                            .into_iter()
                            .filter(|t| self.allowed(&t.name))
                            .map(|t| t.raw)
                            .collect();
                        (Some(ok(id, json!({ "tools": tools }))), false)
                    }
                },
            },
            "tools/call" => {
                let tool = msg["params"]["name"].as_str().unwrap_or("");
                if !self.allowed(tool) {
                    return (
                        Some(err(
                            id,
                            -32602,
                            &format!("the tool '{tool}' is not allowed"),
                        )),
                        false,
                    );
                }
                let args = match msg["params"].get("arguments") {
                    None | Some(Value::Null) => json!({}),
                    Some(a) => a.clone(),
                };
                let c = match self.client(false).await {
                    Ok(c) => c,
                    Err(_) => return (Some(err(id, -32603, "the server did not start")), false),
                };
                match c.call_tool(tool, args).await {
                    Err(e) => {
                        eprintln!("[{}] {tool} failed: {e}", self.args.name);
                        (Some(err(id, -32603, "the server did not answer")), false)
                    }
                    Ok(o) => {
                        let bytes = serde_json::to_vec(&o.raw).map(|b| b.len()).unwrap_or(0);
                        if bytes > self.args.max_result_bytes {
                            let text = format!(
                                "The result was {bytes} bytes, more than the {} this adapter \
                                 passes, and was withheld.",
                                self.args.max_result_bytes
                            );
                            return (
                                Some(ok(
                                    id,
                                    json!({"content": [{"type": "text", "text": text}], "isError": true}),
                                )),
                                false,
                            );
                        }
                        (Some(ok(id, o.raw)), false)
                    }
                }
            }
            _ => (
                Some(err(
                    id,
                    -32601,
                    "the adapter serves initialize, tools/list and tools/call only",
                )),
                true,
            ),
        }
    }
}

fn ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn err(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

const SESSION_HEADER: &str = "mcp-session-id";

async fn post_mcp(State(a): State<Arc<Adapter>>, headers: HeaderMap, body: Bytes) -> Response {
    use subtle::ConstantTimeEq;
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !bool::from(presented.as_bytes().ct_eq(a.bearer.as_bytes())) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if body.len() > MAX_REQUEST_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let is_initialize = msg.get("method").and_then(|m| m.as_str()) == Some("initialize");
    let sid = headers
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if !is_initialize {
        let live = sid
            .as_ref()
            .is_some_and(|s| a.sessions.lock().map(|g| g.contains(s)).unwrap_or(false));
        if !live {
            return StatusCode::NOT_FOUND.into_response();
        }
    }
    let (reply, close) = a.handle(&msg).await;
    if close {
        if let Some(s) = &sid {
            if let Ok(mut g) = a.sessions.lock() {
                g.remove(s);
            }
        }
    }
    let Some(reply) = reply else {
        return if close {
            StatusCode::BAD_REQUEST.into_response()
        } else {
            StatusCode::ACCEPTED.into_response()
        };
    };
    let status = if close {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };
    let mut resp = (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        reply.to_string(),
    )
        .into_response();
    if is_initialize && reply.get("result").is_some() {
        let id = uuid::Uuid::new_v4().simple().to_string();
        if let Ok(mut g) = a.sessions.lock() {
            g.insert(id.clone());
        }
        if let Ok(h) = id.parse() {
            resp.headers_mut().insert(SESSION_HEADER, h);
        }
    }
    resp
}

async fn delete_mcp(State(a): State<Arc<Adapter>>, headers: HeaderMap) -> Response {
    use subtle::ConstantTimeEq;
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !bool::from(presented.as_bytes().ct_eq(a.bearer.as_bytes())) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if let Some(s) = headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok()) {
        if let Ok(mut g) = a.sessions.lock() {
            g.remove(s);
        }
    }
    StatusCode::OK.into_response()
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("jesse-mcp-http-adapter: {e}\n{USAGE}");
            return std::process::ExitCode::from(2);
        }
    };
    let bearer = match std::env::var(&args.bearer_env) {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!(
                "jesse-mcp-http-adapter: the bearer variable {} is unset",
                args.bearer_env
            );
            return std::process::ExitCode::from(2);
        }
    };
    let listener = match tokio::net::TcpListener::bind(&args.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("jesse-mcp-http-adapter: cannot bind {}: {e}", args.listen);
            return std::process::ExitCode::FAILURE;
        }
    };
    if let Ok(addr) = listener.local_addr() {
        // The one line a supervisor (or a test) reads to learn the bound port.
        println!("listening {addr}");
    }
    let adapter = Arc::new(Adapter {
        args,
        bearer,
        client: tokio::sync::Mutex::new(None),
        sessions: std::sync::Mutex::new(Default::default()),
    });
    let app = Router::new()
        .route("/mcp", post(post_mcp).delete(delete_mcp))
        .with_state(adapter);
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("jesse-mcp-http-adapter: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn the_arguments_parse_and_the_command_follows_the_separator() {
        let p = parse_args(&a(&[
            "--listen",
            "127.0.0.1:0",
            "--name",
            "probe",
            "--bearer-env",
            "T",
            "--allow-tool",
            "probe_echo",
            "--pass-env",
            "PATH",
            "--",
            "server",
            "--flag",
        ]))
        .unwrap();
        assert_eq!(p.allow, vec!["probe_echo"]);
        assert_eq!(p.pass_env, vec!["PATH"]);
        assert_eq!(p.command, "server");
        assert_eq!(p.args, vec!["--flag"]);
        assert_eq!(p.max_result_bytes, DEFAULT_MAX_RESULT_BYTES);
    }

    #[test]
    fn a_missing_requirement_is_refused() {
        for bad in [
            &["--name", "x", "--bearer-env", "T", "--", "s"][..],
            &["--listen", "a", "--bearer-env", "T", "--", "s"],
            &["--listen", "a", "--name", "x", "--", "s"],
            &["--listen", "a", "--name", "x", "--bearer-env", "T"],
            &[
                "--listen",
                "a",
                "--name",
                "x",
                "--bearer-env",
                "T",
                "--bogus",
                "1",
                "--",
                "s",
            ],
        ] {
            assert!(parse_args(&a(bad)).is_err(), "{bad:?}");
        }
    }
}
