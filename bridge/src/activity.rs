//! **THE ACTIVITY LEDGER** — one content-free JSON line per turn and per gateway tool call.
//!
//! `<state_dir>/activity/YYYY-MM-DD.jsonl`, the file `0600` and its directory `0700`, one JSON
//! object per line, each with a versioned `kind` (`"kind": "turn", "v": 1`). Days are the
//! bridge's local calendar. Files older than [`ACTIVITY_RETENTION_DAYS`] are removed by the
//! periodic retention sweep ([`crate::spawn_eviction_task`]).
//!
//! ---- WHAT IT CAN HOLD, BY CONSTRUCTION ------------------------------------------
//!
//! Identifiers, enumerations, timestamps and counts. Never a prompt, an argument, a result, a
//! URL, a path or an error message. The guarantee is in the TYPES rather than in the callers'
//! care: every string field is a [`Token`], which is either a whole identifier
//! (`[A-Za-z0-9._:+-]`, at most [`TOKEN_MAX`] characters) or the fixed word `_invalid`. A
//! caller that passed a tool's arguments, a body or a URL path by mistake writes `_invalid`,
//! never a fragment of the content: a token is not filtered character by character, it is
//! accepted or replaced whole.
//! [`tests::no_field_of_any_kind_can_carry_content`] walks every field of every kind.
//!
//! It is the record the cluster's audit rests on: what ran, who asked, which tool, how long,
//! how it ended, and how many bytes crossed, so an operator can see the SHAPE of every turn
//! without the ledger becoming a second copy of anybody's mail.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

/// Activity files are kept this long.
pub const ACTIVITY_RETENTION_DAYS: i64 = 90;

/// The schema version every line carries.
pub const ACTIVITY_SCHEMA: u32 = 1;

/// The longest a [`Token`] may be.
pub const TOKEN_MAX: usize = 96;

/// An identifier: `[A-Za-z0-9._:+-]`, non-empty, at most [`TOKEN_MAX`] characters, or else
/// [`Token::INVALID`] in its place. The only string a ledger line can hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Token(String);

impl Token {
    /// What any non-identifier becomes, whole.
    pub const INVALID: &'static str = "_invalid";

    pub fn new(s: &str) -> Token {
        let ok = !s.is_empty()
            && s.len() <= TOKEN_MAX
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._:+-".contains(c));
        Token(if ok {
            s.to_string()
        } else {
            Self::INVALID.to_string()
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Ok,
    Failed,
    Cancelled,
}

/// How one gateway `tools/call` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    /// The upstream answered (its own `isError` included: that is a tool's answer).
    Ok,
    /// The tool is not granted to this turn; nothing was sent upstream.
    Refused,
    /// The upstream could not be reached, broke protocol or timed out.
    Failed,
    /// The upstream answered with more than the row's result cap; the turn got an error.
    Truncated,
}

/// One turn.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TurnLine {
    pub job_id: Token,
    pub harness: Token,
    pub model: Token,
    pub capability: Token,
    /// `phone`, `schedule:<id>` or `strand-tick`.
    pub origin: Token,
    /// Unix milliseconds.
    pub start_ms: u64,
    pub end_ms: u64,
    pub outcome: TurnOutcome,
}

/// One gateway `tools/call`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolCallLine {
    pub job_id: Token,
    pub server: Token,
    pub tool: Token,
    /// Unix milliseconds at which the call arrived.
    pub at_ms: u64,
    pub duration_ms: u64,
    pub outcome: ToolOutcome,
    /// Bytes of the call's `arguments`, serialised. A COUNT; the arguments are never kept.
    pub request_bytes: u64,
    /// Bytes of the result the upstream sent, serialised (zero when nothing came back).
    pub response_bytes: u64,
}

/// A ledger line: the kind, its version, and its fields, flattened.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActivityLine {
    Turn {
        v: u32,
        #[serde(flatten)]
        line: TurnLine,
    },
    ToolCall {
        v: u32,
        #[serde(flatten)]
        line: ToolCallLine,
    },
}

impl ActivityLine {
    pub fn turn(line: TurnLine) -> ActivityLine {
        ActivityLine::Turn {
            v: ACTIVITY_SCHEMA,
            line,
        }
    }

    pub fn tool_call(line: ToolCallLine) -> ActivityLine {
        ActivityLine::ToolCall {
            v: ACTIVITY_SCHEMA,
            line,
        }
    }
}

