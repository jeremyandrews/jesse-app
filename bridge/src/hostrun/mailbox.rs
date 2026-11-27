//! **THE MAILBOX** — a message to a run while it is running.
//!
//! `devjob_reply` on a `running` run does not wait for the round to end: it queues the text as
//! one file in the run's mailbox, and a `PostToolUse` hook (`jesse-hook steer`) hands every
//! undelivered message to the agent as added context after its current tool call. Nothing
//! interrupts the tool call itself.
//!
//! ONE MECHANISM ON BOTH HOSTS. A message is one JSON file ([`MailMessage`]) named
//! `<sortable time>-<id>.json` ([`file_name`]); the hook reads them in name order, so in the
//! order they were sent. It claims each one by creating `<id>.claim` in the marker directory
//! with `O_EXCL` ([`claim`]), so two hooks firing at once (parallel tool calls) never both
//! deliver it; prints the hook output ([`hook_output`]); and only then marks it `<id>.delivered`
//! and, when told where, prints one [`DELIVERED_LINE`] per message ([`run_hook`]). A restart
//! never delivers a marked message again.
//!
//!   * **The Studio**: the mailbox is `<run dir>/mailbox`; the markers sit beside the messages,
//!     and the bridge reads them into the record's `delivered_at` ([`delivered_marks`]).
//!   * **The cloud**: the mailbox is the ConfigMap `steer-<run_id>` mounted read only (one key
//!     per message, the same name and body as the file); the markers go to a writable directory
//!     in the pod ([`MARKS_ENV`]) and each delivery is also printed to the agent's log
//!     ([`DELIVERY_LOG_ENV`]), where the bridge, which already reads that log, parses it
//!     ([`parse_delivered_lines`]).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The run directory's mailbox, on the Studio.
pub const MAILBOX_DIR: &str = "mailbox";
/// The agent's variable naming its mailbox. Absent, or naming no directory: the hook does
/// nothing.
pub const MAILBOX_ENV: &str = "JESSE_RUN_MAILBOX";
/// Where the hook writes its delivery markers when the mailbox is read only (the cloud's
/// ConfigMap mount). Absent: beside the messages.
pub const MARKS_ENV: &str = "JESSE_RUN_MAILBOX_MARKS";
/// A file the hook appends one [`DELIVERED_LINE`] to per delivery: the pod's own log in the
/// cloud. Absent (the Studio): nothing is printed.
pub const DELIVERY_LOG_ENV: &str = "JESSE_RUN_DELIVERY_LOG";
/// The line the hook prints per delivery: `JESSE_MESSAGE_DELIVERED <id> <delivered_at>`.
pub const DELIVERED_LINE: &str = "JESSE_MESSAGE_DELIVERED";
/// A marker's suffix: `<id>.delivered`, holding the RFC 3339 time it was delivered.
pub const MARK_SUFFIX: &str = ".delivered";
/// Where the cloud's Job mounts the ConfigMap.
pub const CLOUD_MAILBOX_PATH: &str = "/jesse/steer";
/// The first line of every delivered message.
pub const HEADER: &str = "Message from Jeremy while you were running:";
/// The last line of every delivered message.
pub const FOOTER: &str = "Act on this now; it overrides the prompt where they conflict.";

/// [`crate::hostrun::RunMessage::channel`]: the hook hands it over after the current tool call.
pub const CHANNEL_TOOL_CALL: &str = "after_tool_call";
/// [`crate::hostrun::RunMessage::channel`]: it goes at the head of the next round's text, the
/// agent taking no added context, or the round having ended before the hook delivered it.
pub const CHANNEL_NEXT_ROUND: &str = "next_round";

/// One message, as a mailbox file (and as a ConfigMap value) holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailMessage {
    pub id: String,
    pub text: String,
    /// RFC 3339.
    pub queued_at: String,
}

/// A new message id: `m` and 12 hex digits, valid as a file name and a ConfigMap key.
pub fn new_id() -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("m{}", &u[..12])
}

/// `<UTC time to the millisecond>-<id>.json`: sorts in the order the messages were sent, and is
/// a valid ConfigMap key (`[-._a-zA-Z0-9]`).
pub fn file_name(queued_ms: u64, id: &str) -> String {
    let t = chrono::DateTime::from_timestamp_millis(queued_ms as i64).unwrap_or_default();
    format!("{}-{id}.json", t.format("%Y%m%dT%H%M%S%3fZ"))
}

