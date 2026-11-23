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

pub mod cloud;
pub mod git;
pub mod review;
pub mod runner;
pub mod stream;
pub mod sys;
pub mod work;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

use serde::Serialize;

use crate::devjob::{is_local_repo, local_repo, valid_repo, valid_slug, Notifier};
use crate::strandjobs::{fm, DRAFTS_DIR};
use crate::strands::{parse_strand, resolve_target, StrandSection, ARCHIVE_SEGMENT, STRANDS_DIR};
use crate::*;

use self::review::{Finding, Review, ReviewStatus, AUTO_REVIEW};
use self::sys::{Forge, Launcher, PrState};

// ---- CONSTANTS -----------------------------------------------------------------------

/// The default `[hostrun] root`, under the bridge user's home.
pub const DEFAULT_ROOT: &str = "jesse-runs";
/// The default `[hostrun] local_checkout_root`, under the bridge user's home: where a plain,
/// readable checkout of each local repository's `main` is kept.
pub const DEFAULT_LOCAL_CHECKOUT_ROOT: &str = "jesse/Code/local";
/// The local repository a loose draft that names none goes to.
pub const LOCAL_SCRATCH: &str = "scratch";
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
/// `closed_reason` of a queued run a free cloud slot took (`devjob-host: any`), then ` as
/// <job>` once its Job exists. See `docs/devjob-cloud.md`.
pub const MOVED_TO_CLOUD: &str = "moved to the cloud";
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
/// `archived_reason` when Jeremy archives a run, and the `closed_reason` of a `needs_you` or
/// `interrupted` run his archive closes first.
pub const ARCHIVED_BY_JEREMY: &str = "archived by Jeremy";
/// `archived_reason` when the run's Drafts line is checked or sits under `### Done`.
pub const ARCHIVED_ITEM_TICKED: &str = "item ticked";
/// `archived_reason` when every report of the run has moved to `Inbox/archive/`.
pub const ARCHIVED_REPORT_ARCHIVED: &str = "report archived";
/// A finished run is archived by itself this many days after its last status change.
pub const AUTO_ARCHIVE_AFTER_DAYS: u64 = 14;
/// `archived_reason` when the run finished more than [`AUTO_ARCHIVE_AFTER_DAYS`] ago.
pub const ARCHIVED_AGED: &str = "finished over 14 days ago";

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
    /// Where the readable checkout of each local repository's `main` is kept. Default
    /// [`DEFAULT_LOCAL_CHECKOUT_ROOT`] under the home; `~` is expanded.
    pub local_checkout_root: Option<PathBuf>,
    /// How long one reviewer answer may take, in minutes. Default
    /// [`review::DEFAULT_REVIEW_TIMEOUT_MINUTES`].
    pub review_timeout_minutes: Option<u64>,
    /// Who reviews a Studio run that names no reviewer and does not say `none`: registry ids,
    /// at most [`review::MAX_REVIEWERS`]. Default [`review::DEFAULT_REVIEWERS`]; `[]` turns the
    /// default off for the whole bridge.
    pub default_reviewers: Option<Vec<String>>,
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
    /// `<local_checkout_root>/<name>` is a plain checkout of `local/<name>`'s `main`, brought
    /// up to date after each finished round of a run on it.
    pub local_checkout_root: PathBuf,
    /// How long one reviewer answer may take, in minutes.
    pub review_timeout_minutes: u64,
    /// Who reviews a run that names no reviewer ([`HostRunToml::default_reviewers`]).
    pub default_reviewers: Vec<String>,
    pub service: HostRunSlot,
    /// `<state_dir>/attribution`: the bridge's git hooks and Claude Code settings, which every
    /// round's agent is pointed at ([`crate::attribution`]). `None` without a state dir.
    pub attribution_dir: Option<PathBuf>,
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
            local_checkout_root: match t.local_checkout_root {
                Some(p) => expand_home(&p, home),
                None => Path::new(home).join(DEFAULT_LOCAL_CHECKOUT_ROOT),
            },
            review_timeout_minutes: t
                .review_timeout_minutes
                .unwrap_or(review::DEFAULT_REVIEW_TIMEOUT_MINUTES),
            default_reviewers: t.default_reviewers.unwrap_or_else(|| {
                review::DEFAULT_REVIEWERS
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            }),
            service: HostRunSlot::default(),
            attribution_dir: None,
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

    /// The readable checkout of a local repository's `main`: `<local_checkout_root>/<name>`.
    pub fn local_checkout(&self, repo: &str) -> Option<PathBuf> {
        if !is_local_repo(repo) {
            return None;
        }
        let (_, name) = repo.split_once('/')?;
        Some(self.local_checkout_root.join(name))
    }

    /// The clone URL of `owner/name`. Never asked for a local repository, which has none.
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
        if self.review_timeout_minutes == 0 {
            out.push("[hostrun] review_timeout_minutes must be at least 1".into());
        }
        // `[]` is allowed (no default review); `["none"]` would be the same thing said
        // ambiguously, and is refused with the rest.
        if !self.default_reviewers.is_empty() {
            let arg = review::ReviewerArg::Many(self.default_reviewers.clone());
            match review::choose(Some(&arg)) {
                Ok(review::ReviewerChoice::Ids(_)) => {}
                Ok(_) => out.push(
                    "[hostrun] default_reviewers: write [] for no default review, not \"none\""
                        .into(),
                ),
                Err(e) => out.push(format!("[hostrun] default_reviewers: {e}")),
            }
        }
        let lcr = &self.local_checkout_root;
        if !lcr.is_absolute() {
            out.push(format!(
                "[hostrun] local_checkout_root {} is not an absolute path",
                lcr.display()
            ));
        } else if sys::is_under(lcr, &self.root) || sys::is_under(&self.root, lcr) {
            // Bridge sessions read the checkout and must not read the run root.
            out.push(format!(
                "[hostrun] local_checkout_root {} overlaps root {}",
                lcr.display(),
                self.root.display()
            ));
        } else if let Err(e) = std::fs::create_dir_all(lcr) {
            out.push(format!(
                "[hostrun] local_checkout_root {} is unwritable: {e}",
                lcr.display()
            ));
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
    /// A second model is reading the run's work ([`review`]); the bridge hands its findings to
    /// the run's session when it answers. Moving, like `running`: nothing to reply to yet.
    Reviewing,
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
            RunStatus::Reviewing => "reviewing",
        }
    }

    /// What the Inbox front matter's `outcome` says.
    pub fn outcome(self) -> &'static str {
        match self {
            RunStatus::NeedsYou => "needs you",
            other => other.label(),
        }
    }

    /// Queued, running, waiting on Jeremy, paused for the usage limit or being reviewed: a
    /// second start of the item is refused.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            RunStatus::Queued
                | RunStatus::Running
                | RunStatus::NeedsYou
                | RunStatus::Paused
                | RunStatus::Reviewing
        )
    }

    /// Reported, failed, cancelled or closed: done with, unless someone replies.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Reported | RunStatus::Failed | RunStatus::Cancelled | RunStatus::Closed
        )
    }

    /// Not queued, running or paused (those are refused), nor waiting on Jeremy or cut off
    /// (an archive closes those first): what may carry `archived_at` and leave the list.
    pub fn is_archivable(self) -> bool {
        self.is_terminal()
    }

    /// Needs Jeremy: waiting on him, cut off, or failed. The list's `needs_me` count.
    pub fn needs_me(self) -> bool {
        matches!(
            self,
            RunStatus::NeedsYou | RunStatus::Interrupted | RunStatus::Failed
        )
    }

    /// Queued, running, paused or reviewing: the list's `moving` count.
    pub fn is_moving(self) -> bool {
        !self.is_finished()
    }

    /// Waiting on Jeremy, cut off or failed, with nothing running: what the automatic closes
    /// (a ticked line, a later run of the item) may settle.
    pub fn is_settleable(self) -> bool {
        matches!(
            self,
            RunStatus::NeedsYou | RunStatus::Interrupted | RunStatus::Failed
        )
    }

    /// Not queued, not running, not paused (a paused run continues by itself) and not being
    /// reviewed (a reviewed one continues by itself too).
    pub fn is_finished(self) -> bool {
        !matches!(
            self,
            RunStatus::Queued | RunStatus::Running | RunStatus::Paused | RunStatus::Reviewing
        )
    }

    /// Running, waiting on Jeremy, paused or being reviewed: what a strand's `runs_active`
    /// counts.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            RunStatus::Running | RunStatus::NeedsYou | RunStatus::Paused | RunStatus::Reviewing
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
    /// The push before a finished run's `extra/` goes, a day before `keep_days` is up. Left out
    /// of the JSON until set, so the app's wire fixtures do not move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_warned: Option<String>,
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
    /// what Jeremy gave. Only while `closed`; absent from records written before 0.200.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_reason: Option<String>,
    /// When the run was closed (RFC 3339). Only while `closed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    /// When the run left the runs list (RFC 3339): archived by Jeremy or by the watcher. A flag
    /// only: the run's directory, log and reports stay where they are. Absent from records
    /// written before 0.204.0, and after an unarchive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// Why it was archived: [`ARCHIVED_BY_JEREMY`], [`ARCHIVED_ITEM_TICKED`],
    /// [`ARCHIVED_REPORT_ARCHIVED`] or [`ARCHIVED_AGED`]. Set exactly with `archived_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_reason: Option<String>,
    /// A REVIEWED run's first reviewer: a registry id, from the start's `reviewer`, the
    /// draft's `devjob-reviewer:` or `[hostrun] default_reviewers`. Its implementing round is
    /// reviewed once before Jeremy hears of it. Every reviewer is in `reviewers`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    /// Every reviewer's label when the run was started, joined with ` + ` (`Codex + GLM 5`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_label: Option<String>,
    /// Every reviewer, in order (since 0.213.0; before it, `reviewer` alone).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewers: Vec<String>,
    /// Each reviewer's own label, beside `reviewers`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewer_labels: Vec<String>,
    /// Where the reviewers came from: [`review::SOURCE_EXPLICIT`] (the start or the draft),
    /// [`review::SOURCE_DEFAULT`] or [`review::SOURCE_NONE`]. Absent before 0.213.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewers_source: Option<String>,
    /// Default reviewers that could not review when the run was started, and why: the run went
    /// on without them, and its final report's first line says so.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewers_skipped: Vec<review::Skipped>,
    /// What the start asked the reviewer to look at most closely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_focus: Option<String>,
    /// Every review of the run, oldest first ([`review::Review`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviews: Vec<Review>,
}

impl RunRecord {
    /// Every reviewer of a reviewed run, in order: `reviewers`, or a 0.211.0 record's one
    /// `reviewer`. Empty for a run that is not reviewed.
    pub fn reviewer_ids(&self) -> Vec<String> {
        if self.reviewers.is_empty() {
            self.reviewer.clone().into_iter().collect()
        } else {
            self.reviewers.clone()
        }
    }

    /// The label of the `i`th reviewer: `reviewer_labels`, or a 0.211.0 record's one label.
    fn reviewer_label_of(&self, i: usize) -> String {
        match self.reviewer_labels.get(i) {
            Some(l) => l.clone(),
            None if self.reviewer_labels.is_empty() && i == 0 => {
                self.reviewer_label.clone().unwrap_or_default()
            }
            None => String::new(),
        }
    }

    /// [`RunRecord::reviewers_source`], or for a record before 0.213.0 what it must have been.
    pub fn source_of_reviewers(&self) -> &str {
        match self.reviewers_source.as_deref() {
            Some(s) => s,
            None if self.reviewer.is_some() => review::SOURCE_EXPLICIT,
            None => review::SOURCE_NONE,
        }
    }

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

    /// Out of the runs list: archived, and in a status an archive accepts. A run a reply has
    /// put back in the queue (or that came back waiting on Jeremy) shows again even though
    /// `archived_at` is still set, so a live run is never hidden.
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some() && self.status.is_archivable()
    }

    /// When the run's status last changed, for the age rule: when it closed, else when it
    /// finished, else its last event, else its creation.
    pub fn settled_at(&self) -> Option<u64> {
        [&self.closed_at, &self.finished_at]
            .into_iter()
            .filter_map(ms_opt)
            .max()
            .or_else(|| ms_opt(&self.last_event_at))
            .or_else(|| ms_of(&self.created_at))
    }
}

/// Which runs `GET /jesse/runs` serves: `?archived=` absent (the list), `only` or `all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArchivedMode {
    #[default]
    Hide,
    Only,
    All,
}

