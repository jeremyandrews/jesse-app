//! `jesse-healthdb-mcp` — the health archive, read-only, spoken as MCP over stdio.
//!
//! The logic and every guard live in [`jesse_bridge::healthquery`]; this file is a transport
//! and nothing else, for the reason `jesse-places-mcp` is: a `"command": "jesse-healthdb-mcp"`
//! entry is a BARE NAME resolved from the child's `PATH` and reads identically everywhere.
//!
//! It is started with two paths in its environment, `JESSE_HEALTHDB_RO_URL_FILE` and
//! `JESSE_HEALTHDB_CA_FILE`, and reads the read-only role's URL itself, with the same `0600`
//! and owner checks the bridge makes. It never sees the owner's credential.
//!
//! One stderr line per call: the tool, the outcome, the duration and the row count. Never the
//! SQL, a value, an error message or the URL.

use std::time::Instant;

use jesse_bridge::{run_health_tool, HealthDbTool, HealthQueryConfig, HealthQueryLedger};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// The MCP protocol version this server falls back to when a client does not name one.
const FALLBACK_PROTOCOL_VERSION: &str = "2024-11-05";

#[tokio::main]
async fn main() {
    let cfg = match HealthQueryConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("jesse-healthdb-mcp: {e}");
            std::process::exit(1);
        }
    };
    let ledger = HealthQueryLedger::from_env();

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = tokio::io::stdout();

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            // A malformed line is answered and survived, as in `jesse-places-mcp`.
            Err(e) => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {"code": -32700, "message": format!("parse error: {e}")}
                });
                let _ = write_line(&mut stdout, &err).await;
                continue;
            }
        };

        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        // A notification (no `id`) is never answered.
        let Some(id) = req.get("id").cloned() else {
            continue;
        };

        let response = match method {
            "initialize" => {
                let version = req
                    .get("params")
                    .and_then(|p| p.get("protocolVersion"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(FALLBACK_PROTOCOL_VERSION)
                    .to_string();
                ok(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {}},
                        "serverInfo": {
                            "name": "jesse-healthdb-mcp",
                            "version": env!("CARGO_PKG_VERSION"),
                        }
                    }),
                )
            }
            "ping" => ok(id, json!({})),
            "tools/list" => ok(id, json!({"tools": tool_list()})),
            "tools/call" => {
                let params = req.get("params");
                let name = params
                    .and_then(|p| p.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                let args = params
                    .and_then(|p| p.get("arguments"))
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match HealthDbTool::parse(name) {
                    Some(tool) => {
                        let started = Instant::now();
                        let reply = run_health_tool(&cfg, tool, &args).await;
                        let elapsed = started.elapsed();
                        eprintln!(
                            "jesse-healthdb-mcp: {} {} in {}ms, {} rows",
                            tool.tool_name(),
                            reply.outcome.as_str(),
                            elapsed.as_millis(),
                            reply.rows
                        );
                        ledger.record(tool, &reply, elapsed);
                        // A failure is reported as a failure, with Postgres's message, so the
                        // turn can correct its query rather than read "no rows" as an answer.
                        ok(
                            id,
                            json!({
                                "content": [{"type": "text", "text": reply.text}],
                                "isError": reply.is_error,
                            }),
                        )
                    }
                    None => err(id, -32602, format!("unknown tool: {name}")),
                }
            }
            other => err(id, -32601, format!("unknown method: {other}")),
        };

        if write_line(&mut stdout, &response).await.is_err() {
            break; // the client is gone
        }
    }
}

/// The advertised tools, built from [`HealthDbTool::ALL`].
fn tool_list() -> Vec<Value> {
    HealthDbTool::ALL
        .into_iter()
        .map(|t| {
            json!({
                "name": t.tool_name(),
                "description": t.description(),
                "inputSchema": t.input_schema(),
                "annotations": {"readOnlyHint": true},
            })
        })
        .collect()
}

fn ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn err(id: Value, code: i64, message: String) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

async fn write_line<W>(w: &mut W, v: &Value) -> std::io::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let mut s = serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string());
    s.push('\n');
    w.write_all(s.as_bytes()).await?;
    w.flush().await
}
