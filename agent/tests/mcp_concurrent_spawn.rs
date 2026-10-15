//! Many stdio servers started at once, from several runtime threads, all connect.
//!
//! The regression this pins: on macOS a spawn racing another thread's fork could inherit a
//! sibling's pipe and block in `read` forever (see `SPAWN_LOCK` in `src/mcp/client.rs`). The
//! gateway starts a turn's whole server set concurrently, which is the shape below.

use std::collections::BTreeMap;
use std::time::Duration;

use jesse_agent::mcp::McpClient;

const PROBE_SERVER: &str = env!("CARGO_BIN_EXE_jesse-mcp-probe-server");

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn two_dozen_servers_started_together_all_connect_round_after_round() {
    // A BARE NAME found through the child's own PATH, as every gateway row is: that is what
    // sends std's macOS spawn down its fork path (an explicit PATH plus a lookup rules out
    // `posix_spawn`), and the fork path is the one that reads a pipe until EOF.
    let dir = std::path::Path::new(PROBE_SERVER)
        .parent()
        .expect("a directory");
    let program = std::path::Path::new(PROBE_SERVER)
        .file_name()
        .and_then(|n| n.to_str())
        .expect("a name")
        .to_string();
    let env = BTreeMap::from([(
        "PATH".to_string(),
        format!("{}:/usr/bin:/bin", dir.display()),
    )]);
    // Every client is KEPT until the end, as a turn keeps its upstreams: a pipe a sibling
    // inherited is then never closed, and a spawn blocked on it never returns.
    let mut alive = Vec::new();
    for round in 0..15 {
        let starts = (0..24).map(|i| {
            let env = env.clone();
            let program = program.clone();
            tokio::spawn(async move {
                let c = McpClient::connect(
                    &format!("probe{i}"),
                    &program,
                    &[],
                    &env,
                    Duration::from_secs(10),
                    Duration::from_secs(10),
                )
                .await
                .expect("connect");
                c.list_tools().await.expect("list");
                c
            })
        });
        let all = async {
            let mut out = Vec::new();
            for h in starts.collect::<Vec<_>>() {
                out.push(h.await.expect("task"));
            }
            out
        };
        let clients = tokio::time::timeout(Duration::from_secs(60), all)
            .await
            .unwrap_or_else(|_| panic!("round {round}: a server never finished connecting"));
        assert_eq!(clients.len(), 24);
        alive.extend(clients);
    }
}
