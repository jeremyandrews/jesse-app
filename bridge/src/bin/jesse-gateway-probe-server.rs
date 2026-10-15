//! `jesse-gateway-probe-server` — the gateway's own fake upstream, for its tests and its
//! containment probes. Built only with the `containment-probe` feature: a release build carries
//! no binary that pretends to be an MCP server.
//!
//! A real stdio MCP server the gateway really starts, so a verdict is the PROCESS'S OWN
//! account rather than anything the gateway says about itself:
//!
//!   * `JESSE_GWPROBE_LOG` names a file it appends one line per `tools/call` to: the tool's
//!     name. A probe that an ungranted tool was refused reads this file and finds no line.
//!   * `JESSE_GWPROBE_ENV_DUMP` names a file it writes its WHOLE environment to at startup,
//!     one `NAME=value` per line. A probe that the bridge's secrets stayed in the bridge reads
//!     this file and finds no planted nonce.
//!
//! At `initialize` it also writes the `clientInfo` name it was given to `<dump>.client`.
//!
//! Both arrive by NAME through the gateway's environment column (they are the probe row's
//! `env`), which is itself part of what is being shown: a named variable reaches the
//! upstream, and an unnamed one does not.
//!
//! Tools: `probe_echo` (returns its `text` argument) and `probe_hidden` (returns a canary; no
//! grant ever names it).

use std::io::{BufRead, Write};

const HIDDEN_CANARY: &str = "CANARY-GATEWAY-HIDDEN-9d41c6e2a07b";

fn main() {
    let log = std::env::var("JESSE_GWPROBE_LOG").ok();
    let dump = std::env::var("JESSE_GWPROBE_ENV_DUMP").ok();
    if let Some(path) = &dump {
        let dump: String = std::env::vars()
            .map(|(k, v)| format!("{k}={v}\n"))
            .collect();
        let _ = std::fs::write(path, dump);
    }
    let mut out = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let method = msg["method"].as_str().unwrap_or("");
        if method == "initialize" {
            // The client name it was introduced to, beside the dump, so a test can see the
            // gateway relay the child's own `clientInfo`.
            if let Some(path) = &dump {
                let name = msg["params"]["clientInfo"]["name"].as_str().unwrap_or("");
                let _ = std::fs::write(format!("{path}.client"), name);
            }
        }
        let result = match method {
            "initialize" => serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "jesse-gateway-probe-server", "version": "1"},
            }),
            "tools/list" => serde_json::json!({"tools": [
                {"name": "probe_echo", "description": "Echo the text argument back.",
                 "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
                 "annotations": {"readOnlyHint": true}},
                {"name": "probe_hidden", "description": "Never granted.",
                 "inputSchema": {"type": "object"}},
            ]}),
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("").to_string();
                if let Some(path) = &log {
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                    {
                        let _ = writeln!(f, "{name}");
                    }
                }
                let text = match name.as_str() {
                    "probe_echo" => format!(
                        "echo:{}",
                        msg["params"]["arguments"]["text"].as_str().unwrap_or("")
                    ),
                    "probe_hidden" => HIDDEN_CANARY.to_string(),
                    _ => format!("no such tool {name}"),
                };
                serde_json::json!({"content": [{"type": "text", "text": text}]})
            }
            _ => {
                let e = serde_json::json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": "method not found"}});
                let _ = writeln!(out, "{e}");
                let _ = out.flush();
                continue;
            }
        };
        let reply = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
        if writeln!(out, "{reply}").is_err() || out.flush().is_err() {
            break;
        }
    }
}
