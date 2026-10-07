//! Suite + task schema and the load-bearing vault-readonly allowlist check.
//!
//! A suite is a JSON file (see `eval/README.md` for the documented schema and a
//! full example task). Tasks are hermetic: a `fixture` task runs in a fresh temp
//! dir populated from its inline `fixture_files`; a `vault-readonly` task runs
//! against the real vault (`$JESSE_VAULT`, else `~/vault`) and MUST be restricted
//! to read tools only — enforced by [`Task::validate`].

use jesse_agent::{Level, PersonaPack};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// A full eval suite: a name plus an ordered list of tasks.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Suite {
    pub name: String,
    /// How many times each task runs; a task passes only when EVERY run passes (pass^k).
    /// Absent means 1, which is every suite written before `workflows-v1`. `--runs`
    /// overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<u32>,
    pub tasks: Vec<Task>,
}

/// The harness ids a task may name in `harnesses`, as the bridge's model registry spells them.
pub const KNOWN_HARNESSES: &[&str] = &["claude-code", "codex", "direct"];

/// Where a task runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Workspace {
    /// A fresh temp dir populated from `fixture_files` before the run. Hermetic.
    Fixture,
    /// The real vault (`$JESSE_VAULT`, else `~/vault`), read-only. Allowlist is hard-capped
    /// to read tools by [`Task::validate`] so an eval run can never mutate it.
    VaultReadonly,
}

