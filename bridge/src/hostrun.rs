//! **CODING AGENTS ON THE STUDIO** (`hostrun`) — long coding agent runs on this Mac, outside any
//! turn, with no clock.
//!
//! ---- WHAT IT DOES ----------------------------------------------------------------
//!
//! A run is started from the app (`POST /jesse/runs` with a strand item or a draft path), from a
//! chat turn (`devjob_start` with `host: "studio"`), or from a `kind = "devjob"` strand schedule
//! entry whose draft says `devjob-repo` and `devjob-host: studio`. A person's start needs neither
//! key: asking is the opt in ([`OptIn`]). All three go through [`HostRuns::create`]. The bridge gives
//! the run its own git worktree of a shared bare clone, its own branch and its own
//! `CARGO_TARGET_DIR`, then starts `jesse-runner` as **its own launchd job**
//! (`<ns>.jesse-run.<run_id>`, `<ns>` the bridge's own label's namespace). The runner runs `claude -p` in the worktree and writes the
//! stream, the exit and the agent's report into the run directory. A run outlives the bridge:
//! a restart or a deploy of the bridge does not touch it.
//!
//! The watcher ([`HostRuns::run_watcher`]), a background task owned by no turn, reads every run
//! directory every [`WATCH_INTERVAL`]: it writes each finished round's report into the Inbox
//! byte for byte, pushes, sends the strand one agent turn, notices a quiet or an interrupted
//! run (and gives an interrupted one a single automatic round), pauses a run the plan's usage
//! limit stopped and resumes it at the reset, starts queued runs when the caps, the disk and the
//! limit allow, and cleans up.
//!
//! ---- WHY NOT A CHAT TURN -----------------------------------------------------------
//!
//! A chat turn is one child tied to one request and keeps its run limit (`JESSE_TIMEOUT` under
//! `HARD_TIMEOUT_CEILING`). Long work belongs here, where **nothing ends a run because of
//! elapsed time**: a quiet run is reported (`stalled_since`), never killed.
//!
//! ---- RESTART SAFETY ------------------------------------------------------------------
//!
//! **ALL STATE IS ON DISK**, in `<root>/runs/<run_id>/run.json`, written atomically under a
//! `flock` that the bridge and the runner both take ([`update_run`]). Nothing about a run is
//! kept in memory between passes. A round's report is written once: its Inbox file names the
//! run and the round in its front matter ([`find_existing_report`]), and the round records the
//! path, so a bridge that dies between the two writes nothing again.
//!
//! ---- WHAT IT NEVER DOES ---------------------------------------------------------------
//!
//! It writes nothing under `Strands/` or `Projects/drafts/`; its only vault writes are the Inbox
//! reports, and strand bookkeeping is left to the two agent turns it sends. It deletes nothing
//! outside `root` except the DerivedData folders a run recorded as its own, and never a run
//! directory.

pub mod git;
pub mod runner;
pub mod stream;
pub mod sys;

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Serialize;

use crate::devjob::{valid_repo, valid_slug, Notifier};
use crate::strandjobs::{fm, DRAFTS_DIR};
use crate::strands::{parse_strand, resolve_target, StrandSection, ARCHIVE_SEGMENT, STRANDS_DIR};
use crate::*;

use self::sys::{Forge, Launcher, PrState};

// ---- CONSTANTS -----------------------------------------------------------------------

/// The default `[hostrun] root`, under the bridge user's home.
pub const DEFAULT_ROOT: &str = "jesse-runs";
pub const DEFAULT_MAX_CONCURRENT: usize = 3;
pub const DEFAULT_MAX_PER_REPO: usize = 1;
pub const DEFAULT_STALL_MINUTES: u64 = 60;
pub const DEFAULT_PERMISSION_MODE: &str = "bypassPermissions";
pub const DEFAULT_KEEP_DAYS: u64 = 7;
pub const DEFAULT_AUTO_RESUME_INTERRUPTED: bool = true;
pub const DEFAULT_MIN_FREE_GB: u64 = 100;
pub const DEFAULT_MAX_TOTAL_GB: u64 = 300;
/// How long a `needs_you` or `interrupted` run keeps its worktree.
pub const PAUSED_KEEP_DAYS: u64 = 30;
/// After this many days a finished run's `stream.jsonl` is gzipped.
pub const GZIP_AFTER_DAYS: u64 = 7;
/// SIGTERM, then SIGKILL this long after.
pub const CANCEL_GRACE_MS: u64 = 30_000;
/// A `running` run is only judged interrupted once its round is this old.
const INTERRUPT_SETTLE_MS: u64 = 10_000;
/// Every permission mode `claude --help` lists on 2.1.319.
pub const PERMISSION_MODES: [&str; 6] = [
    "acceptEdits",
    "auto",
    "bypassPermissions",
    "manual",
    "dontAsk",
    "plan",
];

/// How often the watcher reads the run directories when nothing wakes it.
pub const WATCH_INTERVAL: Duration = Duration::from_secs(15);
/// How often the cleanup runs.
pub const CLEANUP_EVERY_MS: u64 = 60 * 60 * 1000;
/// How often the reconcile runs (and once at start).
pub const RECONCILE_EVERY_MS: u64 = 24 * 60 * 60 * 1000;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
const GB: u64 = 1024 * 1024 * 1024;

/// Log lines a fallback report carries.
pub const FALLBACK_LOG_LINES: usize = 200;
/// Default and most lines `GET /jesse/runs/:id/log` serves.
pub const LOG_DEFAULT: usize = 40;
pub const LOG_MAX: usize = 500;
/// The longest branch slug taken from an item's text.
pub const BRANCH_TEXT_CHARS: usize = 40;

/// The first line of a report that asks for a decision.
pub const NEEDS_YOU_PREFIX: &str = "NEEDS YOU:";
/// The reply that resumes a run from the app, and the round a run paused for the usage limit
/// gets at its `resume_at`.
pub const CONTINUE_TEXT: &str = "Continue where you left off.";
/// The one automatic round an interrupted run gets (`[hostrun] auto_resume_interrupted`).
pub const RESTART_TEXT: &str =
    "Continue where you left off. The machine restarted during your previous round.";
/// [`Round::auto`] of the round a usage limit pause queues at its `resume_at`.
pub const AUTO_LIMIT: &str = "limit";
/// [`Round::auto`] of the one round an interrupted run gets by itself.
pub const AUTO_RESTART: &str = "restart";
/// A pause for the usage limit whose event names no reset waits this long, and so does each
/// retry that meets the limit again with no reset named.
pub const LIMIT_RETRY_MS: u64 = 60 * 60 * 1000;
/// The owner whose repositories get `[<run_id>]` on pull request titles by default.
pub const TITLE_TAG_OWNER: &str = "jeremyandrews";
/// The front matter key that sends a draft to the Studio.
pub const HOST_KEY: &str = "devjob-host";
/// The `origin` of the agent turns this module sends.
pub const STUDIO_RUN_ORIGIN: &str = "studio-run";
/// `closed_reason` when Jeremy closes a run and gives no reason.
pub const CLOSED_BY_JEREMY: &str = "closed by Jeremy";
/// `closed_reason` when the run's Drafts line is checked or sits under `### Done`.
pub const CLOSED_ITEM_TICKED: &str = "item ticked";
/// `closed_reason` when every report of the run has moved to `Inbox/archive/`.
pub const CLOSED_REPORT_ARCHIVED: &str = "report archived";
/// The longest reason a close takes.
pub const MAX_CLOSE_REASON_BYTES: usize = 500;
/// Where an archived Inbox report goes, under the notes directory.
pub const INBOX_ARCHIVE_DIR: &str = "Inbox/archive";

/// The environment a run may see, by name. Everything else (every bridge secret, the APNs key,
/// the gateway's credentials, the model tokens) is stripped. `JESSE_RUN_ID`, `JESSE_RUN_REPORT`
/// and `CARGO_TARGET_DIR` are set by the runner itself.
pub const ENV_ALLOWLIST: [&str; 20] = [
    "HOME",
    "PATH",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "SHELL",
    "TERM",
    "TZ",
    // git and gh over SSH, and gh's own config.
    "SSH_AUTH_SOCK",
    "GH_CONFIG_DIR",
    "GIT_CONFIG_GLOBAL",
    // cargo and rustup.
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    // xcodebuild, xcrun and the simulator.
    "DEVELOPER_DIR",
    "SDKROOT",
    "__CF_USER_TEXT_ENCODING",
];

// ---- SETTINGS ------------------------------------------------------------------------

/// `[hostrun.repos."owner/name"]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoCapToml {
    pub max_concurrent: Option<usize>,
    /// `KEY=value` lines added to the agent's environment for this repository's runs. The
    /// values are read by the runner when a round starts and go nowhere else.
    pub env_file: Option<PathBuf>,
    /// Whether the generated section asks for `[<run_id>]` at the start of the pull request
    /// title. Default: true for [`TITLE_TAG_OWNER`]'s repositories, false otherwise.
    pub pr_title_tag: Option<bool>,
}

/// `[hostrun]` in the bridge TOML.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRunToml {
    #[serde(default)]
    pub enabled: bool,
    pub root: Option<PathBuf>,
    pub max_concurrent: Option<usize>,
    pub max_per_repo: Option<usize>,
    #[serde(default)]
    pub repos: BTreeMap<String, RepoCapToml>,
    pub stall_minutes: Option<u64>,
    pub permission_mode: Option<String>,
    pub claude: Option<String>,
    pub keep_days: Option<u64>,
    pub min_free_gb: Option<u64>,
    pub max_total_gb: Option<u64>,
    /// The launchd label prefix of the runs. Default: from the bridge's own label.
    pub label_prefix: Option<String>,
    /// An `interrupted` run gets one automatic round. Default true.
    pub auto_resume_interrupted: Option<bool>,
}

/// Where the running [`HostRuns`] lives once `main` has built it.
#[derive(Clone, Default)]
pub struct HostRunSlot(pub Arc<OnceLock<Arc<HostRuns>>>);

impl std::fmt::Debug for HostRunSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HostRunSlot(running: {})", self.0.get().is_some())
    }
}

/// `[hostrun]`, resolved.
#[derive(Debug, Clone)]
pub struct HostRunSettings {
    pub enabled: bool,
    pub root: PathBuf,
    pub max_concurrent: usize,
    pub max_per_repo: usize,
    /// `owner/name` → its own cap. Keys are checked by [`HostRunSettings::problems`].
    pub repo_caps: BTreeMap<String, usize>,
    /// `owner/name` → its `env_file`, with `~` expanded.
    pub repo_env_files: BTreeMap<String, PathBuf>,
    /// `owner/name` → its `pr_title_tag`, where one is set.
    pub repo_title_tags: BTreeMap<String, bool>,
    /// Every key of `[hostrun.repos]`, for [`HostRunSettings::problems`].
    pub repo_keys: Vec<String>,
    pub auto_resume_interrupted: bool,
    pub stall_minutes: u64,
    pub permission_mode: String,
    /// The `claude` binary as configured (or the bridge's own `JESSE_CLAUDE_BIN`).
    pub claude: String,
    pub keep_days: u64,
    pub min_free_bytes: u64,
    pub max_total_bytes: u64,
    /// The bridge user's home.
    pub home: PathBuf,
    /// The runner binary: beside the bridge's own executable, else on `PATH`.
    pub runner: PathBuf,
    /// Xcode's DerivedData folder.
    pub derived_data_root: PathBuf,
    /// The clone URL prefix: `https://github.com/` (a test points it at a local folder).
    pub remote_base: String,
    /// `<ns>.jesse-run.`: a run's label is this and its id.
    pub label_prefix: String,
    pub service: HostRunSlot,
}

impl Default for HostRunSettings {
    fn default() -> Self {
        HostRunSettings::from_toml(None, "", "claude")
    }
}

fn expand_home(p: &Path, home: &str) -> PathBuf {
    match p.strip_prefix("~") {
        Ok(rest) if !home.is_empty() => Path::new(home).join(rest),
        _ => p.to_path_buf(),
    }
}

/// The runner binary beside the running executable, else the bare name.
fn default_runner() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("jesse-runner")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("jesse-runner"))
}

