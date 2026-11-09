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