/// One assertion. A task passes iff every assertion passes.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Assertion {
    /// Regex must match somewhere in the final answer.
    AnswerMatches { pattern: String },
    /// Regex must NOT match anywhere in the final answer.
    AnswerExcludes { pattern: String },
    /// Every segment of the final answer that matches `pattern` must ALSO match
    /// `qualifier`; an answer that never mentions `pattern` passes.
    ///
    /// This is how a suite says "X may appear, but only as Y" — the shape
    /// `answer_excludes` cannot express, because the ideal answer to a decoy or a
    /// finished-item trap NAMES the trap while disowning it. A segment is a line, or a
    /// sentence ended by `.`, `;`, `!` or `?` followed by whitespace; the whitespace
    /// condition keeps a version number like `3.1` in one piece. See
    /// `eval/suites/README.md` for which of the two assertions a situation calls for.
    AnswerMentionsOnlyWith { pattern: String, qualifier: String },
    /// A file in the task workspace must have exactly this content.
    FileEquals { path: String, content: String },
    /// Regex must match somewhere in a workspace file's content.
    ///
    /// The file is `path`, or (when `path` is empty) every file directly inside `dir` whose
    /// NAME matches `name_pattern`; the selector form passes when ANY selected file matches,
    /// and fails when none is selected. It exists for a file whose name the turn chooses (a
    /// draft named `YYYY-MM-DD-HHMM-…`), which no fixed path can name in advance.
    FileMatches {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        path: String,
        pattern: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name_pattern: Option<String>,
    },
    /// A workspace path must exist (file or directory). Same selector as `file_matches`:
    /// `path`, or at least one entry of `dir` whose name matches `name_pattern`.
    FileExists {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name_pattern: Option<String>,
    },
    /// A workspace path must NOT exist. With the selector form: no entry of `dir` may have a
    /// name matching `name_pattern` (a missing `dir` passes, since nothing is in it).
    FileAbsent {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name_pattern: Option<String>,
    },
    /// The LAST data row of a CSV file, read with a real RFC 4180 reader (quoted commas and
    /// doubled quotes are one cell, never a column shift). Every `columns` entry names a
    /// header and a regex the cell must match IN FULL (anchored, so `Banana` is not
    /// satisfied by `Banana bread`; spell `(?i).*banana.*` for a substring). `row_count`,
    /// when set, is the exact number of data rows the file must hold, which is how a suite
    /// says "updated in place, not appended".
    CsvLastRow {
        path: String,
        columns: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        row_count: Option<usize>,
    },
    /// The subject and body of the workspace repository's HEAD commit must match `pattern`.
    /// Read through a constant `git` argv, never a shell.
    GitHeadMessageMatches {
        pattern: String,
        /// The repository, relative to the workspace. Defaults to the workspace itself.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
    },
    /// `path` must differ between the seed commit the runner recorded (`refs/eval/seed`)
    /// and HEAD, i.e. the turn COMMITTED a change to it. With `committed: false` the
    /// working tree is compared instead, which also counts an uncommitted edit.
    GitPathChangedSince {
        path: String,
        #[serde(default = "default_true")]
        committed: bool,
    },
    /// A value inside a generated `.js` data file that starts with an assignment
    /// (`window.DIET_TODAY = { … };`). Leading comments and everything up to the first `=`
    /// are stripped, a trailing `;` dropped, and the rest parsed as JSON5 (which is what a JS
    /// object literal with bare keys and trailing commas is). `pointer` is an RFC 6901 JSON
    /// pointer (`/date`, `/meals/0/items/1/item`); the value there must EQUAL `value`.
    JsonPathEquals {
        path: String,
        pointer: String,
        value: serde_json::Value,
    },
    /// One of the CLOSED table of diet validators, run with `node` in the workspace, must exit
    /// zero. Never a free command: `validator` is an enum and `day` is checked to be a date
    /// before it reaches the argv.
    ProcessExitZero {
        validator: Validator,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        day: Option<String>,
    },
    /// Total tool-call count must be <= this ceiling.
    MaxToolCalls { max: u32 },
    /// A numeric value — capture group 1 of `pattern`, parsed as an f64 — must
    /// fall within the inclusive band `[min, max]`. When `path` is set the value
    /// is captured from that workspace file; otherwise from the final answer.
    /// This is the mechanical macro-band check (e.g. logged Calories in range),
    /// replacing brittle regex-alternation of every acceptable number.
    NumberInRange {
        #[serde(default)]
        path: Option<String>,
        pattern: String,
        min: f64,
        max: f64,
    },
    /// Two numbers must agree within `tolerance`: capture group 1 of
    /// `file_pattern` from the workspace file at `path`, and capture group 1 of
    /// `answer_pattern` from the final answer. Passes iff both parse and their
    /// absolute difference is `<= tolerance` (default `0.0` = exact). This is the
    /// mirror-vs-CSV consistency check — the emitted `JESSE_MEAL_LOG` macro must
    /// equal the appended row's macro.
    NumbersConsistent {
        path: String,
        file_pattern: String,
        answer_pattern: String,
        #[serde(default)]
        tolerance: f64,
    },
    /// A terminal `result` line must have arrived at all.
    Completed,
    /// The final answer must break NOTHING in the task's [`PersonaPack`] — the style
    /// checker (`jesse_agent::persona::check`) reports at most `max_hits` findings.
    ///
    /// The pack comes from the TASK, not from the assertion, so the prose the model was
    /// asked to write in and the rules it is graded against cannot drift apart: one pack is
    /// rendered into the system prefix and handed to the checker. A task with no `persona`
    /// fails this assertion with a message saying so rather than passing vacuously.
    StyleClean {
        /// Findings tolerated. `0` — the default — is the useful setting; a non-zero
        /// ceiling is for a task deliberately grading "mostly clean".
        #[serde(default)]
        max_hits: usize,
    },
    /// Every one of these tool names must appear in the transcript's tool calls.
    ToolsInclude { names: Vec<String> },
    /// NONE of these tool names may appear in the transcript's tool calls.
    ToolsExclude { names: Vec<String> },
}

fn default_true() -> bool {
    true
}

/// The closed table `process_exit_zero` may run. Two entries, both the vault's own diet
/// validators, vendored into the fixture vault at `vault/<script>`; the argv is
/// `node vault/<script> [--day <YYYY-MM-DD>]`, built in `crate::assertions` and nowhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Validator {
    /// `vault/validate-diet-today.js`: the derived day file is well formed.
    ValidateDietToday,
    /// `vault/verify-diet-consistency.js`: the day file agrees with the three CSVs.
    VerifyDietConsistency,
}

impl Validator {
    /// The script path, relative to the workspace.
    pub fn script(self) -> &'static str {
        match self {
            Validator::ValidateDietToday => "vault/validate-diet-today.js",
            Validator::VerifyDietConsistency => "vault/verify-diet-consistency.js",
        }
    }
}