/// Appends lines to the day's file. Cheap to share: one mutex around an open-append-close.
#[derive(Debug)]
pub struct ActivityLedger {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl ActivityLedger {
    /// The ledger under `<state_dir>/activity`. Nothing is created until the first line.
    pub fn new(state_dir: &Path) -> ActivityLedger {
        ActivityLedger {
            dir: state_dir.join("activity"),
            lock: Mutex::new(()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Append one line. A write that fails is logged and dropped: the ledger records turns, it
    /// never decides them.
    pub fn record(&self, line: &ActivityLine) {
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        if let Err(e) = self.append(&day, line) {
            eprintln!("activity ledger: could not append a line: {e}");
        }
    }

    fn append(&self, day: &str, line: &ActivityLine) -> std::io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        if !self.dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.dir)?;
        }
        let path = self.dir.join(format!("{day}.jsonl"));
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        // A file created by an older build or by hand is brought to 0600 too.
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let mut text = serde_json::to_string(line).map_err(std::io::Error::other)?;
        text.push('\n');
        f.write_all(text.as_bytes())
    }

    /// Remove day files older than [`ACTIVITY_RETENTION_DAYS`] before `today`. Returns how
    /// many were removed. Files whose names are not a date are left alone.
    pub fn prune(&self, today: chrono::NaiveDate) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return 0;
        };
        let mut removed = 0;
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".jsonl") else {
                continue;
            };
            let Ok(day) = chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d") else {
                continue;
            };
            if (today - day).num_days() > ACTIVITY_RETENTION_DAYS
                && std::fs::remove_file(e.path()).is_ok()
            {
                removed += 1;
            }
        }
        removed
    }
}

/// The process's ledger under `cfg`'s state directory, shared by every writer so appends go
/// through one lock. `None` when persistence is off.
pub fn global(cfg: &crate::Config) -> Option<std::sync::Arc<ActivityLedger>> {
    static LEDGER: std::sync::OnceLock<Option<std::sync::Arc<ActivityLedger>>> =
        std::sync::OnceLock::new();
    LEDGER
        .get_or_init(|| {
            cfg.state_dir
                .as_deref()
                .map(|d| std::sync::Arc::new(ActivityLedger::new(Path::new(d))))
        })
        .clone()
}

/// Writes a turn's line when it is dropped, with the outcome read off the job's final state.
pub struct TurnRecorder {
    ledger: Option<std::sync::Arc<ActivityLedger>>,
    jobs: std::sync::Arc<crate::JobStore>,
    job_id: String,
    harness: Token,
    model: Token,
    capability: Token,
    origin: Token,
    start_ms: u64,
}

impl TurnRecorder {
    pub fn start(
        ledger: Option<std::sync::Arc<ActivityLedger>>,
        jobs: std::sync::Arc<crate::JobStore>,
        job_id: &str,
        harness: &str,
        model: &str,
        capability: &str,
        origin: &str,
    ) -> TurnRecorder {
        TurnRecorder {
            ledger,
            jobs,
            job_id: job_id.to_string(),
            harness: Token::new(harness),
            model: Token::new(model),
            capability: Token::new(capability),
            origin: Token::new(origin),
            start_ms: now_ms(),
        }
    }
}

impl Drop for TurnRecorder {
    fn drop(&mut self) {
        let Some(ledger) = &self.ledger else {
            return;
        };
        let outcome = match self.jobs.get(&self.job_id) {
            Some(crate::JobState::Done { .. }) => TurnOutcome::Ok,
            Some(crate::JobState::Cancelled) => TurnOutcome::Cancelled,
            _ => TurnOutcome::Failed,
        };
        ledger.record(&ActivityLine::turn(TurnLine {
            job_id: Token::new(&self.job_id),
            harness: self.harness.clone(),
            model: self.model.clone(),
            capability: self.capability.clone(),
            origin: self.origin.clone(),
            start_ms: self.start_ms,
            end_ms: now_ms(),
            outcome,
        }));
    }
}