impl HostRunSettings {
    /// `claude_bin` is what the bridge runs for its own turns (`JESSE_CLAUDE_BIN`, else
    /// `claude`), the default for `[hostrun] claude`.
    pub fn from_toml(t: Option<HostRunToml>, home: &str, claude_bin: &str) -> HostRunSettings {
        let t = t.unwrap_or_default();
        let root = match t.root {
            Some(p) => expand_home(&p, home),
            None => Path::new(home).join(DEFAULT_ROOT),
        };
        HostRunSettings {
            enabled: t.enabled,
            root,
            max_concurrent: t.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT),
            max_per_repo: t.max_per_repo.unwrap_or(DEFAULT_MAX_PER_REPO),
            repo_caps: t
                .repos
                .iter()
                .filter_map(|(k, v)| v.max_concurrent.map(|c| (k.clone(), c)))
                .collect(),
            repo_env_files: t
                .repos
                .iter()
                .filter_map(|(k, v)| {
                    v.env_file
                        .as_ref()
                        .map(|p| (k.clone(), expand_home(p, home)))
                })
                .collect(),
            repo_title_tags: t
                .repos
                .iter()
                .filter_map(|(k, v)| v.pr_title_tag.map(|b| (k.clone(), b)))
                .collect(),
            repo_keys: t.repos.keys().cloned().collect(),
            auto_resume_interrupted: t
                .auto_resume_interrupted
                .unwrap_or(DEFAULT_AUTO_RESUME_INTERRUPTED),
            stall_minutes: t.stall_minutes.unwrap_or(DEFAULT_STALL_MINUTES),
            permission_mode: t
                .permission_mode
                .unwrap_or_else(|| DEFAULT_PERMISSION_MODE.to_string()),
            claude: t.claude.unwrap_or_else(|| claude_bin.to_string()),
            keep_days: t.keep_days.unwrap_or(DEFAULT_KEEP_DAYS),
            min_free_bytes: t
                .min_free_gb
                .unwrap_or(DEFAULT_MIN_FREE_GB)
                .saturating_mul(GB),
            max_total_bytes: t
                .max_total_gb
                .unwrap_or(DEFAULT_MAX_TOTAL_GB)
                .saturating_mul(GB),
            home: PathBuf::from(home),
            runner: default_runner(),
            derived_data_root: Path::new(home).join("Library/Developer/Xcode/DerivedData"),
            remote_base: "https://github.com/".to_string(),
            label_prefix: t.label_prefix.unwrap_or_else(|| {
                sys::label_prefix_from(std::env::var("XPC_SERVICE_NAME").ok().as_deref())
            }),
            service: HostRunSlot::default(),
        }
    }

    /// The running service, when `[hostrun] enabled` and startup built it.
    pub fn service(&self) -> Option<Arc<HostRuns>> {
        self.service.0.get().cloned()
    }

    /// The cap for one repository.
    pub fn repo_cap(&self, repo: &str) -> usize {
        self.repo_caps
            .get(repo)
            .copied()
            .unwrap_or(self.max_per_repo)
    }

    /// Whether the generated section asks for `[<run_id>]` on the pull request title.
    pub fn pr_title_tag(&self, repo: &str) -> bool {
        self.repo_title_tags.get(repo).copied().unwrap_or_else(|| {
            repo.split_once('/')
                .is_some_and(|(owner, _)| owner == TITLE_TAG_OWNER)
        })
    }

    /// The repository's `env_file`, if it has one.
    pub fn env_file(&self, repo: &str) -> Option<&Path> {
        self.repo_env_files.get(repo).map(PathBuf::as_path)
    }

    /// The `claude` binary, resolved to a file.
    pub fn claude_path(&self) -> Option<PathBuf> {
        sys::resolve_program(&self.claude, None)
    }

    /// The runner binary, resolved to a file.
    pub fn runner_path(&self) -> Option<PathBuf> {
        sys::resolve_program(&self.runner.to_string_lossy(), None)
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.root.join("runs")
    }

    pub fn run_dir(&self, run_id: &str) -> PathBuf {
        self.runs_dir().join(run_id)
    }

    /// The launchd label of one run.
    pub fn label(&self, run_id: &str) -> String {
        format!("{}{run_id}", self.label_prefix)
    }

    /// The clone URL of `owner/name`.
    pub fn repo_url(&self, repo: &str) -> String {
        format!("{}{repo}.git", self.remote_base)
    }

    /// Why this table may not boot, by name. Empty when it is off: nothing is touched then.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.enabled {
            return out;
        }
        match std::fs::create_dir_all(&self.root) {
            Err(e) => out.push(format!(
                "[hostrun] root {} is unwritable: {e}",
                self.root.display()
            )),
            Ok(()) => {
                let probe = self
                    .root
                    .join(format!(".write-probe-{}", std::process::id()));
                match std::fs::write(&probe, b"") {
                    Ok(()) => {
                        let _ = std::fs::remove_file(&probe);
                    }
                    Err(e) => out.push(format!(
                        "[hostrun] root {} is unwritable: {e}",
                        self.root.display()
                    )),
                }
            }
        }
        match (sys::volume_of(&self.root), sys::volume_of(&self.home)) {
            (Some(a), Some(b)) if a == b => {}
            (Some(_), Some(_)) => out.push(format!(
                "[hostrun] root {} is on a different volume than {}",
                self.root.display(),
                self.home.display()
            )),
            _ => out.push(format!(
                "[hostrun] root {}: its volume could not be read",
                self.root.display()
            )),
        }
        if self.claude_path().is_none() {
            out.push(format!(
                "[hostrun] claude {:?} is missing or not executable",
                self.claude
            ));
        }
        if self.runner_path().is_none() {
            out.push(format!(
                "[hostrun] the runner {} is missing (it is installed beside the bridge)",
                self.runner.display()
            ));
        }
        if !PERMISSION_MODES.contains(&self.permission_mode.as_str()) {
            out.push(format!(
                "[hostrun] permission_mode {:?} is not one of {}",
                self.permission_mode,
                PERMISSION_MODES.join(", ")
            ));
        }
        if self.max_concurrent == 0 {
            out.push("[hostrun] max_concurrent must be at least 1".into());
        }
        if self.max_per_repo == 0 {
            out.push("[hostrun] max_per_repo must be at least 1".into());
        }
        for repo in &self.repo_keys {
            if !valid_repo(repo) {
                out.push(format!("[hostrun.repos.{repo:?}] is not owner/name"));
            }
        }
        for (repo, cap) in &self.repo_caps {
            if *cap == 0 {
                out.push(format!(
                    "[hostrun.repos.{repo:?}] max_concurrent must be at least 1"
                ));
            }
        }
        if self.label_prefix.is_empty()
            || !self.label_prefix.ends_with('.')
            || !self
                .label_prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            out.push(format!(
                "[hostrun] label_prefix {:?} is not a reverse-DNS prefix ending in a dot",
                self.label_prefix
            ));
        }
        if self.stall_minutes == 0 {
            out.push("[hostrun] stall_minutes must be at least 1".into());
        }
        out
    }
}

/// The startup gate's `[hostrun]` half.
pub fn validate_hostrun(cfg: &Config) -> Vec<ConfigError> {
    cfg.hostrun
        .problems()
        .into_iter()
        .map(|message| ConfigError {
            model: None,
            message,
        })
        .collect()
}

// ---- THE RECORD ----------------------------------------------------------------------

/// How a run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    #[default]
    Queued,
    Running,
    NeedsYou,
    Reported,
    Failed,
    Cancelled,
    Interrupted,
    /// The plan's usage limit ended the round; the run continues by itself at `resume_at`.
    Paused,
    /// Settled without a reply: superseded by a later run of its item, its Drafts line ticked,
    /// its report archived, or closed by Jeremy. `closed_reason` says which. Terminal; a reply
    /// still continues it, as it does a reported run.
    Closed,
}

impl RunStatus {
    pub fn label(self) -> &'static str {
        match self {
            RunStatus::Queued => "queued",
            RunStatus::Running => "running",
            RunStatus::NeedsYou => "needs_you",
            RunStatus::Reported => "reported",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Interrupted => "interrupted",
            RunStatus::Paused => "paused",
            RunStatus::Closed => "closed",
        }
    }

    /// What the Inbox front matter's `outcome` says.
    pub fn outcome(self) -> &'static str {
        match self {
            RunStatus::NeedsYou => "needs you",
            other => other.label(),
        }
    }

    /// Queued, running, waiting on Jeremy or paused for the usage limit: a second start of the
    /// item is refused.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            RunStatus::Queued | RunStatus::Running | RunStatus::NeedsYou | RunStatus::Paused
        )
    }

    /// Reported, failed, cancelled or closed: done with, unless someone replies.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Reported | RunStatus::Failed | RunStatus::Cancelled | RunStatus::Closed
        )
    }

    /// Waiting on Jeremy with nothing running: what the automatic closes may settle.
    pub fn is_settleable(self) -> bool {
        matches!(self, RunStatus::NeedsYou | RunStatus::Interrupted)
    }

    /// Not queued, not running and not paused (a paused run continues by itself).
    pub fn is_finished(self) -> bool {
        !matches!(
            self,
            RunStatus::Queued | RunStatus::Running | RunStatus::Paused
        )
    }
}

/// One round: the first run of the prompt, or one reply.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Round {
    pub n: u32,
    /// The reply text; `None` for round 1, which runs `prompt.md`.
    pub text: Option<String>,
    pub queued_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// The Inbox report's path under the notes directory, once written.
    pub report_path: Option<String>,
    pub outcome: Option<RunStatus>,
    /// Where this round's events start in `stream.jsonl`.
    #[serde(default)]
    pub stream_offset: u64,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub cancel_requested_at: Option<String>,
    /// The push and the agent turn for this round's report were sent.
    #[serde(default)]
    pub notified: bool,
    /// Who queued the round when no person did: [`AUTO_LIMIT`] or [`AUTO_RESTART`].
    #[serde(default)]
    pub auto: Option<String>,
}

/// What cleanup did to a run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cleanup {
    /// True while nothing of the run has been removed.
    #[serde(default = "yes")]
    pub kept: bool,
    pub artifacts_removed: Option<String>,
    pub worktree_removed: Option<String>,
    /// The push before a paused run's worktree goes.
    pub worktree_warned: Option<String>,
    pub stream_gzipped: Option<String>,
    /// The branch was seen on origin once, so its absence later means it was deleted.
    #[serde(default)]
    pub branch_seen_on_origin: bool,
}

fn yes() -> bool {
    true
}

/// `run.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub strand: Option<String>,
    pub item_id: Option<String>,
    /// The draft the prompt came from, under the notes directory.
    pub prompt_path: Option<String>,
    pub item_text: Option<String>,
    pub repo: String,
    pub branch: String,
    pub default_branch: Option<String>,
    pub base_sha: Option<String>,
    pub session_id: Option<String>,
    /// `app`, `turn` or `schedule:<id>`.
    pub origin: String,
    pub status: RunStatus,
    /// Why a queued run is not running: `slot`, `repo <owner/name>`, `disk`, `limit`.
    pub waiting_for: Option<String>,
    pub created_at: String,
    /// When the current wait in the queue began: the order queued runs start in.
    pub queued_at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub last_event_at: Option<String>,
    pub stalled_since: Option<String>,
    #[serde(default)]
    pub rounds: Vec<Round>,
    pub desk_command: Option<String>,
    #[serde(default)]
    pub derived_data: Vec<String>,
    pub disk_bytes: Option<u64>,
    #[serde(default)]
    pub cleanup: Cleanup,
    /// The model the draft asked for (`devjob-model`).
    pub model: Option<String>,
    /// The `claude` the runner runs, resolved at creation.
    pub claude: String,
    pub permission_mode: String,
    /// The run's launchd label, set when its round is started.
    pub label: Option<String>,
    /// The agent's process group, while it runs.
    pub pgid: Option<i32>,
    /// The one "waiting for disk" push was sent for this wait.
    #[serde(default)]
    pub disk_pushed: bool,
    /// Why the run could not start, when it could not.
    pub error: Option<String>,
    /// While `paused`: when the watcher queues the next round.
    #[serde(default)]
    pub resume_at: Option<String>,
    /// The one push for this usage limit pause was sent; cleared once a round gets past it.
    #[serde(default)]
    pub limit_pushed: bool,
    /// The one automatic round after an interruption was queued; cleared when a round completes.
    #[serde(default)]
    pub restart_resumed: bool,
    /// The repository's `env_file` at creation: its path only, never its contents.
    #[serde(default)]
    pub env_file: Option<String>,
    /// Why the run was closed: `superseded by <run_id>`, `item ticked`, `report archived`, or
    /// what Jeremy gave. Only while `closed`; absent from records written before 0.199.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_reason: Option<String>,
    /// When the run was closed (RFC 3339). Only while `closed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
}

impl RunRecord {
    pub fn round(&self) -> Option<&Round> {
        self.rounds.last()
    }

    pub fn round_mut(&mut self) -> Option<&mut Round> {
        self.rounds.last_mut()
    }

    /// The strand's note slug and the item, when the run is a strand item's.
    pub fn step(&self) -> Option<(&str, &str)> {
        Some((self.strand.as_deref()?, self.item_id.as_deref()?))
    }
}

/// `exit.json`, written by the runner when its agent exits.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExitInfo {
    pub round: u32,
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub ended_at: String,
}

pub const RUN_JSON: &str = "run.json";
pub const RUN_LOCK: &str = "run.lock";
pub const PROMPT_MD: &str = "prompt.md";
pub const STREAM: &str = "stream.jsonl";
pub const REPORT_MD: &str = "report.md";
pub const RUNNER_LOG: &str = "runner.log";
pub const EXIT_JSON: &str = "exit.json";
pub const JOB_PLIST: &str = "job.plist";
pub const TARGET_DIR: &str = "target";
pub const WORKTREE_DIR: &str = "worktree";
/// Where a run clones any further repository it needs. Removed with the build artifacts.
pub const EXTRA_DIR: &str = "extra";
/// The prompt as the caller gave it (a draft's `## PROMPT` section or a free prompt), until
/// the first round renders `prompt.md` from it.
pub const SECTION_MD: &str = "section.md";

// ---- THE ENV FILE --------------------------------------------------------------------