/// A bare git repository the runner builds OUTSIDE the workspace before the task runs, so a
/// task can clone it as if it were a remote. `name` is `<host>/<owner>/<repo>`; the prompt
/// and the assertions reach it through `{{remote_url:NAME}}`, `{{remote_head:NAME}}` and
/// `{{remote_head_short:NAME}}`, substituted by the runner. Commits are made with a fixed
/// author, committer and date, so a head SHA is the same on every machine.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RemoteSpec {
    pub name: String,
    /// The branch the commits land on. Defaults to `main`.
    #[serde(default = "default_branch")]
    pub branch: String,
    pub commits: Vec<RemoteCommit>,
}

/// One commit of a [`RemoteSpec`]: files written (whole) on top of the previous commit.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RemoteCommit {
    pub message: String,
    pub files: BTreeMap<String, String>,
}

fn default_branch() -> String {
    "main".to_string()
}

/// A single eval task.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Task {
    pub id: String,
    /// Task class, used to group the scorecard (e.g. `titles`, `extraction`).
    pub class: String,
    /// The prompt handed to `claude -p`.
    pub prompt: String,
    pub workspace: Workspace,
    /// Tools passed to `--allowedTools` (comma-joined). Empty = no tools.
    ///
    /// These are the CLI's names, and they stay the CLI's names: the direct driver maps
    /// them onto its own manifest by the table in `eval/README.md` rather than the suite
    /// carrying two allowlists that could disagree.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// How much the turn is trusted with, for a driver that has levels.
    ///
    /// `None` takes the default from the workspace — `read` for `vault-readonly`, `write`
    /// for `fixture` — which is what the two workspaces already mean. Spelling it out is
    /// for the task that wants LESS than its workspace's default (a refusal task granted
    /// only `read`), and `vault-readonly` + `write` is refused by [`Task::validate`]
    /// alongside the tool allowlist, for the same reason.
    #[serde(default)]
    pub level: Option<Level>,
    /// Extra system-prefix text, as fixture blocks, ahead of the persona.
    ///
    /// The direct driver passes these as [`jesse_agent::SystemBlock`]s. The CLI takes no
    /// system prefix on the flags this harness uses, so its driver prepends the same text
    /// to the prompt — the model sees the same instructions either way, which is what makes
    /// a suite carrying `system` runnable on both drivers.
    #[serde(default)]
    pub system: Vec<String>,
    /// The persona this task's answer is written under AND graded against.
    ///
    /// ONE pack, two uses: the direct driver renders it into the system prefix with
    /// `jesse_agent::render_persona`, and the `style_clean` assertion checks the answer
    /// against the same value. A suite that carried the rendered prose in `system` and the
    /// rules in the assertion would be carrying the same pack twice, in two spellings.
    #[serde(default)]
    pub persona: Option<PersonaPack>,
    /// For `fixture` workspaces: files written into the temp dir before the run.
    #[serde(default)]
    pub fixture_files: BTreeMap<String, String>,
    /// Judged tasks have their final answer saved as an artifact for `judge`.
    #[serde(default)]
    pub judged: bool,
    /// Grading rubric text, required for judged tasks; presented to the judge.
    #[serde(default)]
    pub rubric: Option<String>,
    pub assertions: Vec<Assertion>,
    /// The harnesses this task applies to (`claude-code`, `codex`, `direct`). Empty means
    /// every harness. A driver that knows its harness SKIPS a task that does not name it, and
    /// the scorecard reports it as a skip, never a fail. Drivers with no harness of their own
    /// (`claude-cli`, `direct`) run every task, as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub harnesses: Vec<String>,
    /// A directory under the suite's `fixtures/` root (`<suite dir>/../fixtures/<this>`)
    /// copied into the workspace BEFORE `fixture_files`, which then overlay it. This is how
    /// twenty tasks share one realistic vault without twenty copies of it in the suite file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixture_base: Option<String>,
    /// Make the workspace a git repository with one seed commit of the fixture, recorded at
    /// `refs/eval/seed`, before the turn. The `git_*` assertions need it.
    #[serde(default)]
    pub git_init: bool,
    /// Bare repositories the task clones from. See [`RemoteSpec`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remotes: Vec<RemoteSpec>,
    /// Further turns in the SAME conversation, sent in order after `prompt` has finished.
    /// The final answer graded is the last turn's. Only a driver that holds a conversation
    /// (the `bridge` driver) can run one; the others fail it as a harness error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub followups: Vec<String>,
    /// `ask` (the default) or `tell`, the phone's two modes, for a driver that has them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The device health block the phone attaches, sent verbatim as `health_context`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_context: Option<String>,
}

