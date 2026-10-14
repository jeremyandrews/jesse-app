//! **The tool gateway, end to end**, against real processes: the probe upstream
//! (`jesse-gateway-probe-server`), the stub a child starts (`jesse-gateway-mcp`), the HTTP
//! adapter (`jesse-mcp-http-adapter`), and a scratch bridge whose turn is driven by a fake
//! child that speaks MCP to the stubs its `--mcp-config` names.
//!
//! Every verdict about what reached an upstream is read off THAT UPSTREAM'S OWN LOG (or, for
//! the in-process one, its own record), never off what the gateway reports about itself.
//!
//! The probe upstream learns where to log from two variables in the BRIDGE's environment, so
//! the tests that set them hold [`ENV_LOCK`] for their whole run.

mod common;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use common::*;
use jesse_bridge::activity::ActivityLedger;
use jesse_bridge::gateway::{
    self, Admitted, Claims, Gateway, GatewaySettings, InProcessServer, McpOverrideToml, TurnSpec,
};
use jesse_bridge::*;
use serde_json::{json, Value};
use tower::ServiceExt;

static ENV_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
static N: AtomicUsize = AtomicUsize::new(0);

const PROBE: &str = env!("CARGO_BIN_EXE_jesse-gateway-probe-server");
const STUB: &str = env!("CARGO_BIN_EXE_jesse-gateway-mcp");
const ADAPTER: &str = env!("CARGO_BIN_EXE_jesse-mcp-http-adapter");