/// Whether `k` can name an environment variable.
fn valid_env_key(k: &str) -> bool {
    let mut cs = k.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A repository's `env_file`: `KEY=value` lines; blank lines and `#` comments are skipped, an
/// `export ` prefix is allowed and one pair of matching quotes around a value is removed. An
/// error names the file and a line number, NEVER a value.
pub fn read_env_file(path: &Path) -> Result<Vec<(String, String)>, String> {
    let text = match std::fs::read(path) {
        Ok(b) => String::from_utf8(b)
            .map_err(|_| format!("the env file {} is not UTF-8", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("the env file {} is missing", path.display()))
        }
        Err(e) => {
            return Err(format!(
                "the env file {} is unreadable: {}",
                path.display(),
                e.kind()
            ))
        }
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let t = t.strip_prefix("export ").map(str::trim_start).unwrap_or(t);
        let Some((k, v)) = t.split_once('=') else {
            return Err(format!(
                "the env file {}: line {} is not KEY=value",
                path.display(),
                i + 1
            ));
        };
        let k = k.trim();
        if !valid_env_key(k) {
            return Err(format!(
                "the env file {}: line {} does not start with a variable name",
                path.display(),
                i + 1
            ));
        }
        let v = v.trim();
        let v = [('"', '"'), ('\'', '\'')]
            .iter()
            .find_map(|(a, b)| {
                (v.len() >= 2 && v.starts_with(*a) && v.ends_with(*b)).then(|| &v[1..v.len() - 1])
            })
            .unwrap_or(v);
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

// ---- TIME ----------------------------------------------------------------------------

pub fn now_ms() -> u64 {
    system_time_to_ms(SystemTime::now())
}

/// RFC 3339 in UTC, to the millisecond.
pub fn rfc(ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Utc
        .timestamp_millis_opt(ms as i64)
        .single()
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

pub fn ms_of(s: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.timestamp_millis().max(0) as u64)
}

fn ms_opt(s: &Option<String>) -> Option<u64> {
    s.as_deref().and_then(ms_of)
}

// ---- THE RUN DIRECTORY ---------------------------------------------------------------

/// Read `run.json`.
pub fn read_run(dir: &Path) -> Result<RunRecord, String> {
    let text = std::fs::read_to_string(dir.join(RUN_JSON))
        .map_err(|e| format!("{}: {e}", dir.join(RUN_JSON).display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", dir.join(RUN_JSON).display()))
}

fn write_run(dir: &Path, rec: &RunRecord) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(rec).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(RUN_JSON), &bytes)
        .map_err(|e| format!("could not write {}: {e}", dir.join(RUN_JSON).display()))
}

/// Read, change and write `run.json` under the run's `flock`, which the runner takes too.
pub fn update_run(dir: &Path, f: impl FnOnce(&mut RunRecord)) -> Result<RunRecord, String> {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(RUN_LOCK))
        .map_err(|e| format!("could not open {}: {e}", dir.join(RUN_LOCK).display()))?;
    git::flock_exclusive(&lock).map_err(|e| format!("could not lock run.json: {e}"))?;
    let mut rec = read_run(dir)?;
    f(&mut rec);
    write_run(dir, &rec)?;
    drop(lock);
    Ok(rec)
}

/// `exit.json`, if the runner wrote one.
pub fn read_exit(dir: &Path) -> Option<ExitInfo> {
    let text = std::fs::read_to_string(dir.join(EXIT_JSON)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write `exit.json`.
pub fn write_exit(dir: &Path, exit: &ExitInfo) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(exit).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(EXIT_JSON), &bytes)
        .map_err(|e| format!("could not write exit.json: {e}"))
}

/// Every run directory with a readable `run.json`, and the names of those without one.
pub fn scan_runs(runs_dir: &Path) -> (Vec<(PathBuf, RunRecord)>, Vec<String>) {
    let mut runs = Vec::new();
    let mut orphans = Vec::new();
    let Ok(rd) = std::fs::read_dir(runs_dir) else {
        return (runs, orphans);
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        match read_run(&p) {
            Ok(r) => runs.push((p, r)),
            Err(_) if !p.join(RUN_JSON).exists() => {
                orphans.push(e.file_name().to_string_lossy().into_owned())
            }
            Err(e) => eprintln!("jesse-bridge: hostrun: {e}"),
        }
    }
    runs.sort_by(|a, b| {
        a.1.created_at
            .cmp(&b.1.created_at)
            .then(a.1.run_id.cmp(&b.1.run_id))
    });
    orphans.sort();
    (runs, orphans)
}

// ---- RUN IDS, BRANCHES AND THE PROMPT --------------------------------------------------

/// The `n`th name a base takes: the base itself, then `b` … `z`, then `-27`, `-28`, ….
pub fn nth_run_id(base: &str, n: usize) -> String {
    match n {
        0 => base.to_string(),
        1..=25 => format!("{base}{}", (b'a' + n as u8) as char),
        _ => format!("{base}-{}", n + 1),
    }
}

/// Reserve the next free run id for `base` by creating its directory, so two starts at once can
/// never take the same one. Every earlier directory counts, whatever its status, so an id (and the
/// branch, the Inbox file and the launchd label made from it) is never reused.
pub fn allocate_run_id(runs_dir: &Path, base: &str) -> Result<String, String> {
    std::fs::create_dir_all(runs_dir)
        .map_err(|e| format!("could not create {}: {e}", runs_dir.display()))?;
    for n in 0..10_000 {
        let id = nth_run_id(base, n);
        match std::fs::create_dir(runs_dir.join(&id)) {
            Ok(()) => return Ok(id),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not create the run directory {id}: {e}")),
        }
    }
    Err(format!("no free run id for {base}"))
}

/// Lowercase letters, digits and single hyphens.
pub fn slugify(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() {
            c
        } else {
            '-'
        };
        if c == '-' && (out.is_empty() || out.ends_with('-')) {
            continue;
        }
        out.push(c);
        if out.chars().count() >= max {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

/// `<run_id lowercased>-<slug of the item text, at most 40>`, or the lowercased id alone.
pub fn branch_name(run_id: &str, text: Option<&str>) -> String {
    let id = run_id.to_lowercase();
    match text.map(|t| slugify(t, BRANCH_TEXT_CHARS)) {
        Some(s) if !s.is_empty() => format!("{id}-{s}"),
        _ => id,
    }
}

fn is_rule(line: &str) -> bool {
    let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3
        && (t.chars().all(|c| c == '-')
            || t.chars().all(|c| c == '*')
            || t.chars().all(|c| c == '_'))
}

fn is_checkbox(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("- [ ]") || t.starts_with("- [x]") || t.starts_with("- [X]")
}

/// The draft's `## PROMPT` section, byte for byte: everything after the heading line, up to the
/// horizontal rule that comes before its archive checkboxes (or the end of the file). `None`
/// when there is no such heading or the section is blank.
pub fn prompt_section(src: &str) -> Option<String> {
    let mut start = None;
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end() == "## PROMPT" {
            start = Some(offset);
            break;
        }
    }
    let body = &src[start?..];
    let lines: Vec<&str> = body.split_inclusive('\n').collect();
    let footer = lines.iter().rposition(|l| is_rule(l)).filter(|&hr| {
        let after = &lines[hr + 1..];
        after.iter().any(|l| is_checkbox(l))
            && after.iter().all(|l| l.trim().is_empty() || is_checkbox(l))
    });
    let end: usize = match footer {
        Some(hr) => lines[..hr].iter().map(|l| l.len()).sum(),
        None => body.len(),
    };
    let section = &body[..end];
    (!section.trim().is_empty()).then(|| section.to_string())
}

/// Where the run is, for the generated section: the repository, its branch, the branch it was
/// cut from, and the run directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPlace<'a> {
    pub run_id: &'a str,
    pub repo: &'a str,
    pub branch: &'a str,
    pub default_branch: &'a str,
    pub run_dir: &'a Path,
    /// Ask for `[<run_id>]` at the start of the pull request title.
    pub pr_title_tag: bool,
}

/// The generated section's first paragraph, exactly: where the run is, and that this replaces
/// whatever machine or directory the prompt names.
pub fn place_paragraph(p: &RunPlace) -> String {
    format!(
        "You are running on the Mac Studio as a Studio run, in a fresh worktree of {} on branch \
         {}, cut from origin/{}. Wherever the prompt above names another machine, host, user or \
         directory to work in, this worktree replaces it: do not change machines, do not stop \
         because the host differs, and do not work in any other checkout of this repository. If \
         the work needs another repository, clone it under {}/{EXTRA_DIR}/ and work there.",
        p.repo,
        p.branch,
        p.default_branch,
        p.run_dir.display()
    )
}

/// The generated section's second paragraph, exactly. The pull request title sentence only
/// when `pr_title_tag`.
pub fn generated_paragraph(run_id: &str, pr_title_tag: bool) -> String {
    let lower = run_id.to_lowercase();
    let title = if pr_title_tag {
        format!(" and put [{run_id}] at the start of the pull request title")
    } else {
        String::new()
    };
    format!(
        "This run is {run_id}. Start every branch name with {lower}-{title}. Other runs and \
         people work on this repository at the \
         same time: before your final verification, and again right before any merge, fetch \
         the latest default branch, merge or rebase it into your branch, resolve any conflicts, \
         and re-run your checks. Keep everything you create inside your working directory or \
         the system temporary directory. When you finish, or stop early, write your complete \
         report back, the same text you would answer with, to the file named in the \
         environment variable JESSE_RUN_REPORT, then answer as usual. If you need a decision \
         from Jeremy to continue, stop, make the first line of that report NEEDS YOU: followed \
         by the one question, and end the run."
    )
}

/// The prompt a run is served: the section byte for byte, a blank line, then the generated
/// section (where the run is, then the run's rules) and nothing else.
pub fn served_prompt(section: &str, place: &RunPlace) -> String {
    let mut s = section.to_string();
    if !s.ends_with('\n') {
        s.push('\n');
    }
    if !s.ends_with("\n\n") {
        s.push('\n');
    }
    s.push_str("### Run ID and report\n\n");
    s.push_str(&place_paragraph(place));
    s.push_str("\n\n");
    s.push_str(&generated_paragraph(place.run_id, place.pr_title_tag));
    s.push('\n');
    s
}

// ---- ELIGIBILITY -----------------------------------------------------------------------

/// A strand item that may run on the Studio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StudioItem {
    pub strand: String,
    pub item_id: String,
    pub item_text: String,
    pub prompt_path: String,
    pub repo: String,
    pub model: Option<String>,
    /// The `## PROMPT` section, byte for byte.
    pub section: String,
}

/// Whether a draft's front matter sends it to the Studio.
pub fn draft_is_studio(src: &str) -> bool {
    fm(src, HOST_KEY).as_deref() == Some("studio")
}

/// How a strand item came to be started, which decides how its repository is found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptIn<'a> {
    /// A `kind = "devjob"` strand schedule entry: the draft must say `devjob-repo` and
    /// `devjob-host: studio`, exactly as before.
    Schedule,
    /// A person asked (`POST /jesse/runs`, `devjob_start` with `host: "studio"`): asking is the
    /// opt in. The repository is the draft's `devjob-repo`, else `repo`, else the strand's one
    /// `repos:` entry.
    Request { repo: Option<&'a str> },
}

/// A model id a draft's `devjob-model` may name.
fn check_model(path: &str, model: &Option<String>) -> Result<(), String> {
    if let Some(m) = model {
        if m.is_empty()
            || m.len() > 200
            || !m.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/' | '@')
            })
        {
            return Err(format!("{path}: devjob-model {m:?} is not a model id"));
        }
    }
    Ok(())
}

/// The repository a requested run of a draft goes to: the draft's `devjob-repo`, else the
/// caller's `repo`, else the strand's `repos:` when it lists exactly one.
fn request_repo(
    path: &str,
    src: &str,
    repo: Option<&str>,
    strand: Option<(&str, &[String])>,
) -> Result<String, String> {
    if let Some(r) = fm(src, "devjob-repo") {
        if !valid_repo(&r) {
            return Err(format!("{path}: devjob-repo {r:?} is not owner/name"));
        }
        return Ok(r);
    }
    if let Some(r) = repo {
        if !valid_repo(r) {
            return Err(format!("repo {r:?} is not owner/name"));
        }
        return Ok(r.to_string());
    }
    let Some((slug, repos)) = strand else {
        return Err(format!("{path} has no devjob-repo: pass repo"));
    };
    match repos {
        [one] if valid_repo(one) => Ok(one.clone()),
        [one] => Err(format!(
            "{path} has no devjob-repo and strand {slug}'s repos entry {one:?} is not owner/name: pass repo"
        )),
        [] => Err(format!(
            "{path} has no devjob-repo and strand {slug} lists no repos: pass repo"
        )),
        many => Err(format!(
            "{path} has no devjob-repo and strand {slug} lists several repositories ({}): pass \
             repo with the one to use",
            many.join(", ")
        )),
    }
}

/// Step 8's checks for one item of a parsed note. `Err` is the reason it may not run.
/// `strand_repos` is the note's `repos:` front matter.
pub fn studio_item_of(
    notes_root: &Path,
    slug: &str,
    item: &crate::strands::StrandItem,
    strand_repos: &[String],
    opt_in: OptIn,
) -> Result<StudioItem, String> {
    if item.id.is_empty() {
        return Err("the line has no id".into());
    }
    if !devjob::valid_label_value(&item.id) {
        return Err(format!("the id {:?} cannot name a run", item.id));
    }
    if item.checked || item.section == StrandSection::Done {
        return Err(format!("{} is checked", item.id));
    }
    let Some(link) = item.link.as_deref() else {
        return Err(format!("{} links no draft", item.id));
    };
    let Some(path) = resolve_target(notes_root, link) else {
        return Err(format!("{}: the link [[{link}]] does not resolve", item.id));
    };
    if !path.starts_with(&format!("{DRAFTS_DIR}/")) {
        return Err(format!("{}: {path} is not under {DRAFTS_DIR}/", item.id));
    }
    if path.split('/').any(|seg| seg == ARCHIVE_SEGMENT) {
        return Err(format!("{}: {path} is archived", item.id));
    }
    let src = std::fs::read_to_string(notes_root.join(&path))
        .map_err(|e| format!("{}: {path} is unreadable: {e}", item.id))?;
    match fm(&src, "strand") {
        Some(s) if s == slug => {}
        Some(s) => return Err(format!("{path} belongs to strand {s}, not {slug}")),
        None => return Err(format!("{path} has no strand")),
    }
    match fm(&src, "strand-id") {
        Some(s) if s == item.id => {}
        Some(s) => return Err(format!("{path} is step {s}, not {}", item.id)),
        None => return Err(format!("{path} has no strand-id")),
    }
    let repo = match opt_in {
        OptIn::Schedule => {
            let Some(repo) = fm(&src, "devjob-repo") else {
                return Err(format!("{path} has no devjob-repo"));
            };
            if !valid_repo(&repo) {
                return Err(format!("{path}: devjob-repo {repo:?} is not owner/name"));
            }
            if !draft_is_studio(&src) {
                return Err(format!("{path} does not say {HOST_KEY}: studio"));
            }
            repo
        }
        OptIn::Request { repo } => request_repo(&path, &src, repo, Some((slug, strand_repos)))?,
    };
    let model = fm(&src, "devjob-model");
    check_model(&path, &model)?;
    let Some(section) = prompt_section(&src) else {
        return Err(format!("{path} has no ## PROMPT section"));
    };
    Ok(StudioItem {
        strand: slug.to_string(),
        item_id: item.id.clone(),
        item_text: item.text.clone(),
        prompt_path: path,
        repo,
        model,
        section,
    })
}

/// Step 8's checks for the line `item_id` of `Strands/<slug>.md`.
pub fn studio_item(
    notes_root: &Path,
    slug: &str,
    item_id: &str,
    opt_in: OptIn,
) -> Result<StudioItem, String> {
    if !crate::strands::is_safe_slug(slug) {
        return Err(format!("no strand {slug:?}"));
    }
    let rel = format!("{STRANDS_DIR}/{slug}.md");
    let src = std::fs::read_to_string(notes_root.join(&rel))
        .map_err(|_| format!("no strand {slug:?}"))?;
    let strand = parse_strand(slug, &src);
    if strand.state.trim() == "done" {
        return Err(format!("strand {slug} is done"));
    }
    let Some(item) = strand.items.iter().find(|i| i.id == item_id) else {
        return Err(format!("{slug} has no line {item_id}"));
    };
    studio_item_of(notes_root, slug, item, &strand.repos, opt_in)
}

/// A draft started by its path, outside any strand line: a free run named after the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StudioDraft {
    pub prompt_path: String,
    pub repo: String,
    pub model: Option<String>,
    pub section: String,
    /// The file's stem as a branch slug: the run is `s-<slug>`.
    pub slug: String,
}

/// The checks for a draft named by its path (`Projects/drafts/x.md`, or a wiki target that
/// resolves to one): under `Projects/drafts/` outside `archive`, with a `## PROMPT` section.
/// Its strand and strand id are not consulted.
pub fn studio_draft(
    notes_root: &Path,
    draft: &str,
    repo: Option<&str>,
) -> Result<StudioDraft, String> {
    let target = draft.trim();
    let Some(path) = resolve_target(notes_root, target) else {
        return Err(format!("the draft {target:?} does not resolve to a file"));
    };
    if path.split('/').any(|seg| seg == "..") {
        return Err(format!("the draft {target:?} does not resolve to a file"));
    }
    if !path.starts_with(&format!("{DRAFTS_DIR}/")) {
        return Err(format!("{path} is not under {DRAFTS_DIR}/"));
    }
    if path.split('/').any(|seg| seg == ARCHIVE_SEGMENT) {
        return Err(format!("{path} is archived"));
    }
    let src = std::fs::read_to_string(notes_root.join(&path))
        .map_err(|e| format!("{path} is unreadable: {e}"))?;
    let repo = request_repo(&path, &src, repo, None)?;
    let model = fm(&src, "devjob-model");
    check_model(&path, &model)?;
    let Some(section) = prompt_section(&src) else {
        return Err(format!("{path} has no ## PROMPT section"));
    };
    let stem = path
        .rsplit('/')
        .next()
        .unwrap_or(&path)
        .trim_end_matches(".md");
    let slug = slugify(stem, BRANCH_TEXT_CHARS);
    if !valid_slug(&slug) {
        return Err(format!("{path}: its name makes no branch slug"));
    }
    Ok(StudioDraft {
        prompt_path: path,
        repo,
        model,
        section,
        slug,
    })
}

// ---- WHERE REPORTS GO -----------------------------------------------------------------