impl ArchivedMode {
    pub fn parse(s: Option<&str>) -> Result<ArchivedMode, String> {
        match s {
            None | Some("") | Some("none") => Ok(ArchivedMode::Hide),
            Some("only") => Ok(ArchivedMode::Only),
            Some("all") => Ok(ArchivedMode::All),
            Some(other) => Err(format!("archived is {other:?}; it is only, all or absent")),
        }
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

/// The `extra/` warning was recorded for this finish (a reply that finishes the run again
/// moves `finished_at` past an older warning, which then no longer counts).
fn extra_warned_since(rec: &RunRecord, fin: u64) -> bool {
    ms_opt(&rec.cleanup.extra_warned).is_some_and(|w| w >= fin)
}

fn dir_has_entries(p: &Path) -> bool {
    std::fs::read_dir(p).is_ok_and(|mut d| d.next().is_some())
}

fn mark_artifacts_removed(dir: &Path, now: u64) {
    let at = rfc(now);
    let _ = update_run(dir, |r| {
        r.cleanup.artifacts_removed = Some(at.clone());
        r.cleanup.kept = false;
    });
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
    /// `keep_days`: how long after the run finishes its `extra/` is kept.
    pub keep_days: u64,
    /// Ask for `[<run_id>]` at the start of the pull request title.
    pub pr_title_tag: bool,
    /// A reviewed run: [`review::REVIEWED_RUN_SENTENCE`] (or its local form) follows the rules.
    pub reviewed: bool,
}

/// The generated section's first paragraph, exactly: where the run is, and that this replaces
/// whatever machine or directory the prompt names.
pub fn place_paragraph(p: &RunPlace) -> String {
    if is_local_repo(p.repo) {
        return format!(
            "You are running on the Mac Studio as a Studio run, in a fresh worktree of {} (Studio \
             only) on branch {}, cut from {}. Wherever the prompt above names another machine, \
             host, user or directory to work in, this worktree replaces it: do not change \
             machines, do not stop because the host differs, and do not work in any other \
             checkout of this repository. If the work needs another repository, clone it under \
             {}/{EXTRA_DIR}/ and work there. {}",
            p.repo,
            p.branch,
            p.default_branch,
            p.run_dir.display(),
            extra_sentence(p)
        );
    }
    format!(
        "You are running on the Mac Studio as a Studio run, in a fresh worktree of {} on branch \
         {}, cut from origin/{}. Wherever the prompt above names another machine, host, user or \
         directory to work in, this worktree replaces it: do not change machines, do not stop \
         because the host differs, and do not work in any other checkout of this repository. If \
         the work needs another repository, clone it under {}/{EXTRA_DIR}/ and work there. {}",
        p.repo,
        p.branch,
        p.default_branch,
        p.run_dir.display(),
        extra_sentence(p)
    )
}

/// The sentence after the `extra/` instruction: that folder is temporary.
fn extra_sentence(p: &RunPlace) -> String {
    let days = if p.keep_days == 1 {
        "1 day".to_string()
    } else {
        format!("{} days", p.keep_days)
    };
    format!(
        "Everything under {}/{EXTRA_DIR}/ is deleted {days} after the run finishes, so anything \
         a later run or a person needs must be pushed to its remote or written outside the run \
         directory before the run ends.",
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

/// What a local repository's run is told in place of the push, pull request and fetch
/// sentences, exactly.
pub const LOCAL_REPO_SENTENCE: &str = "This repository exists only on the Mac Studio and has no \
remote: there is no push, no pull request and no CI. Wherever the prompt above asks for a branch \
push, a pull request, CI or a merge, do this instead: commit on your branch, and once your \
verification passes, merge your branch into main of this repository with a fast forward (rebase \
onto main first if it moved), then report the main commit.";

/// [`generated_paragraph`] for a local repository: the same rules, with [`LOCAL_REPO_SENTENCE`]
/// where the pull request title and the fetch before a merge were.
pub fn generated_paragraph_local(run_id: &str) -> String {
    let lower = run_id.to_lowercase();
    format!(
        "This run is {run_id}. Start every branch name with {lower}-. {LOCAL_REPO_SENTENCE} Keep \
         everything you create inside your working directory or the system temporary directory. \
         When you finish, or stop early, write your complete report back, the same text you \
         would answer with, to the file named in the environment variable JESSE_RUN_REPORT, \
         then answer as usual. If you need a decision from Jeremy to continue, stop, make the \
         first line of that report NEEDS YOU: followed by the one question, and end the run."
    )
}

/// The prompt a run is served: the section byte for byte, a blank line, then the generated
/// section (where the run is, then the run's rules, then for a reviewed run
/// [`review::REVIEWED_RUN_SENTENCE`] or [`review::REVIEWED_RUN_SENTENCE_LOCAL`]) and nothing else.
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
    if is_local_repo(place.repo) {
        s.push_str(&generated_paragraph_local(place.run_id));
    } else {
        s.push_str(&generated_paragraph(place.run_id, place.pr_title_tag));
    }
    if place.reviewed {
        s.push_str("\n\n");
        s.push_str(if is_local_repo(place.repo) {
            review::REVIEWED_RUN_SENTENCE_LOCAL
        } else {
            review::REVIEWED_RUN_SENTENCE
        });
    }
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
    /// The draft's `devjob-reviewer:`, when it has one: an id, a list, or `none`.
    pub reviewer: Option<review::ReviewerArg>,
    /// The `## PROMPT` section, byte for byte.
    pub section: String,
}

/// The front matter key that makes a draft's run a reviewed one.
pub const REVIEWER_KEY: &str = "devjob-reviewer";

/// A draft's `devjob-reviewer:`: one id (`devjob-reviewer: codex-write`), a YAML list (flow,
/// `[codex-write, glm]`, or block, one `- id` per line under the key), or `none`. Checked for
/// shape ([`review::choose`]); whether the registry knows each id is asked when the run is
/// started.
pub fn draft_reviewer(path: &str, src: &str) -> Result<Option<review::ReviewerArg>, String> {
    let Some(arg) = front_matter_reviewer(src) else {
        return Ok(None);
    };
    match review::choose(Some(&arg)) {
        Ok(_) => Ok(Some(arg)),
        Err(e) => Err(format!(
            "{path}: {REVIEWER_KEY} {}",
            e.strip_prefix("reviewer ").unwrap_or(&e)
        )),
    }
}

/// One YAML scalar with one pair of surrounding quotes removed, trimmed.
fn unquote(v: &str) -> String {
    let v = v.trim();
    ['"', '\'']
        .iter()
        .find_map(|q| v.strip_prefix(*q).and_then(|r| r.strip_suffix(*q)))
        .unwrap_or(v)
        .trim()
        .to_string()
}

/// [`REVIEWER_KEY`] in a draft's front matter, as written: a scalar, a flow list or a block
/// list. `None` when the key is absent or empty.
fn front_matter_reviewer(src: &str) -> Option<review::ReviewerArg> {
    let lines: Vec<&str> = src.lines().collect();
    if lines.first().map(|l| l.trim()) != Some("---") {
        return None;
    }
    let end = lines.iter().skip(1).position(|l| l.trim() == "---")? + 1;
    let body = &lines[1..end];
    let at = body.iter().position(|l| {
        l.split_once(':')
            .is_some_and(|(k, _)| k.trim() == REVIEWER_KEY && !l.starts_with([' ', '\t']))
    })?;
    let value = body[at]
        .split_once(':')
        .map(|(_, v)| strip_comment(v))
        .unwrap_or("");
    if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        let items: Vec<String> = inner
            .split(',')
            .map(unquote)
            .filter(|s| !s.is_empty())
            .collect();
        return Some(review::ReviewerArg::Many(items));
    }
    if !value.is_empty() {
        let v = unquote(value);
        return (!v.is_empty()).then_some(review::ReviewerArg::One(v));
    }
    // A block list: one `- id` a line, blank lines and comments between them skipped, ending at
    // the next key. A key with nothing under it is no setting at all.
    let mut items: Vec<String> = Vec::new();
    for l in &body[at + 1..] {
        let t = strip_comment(l);
        if t.is_empty() {
            continue;
        }
        match t.strip_prefix('-') {
            Some(item) if l.starts_with([' ', '\t', '-']) => {
                let v = unquote(item);
                if !v.is_empty() {
                    items.push(v);
                }
            }
            _ => break,
        }
    }
    (!items.is_empty()).then_some(review::ReviewerArg::Many(items))
}

/// A front matter value without its YAML comment (` # …`, or a line that is only one), trimmed.
fn strip_comment(v: &str) -> &str {
    let t = v.trim();
    if t.starts_with('#') {
        return "";
    }
    match t.find(" #").or_else(|| t.find("\t#")) {
        Some(i) => t[..i].trim(),
        None => t,
    }
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
    /// [`OptIn::Request`], and where that names no repository because the strand lists none,
    /// the Studio only `local/<strand slug, lowercased>`. What a run started on the Studio uses.
    RequestOrLocal { repo: Option<&'a str> },
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

/// The local repository a strand that lists no repos falls back to: `local/<slug, lowercased>`.
pub fn strand_local_repo(slug: &str) -> Result<String, String> {
    local_repo(&slug.to_lowercase()).ok_or_else(|| {
        format!(
            "strand {slug} lists no repos and its slug makes no local repository name: pass repo"
        )
    })
}

/// The repository a requested run of a draft goes to: the draft's `devjob-repo`, else the
/// caller's `repo`, else the strand's `repos:` when it lists exactly one. With `local`, a strand
/// that lists none gives [`strand_local_repo`] instead of an error.
fn request_repo(
    path: &str,
    src: &str,
    repo: Option<&str>,
    strand: Option<(&str, &[String])>,
    local: bool,
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
        [] if local => strand_local_repo(slug),
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
        OptIn::Request { repo } => {
            request_repo(&path, &src, repo, Some((slug, strand_repos)), false)?
        }
        OptIn::RequestOrLocal { repo } => {
            request_repo(&path, &src, repo, Some((slug, strand_repos)), true)?
        }
    };
    let model = fm(&src, "devjob-model");
    check_model(&path, &model)?;
    let reviewer = draft_reviewer(&path, &src)?;
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
        reviewer,
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
    /// The draft's `devjob-reviewer:`, when it has one: an id, a list, or `none`.
    pub reviewer: Option<review::ReviewerArg>,
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
    studio_draft_with(notes_root, draft, repo, false)
}

/// [`studio_draft`], and where neither the draft nor `repo` names a repository, a local one: a
/// draft of a strand that lists no repos uses `local/<strand slug, lowercased>`, a loose draft
/// (no strand, or no such strand note) `local/scratch`. A draft of a strand that lists
/// repositories still needs `repo`. What a run started on the Studio uses.
pub fn studio_draft_or_local(
    notes_root: &Path,
    draft: &str,
    repo: Option<&str>,
) -> Result<StudioDraft, String> {
    studio_draft_with(notes_root, draft, repo, true)
}

/// The local repository a draft that names none falls back to, or `None` when its strand lists
/// repositories (so the caller must still pick one).
fn draft_local_repo(notes_root: &Path, src: &str) -> Result<Option<String>, String> {
    let Some(slug) = fm(src, "strand").filter(|s| crate::strands::is_safe_slug(s)) else {
        return Ok(local_repo(LOCAL_SCRATCH));
    };
    let Ok(note) = std::fs::read_to_string(notes_root.join(format!("{STRANDS_DIR}/{slug}.md")))
    else {
        return Ok(local_repo(LOCAL_SCRATCH));
    };
    if parse_strand(&slug, &note).repos.is_empty() {
        strand_local_repo(&slug).map(Some)
    } else {
        Ok(None)
    }
}

fn studio_draft_with(
    notes_root: &Path,
    draft: &str,
    repo: Option<&str>,
    local: bool,
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
    let local_fallback = if local && repo.is_none() && fm(&src, "devjob-repo").is_none() {
        draft_local_repo(notes_root, &src)?
    } else {
        None
    };
    let repo = match local_fallback {
        Some(r) => r,
        None => request_repo(&path, &src, repo, None, false)?,
    };
    let model = fm(&src, "devjob-model");
    check_model(&path, &model)?;
    let reviewer = draft_reviewer(&path, &src)?;
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
        reviewer,
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

/// A registry model, as a reviewer: what [`Reviewer::check`] answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerInfo {
    /// The registry id, canonical (an alias resolved).
    pub id: String,
    pub label: String,
    pub harness: String,
}

/// One reviewer answer to get: the model, the whole prompt, the reviewer's own worktree and the
/// longest it may take.
#[derive(Debug, Clone)]
pub struct ReviewCall {
    pub reviewer: String,
    pub prompt: String,
    pub cwd: PathBuf,
    pub timeout: Duration,
}

/// Runs a reviewer. The bridge's own ([`AppReviewer`]: the model registry, its health and its
/// harnesses), or a test's.
pub trait Reviewer: Send + Sync {
    /// Whether `id` may review now: known to the registry, configured, healthy, and on a harness
    /// with a read only posture its level allows ([`review::review_capability`]). `Err` says why
    /// not, by name.
    fn check(&self, id: &str) -> Result<ReviewerInfo, String>;
    /// One answer, through the reviewer's own harness at its read only posture.
    fn review(
        &self,
        call: ReviewCall,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;
}

/// The bridge's reviewer: any [`ModelRegistry`] entry, through whichever harness the entry names.
pub struct AppReviewer {
    pub st: AppState,
}

/// [`Reviewer::check`] against a config and a health store: the registry entry, its health, and
/// its harness's read only posture.
pub fn check_reviewer(
    cfg: &Config,
    health: &HealthStore,
    id: &str,
) -> Result<(ReviewerInfo, Capability), String> {
    let id = id.trim();
    let Some(m) = cfg.model_registry.get(id) else {
        return Err(format!("reviewer {id:?} is not a model this bridge knows"));
    };
    let h = model_health(m, health);
    if !h.configured {
        return Err(format!(
            "reviewer {:?} is not configured on this bridge",
            m.id
        ));
    }
    if !h.healthy {
        return Err(format!("reviewer {:?} is unhealthy right now", m.id));
    }
    let cap = review::review_capability(&m.harness)
        .map_err(|e| format!("reviewer {:?} cannot review: {e}", m.id))?;
    let Some(harness) = cfg.harnesses.get(&m.harness) else {
        return Err(format!(
            "reviewer {:?} names harness {:?}, which this bridge does not run",
            m.id, m.harness
        ));
    };
    if !harness.expresses(cap) {
        return Err(format!(
            "reviewer {:?} cannot review: harness {} has no {} posture",
            m.id,
            m.harness,
            capability_label(cap)
        ));
    }
    if m.level < cap {
        return Err(format!(
            "reviewer {:?} cannot review: its level is {} and a review on {} needs {}",
            m.id,
            capability_label(m.level),
            m.harness,
            capability_label(cap)
        ));
    }
    Ok((
        ReviewerInfo {
            id: m.id.clone(),
            label: m.label.clone(),
            harness: m.harness.clone(),
        },
        cap,
    ))
}

impl Reviewer for AppReviewer {
    fn check(&self, id: &str) -> Result<ReviewerInfo, String> {
        check_reviewer(&self.st.cfg, &self.st.health, id).map(|(info, _)| info)
    }

    fn review(
        &self,
        call: ReviewCall,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
        let st = self.st.clone();
        Box::pin(async move {
            let (info, cap) = check_reviewer(&st.cfg, &st.health, &call.reviewer)?;
            let Some(m) = st.cfg.model_registry.get(&info.id) else {
                return Err(format!("reviewer {:?} left the registry", info.id));
            };
            eprintln!(
                "jesse-bridge: hostrun REVIEW → model '{}' (harness {}, {})",
                m.id,
                m.harness,
                capability_label(cap)
            );
            crate::claude::run_review_child(
                &st.cfg,
                m,
                cap,
                &call.prompt,
                &call.cwd,
                call.timeout.as_secs().max(1),
            )
            .await
            .map_err(|(status, message)| format!("the reviewer failed ({status}): {message}"))
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
    render_inbox_with(rec, round, status, report, None)
}

/// [`render_inbox`], and for a local repository, after the report: where the work is, since
/// there is no pull request to point at. `local_main` is its `main`'s commit now.
pub fn render_inbox_with(
    rec: &RunRecord,
    round: u32,
    status: RunStatus,
    report: &str,
    local_main: Option<&str>,
) -> String {
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
    if is_local_repo(&rec.repo) {
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&local_footer(rec, local_main));
    }
    s
}

/// The lines a local repository's Inbox report ends with.
pub fn local_footer(rec: &RunRecord, local_main: Option<&str>) -> String {
    format!(
        "\nRepository: {} (Studio only)\nBranch: {}\nmain: {}\n",
        rec.repo,
        rec.branch,
        local_main.unwrap_or("unknown")
    )
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

/// Whether a free cloud slot may take this run ([`HostRuns::take_for_cloud`]), its draft aside:
/// queued and waiting, never started, one round, a draft, a GitHub repository.
pub fn drainable(r: &RunRecord) -> bool {
    r.status == RunStatus::Queued
        && r.waiting_for.is_some()
        && r.started_at.is_none()
        && r.rounds.len() <= 1
        && r.round()
            .is_none_or(|x| x.started_at.is_none() && x.auto.is_none())
        && r.prompt_path.is_some()
        && !is_local_repo(&r.repo)
}

/// Archive the run in `dir` with `reason` if, read again under its lock, its status is one
/// `may` accepts. An archived run keeps its first `archived_at` and reason. `Ok(Err(status))`
/// when its status had moved to one `may` refuses.
fn archive_run(
    dir: &Path,
    reason: &str,
    may: impl Fn(RunStatus) -> bool,
    now: u64,
) -> Result<Result<RunRecord, RunStatus>, String> {
    let mut refused = None;
    let rec = update_run(dir, |r| {
        if !may(r.status) {
            refused = Some(r.status);
            return;
        }
        if r.archived_at.is_none() {
            r.archived_at = Some(rfc(now));
            r.archived_reason = Some(reason.to_string());
        }
    })?;
    Ok(match refused {
        Some(s) => Err(s),
        None => Ok(rec),
    })
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

/// Move a note the bridge wrote under `Inbox/` into `Inbox/archive/` under the same name.
/// Returns where it is now; `None` when it is in neither place.
pub fn archive_note(notes_root: &Path, rel: &str) -> Option<String> {
    let name = Path::new(rel).file_name()?.to_str()?;
    if name.is_empty() || name == ".." || name == "." {
        return None;
    }
    let to_rel = format!("{INBOX_ARCHIVE_DIR}/{name}");
    let (from, to) = (notes_root.join(rel), notes_root.join(&to_rel));
    if !from.is_file() {
        return to.is_file().then_some(to_rel);
    }
    if from == to {
        return Some(to_rel);
    }
    std::fs::create_dir_all(to.parent()?).ok()?;
    match std::fs::rename(&from, &to) {
        Ok(()) => {
            eprintln!("jesse-bridge: hostrun: archived {rel} to {to_rel}");
            Some(to_rel)
        }
        Err(e) => {
            eprintln!("jesse-bridge: hostrun: could not archive {rel}: {e}");
            None
        }
    }
}

/// Who reviews a new run ([`HostRuns::resolve_reviewers`]).
struct Picked {
    infos: Vec<ReviewerInfo>,
    /// Default reviewers that cannot review now.
    skipped: Vec<review::Skipped>,
    source: &'static str,
}

/// The base, head and diff one review cycle reads ([`HostRuns::cycle_range`]).
#[derive(Debug, Clone)]
struct ReviewRange {
    base: String,
    head: String,
    diff: String,
}

/// What a finished reviewer gave: the range it read, its findings and the note.
#[derive(Debug, Clone)]
struct ReviewDone {
    head: String,
    base: String,
    findings: Vec<Finding>,
    summary: String,
    path: String,
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
    /// Who reviews the run ([`review`]): a registry id, a list of them, or `none`. Wins over
    /// the draft's `devjob-reviewer:`; with neither, `[hostrun] default_reviewers` applies.
    pub reviewer: Option<review::ReviewerArg>,
    /// What the reviewer should look at most closely; needs a reviewer.
    pub review_focus: Option<String>,
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
    /// Each run's `last_line`, kept while its stream is unchanged.
    last_lines: work::LastLines,
    /// Runs Jeremy unarchived since this bridge started: the automatic archive leaves them
    /// alone, so an Undo is not taken back by the next watcher pass. Not run state; a restart
    /// forgets it, and the rules apply again.
    unarchived: Mutex<HashSet<String>>,
    /// Who reviews ([`HostRuns::attach_reviewer`]). Until it is attached a reviewer is refused.
    reviewer: OnceLock<Arc<dyn Reviewer>>,
    /// The runs whose review task is running in this process. Not run state: a `reviewing` run
    /// with no task here (a restart) gets one at the next pass.
    reviewing: Mutex<HashSet<String>>,
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
            last_lines: work::LastLines::default(),
            unarchived: Mutex::new(HashSet::new()),
            reviewer: OnceLock::new(),
            reviewing: Mutex::new(HashSet::new()),
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

    /// Who reviews. Until this is called a start with a reviewer, and every review, is refused.
    pub fn attach_reviewer(&self, reviewer: Arc<dyn Reviewer>) {
        let _ = self.reviewer.set(reviewer);
    }

    /// Who reviews a new run, in order: the start's `reviewer`, else the draft's
    /// `devjob-reviewer:`, else `[hostrun] default_reviewers`. `none` is no review. An id the
    /// start or the draft named is checked and refused when it cannot review; when any default
    /// one cannot, the run goes on unreviewed, recording which and why.
    fn resolve_reviewers(
        &self,
        start: Option<&review::ReviewerArg>,
        draft: Option<&review::ReviewerArg>,
    ) -> Result<Picked, Refusal> {
        let mut choice = review::choose(start).map_err(Refusal::Invalid)?;
        if choice == review::ReviewerChoice::Unset {
            choice = review::choose(draft).map_err(Refusal::Invalid)?;
        }
        let mut picked = Picked {
            infos: Vec::new(),
            skipped: Vec::new(),
            source: review::SOURCE_DEFAULT,
        };
        match choice {
            review::ReviewerChoice::Off => picked.source = review::SOURCE_NONE,
            review::ReviewerChoice::Ids(ids) => {
                picked.source = review::SOURCE_EXPLICIT;
                for id in ids {
                    let info = self.check_reviewer(&id).map_err(Refusal::Invalid)?;
                    if picked.infos.iter().any(|x| x.id == info.id) {
                        return Err(Refusal::Invalid(format!(
                            "reviewer {:?} is named twice",
                            info.id
                        )));
                    }
                    picked.infos.push(info);
                }
            }
            review::ReviewerChoice::Unset => {
                for id in &self.settings.default_reviewers {
                    match self.check_reviewer(id) {
                        Ok(info) if !picked.infos.iter().any(|x| x.id == info.id) => {
                            picked.infos.push(info)
                        }
                        Ok(_) => {}
                        Err(reason) => picked.skipped.push(review::Skipped {
                            reviewer: id.clone(),
                            reason,
                        }),
                    }
                }
                // A default that cannot review leaves the run unreviewed, not reviewed by the
                // rest of the list: the run goes on as an ordinary one and says why.
                if !picked.skipped.is_empty() {
                    picked.infos.clear();
                }
            }
        }
        Ok(picked)
    }

    /// [`Reviewer::check`], or the refusal when no reviewer is attached.
    fn check_reviewer(&self, id: &str) -> Result<ReviewerInfo, String> {
        if !review::valid_reviewer_id(id.trim()) {
            return Err(format!("reviewer {id:?} is not a model id"));
        }
        match self.reviewer.get() {
            Some(r) => r.check(id.trim()),
            None => Err("this bridge has no reviewer attached".into()),
        }
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
            .filter(|(_, r)| r.strand.as_deref() == Some(strand) && r.status.is_active())
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
        let (base, repo, prompt_for, branch_text, model, step, prompt_path, draft_reviewer) =
            match &req.kind {
                StartKind::Item {
                    strand,
                    item_id,
                    repo,
                    scheduled,
                } => {
                    let opt_in = if *scheduled {
                        OptIn::Schedule
                    } else {
                        OptIn::RequestOrLocal {
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
                        item.reviewer.clone(),
                    )
                }
                StartKind::Draft { draft, repo } => {
                    let d = studio_draft_or_local(&self.notes_root, draft, repo.as_deref())
                        .map_err(Refusal::Invalid)?;
                    (
                        format!("s-{}", d.slug),
                        d.repo,
                        d.section,
                        None,
                        d.model,
                        None,
                        Some(d.prompt_path),
                        d.reviewer,
                    )
                }
                StartKind::Free {
                    repo,
                    prompt,
                    slug,
                    model,
                } => {
                    // A free prompt that names no repository goes to `local/<branch_slug>`.
                    let repo = if repo.trim().is_empty() && valid_slug(slug) {
                        local_repo(slug).unwrap_or_default()
                    } else {
                        repo.clone()
                    };
                    if !valid_repo(&repo) {
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
                        repo,
                        prompt.clone(),
                        None,
                        model.clone(),
                        None,
                        None,
                        None,
                    )
                }
            };
        // A REVIEWED run: the reviewers are resolved and checked now, before anything is
        // created ([`HostRuns::resolve_reviewers`]).
        let focus = review::checked_focus(req.review_focus.as_deref())
            .map_err(|e| Refusal::Invalid(format!("review_{e}")))?;
        let picked = self.resolve_reviewers(req.reviewer.as_ref(), draft_reviewer.as_ref())?;
        if picked.infos.is_empty() && picked.skipped.is_empty() && focus.is_some() {
            return Err(Refusal::Invalid(
                "review_focus needs a reviewer: pass reviewer too".into(),
            ));
        }
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
            reviewer: picked.infos.first().map(|r| r.id.clone()),
            reviewer_label: (!picked.infos.is_empty()).then(|| {
                picked
                    .infos
                    .iter()
                    .map(|r| r.label.as_str())
                    .collect::<Vec<_>>()
                    .join(" + ")
            }),
            reviewers: picked.infos.iter().map(|r| r.id.clone()).collect(),
            reviewer_labels: picked.infos.iter().map(|r| r.label.clone()).collect(),
            reviewers_source: Some(picked.source.to_string()),
            reviewers_skipped: picked.skipped.clone(),
            review_focus: focus,
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
            "jesse-bridge: hostrun CREATE {run_id} repo={} origin={}{}{}",
            rec.repo,
            rec.origin,
            step.as_ref()
                .map(|(s, i)| format!(" strand={s} item={i}"))
                .unwrap_or_default(),
            match rec.reviewers.as_slice() {
                [] => String::new(),
                ids => format!(" reviewers={} ({})", ids.join(","), picked.source),
            }
        );
        for sk in &rec.reviewers_skipped {
            eprintln!("jesse-bridge: hostrun {run_id}: {}", sk.line());
        }
        // A further run of the item settles the earlier ones still waiting on Jeremy, cut off
        // or failed: the question they asked is now the new run's. Queued, running and paused
        // runs go on.
        if req.again {
            if let Some((strand, item_id)) = &step {
                let reason = superseded_reason(&run_id);
                for (d, r) in self.runs() {
                    if r.run_id != run_id
                        && r.step() == Some((strand.as_str(), item_id.as_str()))
                        && r.status.is_settleable()
                    {
                        match close_run(&d, &reason, RunStatus::is_settleable, now_ms()) {
                            Ok(Some(_)) => self.drop_ready_reviews(&d),
                            Ok(None) => {}
                            Err(e) => {
                                eprintln!("jesse-bridge: hostrun {}: not closed: {e}", r.run_id)
                            }
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
                        &self.clone_url(&rec.repo),
                        &worktree,
                        &rec.branch,
                    )?);
                } else {
                    git::restore_worktree(
                        &self.settings.root,
                        &rec.repo,
                        &self.clone_url(&rec.repo),
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
                        keep_days: self.settings.keep_days,
                        pr_title_tag: self.settings.pr_title_tag(&rec.repo),
                        reviewed: !rec.reviewer_ids().is_empty(),
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
            let mut env: Vec<(String, String)> = ENV_ALLOWLIST
                .iter()
                .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
                .collect();
            // Where the runner finds the attribution hooks and settings. The runner's own,
            // not the agent's: it is not on the allowlist the agent's environment is cut to.
            if let Some(a) = &self.settings.attribution_dir {
                env.push((
                    runner::ATTRIBUTION_ENV.to_string(),
                    a.to_string_lossy().into_owned(),
                ));
            }
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
        if text.trim() == CONTINUE_TEXT
            && self
                .get(run_id)
                .is_some_and(|r| r.status == RunStatus::NeedsYou)
        {
            return Err(Refusal::State(format!(
                "{run_id} asked a question and needs an answer: an empty \"{CONTINUE_TEXT}\" \
                 would answer it blind. Reply with the answer."
            )));
        }
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
        self.queue_round_with(run_id, text, auto, |_, _| {})
    }

    /// [`HostRuns::queue_round_locked`], with `also` applied to the record in the same write,
    /// given the new round's number. Only the review's own hand off ([`AUTO_REVIEW`]) may queue
    /// a round on a `reviewing` run.
    fn queue_round_with(
        &self,
        run_id: &str,
        text: &str,
        auto: Option<&str>,
        also: impl FnOnce(&mut RunRecord, u32),
    ) -> Result<RunRecord, Refusal> {
        let rec = self
            .get(run_id)
            .ok_or_else(|| Refusal::NotFound(format!("no run {run_id:?}")))?;
        let handing_off = auto == Some(AUTO_REVIEW) && rec.status == RunStatus::Reviewing;
        if !handing_off
            && !matches!(
                rec.status,
                RunStatus::NeedsYou
                    | RunStatus::Reported
                    | RunStatus::Failed
                    | RunStatus::Interrupted
                    | RunStatus::Paused
                    | RunStatus::Closed
            )
        {
            if rec.status == RunStatus::Reviewing {
                return Err(Refusal::State(format!(
                    "{run_id} is being reviewed; its next round is the review's hand off"
                )));
            }
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
            also(r, n);
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
            // A review in flight: the run is cancelled now and the review is recorded as such;
            // the review task, when its reviewer answers, finds the run no longer reviewing and
            // drops what it got. Nothing reaches the session.
            RunStatus::Reviewing => {
                let updated = update_run(&dir, |r| {
                    r.status = RunStatus::Cancelled;
                    r.finished_at = Some(now.clone());
                    for v in r.reviews.iter_mut() {
                        if matches!(v.status, ReviewStatus::Running | ReviewStatus::Ready) {
                            v.status = ReviewStatus::Failed;
                            v.error = Some("the run was cancelled".into());
                            v.finished_at = Some(now.clone());
                        }
                    }
                })
                .map_err(Refusal::Internal)?;
                eprintln!("jesse-bridge: hostrun CANCEL {run_id} (during its review)");
                Ok(updated)
            }
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
                "{run_id} is {}; only a queued, running, paused or reviewing run can be cancelled",
                other.label()
            ))),
        }
    }

    /// Every run id on disk, in any state: a cloud run takes none of them.
    pub fn run_ids(&self) -> Vec<String> {
        self.runs().into_iter().map(|(_, r)| r.run_id).collect()
    }

    /// The oldest queued run a free cloud slot may take, already taken: one that is waiting
    /// (`waiting_for` set), has never started, runs a draft that says `devjob-host: any`, and is
    /// not on a local repository. It becomes `closed` ([`MOVED_TO_CLOUD`]) under the queue's
    /// gate, so the pump cannot start it as well. `None` when nothing qualifies.
    pub fn take_for_cloud(&self, notes_root: &Path) -> Option<RunRecord> {
        let _gate = lock(&self.gate);
        let mut queued: Vec<(PathBuf, RunRecord)> = self
            .runs()
            .into_iter()
            .filter(|(_, r)| drainable(r))
            .collect();
        queued.sort_by(|a, b| {
            let qa = a.1.queued_at.as_deref().unwrap_or(&a.1.created_at);
            let qb = b.1.queued_at.as_deref().unwrap_or(&b.1.created_at);
            qa.cmp(qb).then(a.1.run_id.cmp(&b.1.run_id))
        });
        for (dir, r) in queued {
            let Some(path) = &r.prompt_path else { continue };
            let Ok(src) = std::fs::read_to_string(notes_root.join(path)) else {
                continue;
            };
            if !crate::devjob::cloud::DraftHost::of(&src).drains_to_cloud() {
                continue;
            }
            let now = rfc(now_ms());
            let mut took = false;
            let rec = update_run(&dir, |x| {
                if !drainable(x) {
                    return;
                }
                x.status = RunStatus::Closed;
                x.closed_reason = Some(MOVED_TO_CLOUD.to_string());
                x.closed_at = Some(now.clone());
                x.waiting_for = None;
                x.finished_at = Some(now.clone());
                if let Some(cur) = x.round_mut() {
                    cur.outcome = Some(RunStatus::Closed);
                    cur.finished_at = Some(now.clone());
                    cur.notified = true;
                }
                took = true;
            })
            .ok()?;
            if took {
                eprintln!("jesse-bridge: hostrun {}: {MOVED_TO_CLOUD}", rec.run_id);
                return Some(rec);
            }
        }
        None
    }

    /// Name the Job a run [`HostRuns::take_for_cloud`] gave away became.
    pub fn note_moved(&self, run_id: &str, job: &str) {
        let _gate = lock(&self.gate);
        let _ = update_run(&self.settings.run_dir(run_id), |x| {
            if x.closed_reason.as_deref() == Some(MOVED_TO_CLOUD) {
                x.closed_reason = Some(format!("{MOVED_TO_CLOUD} as {job}"));
            }
        });
    }

    /// Give a run back to the Studio's queue when the cloud could not make its Job: queued again
    /// at its old place, with the reason in `error`.
    pub fn return_from_cloud(&self, run_id: &str, why: &str) {
        let _gate = lock(&self.gate);
        let res = update_run(&self.settings.run_dir(run_id), |x| {
            if x.status != RunStatus::Closed || x.closed_reason.as_deref() != Some(MOVED_TO_CLOUD) {
                return;
            }
            x.status = RunStatus::Queued;
            x.closed_reason = None;
            x.closed_at = None;
            x.finished_at = None;
            x.error = Some(format!("the cloud could not take it: {why}"));
            if let Some(cur) = x.round_mut() {
                cur.outcome = None;
                cur.finished_at = None;
                cur.notified = false;
            }
        });
        eprintln!("jesse-bridge: hostrun {run_id}: back in the Studio queue: {why}");
        if let Err(e) = res {
            eprintln!("jesse-bridge: hostrun {run_id}: {e}");
        }
        self.wake();
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
            Ok(Some(r)) => {
                self.drop_ready_reviews(&self.settings.run_dir(run_id));
                Ok(self.get(run_id).unwrap_or(r))
            }
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

    /// The watcher's settling: close every `needs_you`, `interrupted` or `failed` run whose
    /// strand item's line is ticked ([`CLOSED_ITEM_TICKED`]) or that a later run of the same
    /// item followed ([`superseded_reason`] of the newest such run, whatever its status), and
    /// every `needs_you` run whose reports have all moved to `Inbox/archive/`
    /// ([`CLOSED_REPORT_ARCHIVED`]). Returns each run closed and why. Never touches a queued,
    /// running or paused run.
    pub fn close_settled(&self, now: u64) -> Vec<(String, String)> {
        let _gate = lock(&self.gate);
        let mut out = Vec::new();
        // Oldest first (by `created_at`, then id), so a later run of an item is one further on.
        let runs = self.runs();
        for (i, (dir, r)) in runs.iter().enumerate() {
            if !r.status.is_settleable() {
                continue;
            }
            let newer = || {
                let step = r.step()?;
                runs[i + 1..]
                    .iter()
                    .rfind(|(_, n)| n.step() == Some(step))
                    .map(|(_, n)| superseded_reason(&n.run_id))
            };
            let reason = if r
                .step()
                .and_then(|(slug, id)| item_ticked(&self.notes_root, slug, id))
                == Some(true)
            {
                CLOSED_ITEM_TICKED.to_string()
            } else if let Some(reason) = newer() {
                reason
            } else if r.status == RunStatus::NeedsYou && reports_archived(&self.notes_root, r) {
                CLOSED_REPORT_ARCHIVED.to_string()
            } else {
                continue;
            };
            let may = |s: RunStatus| match reason.as_str() {
                CLOSED_REPORT_ARCHIVED => s == RunStatus::NeedsYou,
                _ => s.is_settleable(),
            };
            match close_run(dir, &reason, may, now) {
                Ok(Some(_)) => {
                    self.drop_ready_reviews(dir);
                    out.push((r.run_id.clone(), reason))
                }
                Ok(None) => {}
                Err(e) => eprintln!("jesse-bridge: hostrun {}: not closed: {e}", r.run_id),
            }
        }
        out
    }

    // ---- ARCHIVING ---------------------------------------------------------------------

    /// Take a run out of the runs list. Refuses a queued, running or paused run; closes a
    /// `needs_you` or `interrupted` run first ([`HostRuns::close`] with
    /// [`ARCHIVED_BY_JEREMY`]); flags a `failed` one, like every other finished run, in place
    /// with its status kept. Idempotent: an archived run is answered as it is. A flag only:
    /// nothing of the run is moved or deleted. Silent: no push and no turn.
    pub fn archive(&self, run_id: &str) -> Result<RunRecord, Refusal> {
        let rec = self
            .get(run_id)
            .ok_or_else(|| Refusal::NotFound(format!("no run {run_id:?}")))?;
        if rec.status.is_moving() {
            return Err(Refusal::State(format!(
                "{run_id} is {}; a queued, running or paused run cannot be archived",
                rec.status.label()
            )));
        }
        if rec.status.is_settleable() && !rec.status.is_archivable() {
            self.close(run_id, Some(ARCHIVED_BY_JEREMY))?;
        }
        lock(&self.unarchived).remove(run_id);
        let _gate = lock(&self.gate);
        match archive_run(
            &self.settings.run_dir(run_id),
            ARCHIVED_BY_JEREMY,
            RunStatus::is_archivable,
            now_ms(),
        ) {
            Ok(Ok(r)) => {
                self.drop_ready_reviews(&self.settings.run_dir(run_id));
                Ok(self.get(run_id).unwrap_or(r))
            }
            Ok(Err(s)) => Err(Refusal::State(format!(
                "{run_id} is {}; a queued, running or paused run cannot be archived",
                s.label()
            ))),
            Err(e) => Err(Refusal::Internal(e)),
        }
    }

    /// Put a run back in the runs list: clears `archived_at` and `archived_reason` and nothing
    /// else. Idempotent. The automatic archive leaves it alone until the bridge restarts.
    pub fn unarchive(&self, run_id: &str) -> Result<RunRecord, Refusal> {
        if self.get(run_id).is_none() {
            return Err(Refusal::NotFound(format!("no run {run_id:?}")));
        }
        let _gate = lock(&self.gate);
        lock(&self.unarchived).insert(run_id.to_string());
        update_run(&self.settings.run_dir(run_id), |r| {
            r.archived_at = None;
            r.archived_reason = None;
        })
        .map_err(Refusal::Internal)
    }

    /// [`HostRuns::archive`] or [`HostRuns::unarchive`], off the async runtime.
    pub async fn archive_blocking(
        self: &Arc<Self>,
        run_id: &str,
        archived: bool,
    ) -> Result<RunRecord, Refusal> {
        let this = self.clone();
        let id = run_id.to_string();
        tokio::task::spawn_blocking(move || {
            if archived {
                this.archive(&id)
            } else {
                this.unarchive(&id)
            }
        })
        .await
        .map_err(|e| Refusal::Internal(format!("the archive task failed: {e}")))?
    }

    /// Why the watcher would archive `r` now, if it would: its Drafts line is ticked, its
    /// reports have all moved to `Inbox/archive/`, or it finished more than
    /// [`AUTO_ARCHIVE_AFTER_DAYS`] ago. Never a run that is moving, waits on Jeremy, was cut
    /// off or failed, nor one already archived.
    pub fn auto_archive_reason(&self, r: &RunRecord, now: u64) -> Option<&'static str> {
        if r.archived_at.is_some() || !r.status.is_archivable() || r.status.needs_me() {
            return None;
        }
        if r.step()
            .and_then(|(slug, id)| item_ticked(&self.notes_root, slug, id))
            == Some(true)
        {
            return Some(ARCHIVED_ITEM_TICKED);
        }
        if reports_archived(&self.notes_root, r) {
            return Some(ARCHIVED_REPORT_ARCHIVED);
        }
        let age = now.saturating_sub(r.settled_at()?);
        (age > AUTO_ARCHIVE_AFTER_DAYS * DAY_MS).then_some(ARCHIVED_AGED)
    }

    /// The watcher's archiving, after [`HostRuns::close_settled`]: every run
    /// [`HostRuns::auto_archive_reason`] names, except those Jeremy unarchived since the bridge
    /// started. Silent. Returns each run archived and why.
    pub fn archive_settled(&self, now: u64) -> Vec<(String, String)> {
        let held = lock(&self.unarchived).clone();
        let _gate = lock(&self.gate);
        let mut out = Vec::new();
        for (dir, r) in self.runs() {
            if held.contains(&r.run_id) {
                continue;
            }
            let Some(reason) = self.auto_archive_reason(&r, now) else {
                continue;
            };
            let may = |s: RunStatus| s.is_archivable() && !s.needs_me();
            match archive_run(&dir, reason, may, now) {
                Ok(Ok(_)) => {
                    self.drop_ready_reviews(&dir);
                    eprintln!("jesse-bridge: hostrun {}: archived ({reason})", r.run_id);
                    out.push((r.run_id.clone(), reason.to_string()))
                }
                Ok(Err(_)) => {}
                Err(e) => eprintln!("jesse-bridge: hostrun {}: not archived: {e}", r.run_id),
            }
        }
        out
    }

    // ---- REVIEWS -----------------------------------------------------------------------

    /// A reviewed run's implementing round reported: the round is recorded as finished, with no
    /// report written and nothing sent, and the run goes `reviewing` with its one review cycle:
    /// one review per reviewer, tagged `A`, `B`, … when there are several.
    async fn begin_start_review(&self, dir: &Path, exit: &ExitInfo) -> Result<(), String> {
        let at = exit.ended_at.clone();
        let (code, signal) = (exit.code, exit.signal);
        let now = rfc(now_ms());
        let rec = update_run(dir, |r| {
            if r.reviews.iter().any(|v| v.origin == review::ORIGIN_START) {
                return;
            }
            r.status = RunStatus::Reviewing;
            r.finished_at = Some(at.clone());
            r.stalled_since = None;
            r.pgid = None;
            r.restart_resumed = false;
            r.limit_pushed = false;
            r.resume_at = None;
            if let Some(cur) = r.round_mut() {
                cur.finished_at = Some(at.clone());
                cur.outcome = Some(RunStatus::Reported);
                cur.exit_code = code;
                cur.signal = signal;
                // Nothing is sent for this round, ever: its push is the answering round's.
                cur.notified = true;
            }
            let ids = r.reviewer_ids();
            let first = r.reviews.len() as u32 + 1;
            let many = ids.len() > 1;
            for (i, id) in ids.iter().enumerate() {
                let label = r.reviewer_label_of(i);
                r.reviews.push(Review {
                    n: first + i as u32,
                    reviewer: id.clone(),
                    reviewer_label: label,
                    origin: review::ORIGIN_START.into(),
                    status: ReviewStatus::Running,
                    focus: r.review_focus.clone(),
                    auto_reply: true,
                    requested_at: now.clone(),
                    cycle: many.then_some(first),
                    tag: many.then(|| review::tag_for(i)),
                    ..Review::default()
                });
            }
        })?;
        eprintln!(
            "jesse-bridge: hostrun {} round {} REVIEWING (reviewers {})",
            rec.run_id,
            rec.rounds.len(),
            rec.reviewer_ids().join(", ")
        );
        self.lift_limit(now_ms());
        self.wake();
        Ok(())
    }

    /// Give every `reviewing` run a review task, unless this process already runs one for it.
    pub fn start_reviews(self: &Arc<Self>) {
        for (_, r) in self.runs() {
            if r.status != RunStatus::Reviewing {
                continue;
            }
            if !lock(&self.reviewing).insert(r.run_id.clone()) {
                continue;
            }
            let this = self.clone();
            let id = r.run_id.clone();
            tokio::spawn(async move {
                this.drive_review(&id).await;
                lock(&this.reviewing).remove(&id);
                this.wake();
            });
        }
    }

    /// Wait (at most about ten seconds) until no review task runs in this process.
    pub async fn reviews_settled(&self) {
        for _ in 0..2000 {
            if lock(&self.reviewing).is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// One review cycle, start to hand off: every reviewer of the cycle whose note is not yet
    /// written runs at once, each in its own worktree; once all have answered or failed, their
    /// findings are recorded and handed off together, in one round, or for a review that waits,
    /// the run is put back.
    async fn drive_review(self: &Arc<Self>, run_id: &str) {
        let dir = self.settings.run_dir(run_id);
        let Ok(rec) = read_run(&dir) else {
            return;
        };
        if rec.status != RunStatus::Reviewing {
            return;
        }
        let Some(cycle) = rec
            .reviews
            .iter()
            .rfind(|v| matches!(v.status, ReviewStatus::Running | ReviewStatus::Ready))
            .map(Review::cycle_of)
        else {
            // Reviewing with no open review cannot be left so: the run goes back to reported.
            let _ = update_run(&dir, |r| {
                if r.status == RunStatus::Reviewing {
                    r.status = RunStatus::Reported;
                }
            });
            return;
        };
        let running: Vec<Review> = rec
            .reviews
            .iter()
            .filter(|v| v.cycle_of() == cycle && v.status == ReviewStatus::Running)
            .cloned()
            .collect();
        if !running.is_empty() {
            // One range for the whole cycle, read before any reviewer starts, so every reviewer
            // reads the same base and head whatever moves meanwhile. Then every reviewer at
            // once: one task each, all awaited before anything is recorded.
            let mut results: Vec<(u32, Result<ReviewDone, String>)> = Vec::new();
            match self.cycle_range(&dir, &rec).await {
                Err(e) => {
                    for v in &running {
                        results.push((v.n, Err(e.clone())));
                    }
                }
                Ok(range) => {
                    let range = Arc::new(range);
                    let tasks: Vec<_> = running
                        .iter()
                        .map(|v| {
                            let (this, dir, rec, v, range) = (
                                self.clone(),
                                dir.clone(),
                                rec.clone(),
                                v.clone(),
                                range.clone(),
                            );
                            tokio::spawn(
                                async move { this.perform_review(&dir, &rec, &v, range).await },
                            )
                        })
                        .collect();
                    for (v, t) in running.iter().zip(tasks) {
                        let res = t
                            .await
                            .unwrap_or_else(|e| Err(format!("the review task failed: {e}")));
                        results.push((v.n, res));
                    }
                }
            }
            let now = rfc(now_ms());
            let mut applied = false;
            let _ = update_run(&dir, |r| {
                if r.status != RunStatus::Reviewing {
                    return;
                }
                for (n, res) in &results {
                    let Some(x) = r.reviews.iter_mut().find(|x| x.n == *n) else {
                        continue;
                    };
                    if x.status != ReviewStatus::Running {
                        continue;
                    }
                    x.finished_at = Some(now.clone());
                    match res {
                        Ok(out) => {
                            x.status = ReviewStatus::Ready;
                            x.head = Some(out.head.clone());
                            x.base = Some(out.base.clone());
                            x.findings = out.findings.clone();
                            x.summary = Some(out.summary.clone());
                            x.path = Some(out.path.clone());
                        }
                        Err(e) => {
                            x.status = ReviewStatus::Failed;
                            x.error = Some(review::one_line(e));
                        }
                    }
                    applied = true;
                }
            });
            if !applied {
                // Cancelled while the reviewers worked: what they found goes nowhere.
                eprintln!(
                    "jesse-bridge: hostrun {run_id}: no longer reviewing; the review is dropped"
                );
                for (_, res) in &results {
                    if let Ok(out) = res {
                        archive_note(&self.notes_root, &out.path);
                    }
                }
                return;
            }
            for ((n, res), v) in results.iter().zip(&running) {
                match res {
                    Ok(out) => eprintln!(
                        "jesse-bridge: hostrun {run_id} REVIEW {n} by {}: {} finding(s) at {}",
                        v.reviewer,
                        out.findings.len(),
                        out.path
                    ),
                    Err(e) => {
                        eprintln!("jesse-bridge: hostrun {run_id} REVIEW {n} FAILED: {e}")
                    }
                }
            }
        }
        let Ok(rec) = read_run(&dir) else {
            return;
        };
        let members: Vec<Review> = rec
            .reviews
            .iter()
            .filter(|x| x.cycle_of() == cycle)
            .cloned()
            .collect();
        let Some(lead) = members.first().cloned() else {
            return;
        };
        let start = lead.origin == review::ORIGIN_START;
        let any_ready = members.iter().any(|x| x.status == ReviewStatus::Ready);
        if lead.auto_reply || start {
            // A reviewed run is always handed off, every review failed included: it must not
            // strand the run.
            if any_ready || start {
                let this = self.clone();
                let id = run_id.to_string();
                let handed = tokio::task::spawn_blocking(move || this.hand_off(&id, cycle))
                    .await
                    .unwrap_or_else(|e| Err(Refusal::Internal(e.to_string())));
                match handed {
                    Ok(_) => {
                        let effects = self.pump_blocking().await;
                        self.send_effects(effects).await;
                        return;
                    }
                    Err(e) => eprintln!(
                        "jesse-bridge: hostrun {run_id}: the review's hand off failed: {}",
                        e.message()
                    ),
                }
            }
        }
        // A review that waits (or that failed, or whose hand off could not be queued): the run
        // goes back to where it was, and Jeremy is told once.
        let prior = lead.prior_status.unwrap_or(RunStatus::Reported);
        let _ = update_run(&dir, |r| {
            if r.status == RunStatus::Reviewing {
                r.status = prior;
            }
        });
        if let Some(n) = self.sink.get().and_then(|s| s.notifier.clone()) {
            let ready: Vec<&Review> = members
                .iter()
                .filter(|x| x.status == ReviewStatus::Ready)
                .collect();
            let (title, body) = if !ready.is_empty() {
                (
                    format!("Run {run_id} reviewed"),
                    format!(
                        "{} finding(s) from {}; devjob_review_reply hands them off",
                        ready.iter().map(|x| x.count()).sum::<usize>(),
                        ready
                            .iter()
                            .map(|x| x.reviewer_label.as_str())
                            .collect::<Vec<_>>()
                            .join(" + ")
                    ),
                )
            } else {
                (
                    format!("Run {run_id} review failed"),
                    members
                        .iter()
                        .filter_map(|x| x.error.clone())
                        .collect::<Vec<_>>()
                        .join("; ")
                        .chars()
                        .take(180)
                        .collect(),
                )
            };
            n.notify(title, body, run_id.to_string()).await;
        }
    }

    /// The range a review cycle reads, once for every reviewer in it: the default branch
    /// fetched, the merge base and head of the run's worktree, and the diff between them.
    async fn cycle_range(&self, dir: &Path, rec: &RunRecord) -> Result<ReviewRange, String> {
        let worktree = dir.join(WORKTREE_DIR);
        if !worktree.exists() {
            return Err("the run's worktree was removed, so there is nothing to read".into());
        }
        let (root, repo) = (self.settings.root.clone(), rec.repo.clone());
        let base_sha = rec.base_sha.clone();
        let default_branch = rec.default_branch.clone();
        tokio::task::spawn_blocking(move || {
            let main_ref = if is_local_repo(&repo) {
                git::LOCAL_DEFAULT_BRANCH.to_string()
            } else {
                let bare = git::bare_path(&root, &repo);
                let fetched =
                    git::lock_repo(&root, &repo).and_then(|_lock| git::fetch_default(&bare));
                format!(
                    "origin/{}",
                    fetched.unwrap_or_else(|_| default_branch.unwrap_or_else(|| "main".into()))
                )
            };
            let (base, head) = review::review_range(&worktree, &main_ref, base_sha.as_deref())?;
            let diff = review::diff(&worktree, &base, &head)?;
            Ok::<_, String>(ReviewRange { base, head, diff })
        })
        .await
        .map_err(|e| format!("the review task failed: {e}"))?
    }

    /// The reviewer's part, on the cycle's one `range`: its own detached worktree at the head,
    /// the answer (retried once when the schema refuses it), the worktree checked untouched and
    /// removed, and the note written. Touches nothing of the run but its directory's
    /// `review-<n>*` files and the Inbox note.
    async fn perform_review(
        &self,
        dir: &Path,
        rec: &RunRecord,
        v: &Review,
        range: Arc<ReviewRange>,
    ) -> Result<ReviewDone, String> {
        let reviewer = self
            .reviewer
            .get()
            .cloned()
            .ok_or_else(|| "this bridge has no reviewer attached".to_string())?;
        let info = reviewer.check(&v.reviewer)?;
        let sink = self
            .sink
            .get()
            .cloned()
            .ok_or_else(|| "nothing is attached to write the note".to_string())?;
        let n = v.n;
        let review_wt = dir.join(format!("{}{n}", review::REVIEW_WORKTREE_PREFIX));
        let (root, repo) = (self.settings.root.clone(), rec.repo.clone());
        let (base, head, diff) = (range.base.clone(), range.head.clone(), range.diff.clone());
        let (rwt, h) = (review_wt.clone(), head.clone());
        tokio::task::spawn_blocking(move || git::add_detached_worktree(&root, &repo, &rwt, &h))
            .await
            .map_err(|e| format!("the review task failed: {e}"))??;
        let section = std::fs::read_to_string(dir.join(SECTION_MD))
            .or_else(|_| std::fs::read_to_string(dir.join(PROMPT_MD)))
            .unwrap_or_default();
        let prompt = review::reviewer_prompt(&section, v.focus.as_deref(), &base, &head, &diff);
        let timeout = Duration::from_secs(self.settings.review_timeout_minutes.saturating_mul(60));
        let ask = |prompt: String| {
            reviewer.review(ReviewCall {
                reviewer: info.id.clone(),
                prompt,
                cwd: review_wt.clone(),
                timeout,
            })
        };
        let answered: Result<review::ReviewOutput, String> = async {
            let first = ask(prompt.clone()).await?;
            let _ = write_atomic(&dir.join(review::answer_file(n, 1)), first.as_bytes());
            match review::parse_review(&first) {
                Ok(out) => Ok(out),
                Err(e1) => {
                    eprintln!("jesse-bridge: hostrun {} review {n}: the answer failed the schema ({e1}); retrying once", rec.run_id);
                    let second = ask(review::retry_prompt(&prompt, &first, &e1)).await?;
                    let _ = write_atomic(&dir.join(review::answer_file(n, 2)), second.as_bytes());
                    review::parse_review(&second).map_err(|e2| {
                        format!(
                            "the reviewer's answer failed the schema twice ({e2}); both answers are in the run directory ({} and {})",
                            review::answer_file(n, 1),
                            review::answer_file(n, 2)
                        )
                    })
                }
            }
        }
        .await;
        // The reviewer's worktree must be exactly as it was given: a change there is a write
        // the posture should have made impossible, and voids the review.
        let (root, repo, rwt, h) = (
            self.settings.root.clone(),
            rec.repo.clone(),
            review_wt.clone(),
            head.clone(),
        );
        let untouched = tokio::task::spawn_blocking(move || {
            let clean = review::worktree_untouched(&rwt, &h);
            git::remove_detached_worktree(&root, &repo, &rwt);
            clean
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        if let Err(e) = untouched {
            eprintln!(
                "jesse-bridge: WARNING: hostrun {} review {n}: the reviewer's worktree changed: {e}",
                rec.run_id
            );
            return Err(format!(
                "the reviewer's worktree changed ({e}); the review is void"
            ));
        }
        let out = answered?;
        let rel = v.path.clone().unwrap_or_else(|| {
            review::note_rel_path(
                &chrono::Local::now().format("%Y-%m-%d").to_string(),
                &rec.run_id,
                n,
            )
        });
        let noted = Review {
            reviewer: info.id.clone(),
            reviewer_label: info.label.clone(),
            head: Some(head.clone()),
            base: Some(base.clone()),
            findings: out.findings.clone(),
            summary: Some(out.summary.clone()),
            ..v.clone()
        };
        let text = review::render_note(&rec.run_id, &rec.repo, &rec.branch, &noted);
        write_report(&self.notes_root, &sink.broker, &rec.run_id, &rel, &text).await?;
        Ok(ReviewDone {
            head,
            base,
            findings: out.findings,
            summary: out.summary,
            path: rel,
        })
    }

    /// The reviews of a cycle that could not run, and the default reviewers a reviewed run's
    /// own cycle went without: `<id>: <reason>` each.
    fn cycle_failures(rec: &RunRecord, members: &[Review]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if members
            .first()
            .is_some_and(|v| v.origin == review::ORIGIN_START)
        {
            out.extend(rec.reviewers_skipped.iter().map(|sk| {
                format!(
                    "{}: default reviewer unavailable: {}",
                    sk.reviewer,
                    review::one_line(&sk.reason)
                )
            }));
        }
        out.extend(
            members
                .iter()
                .filter(|v| v.status == ReviewStatus::Failed)
                .map(|v| {
                    format!(
                        "{}: {}",
                        v.reviewer,
                        v.error.as_deref().unwrap_or("the review could not run")
                    )
                }),
        );
        out
    }

    /// Why a cycle none of whose reviews ran was handed off unreviewed: a lone review's own
    /// error, as 0.211.0 put it, else `<id>: <reason>` for each, joined with `; `.
    fn unreviewed_reason(rec: &RunRecord, members: &[Review]) -> String {
        match members {
            [one] if rec.reviewers_skipped.is_empty() || one.origin != review::ORIGIN_START => one
                .error
                .clone()
                .unwrap_or_else(|| "the review could not run".into()),
            _ => review::partly_reason(&Self::cycle_failures(rec, members)),
        }
    }

    /// What the hand off says for the review cycle `members` of `rec`: every finding of every
    /// review that ran, one block per reviewer when there are several, and what could not run;
    /// or, when none ran, the unreviewed reply.
    fn handoff_text(rec: &RunRecord, members: &[Review]) -> String {
        let finish = members
            .first()
            .is_some_and(|v| v.origin == review::ORIGIN_START)
            .then(|| {
                if is_local_repo(&rec.repo) {
                    review::HANDOFF_FINISH_LOCAL
                } else {
                    review::HANDOFF_FINISH
                }
            });
        let ran: Vec<&Review> = members
            .iter()
            .filter(|v| v.status != ReviewStatus::Failed)
            .collect();
        if ran.is_empty() {
            return review::unreviewed_reply(&Self::unreviewed_reason(rec, members), finish);
        }
        let blocks: Vec<review::Block> = ran
            .iter()
            .map(|v| review::Block {
                tag: v.tag.as_deref(),
                label: v.name(),
                id: &v.reviewer,
                findings: v.shown_findings(),
            })
            .collect();
        review::handoff_reply_cycle(&blocks, &Self::cycle_failures(rec, members), finish)
    }

    /// Queue the hand off of review cycle `cycle`: the run's next round, in its own worktree
    /// and session, exactly as a reply would, with [`HostRuns::handoff_text`] as the message.
    /// One round for the whole cycle, however many reviewed.
    fn hand_off(&self, run_id: &str, cycle: u32) -> Result<RunRecord, Refusal> {
        let _gate = lock(&self.gate);
        let rec = self
            .get(run_id)
            .ok_or_else(|| Refusal::NotFound(format!("no run {run_id:?}")))?;
        let members: Vec<Review> = rec
            .reviews
            .iter()
            .filter(|x| x.cycle_of() == cycle)
            .cloned()
            .collect();
        if members.is_empty() {
            return Err(Refusal::NotFound(format!("{run_id} has no review {cycle}")));
        }
        if members.iter().any(|v| v.answer_round.is_some()) {
            return Err(Refusal::State(format!(
                "{run_id}'s review {cycle} was already handed off"
            )));
        }
        let text = Self::handoff_text(&rec, &members);
        self.queue_round_with(run_id, &text, Some(AUTO_REVIEW), |r, round| {
            for x in r.reviews.iter_mut().filter(|x| x.cycle_of() == cycle) {
                x.answer_round = Some(round);
                if x.status == ReviewStatus::Ready {
                    x.status = ReviewStatus::HandedOff;
                }
            }
        })
    }

    /// `devjob_review`: a review of a finished run, asked for after the fact, by one reviewer
    /// or several at once (one cycle, one hand off). Refuses a run that is queued, running,
    /// paused or already being reviewed, one with a review still waiting for its hand off, one
    /// with no commits beyond its base, `none`, more than [`review::MAX_REVIEWERS`] or a
    /// repeated one, and any reviewer the registry does not know or marks unhealthy. The run
    /// goes `reviewing`; the watcher runs it.
    pub fn request_review(
        &self,
        run_id: &str,
        reviewer: impl Into<review::ReviewerArg>,
        focus: Option<&str>,
        auto_reply: bool,
    ) -> Result<RunRecord, Refusal> {
        let arg: review::ReviewerArg = reviewer.into();
        let ids = match review::choose(Some(&arg)).map_err(Refusal::Invalid)? {
            review::ReviewerChoice::Ids(ids) => ids,
            review::ReviewerChoice::Unset | review::ReviewerChoice::Off => {
                return Err(Refusal::Invalid(
                    "a review needs a reviewer: name a model id".into(),
                ))
            }
        };
        let focus = review::checked_focus(focus).map_err(Refusal::Invalid)?;
        let _gate = lock(&self.gate);
        let rec = self
            .get(run_id)
            .ok_or_else(|| Refusal::NotFound(format!("{run_id:?} is not a Studio run")))?;
        match rec.status {
            RunStatus::Queued | RunStatus::Running | RunStatus::Paused => {
                return Err(Refusal::State(format!(
                    "{run_id} is still {}; a review reads a finished round",
                    rec.status.label()
                )))
            }
            RunStatus::Reviewing => {
                return Err(Refusal::State(format!(
                    "{run_id} is already being reviewed; one review at a time"
                )))
            }
            _ => {}
        }
        if let Some(w) = rec.reviews.iter().find(|v| v.status == ReviewStatus::Ready) {
            return Err(Refusal::State(format!(
                "{run_id}'s review {} is waiting for its hand off (devjob_review_reply); one \
                 review at a time",
                w.n
            )));
        }
        let mut infos: Vec<ReviewerInfo> = Vec::new();
        for id in &ids {
            let info = self.check_reviewer(id).map_err(Refusal::Invalid)?;
            if infos.iter().any(|x| x.id == info.id) {
                return Err(Refusal::Invalid(format!(
                    "reviewer {:?} is named twice",
                    info.id
                )));
            }
            infos.push(info);
        }
        let dir = self.settings.run_dir(run_id);
        let worktree = dir.join(WORKTREE_DIR);
        if !worktree.exists() {
            return Err(Refusal::State(format!(
                "{run_id}'s worktree was removed, so there is nothing to review"
            )));
        }
        // The commits beyond its base, from the clone's refs as they are: no fetch here.
        let main_ref = if is_local_repo(&rec.repo) {
            git::LOCAL_DEFAULT_BRANCH.to_string()
        } else {
            format!("origin/{}", rec.default_branch.as_deref().unwrap_or("main"))
        };
        review::review_range(&worktree, &main_ref, rec.base_sha.as_deref())
            .map_err(|e| Refusal::State(format!("{run_id}: {e}")))?;
        let now = rfc(now_ms());
        let many = infos.len() > 1;
        let updated = update_run(&dir, |r| {
            let first = r.reviews.len() as u32 + 1;
            let prior = r.status;
            for (i, info) in infos.iter().enumerate() {
                r.reviews.push(Review {
                    n: first + i as u32,
                    reviewer: info.id.clone(),
                    reviewer_label: info.label.clone(),
                    origin: review::ORIGIN_REQUEST.into(),
                    status: ReviewStatus::Running,
                    focus: focus.clone(),
                    auto_reply,
                    requested_at: now.clone(),
                    prior_status: Some(prior),
                    cycle: many.then_some(first),
                    tag: many.then(|| review::tag_for(i)),
                    ..Review::default()
                });
            }
            r.status = RunStatus::Reviewing;
        })
        .map_err(Refusal::Internal)?;
        eprintln!(
            "jesse-bridge: hostrun {run_id} REVIEW REQUESTED (reviewers {}, auto_reply {auto_reply})",
            infos.iter().map(|i| i.id.as_str()).collect::<Vec<_>>().join(", ")
        );
        Ok(updated)
    }

    /// [`HostRuns::request_review`], then the review task at once.
    pub async fn request_review_and_start(
        self: &Arc<Self>,
        run_id: &str,
        reviewer: impl Into<review::ReviewerArg>,
        focus: Option<String>,
        auto_reply: bool,
    ) -> Result<RunRecord, Refusal> {
        let this = self.clone();
        let (id, who) = (run_id.to_string(), reviewer.into());
        let rec = tokio::task::spawn_blocking(move || {
            this.request_review(&id, who, focus.as_deref(), auto_reply)
        })
        .await
        .map_err(|e| Refusal::Internal(format!("the review task failed: {e}")))??;
        self.start_reviews();
        Ok(rec)
    }

    /// `devjob_review_reply`: hand off the review that is waiting (`auto_reply` false), with
    /// the same fixed reply an automatic hand off sends.
    pub fn review_reply(&self, run_id: &str) -> Result<RunRecord, Refusal> {
        let rec = self
            .get(run_id)
            .ok_or_else(|| Refusal::NotFound(format!("{run_id:?} is not a Studio run")))?;
        if rec.status.is_moving() {
            return Err(Refusal::State(format!(
                "{run_id} is {}; the hand off waits until it has stopped",
                rec.status.label()
            )));
        }
        let Some(v) = rec
            .reviews
            .iter()
            .rfind(|v| v.status == ReviewStatus::Ready)
        else {
            return Err(Refusal::State(format!(
                "{run_id} has no review waiting for its hand off"
            )));
        };
        self.hand_off(run_id, v.cycle_of())
    }

    /// [`HostRuns::review_reply`], then start what the queue allows.
    pub async fn review_reply_and_pump(
        self: &Arc<Self>,
        run_id: &str,
    ) -> Result<RunRecord, Refusal> {
        let this = self.clone();
        let id = run_id.to_string();
        tokio::task::spawn_blocking(move || this.review_reply(&id))
            .await
            .map_err(|e| Refusal::Internal(format!("the hand off task failed: {e}")))??;
        let effects = self.pump_blocking().await;
        self.send_effects(effects).await;
        self.wake();
        self.get(run_id)
            .ok_or_else(|| Refusal::NotFound(format!("no run {run_id:?}")))
    }

    /// The run was closed or archived: a review never handed off is dropped, and every note
    /// whose answering round never reported moves to `Inbox/archive/` with it.
    fn drop_ready_reviews(&self, dir: &Path) {
        let now = rfc(now_ms());
        let mut paths = Vec::new();
        let _ = update_run(dir, |r| {
            for v in r.reviews.iter_mut() {
                if v.status == ReviewStatus::Ready {
                    v.status = ReviewStatus::Dropped;
                    v.finished_at.get_or_insert_with(|| now.clone());
                }
                if v.answered_at.is_none() {
                    if let Some(p) = &v.path {
                        paths.push(p.clone());
                    }
                }
            }
        });
        for p in paths {
            archive_note(&self.notes_root, &p);
        }
    }

    /// The leak check of an answering round: its new commit messages, the added lines of its
    /// diff (both from the head that was reviewed to the worktree's head), and the title and
    /// body of the run's pull requests, for [`review::leak_terms_all`] of every review of the
    /// cycle, the ones that failed included (the hand off named them). Never blocks anything.
    async fn review_leaks(
        &self,
        dir: &Path,
        rec: &RunRecord,
        members: &[Review],
        report: &str,
    ) -> Vec<String> {
        let Some(from) = members.iter().find_map(|v| v.head.clone()) else {
            return Vec::new();
        };
        let terms = review::leak_terms_all(members);
        let worktree = dir.join(WORKTREE_DIR);
        let forge = self.forge.clone();
        let url = pull_request_url(report);
        let (repo, branch) = (rec.repo.clone(), rec.branch.clone());
        tokio::task::spawn_blocking(move || {
            let mut places: Vec<(String, String)> = Vec::new();
            if worktree.exists() {
                for (id, msg) in review::commit_messages(&worktree, &from, "HEAD") {
                    places.push((format!("commit {id} message"), msg));
                }
                if let Ok(d) = review::diff(&worktree, &from, "HEAD") {
                    let mut by_file: BTreeMap<String, String> = BTreeMap::new();
                    for (file, line) in review::added_lines(&d) {
                        let t = by_file.entry(file).or_default();
                        t.push_str(&line);
                        t.push('\n');
                    }
                    for (file, text) in by_file {
                        places.push((format!("{file} (added lines)"), text));
                    }
                }
            }
            if !is_local_repo(&repo) {
                if let Some(gh) = forge.github() {
                    let mut pulls = gh.pulls_for_branch(&repo, &branch).unwrap_or_default();
                    if let Some(u) = url.as_deref() {
                        if !pulls.iter().any(|p| p.url == u) {
                            if let Some(p) = gh.pull(u) {
                                pulls.push(p);
                            }
                        }
                    }
                    for p in pulls {
                        places.push((format!("pull request {} title", p.url), p.title.clone()));
                        places.push((format!("pull request {} body", p.url), p.body.clone()));
                    }
                }
            }
            review::leak_hits(&terms, &places)
        })
        .await
        .unwrap_or_default()
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
        let mut report = round_report(&dir, &round);
        let mut status = round_status(&round, &exit, &report);
        // THE ANSWERING ROUND of a review cycle that was handed off: when no review of it ran
        // the report leads with `UNREVIEWED:`, when only some did with `PARTLY REVIEWED:`, and
        // with any `REVIEW LEAK:` the check found over every review that ran.
        let answering: Vec<usize> = rec
            .reviews
            .iter()
            .enumerate()
            .filter(|(_, v)| v.answers(round.n))
            .map(|(i, _)| i)
            .collect();
        let mut leaks = Vec::new();
        if !answering.is_empty() && round.report_path.is_none() {
            let members: Vec<Review> = answering.iter().map(|&i| rec.reviews[i].clone()).collect();
            let ran: Vec<Review> = members
                .iter()
                .filter(|v| v.status != ReviewStatus::Failed)
                .cloned()
                .collect();
            if ran.is_empty() {
                report =
                    review::ensure_unreviewed(&report, &Self::unreviewed_reason(&rec, &members));
            } else {
                let failed = Self::cycle_failures(&rec, &members);
                if !failed.is_empty() {
                    report = review::ensure_partly(&report, &failed);
                }
                leaks = self.review_leaks(&dir, &rec, &members, &report).await;
                if let Some(line) = review::leak_line(&leaks) {
                    eprintln!("jesse-bridge: hostrun {run_id} round {}: {line}", round.n);
                    // `PARTLY REVIEWED:` stays the first line; the leak follows it.
                    report = if failed.is_empty() {
                        format!("{line}\n\n{report}")
                    } else {
                        review::with_second_line(&report, &line)
                    };
                }
            }
        }
        // A run whose default reviewers could none of them review at the start ran as an
        // ordinary run: its finished reports say so first.
        if answering.is_empty()
            && round.report_path.is_none()
            && rec.reviewer_ids().is_empty()
            && !rec.reviewers_skipped.is_empty()
            && status == RunStatus::Reported
        {
            let why = rec
                .reviewers_skipped
                .iter()
                .map(review::Skipped::line)
                .collect::<Vec<_>>()
                .join("; ");
            report = review::ensure_unreviewed(&report, &why);
        }
        // NO AI ATTRIBUTION, the backstop: what the round pushed is read back, and a hit makes
        // the run `needs_you` with the `ATTRIBUTION:` line first, so the Inbox report and the
        // push both lead with it.
        let hits = self.attribution_hits(&rec, &report).await;
        if let Some(line) = crate::attribution::report_line(&hits) {
            eprintln!("jesse-bridge: hostrun {run_id} round {}: {line}", round.n);
            report = format!("{line}\n\n{report}");
            status = RunStatus::NeedsYou;
        }
        // A REVIEWED run's implementing round: nothing is posted and nobody is told. The run
        // goes `reviewing`, once, and the watcher runs the review and hands it off.
        if status == RunStatus::Reported
            && round.report_path.is_none()
            && !rec.reviewer_ids().is_empty()
            && !rec.reviews.iter().any(|v| v.origin == review::ORIGIN_START)
        {
            return self.begin_start_review(&dir, &exit).await;
        }
        // The answering round's note moves to `Inbox/archive/` as its report is written, and
        // the report links it where it now is.
        let notes_now: Vec<String> = answering
            .iter()
            .filter_map(|&i| rec.reviews[i].path.as_deref())
            .map(|p| archive_note(&self.notes_root, p).unwrap_or_else(|| p.to_string()))
            .collect();
        let rel = match round
            .report_path
            .clone()
            .or_else(|| find_existing_report(&self.notes_root, run_id, round.n))
        {
            Some(rel) => rel,
            None => {
                let date = chrono::Local::now().format("%Y-%m-%d").to_string();
                let rel = report_rel_path(&date, run_id, round.n);
                let local_main = if is_local_repo(&rec.repo) {
                    let (root, repo) = (self.settings.root.clone(), rec.repo.clone());
                    tokio::task::spawn_blocking(move || git::local_main_sha(&root, &repo))
                        .await
                        .ok()
                        .flatten()
                } else {
                    None
                };
                let mut text =
                    render_inbox_with(&rec, round.n, status, &report, local_main.as_deref());
                if !notes_now.is_empty() {
                    if !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push('\n');
                    for note in &notes_now {
                        text.push_str(&format!("{}\n", review::note_link(note)));
                    }
                }
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
            for &i in &answering {
                if let Some(v) = r.reviews.get_mut(i) {
                    if v.status == ReviewStatus::HandedOff {
                        v.status = ReviewStatus::Answered;
                    }
                    v.answered_at = Some(at.clone());
                    if !leaks.is_empty() && v.status != ReviewStatus::Failed {
                        v.leaks = leaks.clone();
                    }
                }
            }
        })?;
        if matches!(status, RunStatus::Reported | RunStatus::NeedsYou) {
            self.lift_limit(now_ms());
        }
        if rec.round().is_some_and(|r| r.notified) {
            return Ok(());
        }
        // A local repository's readable copy of `main`, once per finished round.
        if let Some(checkout) = self.settings.local_checkout(&rec.repo) {
            let (root, repo) = (self.settings.root.clone(), rec.repo.clone());
            let res = tokio::task::spawn_blocking(move || {
                git::update_local_checkout(&root, &repo, &checkout)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            if let Err(e) = res {
                eprintln!(
                    "jesse-bridge: hostrun {run_id}: the readable checkout of {}: {e}",
                    rec.repo
                );
            }
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

    /// The post-run attribution check of one finished round: for a GitHub repository, every
    /// pull request whose head is the run's branch (and the one the report names) and every
    /// commit they and the branch carry, read back through the forge; for a local repository,
    /// the commits its clone holds past the run's base, on the branch and on `main`. One entry
    /// per offending pull request or commit; none when nothing was pushed.
    async fn attribution_hits(&self, rec: &RunRecord, report: &str) -> Vec<String> {
        let forge = self.forge.clone();
        let rec = rec.clone();
        let url = pull_request_url(report);
        let root = self.settings.root.clone();
        tokio::task::spawn_blocking(move || {
            if is_local_repo(&rec.repo) {
                let bare = git::bare_path(&root, &rec.repo);
                let base = rec
                    .base_sha
                    .clone()
                    .unwrap_or_else(|| git::LOCAL_DEFAULT_BRANCH.to_string());
                let mut seen = HashSet::new();
                let mut out = Vec::new();
                for head in [rec.branch.as_str(), git::LOCAL_DEFAULT_BRANCH] {
                    if let Some(c) =
                        crate::attribution::local_commits(&bare, &format!("{base}..{head}"))
                    {
                        out.extend(crate::attribution::commit_entries(&c, &mut seen));
                    }
                }
                out
            } else {
                let Some(gh) = forge.github() else {
                    return Vec::new();
                };
                crate::attribution::scan_github(
                    gh,
                    &rec.repo,
                    &rec.branch,
                    rec.default_branch.as_deref().unwrap_or("main"),
                    url.as_deref(),
                )
            }
        })
        .await
        .unwrap_or_default()
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
        // Every run waiting on its reviewer gets its review task (again, after a restart).
        self.start_reviews();
        let this = self.clone();
        let _ = tokio::task::spawn_blocking(move || this.close_settled(now)).await;
        let this = self.clone();
        let _ = tokio::task::spawn_blocking(move || this.archive_settled(now)).await;
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

    /// Remove a run's build output: `target/` and the DerivedData folders it recorded. Both are
    /// rebuildable, so they go as soon as the run is done with. `extra/` is not build output:
    /// see [`Self::remove_extra`].
    fn remove_build_output(&self, dir: &Path, rec: &RunRecord, now: u64) -> u64 {
        let mut freed = self.remove_tier(dir, rec, TARGET_DIR);
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
        mark_artifacts_removed(dir, now);
        freed
    }

    /// Remove a run's `extra/` (the further repositories and outputs it made). Only for a run
    /// that is not live, `keep_days` after it finished, and (when the folder holds anything)
    /// after [`Cleanup::extra_warned`] was recorded on an earlier pass.
    fn remove_extra(&self, dir: &Path, rec: &RunRecord, now: u64) -> u64 {
        if rec.status.is_live() {
            return 0;
        }
        let freed = self.remove_tier(dir, rec, EXTRA_DIR);
        eprintln!("jesse-bridge: hostrun {}: extra/ removed", rec.run_id);
        mark_artifacts_removed(dir, now);
        freed
    }

    fn remove_tier(&self, dir: &Path, rec: &RunRecord, tier: &str) -> u64 {
        let p = dir.join(tier);
        if !p.exists() {
            return 0;
        }
        let freed = sys::tree_bytes(&p);
        if let Err(e) = sys::remove_tree(&p) {
            eprintln!(
                "jesse-bridge: hostrun {}: could not remove {tier}/: {e}",
                rec.run_id
            );
        }
        freed
    }

    fn has_build_output(&self, dir: &Path, rec: &RunRecord) -> bool {
        dir.join(TARGET_DIR).exists()
            || rec.derived_data.iter().any(|d| {
                Path::new(d).parent() == Some(self.settings.derived_data_root.as_path())
                    && Path::new(d).exists()
            })
    }

    /// Whether a finished run's `extra/` may be removed now, and if not, why.
    fn extra_due(&self, rec: &RunRecord, now: u64) -> Result<(), &'static str> {
        let Some(fin) = ms_opt(&rec.finished_at) else {
            return Err("no finish time");
        };
        if now.saturating_sub(fin) < self.settings.keep_days * DAY_MS {
            return Err("within keep_days of finishing");
        }
        if !extra_warned_since(rec, fin) {
            return Err("not warned yet");
        }
        Ok(())
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

    /// The clone URL for a run's repository: none for a local repository, which has no remote.
    fn clone_url(&self, repo: &str) -> String {
        if is_local_repo(repo) {
            String::new()
        } else {
            self.settings.repo_url(repo)
        }
    }

    /// Whether a finished run's pull request is merged or closed, or its branch (seen on origin
    /// before) is gone from it. Records a sighting of the branch. A local repository has no
    /// pull request and the forge is never asked: its run is done with once its branch is in
    /// its `main` and its worktree holds nothing uncommitted.
    fn merged_or_gone(&self, dir: &Path, rec: &RunRecord) -> bool {
        if is_local_repo(&rec.repo) {
            let bare = git::bare_path(&self.settings.root, &rec.repo);
            let wt = dir.join(WORKTREE_DIR);
            return git::merged_into_local_main(&bare, &rec.branch) == Some(true)
                && (!wt.exists() || git::worktree_clean(&wt));
        }
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

    /// The hourly cleanup. Build output goes early, worktrees later, `extra/` only `keep_days`
    /// after the run finished (a day after a warning push), run directories never.
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
            let artifacts = self.has_build_output(&dir, &rec);
            let terminal = rec.status.is_terminal();
            let gone = terminal && (artifacts || wt.exists()) && self.merged_or_gone(&dir, &rec);
            if artifacts && (gone || past_keep) {
                self.remove_build_output(&dir, &rec, now);
            }
            // extra/ holds work that is not in the branch: only keep_days counts, never a merge.
            let extra = dir.join(EXTRA_DIR);
            if !rec.status.is_live() && extra.exists() {
                // An empty extra/ needs no warning.
                if !dir_has_entries(&extra) || extra_warned_since(&rec, fin) {
                    if past_keep {
                        self.remove_extra(&dir, &rec, now);
                    }
                } else if age.saturating_add(DAY_MS) >= self.settings.keep_days * DAY_MS {
                    let at = rfc(now);
                    let _ = update_run(&dir, |r| r.cleanup.extra_warned = Some(at.clone()));
                    effects.push(Effect::Push {
                        title: "Run extra/ removed".into(),
                        body: format!("{}: extra/ is removed in about a day", rec.run_id),
                        run_id: rec.run_id.clone(),
                    });
                }
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
    /// build output of finished runs oldest first, then worktrees of reported, failed and
    /// cancelled runs oldest first, then `extra/` of runs the cleanup would remove it from now
    /// (past `keep_days`, warned), until both hold. Never a running or queued run, nor a paused
    /// run's worktree, nor `extra/` of a run within `keep_days` of finishing.
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
                if self.has_build_output(dir, rec) {
                    let f = self.remove_build_output(dir, rec, now);
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
            // extra/ last, and only where the cleanup itself would remove it now.
            let mut kept: Vec<String> = Vec::new();
            for (dir, rec) in &finished {
                if !over(total) {
                    break;
                }
                if rec.status.is_live() || !dir.join(EXTRA_DIR).exists() {
                    continue;
                }
                match self.extra_due(rec, now) {
                    Ok(()) => {
                        let f = self.remove_extra(dir, rec, now);
                        total = total.saturating_sub(f);
                        freed += f;
                    }
                    Err(why) => kept.push(format!("{} ({why})", rec.run_id)),
                }
            }
            if over(total) && !kept.is_empty() {
                eprintln!(
                    "jesse-bridge: hostrun SPACE PASS still over the limit; kept extra/ of {}: \
                     extra/ goes only keep_days ({}) after a run finishes, after a warning",
                    kept.join(", "),
                    self.settings.keep_days
                );
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

    /// `GET /jesse/runs`: every run not archived ([`ArchivedMode::Hide`]), live and newest
    /// first by creation; only the archived ones, newest archived first; or all of them.
    /// `counts` covers the whole store in the one scan, whatever the mode: the running,
    /// queued, needs_you and paused tallies as before, and the app's four filters, `needs_me`
    /// (needs_you, interrupted, failed), `moving` (queued, running, paused, reviewing), `finished`
    /// (everything else not archived) and `archived`.
    pub fn list_json_mode(&self, mode: ArchivedMode) -> Value {
        let all: Vec<RunRecord> = self.runs().into_iter().map(|(_, r)| r).collect();
        let (archived, open): (Vec<&RunRecord>, Vec<&RunRecord>) =
            all.iter().partition(|r| r.is_archived());
        let count = |s: RunStatus| open.iter().filter(|r| r.status == s).count();
        let needs_me = open.iter().filter(|r| r.status.needs_me()).count();
        let moving = open.iter().filter(|r| r.status.is_moving()).count();
        let counts = json!({
            "running": count(RunStatus::Running),
            "queued": count(RunStatus::Queued),
            "needs_you": count(RunStatus::NeedsYou),
            "paused": count(RunStatus::Paused),
            "needs_me": needs_me,
            "moving": moving,
            "finished": open.len() - needs_me - moving,
            "archived": archived.len(),
        });
        let mut runs: Vec<&RunRecord> = match mode {
            ArchivedMode::Hide => open,
            ArchivedMode::Only => archived,
            ArchivedMode::All => all.iter().collect(),
        };
        if mode == ArchivedMode::Only {
            runs.sort_by(|a, b| {
                b.archived_at
                    .cmp(&a.archived_at)
                    .then(b.run_id.cmp(&a.run_id))
            });
        } else {
            runs.sort_by(|a, b| {
                b.created_at
                    .cmp(&a.created_at)
                    .then(b.run_id.cmp(&a.run_id))
            });
        }
        let mut notes = StrandNotes::default();
        let served: Vec<Value> = runs.iter().map(|r| self.served(r, &mut notes)).collect();
        let h = lock(&self.house);
        json!({
            "runs": served,
            "counts": counts,
            "disk": {
                "total_bytes": h.last_total_bytes,
                "free_bytes": sys::free_bytes(&self.settings.root),
                "last_space_pass": h.last_space_pass,
            },
            "orphans": h.orphans,
        })
    }

    /// `GET /jesse/runs` with no `archived`: the list without archived runs.
    pub fn list_json(&self) -> Value {
        self.list_json_mode(ArchivedMode::Hide)
    }

    /// One run as the routes serve it: [`run_json_located`] plus the Work view's `title`,
    /// `last_line` (the last non blank line of its readable log) and `report_line` (the first
    /// line of its newest report). Additive: nothing `run_json` serves changes.
    pub fn served(&self, r: &RunRecord, notes: &mut StrandNotes) -> Value {
        let mut v = run_json_located(r, &self.notes_root);
        let root = &self.notes_root;
        let dir = self.settings.run_dir(&r.run_id);
        let title = work::run_title(
            r,
            |strand, item| notes.line_title(root, strand, item),
            || {
                r.prompt_path
                    .as_deref()
                    .and_then(|p| std::fs::read_to_string(root.join(locate_report(root, p))).ok())
                    .and_then(|src| work::h1_title(&src))
                    .or_else(|| {
                        [PROMPT_MD, SECTION_MD].iter().find_map(|n| {
                            std::fs::read_to_string(dir.join(n))
                                .ok()
                                .and_then(|src| work::h1_title(&src))
                        })
                    })
            },
        );
        let last_line = if valid_run_id(&r.run_id) {
            self.last_lines.get(&r.run_id, &dir)
        } else {
            None
        };
        let report_line = r
            .rounds
            .iter()
            .rev()
            .find_map(|x| x.report_path.as_deref())
            .and_then(|rel| std::fs::read_to_string(root.join(locate_report(root, rel))).ok())
            .or_else(|| std::fs::read_to_string(dir.join(REPORT_MD)).ok())
            .and_then(|src| work::report_line_of(&src));
        if let Some(o) = v.as_object_mut() {
            o.insert("title".into(), json!(title));
            o.insert("last_line".into(), json!(last_line));
            o.insert("report_line".into(), json!(report_line));
        }
        v
    }

    /// [`HostRuns::served`] for one run, outside a list.
    pub fn served_one(&self, r: &RunRecord) -> Value {
        self.served(r, &mut StrandNotes::default())
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

/// The strand notes one served list has read, by slug, so a list of runs parses each note once.
#[derive(Default)]
pub struct StrandNotes {
    read: HashMap<String, Option<crate::strands::Strand>>,
}

impl StrandNotes {
    /// The title of line `item_id` of `Strands/<slug>.md`, if the note and the line are there.
    pub fn line_title(&mut self, notes_root: &Path, slug: &str, item_id: &str) -> Option<String> {
        let strand = self.read.entry(slug.to_string()).or_insert_with(|| {
            if !crate::strands::is_safe_slug(slug) {
                return None;
            }
            let rel = format!("{STRANDS_DIR}/{slug}.md");
            let src = std::fs::read_to_string(notes_root.join(&rel)).ok()?;
            Some(parse_strand(slug, &src))
        });
        strand
            .as_ref()?
            .items
            .iter()
            .find(|i| i.id == item_id)
            .map(|i| work::line_title(&i.raw))
    }
}

/// A run id names one directory under `runs/` and nothing else.
pub fn valid_run_id(id: &str) -> bool {
    devjob::valid_label_value(id) && !id.contains("..")
}

/// Where a report the run recorded at `rel` lives now, vault relative.
///
/// A report is written to `Inbox/` and its path recorded in `run.json` once. Processing the
/// Inbox then files a read report under `Inbox/archive/`, and the recorded path names a note
/// that is no longer there: the app's Report link opened "doesn't exist on the Studio" for
/// every run whose report had been processed. So the path served is the note's CURRENT one:
/// the recorded path while it exists, else the same file under the folder's `archive/`, else
/// the recorded path unchanged (nothing better is known; the app says it is missing).
pub fn locate_report(notes_root: &Path, rel: &str) -> String {
    if notes_root.join(rel).is_file() {
        return rel.to_string();
    }
    let p = Path::new(rel);
    if let (Some(parent), Some(name)) = (p.parent(), p.file_name()) {
        let archived = parent.join(crate::strands::ARCHIVE_SEGMENT).join(name);
        if notes_root.join(&archived).is_file() {
            return archived.to_string_lossy().into_owned();
        }
    }
    rel.to_string()
}

/// [`run_json`] with every report path resolved to where the note lives now
/// ([`locate_report`]): what the routes serve.
pub fn run_json_located(r: &RunRecord, notes_root: &Path) -> Value {
    let mut v = run_json(r);
    v["report_paths"] = json!(r
        .rounds
        .iter()
        .filter_map(|x| x.report_path.as_deref())
        .map(|rel| locate_report(notes_root, rel))
        .collect::<Vec<_>>());
    if !r.reviews.is_empty() {
        v["reviews"] = reviews_json(r, Some(notes_root));
    }
    v
}

/// The run's reviews as the routes and tools serve them: each one's number, reviewer, origin,
/// status, the head it read and the base, its note's path (where the note is now, when the notes
/// directory is known), its finding count, why it failed, the round that answers it and any
/// leaks. The findings themselves are in the note. Served only for a run that has a review.
pub fn reviews_json(r: &RunRecord, notes_root: Option<&Path>) -> Value {
    json!(r
        .reviews
        .iter()
        .map(|v| json!({
            "n": v.n,
            "reviewer": v.reviewer,
            "reviewer_label": v.reviewer_label,
            "origin": v.origin,
            "status": v.status,
            "head": v.head,
            "base": v.base,
            "path": v.path.as_deref().map(|p| match notes_root {
                Some(n) => locate_report(n, p),
                None => p.to_string(),
            }),
            "findings": v.findings.len(),
            "error": v.error,
            "answer_round": v.answer_round,
            "requested_at": v.requested_at,
            "finished_at": v.finished_at,
            "leaks": v.leaks,
        }))
        .collect::<Vec<_>>())
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
        // Who reviews it and why: every run says, a 0.211.0 record as it must have been.
        o.insert("reviewers".into(), json!(r.reviewer_ids()));
        o.insert("reviewers_source".into(), json!(r.source_of_reviewers()));
        // Only once there is one: a run never reviewed serves no `reviews`.
        if !r.reviews.is_empty() {
            o.insert("reviews".into(), reviews_json(r, None));
        }
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
    today: &str,
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
            .filter(|r| r.strand.as_deref() == Some(strand.slug.as_str()) && r.status.is_active())
            .count();
        obj.insert("runs_active".into(), json!(active as u32));
        let needs_you = runs
            .iter()
            .filter(|r| {
                r.strand.as_deref() == Some(strand.slug.as_str()) && r.status == RunStatus::NeedsYou
            })
            .count();
        obj.insert("runs_needs_you".into(), json!(needs_you as u32));
        // `studio_runnable`'s check, kept with its reason: `Ok` is the draft's path.
        let runnable = |item: &crate::strands::StrandItem| -> Result<String, String> {
            if service.is_none() {
                return Err("Runs are not enabled on this bridge.".into());
            }
            studio_item_of(
                notes_root,
                &strand.slug,
                item,
                &strand.repos,
                OptIn::Request { repo: None },
            )
            .map(|s| s.prompt_path)
        };
        let drafts: Vec<Value> = strand
            .items
            .iter()
            .map(|i| {
                let check = runnable(i);
                let draft_src = check
                    .as_ref()
                    .ok()
                    .and_then(|p| std::fs::read_to_string(notes_root.join(p)).ok());
                let live = runs.iter().any(|r| {
                    r.step() == Some((strand.slug.as_str(), i.id.as_str())) && r.status.is_live()
                });
                let startable = work::startable(i, &check, draft_src.as_deref(), live);
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
                    "studio_runnable": check.is_ok(),
                    "title": work::line_title(&i.raw),
                    "startable": startable.is_ok(),
                    "startable_reason": startable.err(),
                })
            })
            .collect();
        obj.insert("drafts".into(), json!(drafts));
        // The board row's three phrases (0.205.0), from the note's own words and its lines'
        // titles and texts, by id.
        let (lines, texts): (Vec<_>, Vec<_>) = strand
            .items
            .iter()
            .filter(|i| !i.id.is_empty())
            .map(|i| {
                (
                    (i.id.clone(), work::line_title(&i.raw)),
                    (i.id.clone(), i.text.clone()),
                )
            })
            .unzip();
        let headline = strand
            .now
            .as_deref()
            .and_then(|n| work::row::headline(n, &lines, today));
        // The first unticked Queue line, named once: Next shows it, and a remark gate gives way
        // to it in the same words.
        let next = strand
            .items
            .iter()
            .find(|i| !i.checked && i.section == StrandSection::Queue)
            .and_then(|i| work::row::next_title(&i.text, &work::line_title(&i.raw), &lines));
        let your_action = strand
            .waiting
            .as_ref()
            .and_then(|w| work::row::your_action(&w.text, &lines, next.as_deref(), &texts));
        obj.insert("headline".into(), json!(headline));
        obj.insert("next_title".into(), json!(next));
        obj.insert("your_action".into(), json!(your_action));
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
/// (`draft`). `repo` names the repository when the draft does not; `local/<name>` is a Studio
/// only one. With none named, a strand that lists no repos runs in `local/<strand slug>` and a
/// loose draft in `local/scratch` ([`HostRuns::create`]).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartBody {
    pub strand: Option<String>,
    pub item_id: Option<String>,
    pub draft: Option<String>,
    pub repo: Option<String>,
    #[serde(default)]
    pub again: bool,
    /// Who reviews the run before it finishes ([`review`]): a registry id, a list of at most
    /// three, or `none`. Absent: the draft's, else `[hostrun] default_reviewers`.
    pub reviewer: Option<review::ReviewerArg>,
    /// `reviewer`'s list spelling; refused beside it.
    pub reviewers: Option<Vec<String>>,
    /// What the reviewer should look at most closely, at most 4 KiB.
    pub review_focus: Option<String>,
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

/// `POST /jesse/runs/:run_id/review`: `{ reviewer, focus?, auto_reply? }`; `reviewer` is one
/// id or a list of at most three.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewBody {
    pub reviewer: review::ReviewerArg,
    pub focus: Option<String>,
    pub auto_reply: Option<bool>,
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

/// `GET /jesse/runs?archived=only|all`.
#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub archived: Option<String>,
}

#[derive(Deserialize)]
pub struct LogQuery {
    pub tail: Option<usize>,
    /// `cloud` for a cloud run ([`cloud`]).
    pub host: Option<String>,
}

/// `?host=` on a per run route: `cloud` names a cloud run ([`cloud`]).
#[derive(Deserialize, Default)]
pub struct HostQuery {
    pub host: Option<String>,
}

/// The cloud host's server, when this bridge has one.
fn cloud_of(st: &AppState) -> Option<Arc<crate::devjob::DevJobs>> {
    cloud::cloud_server(st.cfg.devjob.service())
}

/// Refuse a per run action on a cloud run ([`cloud::refuse_cloud`]).
async fn refuse_cloud(
    st: &AppState,
    svc: &HostRuns,
    run_id: &str,
    q: &HostQuery,
) -> Result<(), ApiError> {
    let studio_has = svc.get(run_id).is_some();
    cloud::refuse_cloud(
        run_id,
        q.host.as_deref(),
        studio_has,
        cloud::cloud_has(cloud_of(st), run_id),
    )
    .await
}

/// `GET /jesse/runs`: the Studio's runs and, beside them, the cloud's ([`cloud`]). The cloud is
/// asked while the Studio's store is scanned, and for at most [`cloud::CLOUD_LIST_TIMEOUT`].
pub async fn jesse_runs(
    State(st): State<AppState>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    let mode =
        ArchivedMode::parse(q.archived.as_deref()).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let studio = svc.clone();
    let (v, cloud) = tokio::join!(
        tokio::task::spawn_blocking(move || studio.list_json_mode(mode)),
        cloud::list_cloud(cloud_of(&st)),
    );
    let mut v = v.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let v = tokio::task::spawn_blocking(move || {
        svc.add_cloud(&mut v, mode, &cloud);
        v
    })
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
    let reviewer = review::merge_spellings(body.reviewer.clone(), body.reviewers.clone())
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let review_focus = body.review_focus.clone();
    let kind = body.kind().map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let started = svc
        .start(StartRequest {
            kind,
            origin: "app".into(),
            again,
            reviewer,
            review_focus,
        })
        .await
        .map_err(refusal)?;
    Ok(Json(started_json(&started)).into_response())
}

/// `GET /jesse/runs/:run_id[?host=cloud]`: a Studio run, else (or with `host=cloud`, only) a
/// cloud run by its run id or Job name.
pub async fn jesse_run(
    State(st): State<AppState>,
    UrlPath(run_id): UrlPath<String>,
    Query(q): Query<HostQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    if !cloud::asked_cloud(q.host.as_deref())? {
        if let Some(r) = svc.get(&run_id) {
            return Ok(Json(svc.served_one(&r)).into_response());
        }
    }
    let (v, run) =
        cloud::cloud_lookup_answer(&run_id, cloud::cloud_detail(cloud_of(&st), &run_id).await)?;
    let (v, title) = tokio::task::spawn_blocking(move || {
        let t = svc.cloud_title(&mut StrandNotes::default(), &v);
        (v, t)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(cloud::cloud_detail_json(&v, &run, title)).into_response())
}

/// `GET /jesse/runs/:run_id/log?tail=N[&host=cloud]`: a Studio run's readable log, else (or
/// with `host=cloud`, only) the last lines of a cloud run's.
pub async fn jesse_run_log(
    State(st): State<AppState>,
    UrlPath(run_id): UrlPath<String>,
    Query(q): Query<LogQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    let tail = q.tail.unwrap_or(LOG_DEFAULT);
    if !cloud::asked_cloud(q.host.as_deref())? {
        let id = run_id.clone();
        let lines = tokio::task::spawn_blocking(move || svc.log_lines(&id, tail))
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if let Some(l) = lines {
            return Ok(Json(log_json(&run_id, l)).into_response());
        }
    }
    let server = cloud_of(&st);
    let (v, run) =
        cloud::cloud_lookup_answer(&run_id, cloud::cloud_detail(server.clone(), &run_id).await)?;
    let kept = server.and_then(|c| c.kept_tail(&v.name));
    let lines = cloud::cloud_log_lines(kept, &run, tail.clamp(1, LOG_MAX));
    Ok(Json(log_json(&run_id, lines)).into_response())
}

/// `POST /jesse/runs/:run_id/reply` with `{ text }`.
pub async fn jesse_run_reply(
    State(st): State<AppState>,
    UrlPath(run_id): UrlPath<String>,
    Query(q): Query<HostQuery>,
    headers: HeaderMap,
    Json(body): Json<ReplyBody>,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    refuse_cloud(&st, &svc, &run_id, &q).await?;
    let r = svc
        .reply_and_pump(&run_id, &body.text)
        .await
        .map_err(refusal)?;
    Ok(Json(svc.served_one(&r)).into_response())
}

/// `POST /jesse/runs/:run_id/review` with `{ reviewer, focus?, auto_reply? }`: a review of a
/// finished run by any registry model, read only ([`HostRuns::request_review`]).
pub async fn jesse_run_review(
    State(st): State<AppState>,
    UrlPath(run_id): UrlPath<String>,
    Query(q): Query<HostQuery>,
    headers: HeaderMap,
    Json(body): Json<ReviewBody>,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    refuse_cloud(&st, &svc, &run_id, &q).await?;
    let r = svc
        .request_review_and_start(
            &run_id,
            body.reviewer,
            body.focus,
            body.auto_reply.unwrap_or(true),
        )
        .await
        .map_err(refusal)?;
    Ok(Json(svc.served_one(&r)).into_response())
}

/// `POST /jesse/runs/:run_id/cancel`.
pub async fn jesse_run_cancel(
    State(st): State<AppState>,
    UrlPath(run_id): UrlPath<String>,
    Query(q): Query<HostQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    refuse_cloud(&st, &svc, &run_id, &q).await?;
    let r = svc.cancel_and_wake(&run_id).await.map_err(refusal)?;
    Ok(Json(svc.served_one(&r)).into_response())
}

/// `POST /jesse/runs/:run_id/close` with an optional `{ reason }`.
pub async fn jesse_run_close(
    State(st): State<AppState>,
    UrlPath(run_id): UrlPath<String>,
    Query(q): Query<HostQuery>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    refuse_cloud(&st, &svc, &run_id, &q).await?;
    let body = CloseBody::parse(&body).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let r = svc
        .close_blocking(&run_id, body.reason)
        .await
        .map_err(refusal)?;
    Ok(Json(run_json(&r)).into_response())
}

/// `POST /jesse/runs/:run_id/archive`: the run as the routes serve it, now archived. 409 for
/// a queued, running or paused run.
pub async fn jesse_run_archive(
    State(st): State<AppState>,
    UrlPath(run_id): UrlPath<String>,
    Query(q): Query<HostQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    refuse_cloud(&st, &svc, &run_id, &q).await?;
    let r = svc.archive_blocking(&run_id, true).await.map_err(refusal)?;
    Ok(Json(svc.served_one(&r)).into_response())
}

/// `POST /jesse/runs/:run_id/unarchive`: the run as the routes serve it, back in the list.
pub async fn jesse_run_unarchive(
    State(st): State<AppState>,
    UrlPath(run_id): UrlPath<String>,
    Query(q): Query<HostQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let svc = guard(&st, &headers)?;
    refuse_cloud(&st, &svc, &run_id, &q).await?;
    let r = svc
        .archive_blocking(&run_id, false)
        .await
        .map_err(refusal)?;
    Ok(Json(svc.served_one(&r)).into_response())
}

#[cfg(test)]
mod tests;