/// The id a message file's name carries, when it is one.
pub fn id_of(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".json")?;
    let (_, id) = stem.split_once('-')?;
    valid_id(id).then_some(id)
}

/// Whether `id` can be a message id: letters and digits only, so it is safe in a file name, a
/// ConfigMap key and a log line.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// The body of a message file.
pub fn body(m: &MailMessage) -> String {
    serde_json::to_string(m).expect("a message serialises")
}

/// Write one message into `mailbox`, atomically, under `name`. Idempotent.
pub fn write_message(mailbox: &Path, name: &str, m: &MailMessage) -> std::io::Result<()> {
    std::fs::create_dir_all(mailbox)?;
    crate::write_atomic(&mailbox.join(name), body(m).as_bytes())
}

/// Every message in `mailbox`, in name order (the order they were sent). A missing mailbox
/// (the cloud's optional mount, absent) is empty; an unreadable file is skipped.
pub fn messages(mailbox: &Path) -> Vec<(String, MailMessage)> {
    let Ok(rd) = std::fs::read_dir(mailbox) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| id_of(n).is_some())
        .collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|n| {
            let text = std::fs::read_to_string(mailbox.join(&n)).ok()?;
            let m: MailMessage = serde_json::from_str(&text).ok()?;
            (Some(m.id.as_str()) == id_of(&n)).then_some((n, m))
        })
        .collect()
}

/// A claim's suffix: `<id>.claim`, created exclusively by the one hook that hands the message
/// over, before it prints anything.
pub const CLAIM_SUFFIX: &str = ".claim";

/// Claim `id` for delivery: create `<id>.claim`, failing if it exists. `true` only for the one
/// caller that created it, so two hooks at once (parallel tool calls) never both hand it over.
pub fn claim(marks: &Path, id: &str) -> bool {
    if std::fs::create_dir_all(marks).is_err() {
        return false;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marks.join(format!("{id}{CLAIM_SUFFIX}")))
        .is_ok()
}

/// Give up a claim the hook could not act on, so the next hook tries again.
pub fn release(marks: &Path, id: &str) {
    let _ = std::fs::remove_file(marks.join(format!("{id}{CLAIM_SUFFIX}")));
}

/// Mark `id` delivered at `at`, once the agent's CLI has been handed it: `<id>.delivered`,
/// written atomically, then the claim goes.
pub fn mark_delivered(marks: &Path, id: &str, at: &str) {
    let _ = crate::write_atomic(&marks.join(format!("{id}{MARK_SUFFIX}")), at.as_bytes());
    release(marks, id);
}

/// Claim every message in `mailbox` that has neither a delivery marker nor a claim in `marks`,
/// in order. Nothing is marked delivered here: that waits until the hook has printed them.
pub fn claim_waiting(mailbox: &Path, marks: &Path) -> Vec<MailMessage> {
    let has = |m: &MailMessage, suffix: &str| marks.join(format!("{}{suffix}", m.id)).exists();
    messages(mailbox)
        .into_iter()
        .map(|(_, m)| m)
        .filter(|m| !has(m, MARK_SUFFIX) && !has(m, CLAIM_SUFFIX))
        .filter(|m| claim(marks, &m.id))
        .collect()
}

/// Every marker in `marks`: message id to when it was delivered (the marker's text, else its
/// modification time).
pub fn delivered_marks(marks: &Path) -> BTreeMap<String, String> {
    let Ok(rd) = std::fs::read_dir(marks) else {
        return BTreeMap::new();
    };
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(MARK_SUFFIX)?.to_string();
            if !valid_id(&id) {
                return None;
            }
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            let at = match crate::hostrun::ms_of(text.trim()) {
                Some(_) => text.trim().to_string(),
                None => e
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| crate::hostrun::rfc(d.as_millis() as u64))
                    .unwrap_or_default(),
            };
            Some((id, at))
        })
        .collect()
}

/// One message as the agent reads it.
pub fn render(text: &str) -> String {
    format!("{HEADER}\n\n{}\n\n{FOOTER}", text.trim())
}

