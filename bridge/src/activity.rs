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
    /// The turn's egress tier (`closed`, `allowlist`, `open-logged`), when the turn had one.
    /// Absent (not `null`) on a turn that ran with the proxies off, so a line written before
    /// tiers existed and one written without a tier read the same.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<Token>,
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

/// One model call through the model proxy ([`crate::proxy::model`]).
///
/// Tokens are the provider's own usage block; `cost_micros` is millionths of a dollar at the
/// registry's four-rate deck, an integer so the line holds no float formatting.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelCallLine {
    pub job_id: Token,
    /// `anthropic-subscription`, `anthropic-api`, `fireworks`, `local`, `openai-chatgpt`.
    pub route: Token,
    /// The registry id the turn's token names.
    pub model: Token,
    pub at_ms: u64,
    pub duration_ms: u64,
    /// The upstream's HTTP status (0 when it could not be reached).
    pub status: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_micros: u64,
}

/// Whether an egress request reads or sends ([`crate::proxy::egress`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MethodClass {
    Read,
    Send,
    /// A `CONNECT` refused before any inner request was seen.
    None,
}

/// The egress proxy's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressVerdict {
    Allowed,
    Refused,
}

/// Why an egress request was refused. `none` on an allowed line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressReason {
    None,
    /// The turn's tier does not admit the host (or anything, on `closed`).
    Tier,
    /// The host resolved to a private, loopback, link-local or CGNAT address.
    PrivateAddress,
    /// A `send` to a host that is not on the send list.
    SendHost,
    /// The outbound content matched vault fingerprints.
    ContentMatch,
    /// The outbound content carried a canary.
    Canary,
    /// A `CONNECT` whose inner stream is not TLS, or whose client refused the proxy's
    /// certificate.
    Opaque,
    /// No or a bad `Proxy-Authorization`.
    Auth,
}

/// One egress request (or one refused `CONNECT`). HOST, never URL, headers or body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EgressLine {
    pub job_id: Token,
    /// `closed`, `allowlist` or `open-logged`.
    pub tier: Token,
    pub host: Token,
    pub port: u64,
    pub method_class: MethodClass,
    pub at_ms: u64,
    pub bytes_out: u64,
    pub bytes_in: u64,
    pub verdict: EgressVerdict,
    pub reason: EgressReason,
}

/// What a write-lock grant was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultOp {
    Write,
    Edit,
    Delete,
    Other,
}

/// One vault path a turn was granted the write lock for. The PATH is vault-relative and kept
/// only when every component is identifier-shaped ([`VaultPath`]); anything else is
/// `_invalid`, whole.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VaultWriteLine {
    pub job_id: Token,
    pub path: VaultPath,
    pub op: VaultOp,
    pub at_ms: u64,
}

/// One commit the bridge made.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommitLine {
    pub job_id: Token,
    /// The commit's hash (hex).
    pub hash: Token,
    pub files: u64,
    pub at_ms: u64,
}

/// A vault-relative path, kept only when it is a plain relative path whose every component is
/// a [`Token`] (no spaces, no `..`, no leading `/`), at most [`PATH_MAX`] bytes. Anything else
/// is [`Token::INVALID`], whole. A path names a file, never its contents; the shape rule is
/// what keeps a careless caller from writing a sentence here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct VaultPath(String);

/// The longest a [`VaultPath`] may be.
pub const PATH_MAX: usize = 240;