/// Unix milliseconds now.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "jesse-activity-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Every field of every kind, filled with the worst a careless caller could pass: an
    /// argument object, a request body, a URL with a path and a query. None of it survives as
    /// content: every string in every serialised line is identifier-shaped.
    #[test]
    fn no_field_of_any_kind_can_carry_content() {
        let hostile = [
            r#"{"query":"my private mail","to":"someone@example.com"}"#,
            "GET /api/states/lock.front_door?token=abc HTTP/1.1",
            "https://example.com/private/path?q=secret words",
            "the body of a message, with spaces and a newline\nsecond line",
        ];
        for h in hostile {
            let lines = [
                ActivityLine::turn(TurnLine {
                    job_id: Token::new(h),
                    harness: Token::new(h),
                    model: Token::new(h),
                    capability: Token::new(h),
                    origin: Token::new(h),
                    start_ms: 1,
                    end_ms: 2,
                    outcome: TurnOutcome::Ok,
                }),
                ActivityLine::tool_call(ToolCallLine {
                    job_id: Token::new(h),
                    server: Token::new(h),
                    tool: Token::new(h),
                    at_ms: 1,
                    duration_ms: 2,
                    outcome: ToolOutcome::Truncated,
                    request_bytes: 3,
                    response_bytes: 4,
                }),
            ];
            for line in lines {
                let v = serde_json::to_value(&line).unwrap();
                let obj = v.as_object().unwrap();
                assert_eq!(obj["v"], ACTIVITY_SCHEMA);
                for (k, val) in obj {
                    match val {
                        serde_json::Value::Number(_) => {}
                        serde_json::Value::String(s) if k == "kind" => {
                            assert!(s == "turn" || s == "tool_call");
                        }
                        serde_json::Value::String(s) if k == "outcome" => {
                            assert!(!s.contains(' ') && !s.contains('/'));
                        }
                        serde_json::Value::String(s) => {
                            assert_eq!(s, Token::INVALID, "field {k} carries {s:?}");
                        }
                        other => panic!("field {k} is neither a number nor a token: {other}"),
                    }
                }
            }
        }
    }

    #[test]
    fn the_kinds_are_named_and_versioned() {
        let t = serde_json::to_value(ActivityLine::turn(TurnLine {
            job_id: Token::new("j1"),
            harness: Token::new("claude-code"),
            model: Token::new("opus"),
            capability: Token::new("write"),
            origin: Token::new("schedule:morning-brief"),
            start_ms: 10,
            end_ms: 20,
            outcome: TurnOutcome::Failed,
        }))
        .unwrap();
        assert_eq!(t["kind"], "turn");
        assert_eq!(t["origin"], "schedule:morning-brief");
        assert_eq!(t["outcome"], "failed");
        let c = serde_json::to_value(ActivityLine::tool_call(ToolCallLine {
            job_id: Token::new("j1"),
            server: Token::new("google-perseido"),
            tool: Token::new("search_gmail_messages"),
            at_ms: 1,
            duration_ms: 5,
            outcome: ToolOutcome::Refused,
            request_bytes: 0,
            response_bytes: 0,
        }))
        .unwrap();
        assert_eq!(c["kind"], "tool_call");
        assert_eq!(c["server"], "google-perseido");
        assert_eq!(c["outcome"], "refused");
    }

    #[test]
    fn lines_land_in_a_0600_day_file_and_old_days_are_pruned() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp();
        let l = ActivityLedger::new(&dir);
        l.record(&ActivityLine::tool_call(ToolCallLine {
            job_id: Token::new("j"),
            server: Token::new("s"),
            tool: Token::new("t"),
            at_ms: 0,
            duration_ms: 0,
            outcome: ToolOutcome::Ok,
            request_bytes: 0,
            response_bytes: 0,
        }));
        let today = chrono::Local::now().date_naive();
        let f = l.dir().join(format!("{}.jsonl", today.format("%Y-%m-%d")));
        let meta = std::fs::metadata(&f).expect("the day file");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::metadata(l.dir()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let old = today - chrono::Duration::days(ACTIVITY_RETENTION_DAYS + 1);
        let edge = today - chrono::Duration::days(ACTIVITY_RETENTION_DAYS);
        for d in [old, edge] {
            std::fs::write(l.dir().join(format!("{}.jsonl", d.format("%Y-%m-%d"))), "").unwrap();
        }
        std::fs::write(l.dir().join("notes.txt"), "").unwrap();
        assert_eq!(l.prune(today), 1);
        assert!(f.exists());
        assert!(l
            .dir()
            .join(format!("{}.jsonl", edge.format("%Y-%m-%d")))
            .exists());
        assert!(l.dir().join("notes.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