/// Every message as the agent reads it, one after another.
pub fn render_all<'a>(texts: impl IntoIterator<Item = &'a str>) -> String {
    texts
        .into_iter()
        .map(render)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// What a `PostToolUse` hook prints to hand `msgs` to the agent: `hookSpecificOutput` with
/// `additionalContext`, the field both Claude Code (2.1.331) and Codex (codex-cli 0.169.0) add
/// to the model's next turn. `None` with nothing to deliver: the hook prints nothing.
pub fn hook_output(msgs: &[MailMessage]) -> Option<String> {
    if msgs.is_empty() {
        return None;
    }
    let ctx = render_all(msgs.iter().map(|m| m.text.as_str()));
    Some(
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": ctx,
            }
        })
        .to_string(),
    )
}

/// `JESSE_MESSAGE_DELIVERED <id> <at>`.
pub fn delivery_line(id: &str, at: &str) -> String {
    format!("{DELIVERED_LINE} {id} {at}")
}

/// Every delivery a log names: message id to when (the line's time, or `None` when the line
/// carries only the id). A line counts only when it starts with [`DELIVERED_LINE`].
pub fn parse_delivered_lines(log: &str) -> BTreeMap<String, Option<String>> {
    let mut out = BTreeMap::new();
    for line in log.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some(DELIVERED_LINE) {
            continue;
        }
        let Some(id) = parts.next().filter(|i| valid_id(i)) else {
            continue;
        };
        let at = parts
            .next()
            .filter(|t| crate::hostrun::ms_of(t).is_some())
            .map(str::to_string);
        out.entry(id.to_string()).or_insert(at);
    }
    out
}

/// What the hook works from: its three variables.
#[derive(Debug, Clone, Default)]
pub struct HookPlaces {
    pub mailbox: Option<PathBuf>,
    pub marks: Option<PathBuf>,
    pub log: Option<PathBuf>,
}

impl HookPlaces {
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> HookPlaces {
        let path = |k: &str| get(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        HookPlaces {
            mailbox: path(MAILBOX_ENV),
            marks: path(MARKS_ENV),
            log: path(DELIVERY_LOG_ENV),
        }
    }
}

/// The whole hook, apart from reading stdin: claim what waits, write the hook output to `out`,
/// and only once that has been written and flushed mark each message delivered and append its
/// delivery line. Answers what was delivered; nothing (and nothing written) without a mailbox,
/// with a missing one, or with nothing undelivered.
///
/// THE ORDER IS THE GUARANTEE. A message is never marked delivered before the agent's CLI has
/// its text: when the write fails the claims are given up and the next hook tries again. A hook
/// killed after claiming but before printing leaves its claim and no marker, so the bridge still
/// counts the message as waiting, names it in the round's report and puts it at the head of the
/// next round. One killed after printing but before marking may have it handed over a second
/// time that way: a repeat, never a loss and never a delivery claimed that did not happen.
pub fn run_hook(p: &HookPlaces, at: &str, out: &mut dyn Write) -> Vec<MailMessage> {
    let Some(mailbox) = p.mailbox.as_deref().filter(|m| m.is_dir()) else {
        return Vec::new();
    };
    let marks = p.marks.as_deref().unwrap_or(mailbox);
    let got = claim_waiting(mailbox, marks);
    let Some(text) = hook_output(&got) else {
        return Vec::new();
    };
    if writeln!(out, "{text}").and_then(|_| out.flush()).is_err() {
        for m in &got {
            release(marks, &m.id);
        }
        return Vec::new();
    }
    for m in &got {
        mark_delivered(marks, &m.id, at);
    }
    if let Some(log) = &p.log {
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(log) {
            let lines: String = got
                .iter()
                .map(|m| format!("{}\n", delivery_line(&m.id, at)))
                .collect();
            let _ = f.write_all(lines.as_bytes());
        }
    }
    got
}

/// The ConfigMap a cloud run's messages go to: `steer-<run id>`, lowercased (a Kubernetes name).
pub fn config_map_name(run_id: &str) -> String {
    format!("steer-{}", run_id.to_ascii_lowercase())
}

#[cfg(test)]
mod tests;