/// Sends one agent turn. The bridge's own `start_turn`, or a test's recorder.
pub trait TurnSender: Send + Sync {
    fn send(&self, text: String) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
}

/// The bridge's turn, as a [`TurnSender`]: a `tell`, marked as the bridge's own studio-run
/// sender.
pub struct AppTurns {
    pub st: AppState,
}

impl TurnSender for AppTurns {
    fn send(&self, text: String) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
        let st = self.st.clone();
        Box::pin(async move {
            let mut req = JesseRequest::scheduled("tell", text.clone(), None);
            req.set_sent_for("Studio run");
            req.set_origin(STUDIO_RUN_ORIGIN);
            match start_turn(&st, req, None).await {
                Ok(TurnStart::Accepted { job_id, .. }) => {
                    eprintln!("jesse-bridge: hostrun TURN {text:?} job={job_id}");
                    Ok(())
                }
                Ok(TurnStart::Invalid { status, message }) | Err((status, message)) => {
                    Err(format!("the turn was refused ({status}): {message}"))
                }
            }
        })
    }
}

/// Where the watcher writes and whom it tells.
#[derive(Clone)]
pub struct RunSink {
    pub broker: Arc<LockBroker>,
    pub notifier: Option<Arc<dyn Notifier>>,
    pub turns: Option<Arc<dyn TurnSender>>,
}

/// One push or turn the synchronous half of a pass decided on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Push {
        title: String,
        body: String,
        run_id: String,
    },
}

// ---- THE INBOX REPORT --------------------------------------------------------------------

/// `Inbox/<date>-run-<run_id>.md`, or `-r<n>` for round 2 and later.
pub fn report_rel_path(date: &str, run_id: &str, round: u32) -> String {
    let inbox = crate::vaultwrites::INBOX_DIR;
    if round <= 1 {
        format!("{inbox}/{date}-run-{run_id}.md")
    } else {
        format!("{inbox}/{date}-run-{run_id}-r{round}.md")
    }
}

/// The report already written for this run's round, if any.
pub fn find_existing_report(notes_root: &Path, run_id: &str, round: u32) -> Option<String> {
    let inbox = notes_root.join(crate::vaultwrites::INBOX_DIR);
    let suffix = if round <= 1 {
        format!("-run-{run_id}.md")
    } else {
        format!("-run-{run_id}-r{round}.md")
    };
    for e in std::fs::read_dir(&inbox).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(&suffix) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        if fm(&text, "devjob").as_deref() == Some(run_id)
            && fm(&text, "round").as_deref() == Some(&round.to_string())
        {
            return Some(format!("{}/{name}", crate::vaultwrites::INBOX_DIR));
        }
    }
    None
}

/// The first `https://github.com/<owner>/<name>/pull/<n>` the report names.
pub fn pull_request_url(report: &str) -> Option<String> {
    let mut from = 0;
    while let Some(i) = report[from..].find("https://github.com/") {
        let at = from + i;
        let rest = &report[at..];
        let end = rest
            .find(|c: char| {
                c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"' | '\'' | '`' | ',')
            })
            .unwrap_or(rest.len());
        let url = rest[..end].trim_end_matches('.');
        let parts: Vec<&str> = url["https://github.com/".len()..].split('/').collect();
        if parts.len() >= 4
            && parts[2] == "pull"
            && !parts[3].is_empty()
            && parts[3].chars().all(|c| c.is_ascii_digit())
        {
            return Some(format!(
                "https://github.com/{}/{}/pull/{}",
                parts[0], parts[1], parts[3]
            ));
        }
        from = at + "https://github.com/".len();
    }
    None
}

/// The Inbox note for one finished round: the front matter, then the report verbatim, then the
/// pull request link when the report names one.
pub fn render_inbox(rec: &RunRecord, round: u32, status: RunStatus, report: &str) -> String {
    let mut s = format!(
        "---\ndevjob: {}\nrepo: {}\noutcome: {}\n",
        rec.run_id,
        rec.repo,
        status.outcome()
    );
    if let (Some(note), Some(id)) = (&rec.strand, &rec.item_id) {
        s.push_str(&format!(
            "strand: {note}\nstrand-id: {id}\nprompt: {}\n",
            rec.prompt_path.as_deref().unwrap_or("")
        ));
    }
    s.push_str(&format!(
        "host: studio\nsession: {}\nbranch: {}\nround: {round}\n---\n\n",
        rec.session_id.as_deref().unwrap_or(""),
        rec.branch
    ));
    s.push_str(report);
    if let Some(url) = pull_request_url(report) {
        if !report.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&format!("\nPull request: [{url}]({url})\n"));
    }
    s
}

/// The first line of a report, trimmed.
pub fn first_line(report: &str) -> String {
    report
        .trim_start_matches('\u{feff}')
        .trim_start()
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// A round's report: `report.md` when the agent wrote one, else the last `result` event of the
/// round, else the last 200 readable log lines under a line saying no report was written.
pub fn round_report(dir: &Path, round: &Round) -> String {
    if let Ok(text) = std::fs::read_to_string(dir.join(REPORT_MD)) {
        if !text.trim().is_empty() {
            return text;
        }
    }
    let stream = stream::read_stream_from(dir, round.stream_offset);
    if let Some(r) = stream::last_result(&stream) {
        return r;
    }
    let lines = stream::tail_readable(&stream, FALLBACK_LOG_LINES);
    let mut s = String::from("No report was written. The last lines of the run's log follow.\n\n");
    for l in &lines {
        s.push_str(l);
        s.push('\n');
    }
    s
}

/// What a finished round came to.
pub fn round_status(round: &Round, exit: &ExitInfo, report: &str) -> RunStatus {
    if round.cancel_requested_at.is_some() {
        RunStatus::Cancelled
    } else if first_line(report).starts_with(NEEDS_YOU_PREFIX) {
        RunStatus::NeedsYou
    } else if exit.code == Some(0) {
        RunStatus::Reported
    } else {
        RunStatus::Failed
    }
}

/// The turn a start sends, exactly.
pub fn start_turn_text(run_id: &str, item_id: &str, note: &str) -> String {
    format!("Run {run_id} started for {item_id} in Strands/{note}.")
}

/// The turn a report sends, exactly.
pub fn report_turn_text(run_id: &str, note: &str, inbox_rel: &str) -> String {
    let target = inbox_rel.strip_suffix(".md").unwrap_or(inbox_rel);
    format!("Run report for {run_id} in Strands/{note}: [[{target}]]")
}

/// Write `text` at `rel` under the write lock. `rel` names one run's round, so it is free unless
/// that round was already written, which [`find_existing_report`] answered before this.
async fn write_report(
    notes_root: &Path,
    broker: &LockBroker,
    run_id: &str,
    rel: &str,
    text: &str,
) -> Result<(), String> {
    let abs = notes_root.join(rel);
    let turn = format!("hostrun:{run_id}");
    let key = LockKey::Path(resolve_lock_path(&abs, "/"));
    if !broker.acquire_direct(&key, &turn, "report").await {
        return Err(format!("the write lock on {rel} was not free"));
    }
    let written = write_atomic(&abs, text.as_bytes());
    broker.release_call_direct(&turn, "report");
    written.map_err(|e| format!("could not write {rel}: {e}"))
}

// ---- CLOSING A SETTLED RUN ----------------------------------------------------------------

/// `superseded by <run_id>`.
pub fn superseded_reason(run_id: &str) -> String {
    format!("superseded by {run_id}")
}

/// Close the run in `dir` with `reason` if, read again under its lock, its status is one `may`
/// accepts. Silent: one log line, no push and no turn. `Ok(None)` when its status had moved on.
fn close_run(
    dir: &Path,
    reason: &str,
    may: impl Fn(RunStatus) -> bool,
    now: u64,
) -> Result<Option<RunRecord>, String> {
    let mut closed = false;
    let rec = update_run(dir, |r| {
        if !may(r.status) {
            return;
        }
        r.status = RunStatus::Closed;
        r.closed_reason = Some(reason.to_string());
        r.closed_at = Some(rfc(now));
        r.waiting_for = None;
        r.stalled_since = None;
        r.resume_at = None;
        if r.finished_at.is_none() {
            r.finished_at = Some(rfc(now));
        }
        closed = true;
    })?;
    if !closed {
        return Ok(None);
    }
    eprintln!("jesse-bridge: hostrun {}: closed ({reason})", rec.run_id);
    Ok(Some(rec))
}

/// Whether the strand item's Drafts line is checked or under `### Done`. `None` when the note or
/// the line can no longer be found, which settles nothing.
pub fn item_ticked(notes_root: &Path, slug: &str, item_id: &str) -> Option<bool> {
    if !crate::strands::is_safe_slug(slug) {
        return None;
    }
    let src = std::fs::read_to_string(notes_root.join(format!("{STRANDS_DIR}/{slug}.md"))).ok()?;
    let strand = parse_strand(slug, &src);
    let lines: Vec<_> = strand.items.iter().filter(|i| i.id == item_id).collect();
    if lines.is_empty() {
        return None;
    }
    Some(
        lines
            .iter()
            .any(|i| i.checked || i.section == StrandSection::Done),
    )
}

/// Whether every report of the run is gone from where it was written and at least one of them
/// is in `Inbox/archive/` under the same name. A run with no report, or a report that is simply
/// missing, is not archived.
pub fn reports_archived(notes_root: &Path, rec: &RunRecord) -> bool {
    let paths: Vec<&str> = rec
        .rounds
        .iter()
        .filter_map(|x| x.report_path.as_deref())
        .collect();
    if paths.is_empty() || paths.iter().any(|p| notes_root.join(p).exists()) {
        return false;
    }
    paths.iter().any(|p| {
        p.rsplit('/')
            .next()
            .filter(|name| !name.is_empty() && *name != ".." && *name != ".")
            .is_some_and(|name| notes_root.join(INBOX_ARCHIVE_DIR).join(name).is_file())
    })
}

// ---- THE SERVICE -------------------------------------------------------------------------

/// How a run is asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartKind {
    /// A strand's Drafts item. `scheduled` is a strand schedule entry, whose draft must opt in
    /// with `devjob-repo` and `devjob-host: studio`; otherwise a person asked, and `repo` is the
    /// repository they named, used when the draft names none ([`OptIn::Request`]).
    Item {
        strand: String,
        item_id: String,
        repo: Option<String>,
        scheduled: bool,
    },
    /// A draft by its path, with no strand line: a free run `s-<file stem>`.
    Draft { draft: String, repo: Option<String> },
    /// A free prompt from a chat turn: `s-<slug>`.
    Free {
        repo: String,
        prompt: String,
        slug: String,
        model: Option<String>,
    },
}

/// One start: what, who asked, and whether a second live run of the item is wanted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
    pub kind: StartKind,
    pub origin: String,
    pub again: bool,
}

/// What a start answers.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StartedRun {
    pub run_id: String,
    pub status: RunStatus,
    pub waiting_for: Option<String>,
    #[serde(skip)]
    pub step: Option<(String, String)>,
}

/// Why a start, reply or cancel was refused, and how a route should answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The item is not runnable, or the arguments are wrong: 400.
    Invalid(String),
    /// The item already has a live run: 409.
    Live(String),
    /// No such run: 404.
    NotFound(String),
    /// The run is in a state the action does not apply to: 409.
    State(String),
    /// Something on the machine failed: 500.
    Internal(String),
}

