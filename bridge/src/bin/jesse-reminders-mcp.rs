//! `jesse-reminders-mcp` — the bridge's timed reminders, spoken as MCP over stdio.
//!
//! The logic lives in [`jesse_bridge::reminders`]; this file is a transport and nothing else, for
//! the reason `jesse-places-mcp` is: a `"command": "jesse-reminders-mcp"` entry is a BARE NAME
//! resolved from the child's `PATH`, identical on every deployment, where an HTTP entry would
//! have to name a host and port.
//!
//! Four tools: `create_reminder`, `list_reminders`, `cancel_reminder` and `snooze_reminder`. Each
//! reads and writes ONE file, `<state_dir>/reminders.json` (the state dir found as the bridge
//! finds it: `JESSE_STATE_DIR`, else `$HOME/.jesse-bridge`), under the store's advisory lock,
//! and reads the profile zone from `<state_dir>/profile.json` exactly as the bridge does. It
//! writes nothing else, sends nothing anywhere, and starts no process: the bridge's ticker is
//! what turns a stored reminder into a push.

use chrono::Utc;
use jesse_bridge::reminders::{
    profile_zone_in, run_reminder_tool, state_dir_from_env, ReminderStore, ReminderTool,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// The MCP protocol version this server falls back to when a client does not name one.
const FALLBACK_PROTOCOL_VERSION: &str = "2024-11-05";

#[tokio::main]
async fn main() {
    let Some(state_dir) = state_dir_from_env() else {
        eprintln!("jesse-reminders-mcp: no state dir (set JESSE_STATE_DIR or HOME)");
        std::process::exit(1);
    };
    let store = ReminderStore::in_state_dir(&state_dir);

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
                            "name": "jesse-reminders-mcp",
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
                match ReminderTool::parse(name) {
                    Some(tool) => {
                        let store = store.clone();
                        let dir = state_dir.clone();
                        // The store's lock is a blocking `flock`; keep it off the reactor.
                        let result = tokio::task::spawn_blocking(move || {
                            let now = Utc::now();
                            let zone = profile_zone_in(&dir, now);
                            run_reminder_tool(&store, tool, &args, now, &zone)
                        })
                        .await
                        .unwrap_or_else(|e| Err(format!("internal error: {e}")));
                        match result {
                            Ok(v) => ok(
                                id,
                                json!({
                                    "content": [{
                                        "type": "text",
                                        "text": serde_json::to_string_pretty(&v)
                                            .unwrap_or_else(|_| v.to_string()),
                                    }],
                                    "isError": false,
                                }),
                            ),
                            Err(e) => ok(
                                id,
                                json!({
                                    "content": [{"type": "text", "text": e}],
                                    "isError": true,
                                }),
                            ),
                        }
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

/// The advertised tools, built from [`ReminderTool::ALL`] so a new one cannot be added to the
/// enum without appearing here (and in the grant, whose test compares against the same list).
fn tool_list() -> Vec<Value> {
    ReminderTool::ALL
        .into_iter()
        .map(|t| {
            json!({
                "name": t.tool_name(),
                "description": t.description(),
                "inputSchema": t.input_schema(),
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