/// A SHORT scratch directory: a unix socket path is capped at 103 bytes, and the system temp
/// directory on macOS is already half of that.
fn short_tmp(tag: &str) -> PathBuf {
    let d = PathBuf::from(format!(
        "/tmp/jgw-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The bridge-side environment a probe run needs: where `gwprobe` logs and dumps, a nonce the
/// bridge holds that no upstream may see, and the probe server on `PATH`.
fn plant_bridge_env(dir: &Path) -> String {
    let nonce = format!(
        "NONCE-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    std::env::set_var("JESSE_GWPROBE_LOG", dir.join("stdio.log"));
    std::env::set_var("JESSE_GWPROBE_ENV_DUMP", dir.join("stdio.env"));
    std::env::set_var("JESSE_GWPROBE_NONCE", &nonce);
    let bin_dir = Path::new(PROBE).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    if !path.split(':').any(|p| Path::new(p) == bin_dir) {
        std::env::set_var("PATH", format!("{}:{path}", bin_dir.display()));
    }
    nonce
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// The in-process probe: records every call it is handed.
#[derive(Default)]
struct InProc {
    calls: Mutex<Vec<String>>,
}

impl InProcessServer for InProc {
    fn tools(&self) -> Vec<Value> {
        vec![
            json!({"name": "probe_echo", "description": "echo", "inputSchema": {"type": "object"}}),
            json!({"name": "probe_hidden", "description": "hidden", "inputSchema": {"type": "object"}}),
        ]
    }

    fn call<'a>(
        &'a self,
        tool: &'a str,
        _arguments: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send + 'a>>
    {
        Box::pin(async move {
            self.calls.lock().unwrap().push(tool.to_string());
            Ok(json!({"content": [{"type": "text", "text": format!("inproc:{tool}")}]}))
        })
    }
}

/// A running adapter wrapping the probe server; killed on drop.
struct Adapter {
    child: std::process::Child,
    url: String,
}

impl Drop for Adapter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_adapter(dir: &Path, token: &str, allow: &[&str]) -> Adapter {
    let mut cmd = Command::new(ADAPTER);
    cmd.args([
        "--listen",
        "127.0.0.1:0",
        "--name",
        "gwprobe-http",
        "--bearer-env",
        "ADAPTER_BEARER",
    ]);
    for a in allow {
        cmd.args(["--allow-tool", a]);
    }
    cmd.args([
        "--pass-env",
        "PATH",
        "--pass-env",
        "JESSE_GWPROBE_LOG",
        "--pass-env",
        "JESSE_GWPROBE_ENV_DUMP",
        "--",
        PROBE,
    ])
    // The adapter's OWN environment has the nonce (it inherits this test's); its child must
    // not.
    .env("ADAPTER_BEARER", token)
    .env("JESSE_GWPROBE_LOG", dir.join("http.log"))
    .env("JESSE_GWPROBE_ENV_DUMP", dir.join("http.env"))
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("the adapter starts");
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    let addr = first
        .trim()
        .strip_prefix("listening ")
        .expect("the bound address");
    Adapter {
        child,
        url: format!("http://{addr}/mcp"),
    }
}

fn row(name: &str) -> gateway::ResolvedServer {
    gateway::resolve_row(name, &GatewaySettings::default()).unwrap()
}

fn admitted(settings: &GatewaySettings, name: &str, tools: &[&str]) -> Admitted {
    Admitted {
        server: gateway::resolve_row(name, settings).unwrap_or_else(|_| row(name)),
        tools: tools.iter().map(|t| t.to_string()).collect(),
    }
}

/// A stub process, driven one message at a time.
struct Stub {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Stub {
    fn start(args: &[&str]) -> Stub {
        let mut child = Command::new(STUB)
            .args(args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the stub starts");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Stub {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, msg: Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn ask(&mut self, msg: Value) -> Option<Value> {
        self.send(msg);
        let mut line = String::new();
        match self.stdout.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => serde_json::from_str(&line).ok(),
        }
    }

    fn handshake(&mut self) {
        let r = self
            .ask(json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                "params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}))
            .expect("initialize answers");
        assert!(r["result"]["capabilities"]["tools"].is_object(), "{r}");
        self.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    }

    fn list(&mut self) -> Vec<String> {
        let r = self
            .ask(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
            .expect("tools/list answers");
        r["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("{r}"))
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn call(&mut self, id: u64, tool: &str) -> Value {
        self.ask(json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":tool,"arguments":{"text":"hi"}}}))
            .expect("tools/call answers")
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 probes for existence and touches no memory.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// THREE UPSTREAMS, ONE TURN, ONE STUB EACH: a bridge-started stdio probe, the same probe behind
/// the HTTP adapter, and an in-process probe. Granted calls reach each; the ungranted one is
/// absent from every list and reaches none; no upstream sees the bridge's nonce; the end of the
/// turn kills the stdio upstream's whole process group and removes the turn directory.
#[tokio::test]
async fn three_upstreams_from_one_turn_reach_only_granted_tools() {
    let _env = ENV_LOCK.lock().await;
    let dir = short_tmp("three");
    let nonce = plant_bridge_env(&dir);
    std::env::set_var("JESSE_GWPROBE_HTTP_TOKEN", "adapter-bearer-1");
    // The adapter allows BOTH tools: the gateway's grant is what must hold, not the adapter's.
    let adapter = start_adapter(&dir, "adapter-bearer-1", &["probe_echo", "probe_hidden"]);

    let mut settings = GatewaySettings {
        enabled: true,
        ..GatewaySettings::default()
    };
    settings.overrides.insert(
        "gwprobe-http".into(),
        McpOverrideToml {
            url: Some(adapter.url.clone()),
            ..McpOverrideToml::default()
        },
    );
    let inproc = Arc::new(InProc::default());
    let mut registry: HashMap<String, Arc<dyn InProcessServer>> = HashMap::new();
    registry.insert("gwprobe-inproc".into(), inproc.clone());
    let state = dir.join("s");
    let ledger = Arc::new(ActivityLedger::new(&state));
    let gw = Arc::new(Gateway::new(
        &state,
        settings.clone(),
        registry,
        Some(ledger.clone()),
    ));

    let job = "job-three";
    let guard = gw
        .open_turn(TurnSpec {
            job_id: job.into(),
            capability: Capability::Write,
            servers: vec![
                admitted(&settings, "gwprobe", &["probe_echo"]),
                admitted(&settings, "gwprobe-http", &["probe_echo"]),
                admitted(&settings, "gwprobe-inproc", &["probe_echo"]),
            ],
            harness_env_value: None,
            deadline: Duration::from_secs(60),
            bind_sockets: true,
            env_overlay: Default::default(),
        })
        .await
        .expect("the turn opens");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(guard.dir()).unwrap().permissions().mode() & 0o777,
        0o700
    );

    for server in ["gwprobe", "gwprobe-http", "gwprobe-inproc"] {
        let sock = gateway::socket_path(guard.dir(), server);
        let sock = sock.to_str().unwrap().to_string();
        let mut stub = tokio::task::spawn_blocking(move || {
            let mut s = Stub::start(&["--upstream", server, "--socket", &sock]);
            s.handshake();
            s
        })
        .await
        .unwrap();
        let (names, echo, hidden, stub) = tokio::task::spawn_blocking(move || {
            let names = stub.list();
            let echo = stub.call(3, "probe_echo");
            let hidden = stub.call(4, "probe_hidden");
            (names, echo, hidden, stub)
        })
        .await
        .unwrap();
        assert_eq!(
            names,
            vec!["probe_echo"],
            "{server}: the granted intersection only"
        );
        assert!(
            echo["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .ends_with("probe_echo")
                || echo["result"]["content"][0]["text"] == "echo:hi",
            "{server}: {echo}"
        );
        assert!(hidden.get("error").is_some(), "{server}: refused: {hidden}");
        assert!(hidden.get("result").is_none());
        drop(stub);
    }

    // THE UPSTREAMS' OWN ACCOUNTS.
    assert_eq!(read(&dir.join("stdio.log")), "probe_echo\n");
    // The child's own client name reached the upstream, not the gateway's.
    assert_eq!(read(&dir.join("stdio.env.client")), "t");
    assert_eq!(read(&dir.join("http.log")), "probe_echo\n");
    assert_eq!(
        *inproc.calls.lock().unwrap(),
        vec!["probe_echo".to_string()]
    );
    // NO UPSTREAM SAW THE BRIDGE'S NONCE, and the stdio one got only its row's names.
    let stdio_env = read(&dir.join("stdio.env"));
    let http_env = read(&dir.join("http.env"));
    assert!(!stdio_env.is_empty() && !http_env.is_empty(), "both dumped");
    assert!(!stdio_env.contains(&nonce) && !http_env.contains(&nonce));
    let names: Vec<&str> = stdio_env
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k))
        .collect();
    for n in &names {
        assert!(
            ["PATH", "HOME", "TMPDIR", "LANG", "TZ", "JESSE_GWPROBE_LOG", "JESSE_GWPROBE_ENV_DUMP"]
                .contains(n)
                // The platform adds these to every process it starts.
                || n.starts_with("__CF") || *n == "PWD" || *n == "SHLVL" || *n == "_",
            "the stdio upstream got {n}, which its row does not name"
        );
    }

    // THE TURN ENDS: process group killed, directory gone, token revoked.
    let pgids = guard.session().upstream_pgids();
    assert_eq!(pgids.len(), 1, "one bridge-started upstream");
    let turn_dir = guard.dir().to_path_buf();
    drop(guard);
    for _ in 0..50 {
        if !alive(pgids[0]) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(pgids[0]), "the upstream's process group was killed");
    assert!(!turn_dir.exists(), "the turn directory was removed");

    // THE LEDGER: three ok calls and three refusals, content-free.
    let day = chrono::Local::now().format("%Y-%m-%d").to_string();
    let text = read(&ledger.dir().join(format!("{day}.jsonl")));
    let lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 6, "{text}");
    assert_eq!(lines.iter().filter(|l| l["outcome"] == "ok").count(), 3);
    assert_eq!(
        lines.iter().filter(|l| l["outcome"] == "refused").count(),
        3
    );
    assert!(!text.contains("hi\""), "no argument reaches the ledger");
    drop(adapter);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A message outside the four-message subset is a protocol error that CLOSES the session at
/// the gateway; the STUB answers such a request itself and never forwards it, so a client's
/// feature probe (Claude Code's `server/discover`) costs nothing.
#[tokio::test]
async fn outside_the_subset_the_gateway_closes_and_the_stub_answers_locally() {
    let _env = ENV_LOCK.lock().await;
    let dir = short_tmp("subset");
    plant_bridge_env(&dir);
    let settings = GatewaySettings {
        enabled: true,
        ..GatewaySettings::default()
    };
    let gw = Arc::new(Gateway::new(
        &dir.join("s"),
        settings.clone(),
        HashMap::new(),
        None,
    ));
    let guard = gw
        .open_turn(TurnSpec {
            job_id: "job-subset".into(),
            capability: Capability::Write,
            servers: vec![admitted(&settings, "gwprobe", &["probe_echo"])],
            harness_env_value: None,
            deadline: Duration::from_secs(60),
            bind_sockets: true,
            env_overlay: Default::default(),
        })
        .await
        .unwrap();
    let sock = gateway::socket_path(guard.dir(), "gwprobe");
    // THE GATEWAY, spoken to directly (as a child with a shell could): closed.
    let sock2 = sock.clone();
    let closed = tokio::task::spawn_blocking(move || {
        use std::os::unix::net::UnixStream;
        let s = UnixStream::connect(&sock2).unwrap();
        let mut w = s.try_clone().unwrap();
        writeln!(
            w,
            r#"{{"jsonrpc":"2.0","id":9,"method":"resources/list","params":{{}}}}"#
        )
        .unwrap();
        let mut r = BufReader::new(s);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["error"]["code"], -32601, "{v}");
        let mut rest = String::new();
        r.read_line(&mut rest).unwrap() == 0
    })
    .await
    .unwrap();
    assert!(
        closed,
        "the gateway closes the session after a message outside the subset"
    );
    // THE STUB: answers the probe itself and goes on to initialise.
    let sock = sock.to_str().unwrap().to_string();
    let names = tokio::task::spawn_blocking(move || {
        let mut s = Stub::start(&["--upstream", "gwprobe", "--socket", &sock]);
        let r = s
            .ask(json!({"jsonrpc":"2.0","id":"server-discover-probe-1","method":"server/discover"}))
            .expect("answered");
        assert_eq!(r["error"]["code"], -32601, "{r}");
        s.handshake();
        s.list()
    })
    .await
    .unwrap();
    assert_eq!(names, vec!["probe_echo"]);
    drop(guard);
    let _ = std::fs::remove_dir_all(&dir);
}

async fn post(
    url: &str,
    token: Option<&str>,
    session: Option<&str>,
    body: Value,
) -> (u16, Option<String>, String) {
    let client = reqwest::Client::new();
    let mut req = client
        .post(url)
        .header("content-type", "application/json")
        .body(body.to_string());
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    if let Some(s) = session {
        req = req.header("mcp-session-id", s);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    let sid = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    (status, sid, resp.text().await.unwrap_or_default())
}

fn init() -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}})
}

/// THE TCP LISTENER validates the token on every request: a token signed by another key, an
/// expired one, one for another server and one whose turn has ended are all refused; the
/// turn's own token works, and the stub in URL mode reaches an upstream through it.
#[tokio::test]
async fn the_tcp_listener_refuses_forged_expired_misdirected_and_revoked_tokens() {
    let _env = ENV_LOCK.lock().await;
    let dir = short_tmp("tcp");
    plant_bridge_env(&dir);
    let settings = GatewaySettings {
        enabled: true,
        listen: Some("127.0.0.1:0".into()),
        child_url: Some("http://placeholder".into()),
        ..GatewaySettings::default()
    };
    let gw = Arc::new(Gateway::new(
        &dir.join("s"),
        settings.clone(),
        HashMap::new(),
        None,
    ));
    let (addr, _task) = gateway::listen::serve_tcp(gw.clone(), "127.0.0.1:0")
        .await
        .unwrap();
    let base = format!("http://{addr}");
    let guard = gw
        .open_turn(TurnSpec {
            job_id: "job-tcp".into(),
            capability: Capability::Write,
            servers: vec![admitted(&settings, "gwprobe", &["probe_echo"])],
            harness_env_value: None,
            deadline: Duration::from_secs(60),
            bind_sockets: false,
            env_overlay: Default::default(),
        })
        .await
        .unwrap();
    let url = format!("{base}/mcp/gwprobe");
    let good = guard.token().to_string();

    // The turn's own token.
    let (status, sid, _) = post(&url, Some(&good), None, init()).await;
    assert_eq!(status, 200);
    let sid = sid.expect("a session id");
    let (status, _, body) = post(
        &url,
        Some(&good),
        Some(&sid),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"probe_echo","arguments":{"text":"x"}}}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("echo:x"), "{body}");

    // FORGED: the same claims under ANOTHER gateway's key.
    let other = Gateway::new(&dir.join("other"), settings.clone(), HashMap::new(), None);
    let claims: Claims = guard.session().claims().clone();
    let forged = other.sign_claims_for_tests(&claims);
    assert_eq!(post(&url, Some(&forged), None, init()).await.0, 401);
    // EXPIRED: this gateway's key, a past expiry.
    let expired = gw.sign_claims_for_tests(&Claims {
        exp: 1,
        ..claims.clone()
    });
    assert_eq!(post(&url, Some(&expired), None, init()).await.0, 401);
    // A server the token does not grant.
    assert_eq!(
        post(&format!("{base}/mcp/slack"), Some(&good), None, init())
            .await
            .0,
        401
    );
    // No token, and garbage.
    assert_eq!(post(&url, None, None, init()).await.0, 401);
    assert_eq!(post(&url, Some("v1.00.00"), None, init()).await.0, 401);
    // A request with no session id after initialize is not served.
    assert_eq!(
        post(
            &url,
            Some(&good),
            None,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list"})
        )
        .await
        .0,
        404
    );

    // THE STUB IN URL MODE, with the token from the turn's file.
    let token_file = guard.dir().join("token");
    assert_eq!(read(&token_file), good);
    let (base2, tf) = (base.clone(), token_file.to_str().unwrap().to_string());
    let (names, echo) = tokio::task::spawn_blocking(move || {
        let mut s = Stub::start(&[
            "--upstream",
            "gwprobe",
            "--url",
            &base2,
            "--token-file",
            &tf,
        ]);
        s.handshake();
        let names = s.list();
        let echo = s.call(5, "probe_echo");
        (names, echo)
    })
    .await
    .unwrap();
    assert_eq!(names, vec!["probe_echo"]);
    assert_eq!(echo["result"]["content"][0]["text"], "echo:hi");

    // REVOKED: the turn ends and its own token stops working.
    drop(guard);
    assert_eq!(post(&url, Some(&good), None, init()).await.0, 401);
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE ADAPTER: a bearer on every request, only `--allow-tool` tools, and only `--pass-env`
/// names in its child's environment.
#[tokio::test]
async fn the_adapter_holds_its_bearer_allowlist_and_environment() {
    let _env = ENV_LOCK.lock().await;
    let dir = short_tmp("adapter");
    let nonce = plant_bridge_env(&dir);
    let adapter = start_adapter(&dir, "the-bearer", &["probe_echo"]);
    let url = adapter.url.clone();
    assert_eq!(post(&url, None, None, init()).await.0, 401);
    assert_eq!(post(&url, Some("wrong"), None, init()).await.0, 401);
    let (status, sid, _) = post(&url, Some("the-bearer"), None, init()).await;
    assert_eq!(status, 200);
    let sid = sid.unwrap();
    let (_, _, list) = post(
        &url,
        Some("the-bearer"),
        Some(&sid),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;
    let list: Value = serde_json::from_str(&list).unwrap();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["probe_echo"]);
    let (_, _, hidden) = post(
        &url,
        Some("the-bearer"),
        Some(&sid),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"probe_hidden"}}),
    )
    .await;
    assert!(hidden.contains("\"error\""), "{hidden}");
    let (_, _, echo) = post(
        &url,
        Some("the-bearer"),
        Some(&sid),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"probe_echo","arguments":{"text":"y"}}}),
    )
    .await;
    assert!(echo.contains("echo:y"), "{echo}");
    assert_eq!(
        read(&dir.join("http.log")),
        "probe_echo\n",
        "hidden never reached it"
    );
    let env = read(&dir.join("http.env"));
    assert!(!env.contains(&nonce));
    assert!(
        !env.contains("ADAPTER_BEARER"),
        "the adapter's own bearer stays with it"
    );
    for line in env.lines() {
        let name = line.split('=').next().unwrap();
        assert!(
            ["PATH", "JESSE_GWPROBE_LOG", "JESSE_GWPROBE_ENV_DUMP"].contains(&name)
                || name.starts_with("__CF")
                || name == "PWD"
                || name == "SHLVL"
                || name == "_",
            "{name} was not passed"
        );
    }
    drop(adapter);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_startup_sweep_removes_leftover_turn_directories() {
    let dir = short_tmp("sweep");
    for j in ["a", "b"] {
        std::fs::create_dir_all(dir.join("gw").join(j)).unwrap();
        std::fs::write(dir.join("gw").join(j).join("x.sock"), "").unwrap();
    }
    assert_eq!(Gateway::sweep(&dir.join("gw")), 2);
    assert_eq!(std::fs::read_dir(dir.join("gw")).unwrap().count(), 0);
    assert_eq!(Gateway::sweep(&dir.join("absent")), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE FAKE CHILD for the scratch bridge: reads its `--mcp-config`, speaks MCP to every server
/// in it, calls `probe_echo` and `probe_hidden` on each, dumps its own environment, and answers
/// the turn with one stream-json result line.
const FAKE_CHILD: &str = r#"#!/usr/bin/env python3
import json, os, subprocess, sys
argv = sys.argv
cfg = json.loads(argv[argv.index('--mcp-config') + 1])
report = {'env': sorted(os.environ.keys()), 'nonce_seen': False, 'servers': {}}
nonce = '__NONCE__'
report['nonce_seen'] = any(nonce in v for v in os.environ.values())
for name, spec in cfg['mcpServers'].items():
    p = subprocess.Popen([spec['command']] + spec.get('args', []), stdin=subprocess.PIPE,
                         stdout=subprocess.PIPE, env={})
    def ask(m):
        p.stdin.write((json.dumps(m) + '\n').encode()); p.stdin.flush()
        return json.loads(p.stdout.readline())
    ask({'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {'protocolVersion': '2025-06-18', 'capabilities': {}}})
    p.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n'); p.stdin.flush()
    tools = [t['name'] for t in ask({'jsonrpc': '2.0', 'id': 2, 'method': 'tools/list'})['result']['tools']]
    echo = ask({'jsonrpc': '2.0', 'id': 3, 'method': 'tools/call', 'params': {'name': 'probe_echo', 'arguments': {'text': name}}})
    hidden = ask({'jsonrpc': '2.0', 'id': 4, 'method': 'tools/call', 'params': {'name': 'probe_hidden', 'arguments': {}}})
    report['servers'][name] = {'command': spec['command'], 'tools': tools,
                               'echo_ok': 'result' in echo, 'hidden_refused': 'error' in hidden}
    p.stdin.close(); p.wait()
open('__REPORT__', 'w').write(json.dumps(report))
print(json.dumps({'type': 'result', 'is_error': False, 'result': 'gateway turn done', 'session_id': 'sess-gw'}))
"#;

/// A SCRATCH BRIDGE WITH THE GATEWAY ON: a real `POST /jesse` turn whose (fake) Claude Code
/// child is handed one stub per gateway server, drives all three probe upstreams through them,
/// and holds no planted secret itself.
#[tokio::test]
async fn a_scratch_bridge_turn_drives_three_upstreams_through_the_gateway() {
    let _env = ENV_LOCK.lock().await;
    let dir = short_tmp("bridge");
    let nonce = plant_bridge_env(&dir);
    std::env::set_var("JESSE_GWPROBE_HTTP_TOKEN", "adapter-bearer-2");
    let adapter = start_adapter(&dir, "adapter-bearer-2", &["probe_echo", "probe_hidden"]);
    let report = dir.join("report.json");
    let fake = write_fake_claude(
        &FAKE_CHILD
            .replace("__NONCE__", &nonce)
            .replace("__REPORT__", report.to_str().unwrap()),
    );

    let mut gw_settings = GatewaySettings {
        enabled: true,
        stub: Some(PathBuf::from(STUB)),
        ..GatewaySettings::default()
    };
    gw_settings.overrides.insert(
        "gwprobe-http".into(),
        McpOverrideToml {
            url: Some(adapter.url.clone()),
            ..McpOverrideToml::default()
        },
    );
    let inproc = Arc::new(InProc::default());
    gw_settings
        .in_process
        .0
        .insert("gwprobe-inproc".into(), inproc.clone());
    let state = dir.join("s");
    let cfg = Config {
        claude_bin: fake.to_string_lossy().into_owned(),
        state_dir: Some(state.to_string_lossy().into_owned()),
        main_mcp_config: Some(
            r#"{"mcpServers":{"gwprobe":{"type":"stdio","command":"jesse-gateway-probe-server","args":[]},"gwprobe-http":{"type":"http","url":"http://127.0.0.1:9/mcp"},"gwprobe-inproc":{"type":"http","url":""}}}"#
                .to_string(),
        ),
        allowed_tools: format!(
            "{DEFAULT_ALLOWED_TOOLS},mcp__gwprobe__probe_echo,mcp__gwprobe-http__probe_echo,mcp__gwprobe-inproc__probe_echo"
        ),
        gateway: gw_settings,
        ..test_config()
    };
    let st = AppState::new(cfg);
    let resp = app(st.clone())
        .oneshot(jesse_request(
            Some("Bearer test-token"),
            r#"{"mode":"ask","text":"use the probes"}"#,
        ))
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let job_id = match body["job_id"].as_str() {
        Some(j) => j.to_string(),
        None => {
            assert_eq!(body["result"], "gateway turn done", "{body}");
            String::new()
        }
    };
    if !job_id.is_empty() {
        let mut done = false;
        for _ in 0..600 {
            let v = result_status(&st, &job_id).await;
            if v["status"] != "running" {
                assert_eq!(v["status"], "done", "{v}");
                done = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(done, "the gateway turn finished");
    }

    let r: Value = serde_json::from_str(&read(&report)).expect("the fake child reported");
    // THE CHILD holds no planted secret and no vendor credential name.
    assert_eq!(
        r["nonce_seen"], false,
        "the child's environment held the bridge's nonce"
    );
    for name in r["env"].as_array().unwrap() {
        let name = name.as_str().unwrap();
        assert!(
            !name.starts_with("JESSE_GWPROBE") && !name.contains("TOKEN") && !name.contains("KEY"),
            "the child inherited {name}"
        );
    }
    for server in ["gwprobe", "gwprobe-http", "gwprobe-inproc"] {
        let s = &r["servers"][server];
        assert!(
            s["command"]
                .as_str()
                .unwrap()
                .ends_with("jesse-gateway-mcp"),
            "{server} is the stub: {s}"
        );
        assert_eq!(s["tools"], json!(["probe_echo"]), "{server}");
        assert_eq!(s["echo_ok"], true, "{server}");
        assert_eq!(s["hidden_refused"], true, "{server}");
    }
    // The upstreams' own accounts: each was called for the granted tool, and only that.
    assert_eq!(read(&dir.join("stdio.log")), "probe_echo\n");
    assert_eq!(read(&dir.join("http.log")), "probe_echo\n");
    assert_eq!(
        *inproc.calls.lock().unwrap(),
        vec!["probe_echo".to_string()]
    );
    assert!(!read(&dir.join("stdio.env")).contains(&nonce));
    assert!(!read(&dir.join("http.env")).contains(&nonce));
    // The turn's directory is gone once the turn is.
    assert_eq!(
        std::fs::read_dir(state.join("gw"))
            .map(|d| d.count())
            .unwrap_or(0),
        0
    );
    drop(adapter);
    let _ = std::fs::remove_file(&fake);
    let _ = std::fs::remove_dir_all(&dir);
}

/// LIVE, BY HAND (`CLAUDE=<claude bin> cargo test --features containment-probe --test gateway
/// -- --ignored`): a real `claude -p` registers every stub server through a real gateway turn,
/// with exactly the granted tools. This is the check that caught Claude Code 2.1.296 opening
/// every server with `server/discover`. It needs the CLI's login and the public tag1 server.
#[tokio::test]
#[ignore]
async fn live_claude_code_registers_every_stub_server() {
    let _env = ENV_LOCK.lock().await;
    let dir = short_tmp("realclaude");
    plant_bridge_env(&dir);
    let settings = GatewaySettings {
        enabled: true,
        stub: Some(PathBuf::from(STUB)),
        ..GatewaySettings::default()
    };
    let gw = Arc::new(Gateway::new(
        &dir.join("s"),
        settings.clone(),
        HashMap::new(),
        None,
    ));
    let guard = gw
        .open_turn(TurnSpec {
            job_id: "job-real".into(),
            capability: Capability::Write,
            servers: vec![
                admitted(&settings, "gwprobe", &["probe_echo"]),
                admitted(&settings, "tag1", &["search_tag1"]),
            ],
            harness_env_value: None,
            deadline: Duration::from_secs(120),
            bind_sockets: true,
            env_overlay: Default::default(),
        })
        .await
        .unwrap();
    let cfg = gateway::child_mcp_config(
        &settings,
        r#"{"mcpServers":{"gwprobe":{"type":"stdio","command":"x","args":[]},"tag1":{"type":"http","url":"https://www.tag1.com/mcp"}}}"#,
        &dir.join("s"),
        "job-real",
        Path::new(STUB),
    )
    .unwrap();
    let out = tokio::process::Command::new(std::env::var("CLAUDE").unwrap_or("claude".into()))
        .args([
            "-p",
            "Reply with the single word ok.",
            "--output-format",
            "stream-json",
            "--verbose",
            "--strict-mcp-config",
            "--mcp-config",
            &cfg,
            "--tools",
            "",
        ])
        .current_dir(&dir)
        .output()
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let init: Value = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["type"] == "system")
        .expect("the CLI's start-of-turn event");
    eprintln!("mcp_servers: {}", init["mcp_servers"]);
    for s in init["mcp_servers"].as_array().unwrap() {
        assert_eq!(s["status"], "connected", "{s}");
    }
    let mut tools: Vec<&str> = init["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str())
        .filter(|s| s.starts_with("mcp__"))
        .collect();
    tools.sort();
    assert_eq!(
        tools,
        vec!["mcp__gwprobe__probe_echo", "mcp__tag1__search_tag1"]
    );
    drop(guard);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An in-process upstream advertising one tool the agent refuses (a scope-shaped argument).
struct ScopeShaped;

impl InProcessServer for ScopeShaped {
    fn tools(&self) -> Vec<Value> {
        vec![
            json!({"name": "probe_echo", "description": "echo", "inputSchema": {"type": "object"}}),
            json!({"name": "probe_hidden", "description": "asks for a user",
                   "inputSchema": {"type": "object", "properties": {"user_id": {"type": "string"}}}}),
        ]
    }

    fn call<'a>(
        &'a self,
        tool: &'a str,
        _arguments: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send + 'a>>
    {
        Box::pin(async move { Ok(json!({"content": [{"type": "text", "text": tool}]})) })
    }
}

/// `direct`'s gateway set withholds a tool the agent's builder refuses and keeps the rest: one
/// upstream with a `user_id` argument once cost every direct turn its whole gateway set.
#[tokio::test]
async fn direct_set_withholds_a_refused_tool_and_keeps_the_others() {
    let dir = short_tmp("withhold");
    let settings = GatewaySettings {
        enabled: true,
        ..GatewaySettings::default()
    };
    let mut registry: HashMap<String, Arc<dyn InProcessServer>> = HashMap::new();
    registry.insert("gwprobe-inproc".into(), Arc::new(ScopeShaped));
    let gw = Arc::new(Gateway::new(
        &dir.join("s"),
        settings.clone(),
        registry,
        None,
    ));
    let guard = gw
        .open_turn(TurnSpec {
            job_id: "job-withhold".into(),
            capability: Capability::Write,
            servers: vec![admitted(
                &settings,
                "gwprobe-inproc",
                &["probe_echo", "probe_hidden"],
            )],
            harness_env_value: None,
            deadline: Duration::from_secs(30),
            bind_sockets: false,
            env_overlay: Default::default(),
        })
        .await
        .expect("the turn opens");
    let set = gateway::direct::tool_set_for_turn(guard, jesse_agent::tools::Level::Write)
        .await
        .expect("the set builds despite the refused tool");
    use jesse_agent::tools::ToolSet;
    let names: Vec<String> = set.manifest().into_iter().map(|t| t.name).collect();
    assert_eq!(names, vec!["mcp__gwprobe-inproc__probe_echo".to_string()]);
    assert!(
        set.report()
            .iter()
            .any(|l| l.contains("withheld probe_hidden") && l.contains("user_id")),
        "{:?}",
        set.report()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