impl Refusal {
    pub fn message(&self) -> &str {
        match self {
            Refusal::Invalid(m)
            | Refusal::Live(m)
            | Refusal::NotFound(m)
            | Refusal::State(m)
            | Refusal::Internal(m) => m,
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Refusal::Invalid(_) => StatusCode::BAD_REQUEST,
            Refusal::Live(_) | Refusal::State(_) => StatusCode::CONFLICT,
            Refusal::NotFound(_) => StatusCode::NOT_FOUND,
            Refusal::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The last space pass, for `GET /jesse/runs`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SpacePass {
    pub at: String,
    pub freed_bytes: u64,
    pub total_bytes: u64,
    pub free_bytes: Option<u64>,
}

/// Timings of the housekeeping passes and what they last measured. Not run state: a restart
/// simply runs each pass again.
#[derive(Debug, Default)]
struct Housekeeping {
    last_cleanup_ms: Option<u64>,
    last_reconcile_ms: Option<u64>,
    last_space_pass: Option<SpacePass>,
    last_total_bytes: Option<u64>,
    orphans: Vec<String>,
}

/// The `hostrun` service: the queue, the launcher and the watcher. One per process.
pub struct HostRuns {
    pub settings: HostRunSettings,
    notes_root: PathBuf,
    launcher: Arc<dyn Launcher>,
    forge: Arc<dyn Forge>,
    sink: OnceLock<RunSink>,
    /// Serialises every queue decision (a create, a start) against every other.
    gate: Mutex<()>,
    wake: tokio::sync::Notify,
    house: Mutex<Housekeeping>,
}

impl std::fmt::Debug for HostRuns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostRuns")
            .field("root", &self.settings.root)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl HostRuns {
    pub fn new(
        settings: HostRunSettings,
        notes_root: PathBuf,
        launcher: Arc<dyn Launcher>,
        forge: Arc<dyn Forge>,
    ) -> HostRuns {
        HostRuns {
            settings,
            notes_root,
            launcher,
            forge,
            sink: OnceLock::new(),
            gate: Mutex::new(()),
            wake: tokio::sync::Notify::new(),
            house: Mutex::new(Housekeeping::default()),
        }
    }

    /// The real service: launchd and `gh`.
    pub fn production(settings: HostRunSettings, notes_root: PathBuf) -> HostRuns {
        HostRuns::new(
            settings,
            notes_root,
            Arc::new(sys::Launchctl::default()),
            Arc::new(sys::GhForge),
        )
    }

    /// Where reports go. Until this is called finished rounds wait, unreported.
    pub fn attach(&self, sink: RunSink) {
        let _ = self.sink.set(sink);
        self.wake.notify_one();
    }

    pub fn notes_root(&self) -> &Path {
        &self.notes_root
    }

    /// Wake the watcher for a pass now.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    fn runs(&self) -> Vec<(PathBuf, RunRecord)> {
        scan_runs(&self.settings.runs_dir()).0
    }

    /// One run's record.
    pub fn get(&self, run_id: &str) -> Option<RunRecord> {
        if !valid_run_id(run_id) {
            return None;
        }
        read_run(&self.settings.run_dir(run_id)).ok()
    }

    /// The live run (queued, running, needs you) of a strand item, if any.
    pub fn live_run_of(&self, strand: &str, item_id: &str) -> Option<RunRecord> {
        self.runs()
            .into_iter()
            .map(|(_, r)| r)
            .rfind(|r| r.step() == Some((strand, item_id)) && r.status.is_live())
    }

    /// Runs of a strand that are running, need Jeremy or are paused for the usage limit.
    pub fn active_count(&self, strand: &str) -> u32 {
        self.runs()
            .iter()
            .filter(|(_, r)| {
                r.strand.as_deref() == Some(strand)
                    && matches!(
                        r.status,
                        RunStatus::Running | RunStatus::NeedsYou | RunStatus::Paused
                    )
            })
            .count() as u32
    }

    /// Create a run, queued, and try to start it. The caller wakes the watcher and sends the
    /// start turn ([`HostRuns::start`] does both).
    pub fn create(&self, req: &StartRequest) -> Result<StartedRun, Refusal> {
        let claude = self
            .settings
            .claude_path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.settings.claude.clone());
        let (base, repo, prompt_for, branch_text, model, step, prompt_path) = match &req.kind {
            StartKind::Item {
                strand,
                item_id,
                repo,
                scheduled,
            } => {
                let opt_in = if *scheduled {
                    OptIn::Schedule
                } else {
                    OptIn::Request {
                        repo: repo.as_deref(),
                    }
                };
                let item = studio_item(&self.notes_root, strand, item_id, opt_in)
                    .map_err(Refusal::Invalid)?;
                (
                    item.item_id.clone(),
                    item.repo.clone(),
                    item.section.clone(),
                    Some(item.item_text.clone()),
                    item.model.clone(),
                    Some((item.strand.clone(), item.item_id.clone())),
                    Some(item.prompt_path.clone()),
                )
            }
            StartKind::Draft { draft, repo } => {
                let d = studio_draft(&self.notes_root, draft, repo.as_deref())
                    .map_err(Refusal::Invalid)?;
                (
                    format!("s-{}", d.slug),
                    d.repo,
                    d.section,
                    None,
                    d.model,
                    None,
                    Some(d.prompt_path),
                )
            }
            StartKind::Free {
                repo,
                prompt,
                slug,
                model,
            } => {
                if !valid_repo(repo) {
                    return Err(Refusal::Invalid(format!("repo {repo:?} is not owner/name")));
                }
                if prompt.trim().is_empty() {
                    return Err(Refusal::Invalid("prompt is empty".into()));
                }
                if prompt.len() > devjob::MAX_PROMPT_BYTES {
                    return Err(Refusal::Invalid(format!(
                        "prompt is {} bytes; the most is {}",
                        prompt.len(),
                        devjob::MAX_PROMPT_BYTES
                    )));
                }
                if !valid_slug(slug) {
                    return Err(Refusal::Invalid(format!(
                        "branch_slug {slug:?} must be lowercase letters, digits and hyphens"
                    )));
                }
                (
                    format!("s-{slug}"),
                    repo.clone(),
                    prompt.clone(),
                    None,
                    model.clone(),
                    None,
                    None,
                )
            }
        };
        // The repository's env file must be readable now; the runner reads it again at each
        // round. Its contents are dropped here: only the path is recorded.
        let env_file = self.settings.env_file(&repo).map(Path::to_path_buf);
        if let Some(p) = &env_file {
            read_env_file(p).map_err(|e| Refusal::Invalid(format!("{repo}: {e}")))?;
        }
        let _gate = lock(&self.gate);
        if let Some((strand, item_id)) = &step {
            if let Some(live) = self.live_run_of(strand, item_id) {
                if !req.again {
                    return Err(Refusal::Live(format!(
                        "{} is still {}. Start {} too?",
                        live.run_id,
                        live.status.label().replace('_', " "),
                        self.next_free_id(&base)
                    )));
                }
            }
        }
        let runs_dir = self.settings.runs_dir();
        let run_id = allocate_run_id(&runs_dir, &base).map_err(Refusal::Internal)?;
        let dir = runs_dir.join(&run_id);
        let now = rfc(now_ms());
        let rec = RunRecord {
            run_id: run_id.clone(),
            strand: step.as_ref().map(|s| s.0.clone()),
            item_id: step.as_ref().map(|s| s.1.clone()),
            prompt_path,
            item_text: branch_text.clone(),
            repo,
            branch: branch_name(&run_id, branch_text.as_deref()),
            origin: req.origin.clone(),
            status: RunStatus::Queued,
            created_at: now.clone(),
            queued_at: Some(now.clone()),
            rounds: vec![Round {
                n: 1,
                queued_at: now,
                ..Round::default()
            }],
            model,
            claude,
            permission_mode: self.settings.permission_mode.clone(),
            cleanup: Cleanup {
                kept: true,
                ..Cleanup::default()
            },
            env_file: env_file.map(|p| p.to_string_lossy().into_owned()),
            ..RunRecord::default()
        };
        // `prompt.md` is rendered when the first round starts, once the worktree names the
        // branch it was cut from ([`HostRuns::launch`]).
        if let Err(e) = write_atomic(&dir.join(SECTION_MD), prompt_for.as_bytes())
            .map_err(|e| format!("could not write section.md: {e}"))
            .and_then(|_| write_run(&dir, &rec))
        {
            return Err(Refusal::Internal(e));
        }
        eprintln!(
            "jesse-bridge: hostrun CREATE {run_id} repo={} origin={}{}",
            rec.repo,
            rec.origin,
            step.as_ref()
                .map(|(s, i)| format!(" strand={s} item={i}"))
                .unwrap_or_default()
        );
        // A further run of the item settles the earlier ones still waiting on Jeremy: the
        // question they asked is now the new run's. Queued, running and paused runs go on.
        if req.again {
            if let Some((strand, item_id)) = &step {
                let reason = superseded_reason(&run_id);
                for (d, r) in self.runs() {
                    if r.run_id != run_id
                        && r.step() == Some((strand.as_str(), item_id.as_str()))
                        && r.status.is_settleable()
                    {
                        if let Err(e) = close_run(&d, &reason, RunStatus::is_settleable, now_ms()) {
                            eprintln!("jesse-bridge: hostrun {}: not closed: {e}", r.run_id);
                        }
                    }
                }
            }
        }
        drop(_gate);
        Ok(StartedRun {
            run_id,
            status: RunStatus::Queued,
            waiting_for: None,
            step,
        })
    }

    /// The id a further run of `base` would take now (for the "start it too?" question).
    fn next_free_id(&self, base: &str) -> String {
        let dir = self.settings.runs_dir();
        (0..10_000)
            .map(|n| nth_run_id(base, n))
            .find(|id| !dir.join(id).exists())
            .unwrap_or_else(|| base.to_string())
    }

    /// Create and queue a run, start what the queue allows, and send the start turn for a
    /// strand item. Every entry point (the route, the tool, a schedule entry) comes here.
    pub async fn start(self: &Arc<Self>, req: StartRequest) -> Result<StartedRun, Refusal> {
        let this = self.clone();
        let created = tokio::task::spawn_blocking(move || this.create(&req))
            .await
            .map_err(|e| Refusal::Internal(format!("the start task failed: {e}")))??;
        if let Some((strand, item)) = &created.step {
            self.send_turn(start_turn_text(&created.run_id, item, strand))
                .await;
        }
        let effects = self.pump_blocking().await;
        self.send_effects(effects).await;
        self.wake();
        let rec = self.get(&created.run_id);
        Ok(StartedRun {
            status: rec.as_ref().map(|r| r.status).unwrap_or(created.status),
            waiting_for: rec.and_then(|r| r.waiting_for),
            ..created
        })
    }

    async fn pump_blocking(self: &Arc<Self>) -> Vec<Effect> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.pump(now_ms()))
            .await
            .unwrap_or_default()
    }

    async fn send_turn(&self, text: String) {
        let Some(turns) = self.sink.get().and_then(|s| s.turns.clone()) else {
            eprintln!("jesse-bridge: hostrun: no turn sender attached; not sent: {text:?}");
            return;
        };
        if let Err(e) = turns.send(text.clone()).await {
            eprintln!("jesse-bridge: WARNING: hostrun turn {text:?}: {e}");
        }
    }

    async fn send_effects(&self, effects: Vec<Effect>) {
        let notifier = self.sink.get().and_then(|s| s.notifier.clone());
        for e in effects {
            match e {
                Effect::Push {
                    title,
                    body,
                    run_id,
                } => match &notifier {
                    Some(n) => n.notify(title, body, run_id).await,
                    None => eprintln!(
                        "jesse-bridge: hostrun PUSH {run_id}: {title}: {body} (no notifier)"
                    ),
                },
            }
        }
    }

    /// Whether the disk allows a start: free space at least `min_free_gb`.
    fn disk_ok(&self) -> bool {
        sys::free_bytes(&self.settings.root).is_none_or(|f| f >= self.settings.min_free_bytes)
    }

    /// Start every queued run the caps and the disk allow, oldest first, skipping any still
    /// blocked; record why each other one waits.
    pub fn pump(&self, now: u64) -> Vec<Effect> {
        let _gate = lock(&self.gate);
        let mut effects = Vec::new();
        let runs = self.runs();
        let mut running_total = runs
            .iter()
            .filter(|(_, r)| r.status == RunStatus::Running)
            .count();
        let mut per_repo: BTreeMap<String, usize> = BTreeMap::new();
        for (_, r) in runs.iter().filter(|(_, r)| r.status == RunStatus::Running) {
            *per_repo.entry(r.repo.clone()).or_default() += 1;
        }
        let mut queued: Vec<&(PathBuf, RunRecord)> = runs
            .iter()
            .filter(|(_, r)| r.status == RunStatus::Queued)
            .collect();
        // The watcher's own rounds (a run that was running when the usage limit or a restart
        // stopped it) go first, so it gets its slot back before new work takes it; then the
        // order of the wait.
        let resumed = |r: &RunRecord| r.round().is_some_and(|x| x.auto.is_some());
        queued.sort_by(|a, b| {
            let qa = a.1.queued_at.as_deref().unwrap_or(&a.1.created_at);
            let qb = b.1.queued_at.as_deref().unwrap_or(&b.1.created_at);
            resumed(&b.1)
                .cmp(&resumed(&a.1))
                .then(qa.cmp(qb))
                .then(a.1.run_id.cmp(&b.1.run_id))
        });
        // While any run is paused for the usage limit, nothing new starts: only the paused runs'
        // own rounds, which find out whether the limit has passed.
        let limit_hold = runs.iter().any(|(_, r)| r.status == RunStatus::Paused);
        let mut disk_checked: Option<bool> = None;
        for (dir, r) in queued {
            let probe = r.round().and_then(|x| x.auto.as_deref()) == Some(AUTO_LIMIT);
            let waiting = if limit_hold && !probe {
                Some("limit".to_string())
            } else if running_total >= self.settings.max_concurrent {
                Some("slot".to_string())
            } else if per_repo.get(&r.repo).copied().unwrap_or(0) >= self.settings.repo_cap(&r.repo)
            {
                Some(format!("repo {}", r.repo))
            } else {
                let ok = match disk_checked {
                    Some(ok) => ok,
                    None => {
                        let mut ok = self.disk_ok();
                        if !ok {
                            self.space_pass_locked(now);
                            ok = self.disk_ok();
                        }
                        disk_checked = Some(ok);
                        ok
                    }
                };
                (!ok).then(|| "disk".to_string())
            };
            match waiting {
                Some(w) => {
                    let push_disk = w == "disk" && !r.disk_pushed;
                    if r.waiting_for.as_deref() != Some(w.as_str()) || push_disk {
                        let _ = update_run(dir, |x| {
                            x.waiting_for = Some(w.clone());
                            if push_disk {
                                x.disk_pushed = true;
                            }
                        });
                    }
                    if push_disk {
                        let free = sys::free_bytes(&self.settings.root).unwrap_or(0);
                        let held = lock(&self.house).last_total_bytes.unwrap_or(0);
                        effects.push(Effect::Push {
                            title: "Runs waiting for disk".into(),
                            body: format!(
                                "{} is waiting: {} free, the runs hold {}",
                                r.run_id,
                                human_bytes(free),
                                human_bytes(held)
                            ),
                            run_id: r.run_id.clone(),
                        });
                    }
                }
                None => {
                    self.launch(dir, r, now);
                    running_total += 1;
                    *per_repo.entry(r.repo.clone()).or_default() += 1;
                }
            }
        }
        effects
    }

    /// Prepare the worktree and start the runner's launchd job for the run's current round. A
    /// failure to prepare finishes the round as failed, with the reason as its report.
    fn launch(&self, dir: &Path, rec: &RunRecord, now: u64) {
        let Some(round) = rec.round().cloned() else {
            return;
        };
        let worktree = dir.join(WORKTREE_DIR);
        let mut base: Option<git::NewWorktree> = None;
        let prepared: Result<(), String> = (|| {
            if !worktree.exists() {
                if round.n == 1 && rec.base_sha.is_none() {
                    base = Some(git::add_worktree(
                        &self.settings.root,
                        &rec.repo,
                        &self.settings.repo_url(&rec.repo),
                        &worktree,
                        &rec.branch,
                    )?);
                } else {
                    git::restore_worktree(
                        &self.settings.root,
                        &rec.repo,
                        &self.settings.repo_url(&rec.repo),
                        &worktree,
                        &rec.branch,
                    )?;
                }
            }
            std::fs::create_dir_all(dir.join(TARGET_DIR))
                .map_err(|e| format!("could not create target/: {e}"))?;
            if round.n == 1 && !dir.join(PROMPT_MD).exists() {
                let section = std::fs::read_to_string(dir.join(SECTION_MD))
                    .map_err(|e| format!("could not read section.md: {e}"))?;
                let default_branch = base
                    .as_ref()
                    .map(|b| b.default_branch.clone())
                    .or_else(|| rec.default_branch.clone())
                    .unwrap_or_else(|| "main".to_string());
                let served = served_prompt(
                    &section,
                    &RunPlace {
                        run_id: &rec.run_id,
                        repo: &rec.repo,
                        branch: &rec.branch,
                        default_branch: &default_branch,
                        run_dir: dir,
                        pr_title_tag: self.settings.pr_title_tag(&rec.repo),
                    },
                );
                write_atomic(&dir.join(PROMPT_MD), served.as_bytes())
                    .map_err(|e| format!("could not write prompt.md: {e}"))?;
            }
            if round.n > 1 {
                let prev = round.n - 1;
                for (from, to) in [
                    (REPORT_MD.to_string(), format!("report-r{prev}.md")),
                    (EXIT_JSON.to_string(), format!("exit-r{prev}.json")),
                ] {
                    if dir.join(&from).exists() {
                        std::fs::rename(dir.join(&from), dir.join(&to))
                            .map_err(|e| format!("could not set {from} aside: {e}"))?;
                    }
                }
            }
            Ok(())
        })();
        let offset = std::fs::metadata(dir.join(STREAM))
            .map(|m| m.len())
            .unwrap_or(0);
        let started = rfc(now);
        let base_fields = base.clone();
        let label_for_record = self.settings.label(&rec.run_id);
        let _ = update_run(dir, |r| {
            r.status = RunStatus::Running;
            r.waiting_for = None;
            r.disk_pushed = false;
            r.stalled_since = None;
            r.pgid = None;
            r.finished_at = None;
            r.error = None;
            r.started_at.get_or_insert_with(|| started.clone());
            r.label = Some(label_for_record.clone());
            r.last_event_at = Some(started.clone());
            if let Some(b) = &base_fields {
                r.base_sha = Some(b.base_sha.clone());
                r.default_branch = Some(b.default_branch.clone());
            }
            if let Some(cur) = r.round_mut() {
                cur.started_at = Some(started.clone());
                cur.stream_offset = offset;
            }
            r.cleanup.kept = true;
        });
        let label = self.settings.label(&rec.run_id);
        let outcome = prepared.and_then(|_| {
            let runner = self.settings.runner_path().ok_or_else(|| {
                format!("the runner {} is missing", self.settings.runner.display())
            })?;
            let env: Vec<(String, String)> = ENV_ALLOWLIST
                .iter()
                .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
                .collect();
            let plist = sys::render_plist(&label, &runner, dir, &dir.join(RUNNER_LOG), &env);
            write_atomic(&dir.join(JOB_PLIST), plist.as_bytes())
                .map_err(|e| format!("could not write job.plist: {e}"))?;
            let _ = self.launcher.bootout(&label);
            self.launcher.bootstrap(&dir.join(JOB_PLIST), &label)
        });
        match outcome {
            Ok(()) => eprintln!(
                "jesse-bridge: hostrun START {} round {} label {label}",
                rec.run_id, round.n
            ),
            Err(e) => {
                eprintln!("jesse-bridge: hostrun {}: could not start: {e}", rec.run_id);
                let _ = std::fs::write(
                    dir.join(REPORT_MD),
                    format!("The run could not start: {e}\n"),
                );
                let _ = write_exit(
                    dir,
                    &ExitInfo {
                        round: round.n,
                        code: Some(-1),
                        signal: None,
                        ended_at: rfc(now),
                    },
                );
                let _ = update_run(dir, |r| r.error = Some(e.clone()));
            }
        }
    }

    /// A new round on a finished run: the same worktree and session, the reply on stdin.
    pub fn reply(&self, run_id: &str, text: &str) -> Result<RunRecord, Refusal> {
        if text.trim().is_empty() {
            return Err(Refusal::Invalid("the reply is empty".into()));
        }
        if text.len() > devjob::MAX_PROMPT_BYTES {
            return Err(Refusal::Invalid(format!(
                "the reply is {} bytes; the most is {}",
                text.len(),
                devjob::MAX_PROMPT_BYTES
            )));
        }
        let _gate = lock(&self.gate);
        self.queue_round_locked(run_id, text, None)
    }

    /// Queue a new round on a finished or paused run. The caller holds the gate. `auto` names
    /// the watcher's own rounds ([`AUTO_LIMIT`], [`AUTO_RESTART`]); a person's reply is `None`.
    fn queue_round_locked(
        &self,
        run_id: &str,
        text: &str,
        auto: Option<&str>,
    ) -> Result<RunRecord, Refusal> {
        let rec = self
            .get(run_id)
            .ok_or_else(|| Refusal::NotFound(format!("no run {run_id:?}")))?;
        if !matches!(
            rec.status,
            RunStatus::NeedsYou
                | RunStatus::Reported
                | RunStatus::Failed
                | RunStatus::Interrupted
                | RunStatus::Paused
                | RunStatus::Closed
        ) {
            return Err(Refusal::State(format!(
                "{run_id} is {}; a reply needs it to be needs_you, reported, failed, interrupted, \
                 paused or closed",
                rec.status.label()
            )));
        }
        let dir = self.settings.run_dir(run_id);
        if !dir.join(WORKTREE_DIR).exists()
            && git::branch_on_origin(&self.settings.root, &rec.repo, &rec.branch) != Some(true)
        {
            return Err(Refusal::State(format!(
                "{run_id}'s worktree was removed and its branch {} is not on origin, so there is \
                 nothing to continue from",
                rec.branch
            )));
        }
        let now = rfc(now_ms());
        let rec = update_run(&dir, |r| {
            let n = r.rounds.len() as u32 + 1;
            r.rounds.push(Round {
                n,
                text: Some(text.to_string()),
                queued_at: now.clone(),
                auto: auto.map(str::to_string),
                ..Round::default()
            });
            r.status = RunStatus::Queued;
            r.queued_at = Some(now.clone());
            r.waiting_for = None;
            r.stalled_since = None;
            r.disk_pushed = false;
            r.resume_at = None;
            // A closed run that is answered is open again.
            r.closed_reason = None;
            r.closed_at = None;
            if auto == Some(AUTO_RESTART) {
                r.restart_resumed = true;
            }
        })
        .map_err(Refusal::Internal)?;
        eprintln!(
            "jesse-bridge: hostrun {} {run_id} round {}",
            match auto {
                Some(a) => format!("AUTO ROUND ({a})"),
                None => "REPLY".to_string(),
            },
            rec.rounds.len()
        );
        Ok(rec)
    }

    /// Queue the watcher's round on every run paused for the usage limit whose `resume_at` has
    /// come.
    pub fn resume_due(&self, now: u64) {
        let _gate = lock(&self.gate);
        for (_, r) in self.runs() {
            if r.status != RunStatus::Paused || ms_opt(&r.resume_at).is_some_and(|t| t > now) {
                continue;
            }
            if let Err(e) = self.queue_round_locked(&r.run_id, CONTINUE_TEXT, Some(AUTO_LIMIT)) {
                eprintln!(
                    "jesse-bridge: hostrun {}: the usage limit round could not be queued: {}",
                    r.run_id,
                    e.message()
                );
                // Try again in an hour rather than at every pass.
                let at = rfc(now.saturating_add(LIMIT_RETRY_MS));
                let _ = update_run(&self.settings.run_dir(&r.run_id), |x| {
                    x.resume_at = Some(at.clone())
                });
            }
        }
    }

    /// Reply, then start what the queue allows.
    pub async fn reply_and_pump(
        self: &Arc<Self>,
        run_id: &str,
        text: &str,
    ) -> Result<RunRecord, Refusal> {
        let this = self.clone();
        let (id, t) = (run_id.to_string(), text.to_string());
        tokio::task::spawn_blocking(move || this.reply(&id, &t))
            .await
            .map_err(|e| Refusal::Internal(format!("the reply task failed: {e}")))??;
        let effects = self.pump_blocking().await;
        self.send_effects(effects).await;
        self.wake();
        self.get(run_id)
            .ok_or_else(|| Refusal::NotFound(format!("no run {run_id:?}")))
    }

    /// Cancel: a queued run simply becomes `cancelled`; a running one gets SIGTERM to its process
    /// group now and SIGKILL after [`CANCEL_GRACE_MS`] (the watcher's), and its round is reported
    /// as cancelled from whatever it has.
    pub fn cancel(&self, run_id: &str) -> Result<RunRecord, Refusal> {
        let _gate = lock(&self.gate);
        let rec = self
            .get(run_id)
            .ok_or_else(|| Refusal::NotFound(format!("no run {run_id:?}")))?;
        let dir = self.settings.run_dir(run_id);
        let now = rfc(now_ms());
        match rec.status {
            RunStatus::Queued | RunStatus::Paused => update_run(&dir, |r| {
                r.status = RunStatus::Cancelled;
                r.waiting_for = None;
                r.resume_at = None;
                r.finished_at = Some(now.clone());
                if let Some(cur) = r.round_mut() {
                    cur.outcome = Some(RunStatus::Cancelled);
                    cur.cancel_requested_at = Some(now.clone());
                    cur.finished_at = Some(now.clone());
                    cur.notified = true;
                }
            })
            .map_err(Refusal::Internal),
            RunStatus::Running => {
                if rec.round().is_some_and(|r| r.cancel_requested_at.is_some()) {
                    return Ok(rec);
                }
                let updated = update_run(&dir, |r| {
                    if let Some(cur) = r.round_mut() {
                        cur.cancel_requested_at = Some(now.clone());
                    }
                })
                .map_err(Refusal::Internal)?;
                match rec.pgid {
                    Some(pg) => {
                        sys::signal_group(pg, libc::SIGTERM);
                    }
                    None => {
                        let _ = self.launcher.bootout(&self.settings.label(run_id));
                    }
                }
                eprintln!("jesse-bridge: hostrun CANCEL {run_id}");
                Ok(updated)
            }
            other => Err(Refusal::State(format!(
                "{run_id} is {}; only a queued, running or paused run can be cancelled",
                other.label()
            ))),
        }
    }

    /// Cancel, then wake the watcher now and again once the grace has passed.
    pub async fn cancel_and_wake(self: &Arc<Self>, run_id: &str) -> Result<RunRecord, Refusal> {
        let this = self.clone();
        let id = run_id.to_string();
        let rec = tokio::task::spawn_blocking(move || this.cancel(&id))
            .await
            .map_err(|e| Refusal::Internal(format!("the cancel task failed: {e}")))??;
        self.wake();
        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(CANCEL_GRACE_MS + 1000)).await;
            this.wake();
        });
        Ok(rec)
    }

    /// Close a run Jeremy has settled: `needs_you`, `interrupted` or `failed` only. `reason`
    /// defaults to [`CLOSED_BY_JEREMY`]. Silent: no push and no turn.
    pub fn close(&self, run_id: &str, reason: Option<&str>) -> Result<RunRecord, Refusal> {
        let reason = match reason.map(str::trim) {
            None | Some("") => CLOSED_BY_JEREMY.to_string(),
            Some(r) if r.len() > MAX_CLOSE_REASON_BYTES => {
                return Err(Refusal::Invalid(format!(
                    "the reason is {} bytes; the most is {MAX_CLOSE_REASON_BYTES}",
                    r.len()
                )))
            }
            Some(r) if r.contains(['\n', '\r']) => {
                return Err(Refusal::Invalid("the reason is one line".into()))
            }
            Some(r) => r.to_string(),
        };
        let may = |s: RunStatus| {
            matches!(
                s,
                RunStatus::NeedsYou | RunStatus::Interrupted | RunStatus::Failed
            )
        };
        let _gate = lock(&self.gate);
        let rec = self
            .get(run_id)
            .ok_or_else(|| Refusal::NotFound(format!("no run {run_id:?}")))?;
        let refused = |s: RunStatus| {
            Refusal::State(format!(
                "{run_id} is {}; only a needs_you, interrupted or failed run can be closed",
                s.label()
            ))
        };
        if !may(rec.status) {
            return Err(refused(rec.status));
        }
        match close_run(&self.settings.run_dir(run_id), &reason, may, now_ms()) {
            Ok(Some(r)) => Ok(r),
            Ok(None) => Err(refused(
                self.get(run_id).map(|r| r.status).unwrap_or(rec.status),
            )),
            Err(e) => Err(Refusal::Internal(e)),
        }
    }

    /// Close, off the async runtime.
    pub async fn close_blocking(
        self: &Arc<Self>,
        run_id: &str,
        reason: Option<String>,
    ) -> Result<RunRecord, Refusal> {
        let this = self.clone();
        let id = run_id.to_string();
        tokio::task::spawn_blocking(move || this.close(&id, reason.as_deref()))
            .await
            .map_err(|e| Refusal::Internal(format!("the close task failed: {e}")))?
    }

    /// The watcher's settling: close every `needs_you` or `interrupted` run whose strand item's
    /// line is ticked ([`CLOSED_ITEM_TICKED`]), and every `needs_you` run whose reports have all
    /// moved to `Inbox/archive/` ([`CLOSED_REPORT_ARCHIVED`]). Returns each run closed and why.
    /// Never touches a queued, running or paused run.
    pub fn close_settled(&self, now: u64) -> Vec<(String, String)> {
        let _gate = lock(&self.gate);
        let mut out = Vec::new();
        for (dir, r) in self.runs() {
            if !r.status.is_settleable() {
                continue;
            }
            let reason = if r
                .step()
                .and_then(|(slug, id)| item_ticked(&self.notes_root, slug, id))
                == Some(true)
            {
                CLOSED_ITEM_TICKED
            } else if r.status == RunStatus::NeedsYou && reports_archived(&self.notes_root, &r) {
                CLOSED_REPORT_ARCHIVED
            } else {
                continue;
            };
            let may = |s: RunStatus| match reason {
                CLOSED_REPORT_ARCHIVED => s == RunStatus::NeedsYou,
                _ => s.is_settleable(),
            };
            match close_run(&dir, reason, may, now) {
                Ok(Some(_)) => out.push((r.run_id.clone(), reason.to_string())),
                Ok(None) => {}
                Err(e) => eprintln!("jesse-bridge: hostrun {}: not closed: {e}", r.run_id),
            }
        }
        out
    }

    // ---- THE WATCHER -------------------------------------------------------------------

    /// The synchronous half of a pass over running runs: escalate a cancel, notice an
    /// interrupted or quiet run. Returns the pushes, and the runs with a finished round.
    pub fn scan(&self, now: u64) -> (Vec<Effect>, Vec<String>) {
        let mut effects = Vec::new();
        let mut finished = Vec::new();
        for (dir, r) in self.runs() {
            if r.status != RunStatus::Running {
                continue;
            }
            let Some(round) = r.round().cloned() else {
                continue;
            };
            if read_exit(&dir).is_some_and(|e| e.round == round.n) {
                finished.push(r.run_id.clone());
                continue;
            }
            let label = self.settings.label(&r.run_id);
            // Cancel: SIGKILL once the grace is over.
            if let Some(asked) = ms_opt(&round.cancel_requested_at) {
                if now.saturating_sub(asked) >= CANCEL_GRACE_MS {
                    if let Some(pg) = r.pgid {
                        if sys::group_alive(pg) {
                            eprintln!(
                                "jesse-bridge: hostrun {}: SIGKILL after the grace",
                                r.run_id
                            );
                            sys::signal_group(pg, libc::SIGKILL);
                        }
                    }
                }
            }
            let round_age = ms_opt(&round.started_at)
                .map(|s| now.saturating_sub(s))
                .unwrap_or(u64::MAX);
            if round_age >= INTERRUPT_SETTLE_MS && !self.launcher.is_running(&label) {
                // The runner may have written its exit and gone between the two reads.
                if read_exit(&dir).is_some_and(|e| e.round == round.n) {
                    finished.push(r.run_id.clone());
                    continue;
                }
                if round.cancel_requested_at.is_some() {
                    let _ = write_exit(
                        &dir,
                        &ExitInfo {
                            round: round.n,
                            code: None,
                            signal: Some(libc::SIGKILL),
                            ended_at: rfc(now),
                        },
                    );
                    finished.push(r.run_id.clone());
                    continue;
                }
                let at = rfc(now);
                let _ = update_run(&dir, |x| {
                    x.status = RunStatus::Interrupted;
                    x.finished_at = Some(at.clone());
                    x.pgid = None;
                    if let Some(cur) = x.round_mut() {
                        cur.finished_at = Some(at.clone());
                        cur.outcome = Some(RunStatus::Interrupted);
                    }
                });
                eprintln!(
                    "jesse-bridge: hostrun {}: INTERRUPTED (no runner, no exit)",
                    r.run_id
                );
                // One automatic round, unless this run already had one since its last
                // completed round: a second interruption waits for a person.
                let resumed = self.settings.auto_resume_interrupted && !r.restart_resumed && {
                    let _gate = lock(&self.gate);
                    match self.queue_round_locked(&r.run_id, RESTART_TEXT, Some(AUTO_RESTART)) {
                        Ok(_) => true,
                        Err(e) => {
                            eprintln!(
                                "jesse-bridge: hostrun {}: not resumed: {}",
                                r.run_id,
                                e.message()
                            );
                            false
                        }
                    }
                };
                effects.push(Effect::Push {
                    title: "Run interrupted".into(),
                    body: if resumed {
                        format!(
                            "{} stopped with no exit (a reboot or a crash). Resuming it once.",
                            r.run_id
                        )
                    } else {
                        format!(
                            "{} stopped with no exit (a reboot or a crash). Reply to resume it.",
                            r.run_id
                        )
                    },
                    run_id: r.run_id.clone(),
                });
                continue;
            }
            // Quiet: one push, never a kill. A new event clears it.
            let last = ms_opt(&r.last_event_at).or(ms_opt(&round.started_at));
            match (&r.stalled_since, last) {
                (Some(since), Some(l)) if ms_of(since).is_some_and(|s| l > s) => {
                    let _ = update_run(&dir, |x| x.stalled_since = None);
                }
                (None, Some(l))
                    if now.saturating_sub(l) >= self.settings.stall_minutes * 60_000 =>
                {
                    let at = rfc(now);
                    let _ = update_run(&dir, |x| x.stalled_since = Some(at.clone()));
                    effects.push(Effect::Push {
                        title: "Run quiet".into(),
                        body: format!("{} quiet for {} min", r.run_id, self.settings.stall_minutes),
                        run_id: r.run_id.clone(),
                    });
                }
                _ => {}
            }
        }
        (effects, finished)
    }

    /// Report one finished round: the Inbox file (once), the record, the push and the turn.
    pub async fn finalize(&self, run_id: &str) -> Result<(), String> {
        let Some(sink) = self.sink.get().cloned() else {
            return Ok(());
        };
        let dir = self.settings.run_dir(run_id);
        let rec = read_run(&dir)?;
        let Some(round) = rec.round().cloned() else {
            return Ok(());
        };
        let Some(exit) = read_exit(&dir).filter(|e| e.round == round.n) else {
            return Ok(());
        };
        if round.cancel_requested_at.is_none() {
            let events = stream::read_stream_from(&dir, round.stream_offset);
            if let Some(hit) = stream::usage_limit(&events) {
                return self.pause_for_limit(&dir, &rec, &exit, hit, &sink).await;
            }
        }
        let report = round_report(&dir, &round);
        let status = round_status(&round, &exit, &report);
        let rel = match round
            .report_path
            .clone()
            .or_else(|| find_existing_report(&self.notes_root, run_id, round.n))
        {
            Some(rel) => rel,
            None => {
                let date = chrono::Local::now().format("%Y-%m-%d").to_string();
                let rel = report_rel_path(&date, run_id, round.n);
                let text = render_inbox(&rec, round.n, status, &report);
                write_report(&self.notes_root, &sink.broker, run_id, &rel, &text).await?;
                eprintln!(
                    "jesse-bridge: hostrun REPORT {run_id} round {} {} at {rel}",
                    round.n,
                    status.label()
                );
                rel
            }
        };
        let worktree = dir.join(WORKTREE_DIR);
        let derived = sys::derived_data_for(&self.settings.derived_data_root, &worktree);
        let dir2 = dir.clone();
        let bytes = tokio::task::spawn_blocking(move || sys::tree_bytes(&dir2))
            .await
            .unwrap_or(0);
        let at = exit.ended_at.clone();
        let rel2 = rel.clone();
        let rec = update_run(&dir, |r| {
            r.status = status;
            r.finished_at = Some(at.clone());
            r.stalled_since = None;
            r.pgid = None;
            r.disk_bytes = Some(bytes);
            // A completed round: the next interruption may resume by itself again, and this
            // round got past any usage limit.
            r.restart_resumed = false;
            r.limit_pushed = false;
            r.resume_at = None;
            for d in &derived {
                let d = d.to_string_lossy().into_owned();
                if !r.derived_data.contains(&d) {
                    r.derived_data.push(d);
                }
            }
            if let Some(cur) = r.round_mut() {
                cur.report_path = Some(rel2.clone());
                cur.finished_at = Some(at.clone());
                cur.outcome = Some(status);
                cur.exit_code = exit.code;
                cur.signal = exit.signal;
            }
        })?;
        if matches!(status, RunStatus::Reported | RunStatus::NeedsYou) {
            self.lift_limit(now_ms());
        }
        if rec.round().is_some_and(|r| r.notified) {
            return Ok(());
        }
        if let Some(n) = &sink.notifier {
            let title = format!("Run {run_id} {}", status.outcome());
            let body: String = first_line(&report).chars().take(180).collect();
            n.notify(title, body, run_id.to_string()).await;
        }
        if let Some(note) = &rec.strand {
            self.send_turn(report_turn_text(run_id, note, &rel)).await;
        }
        update_run(&dir, |r| {
            if let Some(cur) = r.round_mut() {
                cur.notified = true;
            }
        })?;
        Ok(())
    }

    /// A round ended on the plan's usage limit: the run is `paused` until the reset the event
    /// names (else an hour from now), with one push for the whole pause and no Inbox report.
    async fn pause_for_limit(
        &self,
        dir: &Path,
        rec: &RunRecord,
        exit: &ExitInfo,
        hit: stream::UsageLimit,
        sink: &RunSink,
    ) -> Result<(), String> {
        let now = now_ms();
        let resume = hit
            .resets_at_ms
            .filter(|&t| t > now)
            .unwrap_or(now.saturating_add(LIMIT_RETRY_MS));
        let resume_at = rfc(resume);
        let at = exit.ended_at.clone();
        let (code, signal) = (exit.code, exit.signal);
        let updated = update_run(dir, |r| {
            r.status = RunStatus::Paused;
            r.resume_at = Some(resume_at.clone());
            r.finished_at = Some(at.clone());
            r.waiting_for = None;
            r.stalled_since = None;
            r.pgid = None;
            r.restart_resumed = false;
            if let Some(cur) = r.round_mut() {
                cur.finished_at = Some(at.clone());
                cur.outcome = Some(RunStatus::Paused);
                cur.exit_code = code;
                cur.signal = signal;
                cur.notified = true;
            }
        })?;
        eprintln!(
            "jesse-bridge: hostrun {} PAUSED for the usage limit until {resume_at}",
            rec.run_id
        );
        if !updated.limit_pushed {
            update_run(dir, |r| r.limit_pushed = true)?;
            if let Some(n) = &sink.notifier {
                n.notify(
                    "Run paused".into(),
                    format!(
                        "{} paused for usage limit until {}",
                        rec.run_id,
                        local_clock(resume, now)
                    ),
                    rec.run_id.clone(),
                )
                .await;
            }
        }
        Ok(())
    }

    /// A round got past the usage limit: every run paused for it resumes at the next pass.
    fn lift_limit(&self, now: u64) {
        let _gate = lock(&self.gate);
        let mut any = false;
        for (dir, r) in self.runs() {
            if r.status == RunStatus::Paused && ms_opt(&r.resume_at).is_some_and(|t| t > now) {
                let at = rfc(now);
                let _ = update_run(&dir, |x| x.resume_at = Some(at.clone()));
                any = true;
            }
        }
        if any {
            eprintln!(
                "jesse-bridge: hostrun a round got past the usage limit; resuming paused runs"
            );
            self.wake.notify_one();
        }
    }

    /// One watcher pass at `now`.
    pub async fn pass(self: &Arc<Self>, now: u64) {
        let this = self.clone();
        let (effects, finished) = tokio::task::spawn_blocking(move || this.scan(now))
            .await
            .unwrap_or_default();
        self.send_effects(effects).await;
        for id in finished {
            if let Err(e) = self.finalize(&id).await {
                eprintln!("jesse-bridge: hostrun {id}: report not written yet: {e}");
            }
        }
        let this = self.clone();
        let _ = tokio::task::spawn_blocking(move || this.close_settled(now)).await;
        let this = self.clone();
        let _ = tokio::task::spawn_blocking(move || this.resume_due(now)).await;
        let effects = self.pump_blocking().await;
        self.send_effects(effects).await;
        let (cleanup_due, reconcile_due) = {
            let h = lock(&self.house);
            (
                h.last_cleanup_ms
                    .is_none_or(|t| now.saturating_sub(t) >= CLEANUP_EVERY_MS),
                h.last_reconcile_ms
                    .is_none_or(|t| now.saturating_sub(t) >= RECONCILE_EVERY_MS),
            )
        };
        if reconcile_due {
            let this = self.clone();
            let _ = tokio::task::spawn_blocking(move || this.reconcile(now)).await;
        }
        if cleanup_due {
            let this = self.clone();
            let effects = tokio::task::spawn_blocking(move || this.cleanup(now))
                .await
                .unwrap_or_default();
            self.send_effects(effects).await;
        }
    }

    /// The watcher: a pass now, then one every [`WATCH_INTERVAL`] or whenever something wakes it.
    pub async fn run_watcher(self: Arc<Self>) {
        loop {
            self.pass(now_ms()).await;
            tokio::select! {
                _ = tokio::time::sleep(WATCH_INTERVAL) => {}
                _ = self.wake.notified() => {}
            }
        }
    }

    pub fn spawn_watcher(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(self.clone().run_watcher())
    }

    // ---- CLEANUP ---------------------------------------------------------------------

    /// Remove a run's build artifacts: `target/`, the DerivedData folders it recorded and, once
    /// the run is no longer live, `extra/` (the further repositories it cloned).
    fn remove_artifacts(&self, dir: &Path, rec: &RunRecord, now: u64) -> u64 {
        let mut freed = 0;
        let mut tiers = vec![TARGET_DIR];
        if !rec.status.is_live() {
            tiers.push(EXTRA_DIR);
        }
        for tier in tiers {
            let p = dir.join(tier);
            if p.exists() {
                freed += sys::tree_bytes(&p);
                if let Err(e) = sys::remove_tree(&p) {
                    eprintln!(
                        "jesse-bridge: hostrun {}: could not remove {tier}/: {e}",
                        rec.run_id
                    );
                }
            }
        }
        for d in &rec.derived_data {
            let p = PathBuf::from(d);
            // Only a folder directly under the DerivedData root, which the run recorded.
            if p.parent() != Some(self.settings.derived_data_root.as_path()) || !p.exists() {
                continue;
            }
            freed += sys::tree_bytes(&p);
            if let Err(e) = sys::remove_tree(&p) {
                eprintln!(
                    "jesse-bridge: hostrun {}: could not remove {d}: {e}",
                    rec.run_id
                );
            }
        }
        let at = rfc(now);
        let _ = update_run(dir, |r| {
            r.cleanup.artifacts_removed = Some(at.clone());
            r.cleanup.kept = false;
        });
        freed
    }

    fn has_artifacts(&self, dir: &Path, rec: &RunRecord) -> bool {
        dir.join(TARGET_DIR).exists()
            || (!rec.status.is_live() && dir.join(EXTRA_DIR).exists())
            || rec.derived_data.iter().any(|d| {
                Path::new(d).parent() == Some(self.settings.derived_data_root.as_path())
                    && Path::new(d).exists()
            })
    }

    /// Remove a run's worktree and its local branch.
    fn remove_worktree(&self, dir: &Path, rec: &RunRecord, now: u64) -> u64 {
        let wt = dir.join(WORKTREE_DIR);
        let freed = sys::tree_bytes(&wt);
        match git::remove_worktree(&self.settings.root, &rec.repo, &wt, &rec.branch) {
            Ok(()) => {
                let at = rfc(now);
                let _ = update_run(dir, |r| {
                    r.cleanup.worktree_removed = Some(at.clone());
                    r.cleanup.kept = false;
                });
                eprintln!("jesse-bridge: hostrun {}: worktree removed", rec.run_id);
                freed
            }
            Err(e) => {
                eprintln!("jesse-bridge: hostrun {}: {e}", rec.run_id);
                0
            }
        }
    }

    /// Whether a finished run's pull request is merged or closed, or its branch (seen on origin
    /// before) is gone from it. Records a sighting of the branch.
    fn merged_or_gone(&self, dir: &Path, rec: &RunRecord) -> bool {
        if matches!(
            self.forge.pr_state(&rec.repo, &rec.branch),
            Some(PrState::Merged | PrState::Closed)
        ) {
            return true;
        }
        match git::branch_on_origin(&self.settings.root, &rec.repo, &rec.branch) {
            Some(true) => {
                if !rec.cleanup.branch_seen_on_origin {
                    let _ = update_run(dir, |r| r.cleanup.branch_seen_on_origin = true);
                }
                false
            }
            Some(false) => rec.cleanup.branch_seen_on_origin,
            None => false,
        }
    }

    /// The hourly cleanup. Build artifacts go early, worktrees later, run directories never.
    pub fn cleanup(&self, now: u64) -> Vec<Effect> {
        let mut effects = Vec::new();
        lock(&self.house).last_cleanup_ms = Some(now);
        let _gate = lock(&self.gate);
        for (dir, rec) in self.runs() {
            if !rec.status.is_finished() {
                continue;
            }
            let Some(fin) = ms_opt(&rec.finished_at) else {
                continue;
            };
            let age = now.saturating_sub(fin);
            let past_keep = age >= self.settings.keep_days * DAY_MS;
            let wt = dir.join(WORKTREE_DIR);
            let artifacts = self.has_artifacts(&dir, &rec);
            let terminal = rec.status.is_terminal();
            let gone = terminal && (artifacts || wt.exists()) && self.merged_or_gone(&dir, &rec);
            if artifacts && (gone || past_keep) {
                self.remove_artifacts(&dir, &rec, now);
            }
            if wt.exists() {
                if terminal && (gone || past_keep) {
                    self.remove_worktree(&dir, &rec, now);
                } else if !terminal && age >= PAUSED_KEEP_DAYS * DAY_MS {
                    match &rec.cleanup.worktree_warned {
                        None => {
                            let at = rfc(now);
                            let _ =
                                update_run(&dir, |r| r.cleanup.worktree_warned = Some(at.clone()));
                            effects.push(Effect::Push {
                                title: "Run worktree removed".into(),
                                body: format!(
                                    "{} has waited {} days as {}; its worktree is removed at the next cleanup",
                                    rec.run_id,
                                    PAUSED_KEEP_DAYS,
                                    rec.status.label().replace('_', " ")
                                ),
                                run_id: rec.run_id.clone(),
                            });
                        }
                        Some(_) => {
                            self.remove_worktree(&dir, &rec, now);
                        }
                    }
                }
            }
            if age >= GZIP_AFTER_DAYS * DAY_MS && dir.join(STREAM).is_file() {
                let ok = std::process::Command::new("gzip")
                    .arg("-f")
                    .arg(dir.join(STREAM))
                    .status()
                    .is_ok_and(|s| s.success());
                if ok {
                    let at = rfc(now);
                    let _ = update_run(&dir, |r| r.cleanup.stream_gzipped = Some(at.clone()));
                }
            }
        }
        drop(_gate);
        self.space_pass(now);
        effects
    }

    /// When the runs together exceed `max_total_gb`, or free space is under `min_free_gb`, remove
    /// build artifacts of finished runs oldest first, then worktrees of reported, failed and
    /// cancelled runs oldest first, until both hold. Never a running or queued run, nor a paused
    /// run's worktree.
    pub fn space_pass(&self, now: u64) {
        let _gate = lock(&self.gate);
        self.space_pass_locked(now);
    }

    fn space_pass_locked(&self, now: u64) {
        let mut total = sys::tree_bytes(&self.settings.root);
        let free = || sys::free_bytes(&self.settings.root);
        let over = |total: u64| {
            total > self.settings.max_total_bytes
                || free().is_some_and(|f| f < self.settings.min_free_bytes)
        };
        let mut freed = 0u64;
        if over(total) {
            let mut finished: Vec<(PathBuf, RunRecord)> = self
                .runs()
                .into_iter()
                .filter(|(_, r)| r.status.is_finished())
                .collect();
            finished.sort_by(|a, b| {
                let fa = a.1.finished_at.as_deref().unwrap_or("");
                let fb = b.1.finished_at.as_deref().unwrap_or("");
                fa.cmp(fb).then(a.1.run_id.cmp(&b.1.run_id))
            });
            for (dir, rec) in &finished {
                if !over(total) {
                    break;
                }
                if self.has_artifacts(dir, rec) {
                    let f = self.remove_artifacts(dir, rec, now);
                    total = total.saturating_sub(f);
                    freed += f;
                }
            }
            for (dir, rec) in &finished {
                if !over(total) {
                    break;
                }
                if rec.status.is_terminal() && dir.join(WORKTREE_DIR).exists() {
                    let f = self.remove_worktree(dir, rec, now);
                    total = total.saturating_sub(f);
                    freed += f;
                }
            }
            eprintln!(
                "jesse-bridge: hostrun SPACE PASS freed {} (runs now hold {})",
                human_bytes(freed),
                human_bytes(total)
            );
        }
        let mut h = lock(&self.house);
        h.last_total_bytes = Some(total);
        if freed > 0 || h.last_space_pass.is_none() {
            h.last_space_pass = Some(SpacePass {
                at: rfc(now),
                freed_bytes: freed,
                total_bytes: total,
                free_bytes: free(),
            });
        }
    }

    /// At start and daily: repair only what is provably this module's own. A worktree a bare
    /// clone registers whose path is gone is pruned; a run label whose plist is under this
    /// root's `runs/` but whose run directory is gone is booted out; a directory under `runs/`
    /// with no `run.json` is reported, never deleted.
    pub fn reconcile(&self, now: u64) {
        lock(&self.house).last_reconcile_ms = Some(now);
        for repo in git::cloned_repos(&self.settings.root) {
            let bare = git::bare_path(&self.settings.root, &repo);
            if git::registered_worktrees(&bare).iter().any(|p| !p.exists()) {
                match git::prune(&self.settings.root, &repo) {
                    Ok(()) => eprintln!("jesse-bridge: hostrun RECONCILE pruned {repo}"),
                    Err(e) => eprintln!("jesse-bridge: hostrun RECONCILE {repo}: {e}"),
                }
            }
        }
        // A label is this root's only when launchd says its plist is under `<root>/runs/`: a
        // run of another root (an old `root`, a test) is never touched.
        let runs_dir = self.settings.runs_dir();
        let prefix = self.settings.label_prefix.clone();
        for label in self.launcher.loaded_labels(&prefix) {
            let id = &label[prefix.len()..];
            let ours = self
                .launcher
                .plist_of(&label)
                .is_some_and(|p| sys::is_under(&p, &runs_dir));
            if ours && (!valid_run_id(id) || !self.settings.run_dir(id).join(RUN_JSON).is_file()) {
                match self.launcher.bootout(&label) {
                    Ok(()) => eprintln!("jesse-bridge: hostrun RECONCILE booted out {label}"),
                    Err(e) => eprintln!("jesse-bridge: hostrun RECONCILE {label}: {e}"),
                }
            }
        }
        let _gate = lock(&self.gate);
        let (_, orphans) = scan_runs(&self.settings.runs_dir());
        for o in &orphans {
            eprintln!(
                "jesse-bridge: hostrun RECONCILE orphan {}: a run directory with no run.json; kept",
                self.settings.run_dir(o).display()
            );
        }
        lock(&self.house).orphans = orphans;
    }

    // ---- READING ---------------------------------------------------------------------

    /// `GET /jesse/runs`.
    pub fn list_json(&self) -> Value {
        let mut runs: Vec<RunRecord> = self.runs().into_iter().map(|(_, r)| r).collect();
        runs.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then(b.run_id.cmp(&a.run_id))
        });
        let count = |s: RunStatus| runs.iter().filter(|r| r.status == s).count();
        let counts = json!({
            "running": count(RunStatus::Running),
            "queued": count(RunStatus::Queued),
            "needs_you": count(RunStatus::NeedsYou),
            "paused": count(RunStatus::Paused),
        });
        let h = lock(&self.house);
        json!({
            "runs": runs.iter().map(run_json).collect::<Vec<_>>(),
            "counts": counts,
            "disk": {
                "total_bytes": h.last_total_bytes,
                "free_bytes": sys::free_bytes(&self.settings.root),
                "last_space_pass": h.last_space_pass,
            },
            "orphans": h.orphans,
        })
    }

    /// The readable log of a run, its last `tail` lines.
    pub fn log_lines(&self, run_id: &str, tail: usize) -> Option<Vec<String>> {
        if !valid_run_id(run_id) {
            return None;
        }
        let dir = self.settings.run_dir(run_id);
        if !dir.join(RUN_JSON).is_file() {
            return None;
        }
        Some(stream::tail_readable(
            &stream::read_stream(&dir),
            tail.clamp(1, LOG_MAX),
        ))
    }
}

