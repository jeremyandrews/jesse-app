//! **ONE TURN'S DESCRIPTOR COST**, measured: how many descriptors a gateway-on turn holds in the
//! bridge at its peak, and that every one of them is released when the turn's guard drops.
//!
//! The turn is the shipped main set with the gateway and `devjob` on, as on the Studio. Every
//! admitted server's socket is bound, connected (as the stub a child starts would connect) and
//! listed, so the gateway starts every upstream, which is what the Claude Code CLI's
//! `tools/list` at connect does on every turn. Each stdio and HTTP upstream is the probe
//! server instead of the real one, so the measurement starts real processes without reaching
//! anything; an HTTP upstream costs fewer descriptors than a stdio one, so the count is an
//! upper bound.
//!
//! Alone in its own test binary because it counts the whole process's descriptors: no other
//! test may open or close one while it runs.
//!
//!   cargo test --features containment-probe --test gateway_fds -- --nocapture

mod common;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use jesse_bridge::gateway::{self, Gateway, GatewaySettings, InProcessServer, TurnSpec};
use jesse_bridge::mcpservers::Transport;
use jesse_bridge::nofile::{MIN_CONCURRENT_TURNS, NOFILE_CAP, TURN_PEAK_FDS};
use jesse_bridge::*;
use serde_json::{json, Value};

const PROBE: &str = env!("CARGO_BIN_EXE_jesse-gateway-probe-server");

/// The descriptors this process holds. Listing the directory opens one more, the same one
/// every time, so counts compare exactly.
fn open_fds() -> usize {
    let dir = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    std::fs::read_dir(dir).expect("list descriptors").count()
}

/// `devjob`'s stand-in: registered, so the main set renders and admits it as on the Studio.
struct Devjob;

impl InProcessServer for Devjob {
    fn tools(&self) -> Vec<Value> {
        vec![
            json!({"name": "probe_echo", "description": "echo", "inputSchema": {"type": "object"}}),
        ]
    }

    fn call<'a>(
        &'a self,
        _tool: &'a str,
        _arguments: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send + 'a>>
    {
        Box::pin(async { Ok(json!({ "content": [] })) })
    }
}

/// Connect to `sock`, initialise and list, and keep the connection open: what the stub does.
fn connect_and_list(sock: PathBuf) -> UnixStream {
    let s = UnixStream::connect(&sock).unwrap_or_else(|e| panic!("{}: {e}", sock.display()));
    let mut w = s.try_clone().unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut ask = |msg: Value| -> Value {
        writeln!(w, "{msg}").unwrap();
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    ask(json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}));
    writeln!(
        s.try_clone().unwrap(),
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    let listed = ask(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}));
    assert_eq!(
        listed["result"]["tools"][0]["name"],
        "probe_echo",
        "{}: {listed}",
        sock.display()
    );
    s
}

/// One turn, start to end. Returns (descriptors at the peak, servers admitted); the peak
/// excludes the test's own client end of each connection, which in a deployment belongs to
/// the stub's process, not the bridge's.
async fn one_turn(gw: &Arc<Gateway>, cfg: &Config, job: &str) -> (usize, usize) {
    let rendered = render_child_mcp(cfg, MAIN_CHILD_MCP_CONFIG.as_str());
    let mut servers = gateway::admitted_servers(
        &cfg.gateway,
        &rendered,
        Capability::Write,
        DEFAULT_ALLOWED_TOOLS,
    )
    .unwrap();
    for a in &mut servers {
        if a.server.transport != Transport::InProcess {
            a.server.transport = Transport::BridgeStdio;
            a.server.command = PROBE.to_string();
            a.server.args = Vec::new();
        }
        a.tools = vec!["probe_echo".to_string()];
    }
    let n = servers.len();
    let guard = gw
        .open_turn(TurnSpec {
            job_id: job.to_string(),
            capability: Capability::Write,
            servers,
            harness_env_value: Some("claude-code".into()),
            deadline: Duration::from_secs(120),
            bind_sockets: true,
            env_overlay: Default::default(),
        })
        .await
        .expect("the turn opens");
    let socks: Vec<PathBuf> = guard
        .session()
        .server_names()
        .iter()
        .map(|s| gateway::socket_path(guard.dir(), s))
        .collect();
    let clients = tokio::task::spawn_blocking(move || {
        socks.into_iter().map(connect_and_list).collect::<Vec<_>>()
    })
    .await
    .unwrap();
    assert_eq!(
        guard.session().upstream_pgids().len(),
        n - 1,
        "every upstream but the in-process devjob was started"
    );
    let peak = open_fds() - clients.len();
    drop(clients);
    drop(guard);
    (peak, n)
}

/// Wait for the descriptors a closed turn held to be released (its connection tasks and the
/// killed upstreams' pipe readers end asynchronously), up to a bound.
async fn settle_to(target: usize) -> usize {
    let start = Instant::now();
    loop {
        let now = open_fds();
        if now <= target || start.elapsed() > Duration::from_secs(10) {
            return now;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The count once it has stopped changing: unchanged for half a second, up to a bound. The
/// baseline a turn is measured from, so a previous turn still being released cannot inflate it.
async fn settle_stable() -> usize {
    let start = Instant::now();
    let mut last = open_fds();
    let mut since = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let now = open_fds();
        if now != last {
            last = now;
            since = Instant::now();
        } else if since.elapsed() >= Duration::from_millis(500)
            || start.elapsed() > Duration::from_secs(10)
        {
            return now;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_turn_releases_every_descriptor_and_fits_the_raised_limit_many_times() {
    let dir = PathBuf::from(format!("/tmp/jgw-fds-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = test_config();
    cfg.state_dir = Some(dir.join("s").display().to_string());
    cfg.gateway = GatewaySettings {
        enabled: true,
        ..GatewaySettings::default()
    };
    cfg.gateway
        .in_process
        .0
        .insert("devjob".to_string(), Arc::new(Devjob));
    let in_process: HashMap<String, Arc<dyn InProcessServer>> = cfg
        .gateway
        .in_process
        .0
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let gw = Arc::new(Gateway::new(
        &dir.join("s"),
        cfg.gateway.clone(),
        in_process,
        None,
    ));

    // A first turn warms what the runtime opens once and keeps (its child-reaping machinery),
    // so the turns measured below start from the process's steady state.
    let _ = one_turn(&gw, &cfg, "warm").await;
    let warmed = settle_stable().await;

    for round in 0..2 {
        let before = settle_stable().await;
        let (peak, servers) = one_turn(&gw, &cfg, &format!("measured-{round}")).await;
        let after = settle_to(before).await;
        let cost = (peak - before) as libc::rlim_t;
        eprintln!(
            "gateway_fds: round {round}: {servers} servers admitted; descriptors before {before}, \
             at the peak {peak} (+{cost}), after the guard dropped {after}"
        );
        assert_eq!(
            after,
            before,
            "a closed turn left {} descriptor(s) open: a leak",
            after as i64 - before as i64
        );
        assert!(
            cost <= TURN_PEAK_FDS,
            "one turn now costs {cost} descriptors, more than TURN_PEAK_FDS ({TURN_PEAK_FDS}): \
             re-measure and raise the constant"
        );
    }
    // ROOM FOR SEVERAL TURNS AT ONCE under the limit the bridge raises itself to.
    assert!(
        MIN_CONCURRENT_TURNS * TURN_PEAK_FDS + warmed as libc::rlim_t <= NOFILE_CAP,
        "{MIN_CONCURRENT_TURNS} turns of {TURN_PEAK_FDS} descriptors and an idle {warmed} do \
         not fit under NOFILE_CAP ({NOFILE_CAP})"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