/// The only tools a `vault-readonly` task may use. Nothing that can write.
pub const VAULT_ALLOWED_TOOLS: &[&str] = &[
    "Read",
    "Grep",
    "Glob",
    "mcp__qmd__query",
    "mcp__qmd__get",
    "mcp__qmd__multi_get",
    "mcp__qmd__status",
];

/// Home directory from `$HOME`. Used to derive the vault path at runtime so no
/// personal absolute path is ever committed (repo guard R5).
pub fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".to_string()))
}

/// The vault working directory: `$JESSE_VAULT` when set, else `~/vault`. Mirrors
/// the bridge's `JESSE_VAULT` resolution so an eval points at the same vault the
/// bridge serves, with no personal absolute path committed (repo guard R5).
pub fn vault_dir() -> PathBuf {
    match std::env::var("JESSE_VAULT") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => home_dir().join("vault"),
    }
}

impl Task {
    /// The comma-joined `--allowedTools` value for this task.
    pub fn allowed_tools_csv(&self) -> String {
        self.allowed_tools.join(",")
    }

    /// The level this task runs at, defaulted from its workspace.
    ///
    /// `write` for a fixture (a hermetic temp dir, which is the point of one) and `read`
    /// for the vault (which is the whole posture of `vault-readonly`). Naming a level in
    /// the task overrides this, EXCEPT that `vault-readonly` + `write` is refused — see
    /// [`Task::validate`].
    pub fn level(&self) -> Level {
        self.level.unwrap_or(match self.workspace {
            Workspace::Fixture => Level::Write,
            Workspace::VaultReadonly => Level::Read,
        })
    }

    /// Load-bearing safety check. A `vault-readonly` task must declare only
    /// read tools and may not ask for `level: write`; any other tool (Write, Edit,
    /// any Bash, …) is refused so an eval run can never modify the vault. Also
    /// requires judged tasks to carry a rubric, and `style_clean` to have a pack to
    /// check against. Returns `Err` with a human-readable reason on any violation.
    pub fn validate(&self) -> Result<(), String> {
        if self.workspace == Workspace::VaultReadonly {
            // THE SAME REFUSAL, ONE RUNG UP. The allowlist below names the CLI's tools; a
            // level names what the DIRECT driver's tool set is built with, and a
            // `vault-readonly` task at `write` would be built with `vault_write` no matter
            // how empty its `allowed_tools` was. Both spellings of "this task may modify
            // the vault" are refused in the same place, before anything runs.
            if self.level == Some(Level::Write) {
                return Err(format!(
                    "task '{}' is vault-readonly but asks for level: write, \
                     which would build a tool set that can modify the vault",
                    self.id
                ));
            }
            for tool in &self.allowed_tools {
                if !VAULT_ALLOWED_TOOLS.contains(&tool.as_str()) {
                    return Err(format!(
                        "task '{}' is vault-readonly but its allowlist contains '{}', \
                         which is not a read-only tool. Allowed: {}",
                        self.id,
                        tool,
                        VAULT_ALLOWED_TOOLS.join(", ")
                    ));
                }
            }
        }
        for h in &self.harnesses {
            if !KNOWN_HARNESSES.contains(&h.as_str()) {
                return Err(format!(
                    "task '{}' names unknown harness '{h}' (known: {})",
                    self.id,
                    KNOWN_HARNESSES.join(", ")
                ));
            }
        }
        if let Some(m) = self.mode.as_deref() {
            if m != "ask" && m != "tell" {
                return Err(format!(
                    "task '{}' has mode '{m}'; the phone has only `ask` and `tell`",
                    self.id
                ));
            }
        }
        if let Some(base) = &self.fixture_base {
            if !is_plain_relative(base) {
                return Err(format!(
                    "task '{}' has fixture_base '{base}', which is not a plain relative path",
                    self.id
                ));
            }
        }
        for r in &self.remotes {
            let parts: Vec<&str> = r.name.split('/').collect();
            let safe = |s: &str| {
                !s.is_empty()
                    && s != "."
                    && s != ".."
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            };
            if parts.len() != 3 || !parts.iter().all(|p| safe(p)) {
                return Err(format!(
                    "task '{}' has remote '{}'; a remote is named <host>/<owner>/<repo>",
                    self.id, r.name
                ));
            }
            if r.commits.is_empty() {
                return Err(format!(
                    "task '{}' has remote '{}' with no commits",
                    self.id, r.name
                ));
            }
        }
        for a in &self.assertions {
            if let Assertion::ProcessExitZero { day: Some(d), .. } = a {
                if !is_iso_date(d) {
                    return Err(format!(
                        "task '{}' passes day '{d}' to a validator; it must be YYYY-MM-DD",
                        self.id
                    ));
                }
            }
            let on_git = matches!(
                a,
                Assertion::GitHeadMessageMatches { .. } | Assertion::GitPathChangedSince { .. }
            );
            if on_git && !self.git_init {
                return Err(format!(
                    "task '{}' asserts on git but does not set git_init",
                    self.id
                ));
            }
        }
        if self.judged && self.rubric.as_deref().unwrap_or("").trim().is_empty() {
            return Err(format!(
                "task '{}' is judged but has no rubric text",
                self.id
            ));
        }
        if self.persona.is_none()
            && self
                .assertions
                .iter()
                .any(|a| matches!(a, Assertion::StyleClean { .. }))
        {
            return Err(format!(
                "task '{}' asserts style_clean but declares no `persona` pack to check against",
                self.id
            ));
        }
        Ok(())
    }
}