/// A run id names one directory under `runs/` and nothing else.
pub fn valid_run_id(id: &str) -> bool {
    devjob::valid_label_value(id) && !id.contains("..")
}

/// One run for the routes and the tools: `run.json`, `host`, and its report paths.
pub fn run_json(r: &RunRecord) -> Value {
    let mut v = serde_json::to_value(r).unwrap_or_else(|_| json!({}));
    if let Some(o) = v.as_object_mut() {
        o.insert("host".into(), json!("studio"));
        o.insert(
            "report_paths".into(),
            json!(r
                .rounds
                .iter()
                .filter_map(|x| x.report_path.clone())
                .collect::<Vec<_>>()),
        );
    }
    v
}

/// What `POST /jesse/runs` answers: the run, its status and why it waits, if it does.
pub fn started_json(s: &StartedRun) -> Value {
    json!({ "run_id": s.run_id, "status": s.status, "waiting_for": s.waiting_for })
}

/// What `GET /jesse/runs/:run_id/log` answers.
pub fn log_json(run_id: &str, lines: Vec<String>) -> Value {
    json!({ "run_id": run_id, "lines": lines })
}

/// `18:00` in local time, or `Nov 10 18:00` when not today.
pub fn local_clock(ms: u64, now: u64) -> String {
    use chrono::TimeZone;
    let at = chrono::Local.timestamp_millis_opt(ms as i64).single();
    let today = chrono::Local.timestamp_millis_opt(now as i64).single();
    match (at, today) {
        (Some(a), Some(t)) if a.date_naive() == t.date_naive() => a.format("%H:%M").to_string(),
        (Some(a), _) => a.format("%b %-d %H:%M").to_string(),
        _ => rfc(ms),
    }
}

