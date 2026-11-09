//! Reading a run's `stream.jsonl`: Claude Code's `--output-format stream-json` events, one per
//! line. Turned into readable lines (the assistant's text, one line per tool call, never raw
//! JSON and never a tool's output), and searched for the last `result` event.

use std::io::Read as _;
use std::path::Path;

use serde_json::Value;

/// The whole stream, from `stream.jsonl` or, once compressed, `stream.jsonl.gz`.
pub fn read_stream(run_dir: &Path) -> String {
    if let Ok(text) = std::fs::read_to_string(run_dir.join("stream.jsonl")) {
        return text;
    }
    let gz = run_dir.join("stream.jsonl.gz");
    if gz.is_file() {
        if let Ok(mut child) = std::process::Command::new("gzip")
            .arg("-dc")
            .arg(&gz)
            .stdout(std::process::Stdio::piped())
            .spawn()
        {
            let mut out = String::new();
            if let Some(mut so) = child.stdout.take() {
                let _ = so.read_to_string(&mut out);
            }
            let _ = child.wait();
            return out;
        }
    }
    String::new()
}

/// The stream from byte `offset` on (one round's events).
pub fn read_stream_from(run_dir: &Path, offset: u64) -> String {
    let all = read_stream(run_dir);
    let at = (offset as usize).min(all.len());
    // Never split a UTF-8 sequence: back up to a char boundary.
    let mut at = at;
    while at > 0 && !all.is_char_boundary(at) {
        at -= 1;
    }
    all[at..].to_string()
}

/// The text of the last `result` event, when it has one.
pub fn last_result(stream: &str) -> Option<String> {
    stream
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("type").and_then(Value::as_str) == Some("result"))
        .filter_map(|v| v.get("result").and_then(Value::as_str).map(str::to_string))
        .find(|s| !s.trim().is_empty())
}

/// The session id the stream's `init` event names, if any.
pub fn session_of(stream: &str) -> Option<String> {
    stream
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| {
            v.get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

/// One line for a tool call: `Bash: cargo test`, `Edit: bridge/src/x.rs`.
fn tool_line(name: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).map(str::to_string);
    let detail = match name {
        "Bash" => s("command"),
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => s("file_path")
            .or_else(|| s("notebook_path"))
            .map(|p| short_path(&p)),
        "Grep" | "Glob" => s("pattern"),
        "WebFetch" => s("url"),
        "WebSearch" => s("query"),
        "Task" | "Agent" => s("description"),
        _ => None,
    };
    match detail {
        Some(d) => {
            let first = d.lines().next().unwrap_or("").trim();
            let first: String = first.chars().take(200).collect();
            format!("{name}: {first}")
        }
        None => name.to_string(),
    }
}

/// A path inside a run's worktree, shown relative to it.
fn short_path(p: &str) -> String {
    match p.find("/worktree/") {
        Some(i) => p[i + "/worktree/".len()..].to_string(),
        None => p.to_string(),
    }
}

/// Readable lines from a stream: the assistant's text, line by line, and one line per tool
/// call. Tool results, system events and anything unparseable are left out.
pub fn readable_lines(stream: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in stream.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(content) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for c in content {
            match c.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(t) = c.get("text").and_then(Value::as_str) {
                        out.extend(
                            t.lines()
                                .filter(|l| !l.trim().is_empty())
                                .map(str::to_string),
                        );
                    }
                }
                Some("tool_use") => {
                    let name = c.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let input = c.get("input").cloned().unwrap_or(Value::Null);
                    out.push(tool_line(name, &input));
                }
                _ => {}
            }
        }
    }
    out
}

/// The last `n` readable lines.
pub fn tail_readable(stream: &str, n: usize) -> Vec<String> {
    let lines = readable_lines(stream);
    lines[lines.len().saturating_sub(n)..].to_vec()
}

/// What [`usage_limit`] found: the round ended on the plan's usage limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageLimit {
    /// When the limit resets, in epoch milliseconds, if the event says.
    pub resets_at_ms: Option<u64>,
}

/// The value marking a refusal by the account's own usage limit, on Claude Code 2.1.319.
pub const USAGE_LIMIT_REACHED: &str = "usage_limit_reached";

/// `resetsAt` is epoch seconds in `rate_limit_info`; a value already in milliseconds is kept.
fn resets_ms(info: &Value) -> Option<u64> {
    let t = info.get("resetsAt").and_then(Value::as_u64)?;
    Some(if t >= 100_000_000_000 { t } else { t * 1000 })
}

/// Whether one round's stream ended on the plan's usage limit, and when it resets.
///
/// Matched on the structured fields Claude Code 2.1.319 writes in `--output-format
/// stream-json`, never on the prose: when the account's usage limit refuses a request, the
/// round's last `assistant` event carries `"error": "rate_limit"`, `"is_api_error_message":
/// true`, `"api_error": "usage_limit_reached"` and `"api_error_params": {"rate_limit_info":
/// {"status": "rejected", "resetsAt": <epoch seconds>, "rateLimitType": …}}`, and the session
/// ends with a `result` event whose `is_error` is true (carrying `api_error` too when the CLI
/// copies it). The CLI's own schema documents `usage_limit_reached` as the one kind for "the
/// account's own usage limit, a plan limit (session, weekly or per-model) or a spend cap", and
/// says the other refusals that share `error: "rate_limit"` (a server throttle, a policy) never
/// carry it. The fixture is `tests/fixtures/hostrun/usage-limit.jsonl`.
///
/// The reset is the error event's `rate_limit_info.resetsAt`, else the last `rate_limit_event`
/// of the round whose status is `rejected`, else unknown.
pub fn usage_limit(stream: &str) -> Option<UsageLimit> {
    let events: Vec<Value> = stream
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    let kind = |v: &Value| v.get("type").and_then(Value::as_str).map(str::to_string);
    let result = events
        .iter()
        .rev()
        .find(|v| kind(v).as_deref() == Some("result"))?;
    if result.get("is_error").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let is_limit =
        |v: &Value| v.get("api_error").and_then(Value::as_str) == Some(USAGE_LIMIT_REACHED);
    let error_event = events
        .iter()
        .rev()
        .find(|v| kind(v).as_deref() == Some("assistant") && is_limit(v));
    if !is_limit(result) && error_event.is_none() {
        return None;
    }
    let from_error = [error_event, Some(result)]
        .into_iter()
        .flatten()
        .find_map(|v| {
            v.get("api_error_params")
                .and_then(|p| p.get("rate_limit_info"))
                .and_then(resets_ms)
        });
    let from_event = || {
        events.iter().rev().find_map(|v| {
            if kind(v).as_deref() != Some("rate_limit_event") {
                return None;
            }
            let info = v.get("rate_limit_info")?;
            if info.get("status").and_then(Value::as_str) != Some("rejected") {
                return None;
            }
            resets_ms(info)
        })
    };
    Some(UsageLimit {
        resets_at_ms: from_error.or_else(from_event),
    })
}