/// `YYYY-MM-DD`, digits in the right places. Not a calendar check: its job is to keep
/// anything that is not a date out of a validator's argv.
pub fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

/// A relative path with no `..`, no root and no empty component.
pub fn is_plain_relative(p: &str) -> bool {
    use std::path::Component;
    let path = std::path::Path::new(p);
    !p.is_empty()
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

impl Suite {
    /// Parse a suite from JSON bytes and validate every task.
    pub fn from_json(bytes: &[u8]) -> Result<Suite, String> {
        let suite: Suite =
            serde_json::from_slice(bytes).map_err(|e| format!("invalid suite JSON: {e}"))?;
        for task in &suite.tasks {
            task.validate()?;
        }
        Ok(suite)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_with(workspace: Workspace, tools: &[&str]) -> Task {
        Task {
            id: "t".into(),
            class: "c".into(),
            prompt: "p".into(),
            workspace,
            allowed_tools: tools.iter().map(|s| s.to_string()).collect(),
            level: None,
            system: vec![],
            persona: None,
            fixture_files: BTreeMap::new(),
            judged: false,
            rubric: None,
            assertions: vec![],
            harnesses: vec![],
            fixture_base: None,
            git_init: false,
            remotes: vec![],
            followups: vec![],
            mode: None,
            health_context: None,
        }
    }

    #[test]
    fn an_unknown_harness_is_refused_and_the_three_known_ones_load() {
        let mut t = task_with(Workspace::Fixture, &[]);
        t.harnesses = vec!["claude-code".into(), "codex".into(), "direct".into()];
        assert!(t.validate().is_ok());
        t.harnesses = vec!["claude".into()];
        assert!(t.validate().unwrap_err().contains("unknown harness"));
    }

    #[test]
    fn a_validator_day_that_is_not_a_date_is_refused() {
        let mut t = task_with(Workspace::Fixture, &[]);
        t.assertions = vec![Assertion::ProcessExitZero {
            validator: Validator::ValidateDietToday,
            day: Some("2026-10-06; rm -rf /".into()),
        }];
        assert!(t.validate().unwrap_err().contains("YYYY-MM-DD"));
        t.assertions = vec![Assertion::ProcessExitZero {
            validator: Validator::ValidateDietToday,
            day: Some("2026-10-06".into()),
        }];
        assert!(t.validate().is_ok());
    }

    #[test]
    fn the_validator_table_is_closed_and_unknown_names_do_not_parse() {
        let ok: Result<Assertion, _> = serde_json::from_value(serde_json::json!(
            {"type": "process_exit_zero", "validator": "verify-diet-consistency"}
        ));
        assert!(ok.is_ok());
        let bad: Result<Assertion, _> = serde_json::from_value(serde_json::json!(
            {"type": "process_exit_zero", "validator": "bash"}
        ));
        assert!(bad.is_err(), "a free command must not deserialize");
    }

    #[test]
    fn git_assertions_need_git_init() {
        let mut t = task_with(Workspace::Fixture, &[]);
        t.assertions = vec![Assertion::GitHeadMessageMatches {
            pattern: "x".into(),
            repo: None,
        }];
        assert!(t.validate().unwrap_err().contains("git_init"));
        t.git_init = true;
        assert!(t.validate().is_ok());
    }

    #[test]
    fn a_remote_must_be_host_owner_repo_and_a_base_must_stay_relative() {
        let mut t = task_with(Workspace::Fixture, &[]);
        t.remotes = vec![RemoteSpec {
            name: "github.com/acme/../x".into(),
            branch: "main".into(),
            commits: vec![RemoteCommit {
                message: "m".into(),
                files: BTreeMap::new(),
            }],
        }];
        assert!(t.validate().is_err());
        t.remotes[0].name = "github.com/acme/widget".into();
        assert!(t.validate().is_ok());
        t.fixture_base = Some("../../etc".into());
        assert!(t.validate().is_err());
        t.fixture_base = Some("workflows-v1/base".into());
        assert!(t.validate().is_ok());
    }

    #[test]
    fn a_mode_other_than_ask_or_tell_is_refused() {
        let mut t = task_with(Workspace::Fixture, &[]);
        t.mode = Some("tell".into());
        assert!(t.validate().is_ok());
        t.mode = Some("shout".into());
        assert!(t.validate().is_err());
    }

    #[test]
    fn vault_allows_read_tools() {
        let t = task_with(
            Workspace::VaultReadonly,
            &["Read", "Grep", "Glob", "mcp__qmd__query", "mcp__qmd__get"],
        );
        assert!(t.validate().is_ok());
    }

    #[test]
    fn vault_refuses_write() {
        let t = task_with(Workspace::VaultReadonly, &["Read", "Write"]);
        let err = t.validate().unwrap_err();
        assert!(
            err.contains("Write"),
            "error should name the offending tool: {err}"
        );
    }

    #[test]
    fn vault_refuses_edit() {
        let t = task_with(Workspace::VaultReadonly, &["Edit"]);
        assert!(t.validate().is_err());
    }

    #[test]
    fn vault_refuses_any_bash() {
        // Even a "harmless"-looking scoped Bash is refused: the check is an
        // allowlist, not a denylist.
        let t = task_with(Workspace::VaultReadonly, &["Read", "Bash(ls:*)"]);
        let err = t.validate().unwrap_err();
        assert!(err.contains("Bash(ls:*)"), "got: {err}");
    }

    #[test]
    fn fixture_allows_anything() {
        // Fixture workspaces are hermetic temp dirs, so any tool is fine there.
        let t = task_with(Workspace::Fixture, &["Write", "Edit", "Bash"]);
        assert!(t.validate().is_ok());
    }

    #[test]
    fn vault_refuses_write_level_even_with_an_empty_allowlist() {
        // The allowlist is empty and every tool in it would have been legal — the refusal
        // is the LEVEL, which is what the direct driver builds its tool set from.
        let mut t = task_with(Workspace::VaultReadonly, &[]);
        t.level = Some(Level::Write);
        let err = t.validate().unwrap_err();
        assert!(err.contains("level: write"), "got: {err}");
    }

    #[test]
    fn vault_allows_an_explicit_read_level() {
        let mut t = task_with(Workspace::VaultReadonly, &["Read"]);
        t.level = Some(Level::Read);
        assert!(t.validate().is_ok());
        t.level = Some(Level::Basic);
        assert!(t.validate().is_ok());
    }

    #[test]
    fn the_level_defaults_to_what_the_workspace_means() {
        assert_eq!(task_with(Workspace::Fixture, &[]).level(), Level::Write);
        assert_eq!(
            task_with(Workspace::VaultReadonly, &[]).level(),
            Level::Read
        );
    }

    #[test]
    fn style_clean_without_a_pack_is_refused_at_load() {
        let mut t = task_with(Workspace::Fixture, &[]);
        t.assertions = vec![Assertion::StyleClean { max_hits: 0 }];
        let err = t.validate().unwrap_err();
        assert!(err.contains("persona"), "got: {err}");
        t.persona = Some(PersonaPack::default());
        assert!(t.validate().is_ok());
    }

    #[test]
    fn judged_requires_rubric() {
        let mut t = task_with(Workspace::Fixture, &[]);
        t.judged = true;
        assert!(t.validate().is_err());
        t.rubric = Some("grade for accuracy".into());
        assert!(t.validate().is_ok());
    }
}