/// `12.3 GB`.
pub fn human_bytes(b: u64) -> String {
    format!("{:.1} GB", b as f64 / GB as f64)
}

// ---- THE STRANDS SNAPSHOT ------------------------------------------------------------------

/// Add the Studio fields to `GET /jesse/strands`: each strand's `runs_active` (its running,
/// needs-you and paused runs), `runs_needs_you` (those of them waiting on Jeremy), and
/// `drafts`, its items each with `studio_runnable` (whether a person's start with no `repo`
/// would be accepted). Additive only.
pub fn annotate_strands(
    value: &mut Value,
    snap: &crate::strands::StrandsSnapshot,
    notes_root: &Path,
    service: Option<&HostRuns>,
) {
    let Some(list) = value.get_mut("strands").and_then(Value::as_array_mut) else {
        return;
    };
    let runs: Vec<RunRecord> = service
        .map(|s| s.runs().into_iter().map(|(_, r)| r).collect())
        .unwrap_or_default();
    for (v, strand) in list.iter_mut().zip(snap.strands.iter()) {
        let Some(obj) = v.as_object_mut() else {
            continue;
        };
        let active = runs
            .iter()
            .filter(|r| {
                r.strand.as_deref() == Some(strand.slug.as_str())
                    && matches!(
                        r.status,
                        RunStatus::Running | RunStatus::NeedsYou | RunStatus::Paused
                    )
            })
            .count();
        obj.insert("runs_active".into(), json!(active as u32));
        let needs_you = runs
            .iter()
            .filter(|r| {
                r.strand.as_deref() == Some(strand.slug.as_str()) && r.status == RunStatus::NeedsYou
            })
            .count();
        obj.insert("runs_needs_you".into(), json!(needs_you as u32));
        let runnable = |item: &crate::strands::StrandItem| {
            service.is_some()
                && studio_item_of(
                    notes_root,
                    &strand.slug,
                    item,
                    &strand.repos,
                    OptIn::Request { repo: None },
                )
                .is_ok()
        };
        let drafts: Vec<Value> = strand
            .items
            .iter()
            .map(|i| {
                json!({
                    "id": i.id,
                    "text": i.text,
                    "section": match i.section {
                        StrandSection::Queue => "queue",
                        StrandSection::Later => "later",
                        StrandSection::Running => "running",
                        StrandSection::Done => "done",
                    },
                    "checked": i.checked,
                    "studio_runnable": runnable(i),
                })
            })
            .collect();
        obj.insert("drafts".into(), json!(drafts));
    }
}