impl VaultPath {
    pub fn new(s: &str) -> VaultPath {
        let ok = !s.is_empty()
            && s.len() <= PATH_MAX
            && !s.starts_with('/')
            && s.split('/')
                .all(|c| !c.is_empty() && c != "." && c != ".." && Token::new(c).as_str() == c);
        VaultPath(if ok {
            s.to_string()
        } else {
            Token::INVALID.to_string()
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
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
    ModelCall {
        v: u32,
        #[serde(flatten)]
        line: ModelCallLine,
    },
    Egress {
        v: u32,
        #[serde(flatten)]
        line: EgressLine,
    },
    VaultWrite {
        v: u32,
        #[serde(flatten)]
        line: VaultWriteLine,
    },
    Commit {
        v: u32,
        #[serde(flatten)]
        line: CommitLine,
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

    pub fn model_call(line: ModelCallLine) -> ActivityLine {
        ActivityLine::ModelCall {
            v: ACTIVITY_SCHEMA,
            line,
        }
    }

    pub fn egress(line: EgressLine) -> ActivityLine {
        ActivityLine::Egress {
            v: ACTIVITY_SCHEMA,
            line,
        }
    }

    pub fn vault_write(line: VaultWriteLine) -> ActivityLine {
        ActivityLine::VaultWrite {
            v: ACTIVITY_SCHEMA,
            line,
        }
    }

    pub fn commit(line: CommitLine) -> ActivityLine {
        ActivityLine::Commit {
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
    tier: Option<Token>,
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
            tier: None,
        }
    }

    /// The turn's egress tier ([`crate::proxy::Egress::label`]), recorded on its line. Set
    /// once the tier is known; a turn whose tier is never set writes no `tier` field.
    pub fn set_tier(&mut self, tier: &str) {
        self.tier = Some(Token::new(tier));
    }

    /// [`TurnRecorder::set_tier`] as a builder.
    pub fn with_tier(mut self, tier: Option<&str>) -> TurnRecorder {
        self.tier = tier.map(Token::new);
        self
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
            tier: self.tier.clone(),
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

/// Woken after every commit line the bridge records, so a reader that indexes the vault (the
/// egress proxy's content index) can rebuild after the tree changed. Process-global: the
/// writers ([`record_commit`]) and the waiters need not share any other handle.
pub fn commit_notify() -> std::sync::Arc<tokio::sync::Notify> {
    static NOTIFY: std::sync::OnceLock<std::sync::Arc<tokio::sync::Notify>> =
        std::sync::OnceLock::new();
    NOTIFY
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Notify::new()))
        .clone()
}

/// Record the commit the bridge just made in `repo` (its `HEAD`): the hash and how many files
/// it touched, never a message, a path or a diff. Call only after a SUCCESSFUL `git commit`.
/// Wakes [`commit_notify`] whether or not a ledger is configured: the index rebuild does not
/// depend on persistence. A git that cannot answer is logged and the line is skipped.
pub async fn record_commit(ledger: Option<&ActivityLedger>, repo: &Path, job_id: &str) {
    if let Some(ledger) = ledger {
        match head_commit(repo).await {
            Ok((hash, files)) => ledger.record(&ActivityLine::commit(CommitLine {
                job_id: Token::new(job_id),
                hash: Token::new(&hash),
                files,
                at_ms: now_ms(),
            })),
            Err(e) => eprintln!("activity ledger: could not read the commit just made: {e}"),
        }
    }
    commit_notify().notify_waiters();
}

/// `HEAD`'s hash and the number of files it changed.
async fn head_commit(repo: &Path) -> Result<(String, u64), String> {
    let git = |args: &'static [&'static str]| {
        let mut c = tokio::process::Command::new("git");
        c.args(args).current_dir(repo);
        c
    };
    let out = git(&["rev-parse", "HEAD"])
        .output()
        .await
        .map_err(|e| format!("git rev-parse: {e}"))?;
    if !out.status.success() {
        return Err("git rev-parse HEAD failed".into());
    }
    let hash = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let out = git(&[
        "diff-tree",
        "--no-commit-id",
        "--name-only",
        "-r",
        "--root",
        "HEAD",
    ])
    .output()
    .await
    .map_err(|e| format!("git diff-tree: {e}"))?;
    if !out.status.success() {
        return Err("git diff-tree HEAD failed".into());
    }
    let files = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .count() as u64;
    Ok((hash, files))
}

/// `abs` made relative to the first of `roots` it lies under, as a [`VaultPath`]. The roots
/// are tried as given and canonicalised (a lock path is resolved, so `/var` may arrive as
/// `/private/var`). A path under none of them is `_invalid`: the line is still written, so a
/// write outside the vault is visible as one, but its path is not kept.
pub fn vault_relative(roots: &[&Path], abs: &Path) -> VaultPath {
    for root in roots {
        let canon = std::fs::canonicalize(root).ok();
        for r in std::iter::once(*root).chain(canon.as_deref()) {
            if let Ok(rel) = abs.strip_prefix(r) {
                return match rel.to_str() {
                    Some(s) => VaultPath::new(s),
                    None => VaultPath::new(""),
                };
            }
        }
    }
    VaultPath::new("")
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
    /// argument object, a request body, a URL with a path and a query, a relative escape, an
    /// absolute path. None of it survives as content: every string in every serialised line is
    /// identifier-shaped, every enum field is one of its fixed words, and a path is `_invalid`.
    #[test]
    fn no_field_of_any_kind_can_carry_content() {
        let hostile = [
            r#"{"query":"my private mail","to":"someone@example.com"}"#,
            "GET /api/states/lock.front_door?token=abc HTTP/1.1",
            "https://example.com/private/path?q=secret words",
            "the body of a message, with spaces and a newline\nsecond line",
            "../../etc/passwd",
            "/srv/vault/Inbox/private note.md",
            "Inbox/a note with spaces.md",
        ];
        // The fixed words each enum field may hold, and nothing else.
        let words: &[(&str, &[&str])] = &[
            (
                "kind",
                &[
                    "turn",
                    "tool_call",
                    "model_call",
                    "egress",
                    "vault_write",
                    "commit",
                ],
            ),
            (
                "outcome",
                &["ok", "failed", "cancelled", "refused", "truncated"],
            ),
            ("method_class", &["read", "send", "none"]),
            ("verdict", &["allowed", "refused"]),
            (
                "reason",
                &[
                    "none",
                    "tier",
                    "private_address",
                    "send_host",
                    "content_match",
                    "canary",
                    "opaque",
                    "auth",
                ],
            ),
            ("op", &["write", "edit", "delete", "other"]),
        ];
        let mut kinds_seen = std::collections::BTreeSet::new();
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
                    tier: Some(Token::new(h)),
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
                ActivityLine::model_call(ModelCallLine {
                    job_id: Token::new(h),
                    route: Token::new(h),
                    model: Token::new(h),
                    at_ms: 1,
                    duration_ms: 2,
                    status: 200,
                    input_tokens: 3,
                    output_tokens: 4,
                    cache_write_tokens: 5,
                    cache_read_tokens: 6,
                    cost_micros: 7,
                }),
                ActivityLine::egress(EgressLine {
                    job_id: Token::new(h),
                    tier: Token::new(h),
                    host: Token::new(h),
                    port: 443,
                    method_class: MethodClass::Send,
                    at_ms: 1,
                    bytes_out: 2,
                    bytes_in: 3,
                    verdict: EgressVerdict::Refused,
                    reason: EgressReason::ContentMatch,
                }),
                ActivityLine::vault_write(VaultWriteLine {
                    job_id: Token::new(h),
                    path: VaultPath::new(h),
                    op: VaultOp::Edit,
                    at_ms: 1,
                }),
                ActivityLine::commit(CommitLine {
                    job_id: Token::new(h),
                    hash: Token::new(h),
                    files: 2,
                    at_ms: 1,
                }),
            ];
            for line in lines {
                let v = serde_json::to_value(&line).unwrap();
                let obj = v.as_object().unwrap();
                assert_eq!(obj["v"], ACTIVITY_SCHEMA);
                kinds_seen.insert(obj["kind"].as_str().unwrap().to_string());
                for (k, val) in obj {
                    match val {
                        serde_json::Value::Number(_) => {}
                        serde_json::Value::String(s) => {
                            match words.iter().find(|(name, _)| name == k) {
                                Some((_, allowed)) => assert!(
                                    allowed.contains(&s.as_str()),
                                    "enum field {k} carries {s:?}"
                                ),
                                None => {
                                    assert_eq!(s, Token::INVALID, "field {k} carries {s:?}")
                                }
                            }
                        }
                        other => panic!("field {k} is neither a number nor a token: {other}"),
                    }
                }
            }
        }
        assert_eq!(
            kinds_seen.into_iter().collect::<Vec<_>>(),
            [
                "commit",
                "egress",
                "model_call",
                "tool_call",
                "turn",
                "vault_write"
            ]
        );
    }

    #[test]
    fn a_plain_vault_path_survives_and_anything_else_is_invalid_whole() {
        for ok in [
            "Inbox/2026-11-11-vault-lint.md",
            "diet-logs/food-log.csv",
            "Strands/Family.md",
            "Today.md",
        ] {
            assert_eq!(VaultPath::new(ok).as_str(), ok);
        }
        for bad in [
            "../x",
            "/abs",
            "/abs/Inbox/x.md",
            "a b/c.md",
            "Inbox//x.md",
            "Inbox/./x.md",
            "Inbox/../x.md",
            "Inbox/",
            "",
            "Inbox\\x.md",
        ] {
            assert_eq!(VaultPath::new(bad).as_str(), Token::INVALID, "{bad:?}");
        }
        let long = format!("{}/x.md", "a".repeat(PATH_MAX));
        assert_eq!(VaultPath::new(&long).as_str(), Token::INVALID);
    }

    #[test]
    fn a_path_is_made_vault_relative_or_invalid() {
        let vault = tmp();
        let notes = vault.join("vault");
        std::fs::create_dir_all(notes.join("Inbox")).unwrap();
        let roots = [notes.as_path(), vault.as_path()];
        assert_eq!(
            vault_relative(&roots, &notes.join("Inbox/2026-11-11-vault-lint.md")).as_str(),
            "Inbox/2026-11-11-vault-lint.md"
        );
        assert_eq!(
            vault_relative(&roots, &vault.join("diet-logs/food-log.csv")).as_str(),
            "diet-logs/food-log.csv"
        );
        // Resolved spelling of the root (macOS `/private/var`) is recognised too.
        let canon = std::fs::canonicalize(&notes).unwrap();
        assert_eq!(
            vault_relative(&roots, &canon.join("Inbox/x.md")).as_str(),
            "Inbox/x.md"
        );
        assert_eq!(
            vault_relative(&roots, Path::new("/etc/passwd")).as_str(),
            Token::INVALID
        );
        assert_eq!(
            vault_relative(&roots, &notes.join("Inbox/private note.md")).as_str(),
            Token::INVALID
        );
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_turn_line_carries_its_tier_only_when_it_has_one() {
        let mut line = TurnLine {
            job_id: Token::new("j1"),
            harness: Token::new("claude-code"),
            model: Token::new("opus"),
            capability: Token::new("write"),
            origin: Token::new("phone"),
            start_ms: 1,
            end_ms: 2,
            outcome: TurnOutcome::Ok,
            tier: None,
        };
        let v = serde_json::to_value(ActivityLine::turn(line.clone())).unwrap();
        assert!(v.get("tier").is_none());
        line.tier = Some(Token::new("allowlist"));
        let v = serde_json::to_value(ActivityLine::turn(line)).unwrap();
        assert_eq!(v["tier"], "allowlist");
    }

    #[tokio::test]
    async fn a_commit_is_recorded_with_hash_and_file_count_and_wakes_the_index() {
        let repo = tmp();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.email=t@example.invalid", "-c", "user.name=t"])
                .args(args)
                .output()
                .expect("git runs")
        };
        if git(&["init", "-q", "."]).status.code() != Some(0) {
            eprintln!("skipping: no usable git");
            return;
        }
        std::fs::write(repo.join("a.md"), "a").unwrap();
        std::fs::write(repo.join("b.md"), "b").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "two files"]);
        let state = tmp();
        let ledger = ActivityLedger::new(&state);
        let notify = commit_notify();
        let woken = notify.notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        record_commit(Some(&ledger), &repo, "diet-pipeline").await;
        tokio::time::timeout(std::time::Duration::from_secs(1), woken)
            .await
            .expect("the commit notify fired");
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        let text = std::fs::read_to_string(ledger.dir().join(format!("{day}.jsonl"))).unwrap();
        let v: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(v["kind"], "commit");
        assert_eq!(v["job_id"], "diet-pipeline");
        assert_eq!(v["files"], 2);
        assert_eq!(v["hash"].as_str().unwrap().len(), 40);
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&state);
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
            tier: None,
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