// ---- THE ROUTES ----------------------------------------------------------------------

fn service_of(st: &AppState) -> Result<Arc<HostRuns>, ApiError> {
    st.cfg.hostrun.service().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "[hostrun] is not enabled on this bridge".to_string(),
    ))
}

fn guard(st: &AppState, headers: &HeaderMap) -> Result<Arc<HostRuns>, ApiError> {
    check_auth(headers, &st.cfg.token)?;
    if !st.limiter.allow() {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded".to_string(),
        ));
    }
    service_of(st)
}

fn refusal(r: Refusal) -> ApiError {
    (r.status(), r.message().to_string())
}

/// `POST /jesse/runs`: a strand item (`strand` and `item_id`), or a draft by its path
/// (`draft`). `repo` names the repository when the draft does not.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartBody {
    pub strand: Option<String>,
    pub item_id: Option<String>,
    pub draft: Option<String>,
    pub repo: Option<String>,
    #[serde(default)]
    pub again: bool,
}

impl StartBody {
    /// What the body asks for, or why it is not well formed.
    pub fn kind(self) -> Result<StartKind, String> {
        let repo = match self.repo.as_deref().map(str::trim) {
            None => None,
            Some(r) if valid_repo(r) => Some(r.to_string()),
            Some(r) => return Err(format!("repo {r:?} is not owner/name")),
        };
        match (self.strand, self.item_id, self.draft) {
            (Some(strand), Some(item_id), None) => Ok(StartKind::Item {
                strand: strand.trim().to_string(),
                item_id: item_id.trim().to_string(),
                repo,
                scheduled: false,
            }),
            (None, None, Some(draft)) => Ok(StartKind::Draft { draft, repo }),
            (_, _, Some(_)) => Err("draft goes alone, without strand and item_id".into()),
            _ => Err("pass strand and item_id, or draft".into()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplyBody {
    pub text: String,
}

/// `POST /jesse/runs/:run_id/close`: an optional `{ "reason": "..." }`.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CloseBody {
    pub reason: Option<String>,
}

impl CloseBody {
    /// An empty body is no reason; anything else must be the JSON object.
    pub fn parse(bytes: &[u8]) -> Result<CloseBody, String> {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(CloseBody::default());
        }
        serde_json::from_slice(bytes).map_err(|e| format!("bad body: {e}"))
    }
}

#[derive(Deserialize)]
pub struct LogQuery {
    pub tail: Option<usize>,
}

/// `GET /jesse/runs`.
pub async fn jesse_runs(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    let v = tokio::task::spawn_blocking(move || svc.list_json())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(v).into_response())
}

/// `POST /jesse/runs` with `{ strand, item_id, repo?, again? }` or `{ draft, repo?, again? }`.
pub async fn jesse_runs_start(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<StartBody>,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    let again = body.again;
    let kind = body.kind().map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let started = svc
        .start(StartRequest {
            kind,
            origin: "app".into(),
            again,
        })
        .await
        .map_err(refusal)?;
    Ok(Json(started_json(&started)).into_response())
}

/// `GET /jesse/runs/:run_id`.
pub async fn jesse_run(
    State(st): State<AppState>,
    UrlPath(run_id): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    match svc.get(&run_id) {
        Some(r) => Ok(Json(run_json(&r)).into_response()),
        None => Err((StatusCode::NOT_FOUND, format!("no run {run_id:?}"))),
    }
}

/// `GET /jesse/runs/:run_id/log?tail=N`.
pub async fn jesse_run_log(
    State(st): State<AppState>,
    UrlPath(run_id): UrlPath<String>,
    Query(q): Query<LogQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    let tail = q.tail.unwrap_or(LOG_DEFAULT);
    let id = run_id.clone();
    let lines = tokio::task::spawn_blocking(move || svc.log_lines(&id, tail))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    match lines {
        Some(l) => Ok(Json(log_json(&run_id, l)).into_response()),
        None => Err((StatusCode::NOT_FOUND, format!("no run {run_id:?}"))),
    }
}

/// `POST /jesse/runs/:run_id/reply` with `{ text }`.
pub async fn jesse_run_reply(
    State(st): State<AppState>,
    UrlPath(run_id): UrlPath<String>,
    headers: HeaderMap,
    Json(body): Json<ReplyBody>,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    let r = svc
        .reply_and_pump(&run_id, &body.text)
        .await
        .map_err(refusal)?;
    Ok(Json(run_json(&r)).into_response())
}

/// `POST /jesse/runs/:run_id/cancel`.
pub async fn jesse_run_cancel(
    State(st): State<AppState>,
    UrlPath(run_id): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    let r = svc.cancel_and_wake(&run_id).await.map_err(refusal)?;
    Ok(Json(run_json(&r)).into_response())
}

/// `POST /jesse/runs/:run_id/close` with an optional `{ reason }`.
pub async fn jesse_run_close(
    State(st): State<AppState>,
    UrlPath(run_id): UrlPath<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    let body = CloseBody::parse(&body).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let r = svc
        .close_blocking(&run_id, body.reason)
        .await
        .map_err(refusal)?;
    Ok(Json(run_json(&r)).into_response())
}

#[cfg(test)]
mod tests;
