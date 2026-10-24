//! **CODING AGENTS ON THE CLUSTER** — the `devjob` in-process server, its queue and its watcher.
//!
//! ---- WHAT IT DOES ----------------------------------------------------------------
//!
//! A main turn calls `devjob_start` with a repository, a prompt and an agent, and gets a job name
//! back in seconds. The bridge creates one ConfigMap (the prompt) and one Job in `jesse-dev`
//! from [`build_job`], a copy of `apps/jesse-dev/job-template.yaml` in the k3s repository,
//! field for field. The Job's entrypoint clones the repository, runs the agent and the test
//! command, pushes `jesse/<slug>` and opens a pull request; it never merges and never pushes to
//! `main`. Its log ends with a `JESSE-DEV-REPORT` block ([`parse_report`]).
//!
//! The bridge, never the agent, then watches the run to its end. [`DevJobs::run_watcher`] is a
//! background task owned by no turn: every [`WATCH_INTERVAL`] (and at once after a start) it
//! lists the labelled Jobs, writes the report of each finished one into the vault as
//! `Inbox/<date>-dev-job-<slug>.md` ([`render_report`]), pushes a notification, deletes the
//! ConfigMap and resumes the oldest queued Job if a slot is free.
//!
//! ---- WHY THE BRIDGE ------------------------------------------------------------------
//!
//! An MCP server a turn starts dies with the turn, and a run takes minutes to hours. The cluster
//! credential is cluster admin, so it lives in this process only: no child, stub or spawned
//! process reads the kubeconfig, which is why `[devjob]` refuses to boot without the gateway
//! ([`DevJobSettings::problems`]). The tool can choose none of a file path, a namespace, an image
//! or a credential: the arguments are [`DevJobSpec`]'s fields and nothing else.
//!
//! ---- RESTART SAFETY ------------------------------------------------------------------
//!
//! **ALL STATE IS THE CLUSTER'S.** Nothing about a run is kept in memory between passes: the queue
//! is the set of suspended Jobs, a finished run is reported when its Job lacks
//! [`ANN_REPORTED`], and the report file names its Job ([`find_existing_report`]), so a bridge
//! that dies between writing a report and annotating its Job writes no second one.
//!
//! ---- THE QUEUE -----------------------------------------------------------------------
//!
//! `[devjob] max_concurrent` (default 2, the namespace quota's pod count). A start while the cap
//! is reached creates the Job with `spec.suspend: true`: Kubernetes starts a Job's
//! `activeDeadlineSeconds` clock on resume, whereas a Job whose pod the quota refuses is retried
//! with its clock already running, so the wait would come out of its time.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{ConfigMap, Event, Pod};
use kube::api::{DeleteParams, ListParams, LogParams, Patch, PatchParams, PostParams};
use kube::{Api, Client};
use serde::Serialize;

use crate::*;

// ---- NAMES ---------------------------------------------------------------------------

/// The server's row name: the middle of every `mcp__devjob__<tool>` grant.
pub const DEVJOB_SERVER: &str = "devjob";
/// Start a run. Classed `write`: the Job it creates can push a branch and open a pull request.
pub const TOOL_START: &str = "devjob_start";
/// One run's phase, pending reason, log tail and report. A read.
pub const TOOL_STATUS: &str = "devjob_status";
/// Running, queued and recently finished runs. A read.
pub const TOOL_LIST: &str = "devjob_list";
/// The three tools, in the order the server lists them.
pub const DEVJOB_TOOLS: [&str; 3] = [TOOL_START, TOOL_STATUS, TOOL_LIST];

/// On every object a run creates: the selector the watcher lists by.
pub const LABEL_DEVJOB: &str = "jesse.dev/devjob";
/// On every object a run creates: the run's branch slug.
pub const LABEL_SLUG: &str = "jesse.dev/slug";
/// Set to `"true"` on a Job once its report is in the vault.
pub const ANN_REPORTED: &str = "jesse.dev/reported";
/// The run's spec minus the prompt, as JSON, on the Job.
pub const ANN_SPEC: &str = "jesse.dev/spec";
/// Who started the run: `turn`, or `schedule:<id>`.
pub const ANN_ORIGIN: &str = "jesse.dev/origin";
/// The outcome written to the vault (`pull request opened`, `no changes`, `failed`).
pub const ANN_OUTCOME: &str = "jesse.dev/outcome";
/// The report's path under the notes directory.
pub const ANN_REPORT_PATH: &str = "jesse.dev/report-path";
/// The pull request URL the report named, when there was one.
pub const ANN_PULL_REQUEST: &str = "jesse.dev/pull-request";
/// The label Kubernetes puts on a Job's pods.
const POD_JOB_LABEL: &str = "batch.kubernetes.io/job-name";

/// The default timeout of a run, in minutes: the template's `AGENT_TIMEOUT_SECONDS`.
pub const DEFAULT_TIMEOUT_MINUTES: u64 = 120;
/// The longest timeout a run may ask for, in minutes. Larger values are capped to this.
pub const MAX_TIMEOUT_MINUTES: u64 = 240;
/// The largest prompt, in bytes.
pub const MAX_PROMPT_BYTES: usize = 64 * 1024;
/// The largest test command, in bytes.
pub const MAX_TEST_COMMAND_BYTES: usize = 1024;
/// The longest branch slug.
pub const MAX_SLUG_CHARS: usize = 60;

/// The default `[devjob] namespace`.
pub const DEFAULT_NAMESPACE: &str = "jesse-dev";
/// The default `[devjob] max_concurrent`: the namespace quota's two pods.
pub const DEFAULT_MAX_CONCURRENT: usize = 2;
/// The default `[devjob] kubeconfig`, under the bridge user's home.
pub const DEFAULT_KUBECONFIG: &str = ".kube/ks-jesse.yaml";
/// The image the k3s template's `:latest` (and `:20260924`) resolved to on 2026-10-24, pinned by
/// digest. `[devjob] image` overrides it with another digest reference.
pub const DEFAULT_IMAGE: &str = "ghcr.io/jeremyandrews/jesse-dev-agent@sha256:90c3d190f98d7fca5b2f31b90585a7528d203ab797696b98a77ce4ca4f328304";

/// How often the watcher lists the labelled Jobs when nothing wakes it.
pub const WATCH_INTERVAL: Duration = Duration::from_secs(15);
/// Log lines a status answer carries.
pub const STATUS_LOG_LINES: usize = 40;
/// Log lines the vault report carries.
pub const REPORT_LOG_LINES: usize = 200;
/// Finished runs `devjob_list` names.
pub const LIST_FINISHED: usize = 20;
/// How much of a finished pod's log is read: the tail, where the report block is.
const LOG_TAIL_LINES: i64 = 5000;

/// The two report markers, each a whole line.
pub const REPORT_START: &str = "JESSE-DEV-REPORT";
pub const REPORT_END: &str = "JESSE-DEV-REPORT-END";

// ---- THE SPEC ----------------------------------------------------------------------

/// The agent a run starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
    Gemini,
}

impl Agent {
    pub fn parse(raw: &str) -> Result<Agent, String> {
        match raw.trim() {
            "claude" => Ok(Agent::Claude),
            "codex" => Ok(Agent::Codex),
            "gemini" => Ok(Agent::Gemini),
            other => Err(format!(
                "agent {other:?} is not one of claude, codex, gemini"
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Gemini => "gemini",
        }
    }
}

/// One run, as asked for. Built only through [`DevJobSpec::validate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DevJobSpec {
    /// `owner/name` on GitHub. Checked by pattern only: there is deliberately no allowlist.
    pub repo: String,
    pub prompt: String,
    pub agent: Agent,
    /// The agent's own model id, passed through as `MODEL`. `None` is the agent's default.
    pub model: Option<String>,
    /// `jesse/<branch_slug>` is the branch; it also names the report.
    pub branch_slug: String,
    pub test_command: Option<String>,
    /// Minutes, already capped at [`MAX_TIMEOUT_MINUTES`].
    pub timeout_minutes: u64,
}

/// What a caller hands in: the tool's arguments, or a schedule entry's fields. Unknown keys are
/// refused, so an argument naming a namespace, an image, a path or a credential is an error
/// rather than a key that is quietly ignored.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevJobArgs {
    pub repo: Option<String>,
    pub prompt: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub branch_slug: Option<String>,
    pub test_command: Option<String>,
    pub timeout_minutes: Option<u64>,
}

/// Whether `repo` is `owner/name` as GitHub spells one.
pub fn valid_repo(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else {
        return false;
    };
    let owner_ok = !owner.is_empty()
        && owner.len() <= 39
        && !owner.starts_with('-')
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let name_ok = !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    owner_ok && name_ok
}

/// Whether `slug` is lowercase letters, digits and hyphens, at most [`MAX_SLUG_CHARS`], starting
/// and ending with a letter or digit (it becomes part of a Kubernetes name and a label value).
pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.chars().count() <= MAX_SLUG_CHARS
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
}

/// Whether `model` is a plausible model id: no whitespace or control characters, at most 200.
fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 200
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/' | '@'))
}

impl DevJobSpec {
    /// Check every field. `default_agent` applies when `agent` is absent.
    pub fn validate(args: DevJobArgs, default_agent: Agent) -> Result<DevJobSpec, String> {
        let repo = args.repo.unwrap_or_default().trim().to_string();
        if !valid_repo(&repo) {
            return Err(format!("repo {repo:?} is not owner/name"));
        }
        let prompt = args.prompt.unwrap_or_default();
        if prompt.trim().is_empty() {
            return Err("prompt is empty".into());
        }
        if prompt.len() > MAX_PROMPT_BYTES {
            return Err(format!(
                "prompt is {} bytes; the most is {MAX_PROMPT_BYTES}",
                prompt.len()
            ));
        }
        let agent = match args.agent.as_deref() {
            None => default_agent,
            Some(a) => Agent::parse(a)?,
        };
        let model = match args.model.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(m) if valid_model(m) => Some(m.to_string()),
            Some(m) => return Err(format!("model {m:?} is not a model id")),
        };
        let branch_slug = args.branch_slug.unwrap_or_default().trim().to_string();
        if !valid_slug(&branch_slug) {
            return Err(format!(
                "branch_slug {branch_slug:?} must be lowercase letters, digits and hyphens, \
                 at most {MAX_SLUG_CHARS}, starting and ending with a letter or digit"
            ));
        }
        let test_command = match args.test_command {
            None => None,
            Some(t) if t.trim().is_empty() => None,
            Some(t) if t.len() > MAX_TEST_COMMAND_BYTES => {
                return Err(format!(
                    "test_command is {} bytes; the most is {MAX_TEST_COMMAND_BYTES}",
                    t.len()
                ))
            }
            Some(t) if t.contains('\0') => return Err("test_command contains a NUL".into()),
            Some(t) => Some(t),
        };
        let timeout_minutes = match args.timeout_minutes {
            None => DEFAULT_TIMEOUT_MINUTES,
            Some(0) => return Err("timeout_minutes must be at least 1".into()),
            Some(m) => m.min(MAX_TIMEOUT_MINUTES),
        };
        Ok(DevJobSpec {
            repo,
            prompt,
            agent,
            model,
            branch_slug,
            test_command,
            timeout_minutes,
        })
    }

    /// The entrypoint's `AGENT_TIMEOUT_SECONDS`.
    pub fn agent_timeout_secs(&self) -> u64 {
        self.timeout_minutes * 60
    }

    /// The Job's `activeDeadlineSeconds`: twice the agent timeout, the template's rule ("so the
    /// entrypoint gets to print its report before Kubernetes kills the pod"). 14400 for the
    /// default two hours, exactly the template's value.
    pub fn active_deadline_secs(&self) -> u64 {
        self.agent_timeout_secs() * 2
    }
}

/// The spec minus the prompt, as the Job carries it in [`ANN_SPEC`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecAnnotation {
    pub repo: String,
    pub agent: String,
    pub model: Option<String>,
    pub branch_slug: String,
    pub test_command: Option<String>,
    pub timeout_minutes: u64,
}

impl From<&DevJobSpec> for SpecAnnotation {
    fn from(s: &DevJobSpec) -> Self {
        SpecAnnotation {
            repo: s.repo.clone(),
            agent: s.agent.label().to_string(),
            model: s.model.clone(),
            branch_slug: s.branch_slug.clone(),
            test_command: s.test_command.clone(),
            timeout_minutes: s.timeout_minutes,
        }
    }
}

// ---- SETTINGS ------------------------------------------------------------------------

/// `[devjob]` in the bridge TOML.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevJobToml {
    #[serde(default)]
    pub enabled: bool,
    pub kubeconfig: Option<PathBuf>,
    pub namespace: Option<String>,
    pub image: Option<String>,
    pub max_concurrent: Option<usize>,
    pub default_agent: Option<String>,
}

/// `[devjob]`, resolved. [`DevJobSettings::problems`] says whether it may boot.
#[derive(Debug, Clone)]
pub struct DevJobSettings {
    pub enabled: bool,
    pub kubeconfig: PathBuf,
    pub namespace: String,
    pub image: String,
    pub max_concurrent: usize,
    /// `Err` holds the unparseable value, for [`DevJobSettings::problems`].
    pub default_agent: Result<Agent, String>,
    /// The running service, filled at startup when enabled. Cloning shares it.
    pub service: DevJobSlot,
}

impl Default for DevJobSettings {
    fn default() -> Self {
        DevJobSettings::from_toml(None, "")
    }
}

/// Where the running [`DevJobs`] lives once `main` has built it.
#[derive(Clone, Default)]
pub struct DevJobSlot(pub Arc<OnceLock<Arc<DevJobs>>>);

impl std::fmt::Debug for DevJobSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DevJobSlot(running: {})", self.0.get().is_some())
    }
}

impl DevJobSettings {
    pub fn from_toml(t: Option<DevJobToml>, home: &str) -> DevJobSettings {
        let t = t.unwrap_or_default();
        let kubeconfig = match t.kubeconfig {
            Some(p) => expand_home(&p, home),
            None => Path::new(home).join(DEFAULT_KUBECONFIG),
        };
        DevJobSettings {
            enabled: t.enabled,
            kubeconfig,
            namespace: t
                .namespace
                .map(|n| n.trim().to_string())
                .unwrap_or_else(|| DEFAULT_NAMESPACE.to_string()),
            image: t
                .image
                .map(|i| i.trim().to_string())
                .unwrap_or_else(|| DEFAULT_IMAGE.to_string()),
            max_concurrent: t.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT),
            default_agent: match t.default_agent {
                None => Ok(Agent::Claude),
                Some(a) => Agent::parse(&a).map_err(|_| a),
            },
            service: DevJobSlot::default(),
        }
    }

    /// The running service, when `[devjob] enabled` and startup built it.
    pub fn service(&self) -> Option<Arc<DevJobs>> {
        self.service.0.get().cloned()
    }

    /// Why this table may not boot, by name. Empty when it is off: nothing is read then.
    pub fn problems(&self, gateway_enabled: bool) -> Vec<String> {
        let mut out = Vec::new();
        if !self.enabled {
            return out;
        }
        if !gateway_enabled {
            out.push(
                "[devjob] enabled needs [gateway] enabled: the kubeconfig must stay in the \
                 bridge process, and only the gateway serves an in-process server"
                    .into(),
            );
        }
        if let Err(e) = read_kubeconfig(&self.kubeconfig) {
            out.push(format!(
                "[devjob] kubeconfig {} is unreadable: {e}",
                self.kubeconfig.display()
            ));
        }
        if !is_digest_reference(&self.image) {
            out.push(format!(
                "[devjob] image {:?} is not pinned by digest (<repository>@sha256:<64 hex>)",
                self.image
            ));
        }
        if self.namespace.is_empty() || !valid_slug(&self.namespace) {
            out.push(format!(
                "[devjob] namespace {:?} is not a namespace name",
                self.namespace
            ));
        }
        if self.max_concurrent == 0 {
            out.push("[devjob] max_concurrent must be at least 1".into());
        }
        if let Err(a) = &self.default_agent {
            out.push(format!(
                "[devjob] default_agent {a:?} is not one of claude, codex, gemini"
            ));
        }
        out
    }
}

/// `~/x` against `home`; anything else as written.
fn expand_home(p: &Path, home: &str) -> PathBuf {
    match p.strip_prefix("~") {
        Ok(rest) if !home.is_empty() => Path::new(home).join(rest),
        _ => p.to_path_buf(),
    }
}

/// Whether `image` is `<repository>@sha256:<64 lowercase hex>`.
pub fn is_digest_reference(image: &str) -> bool {
    let Some((repo, digest)) = image.split_once('@') else {
        return false;
    };
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return false;
    };
    !repo.is_empty()
        && !repo.contains(char::is_whitespace)
        && hex.len() == 64
        && hex
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Read and parse the kubeconfig. The one place the bridge opens it.
pub fn read_kubeconfig(path: &Path) -> Result<kube::config::Kubeconfig, String> {
    kube::config::Kubeconfig::read_from(path).map_err(|e| e.to_string())
}

/// The startup gate's `[devjob]` half.
pub fn validate_devjob(cfg: &Config) -> Vec<ConfigError> {
    cfg.devjob
        .problems(cfg.gateway.enabled)
        .into_iter()
        .map(|message| ConfigError {
            model: None,
            message,
        })
        .collect()
}

// ---- THE REPORT BLOCK ----------------------------------------------------------------

/// The parsed report block. Every key the contract names; `extra` keeps any it does not.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DevJobReport {
    /// `ok` or `failed`.
    pub status: String,
    pub failure: Option<String>,
    pub repo: Option<String>,
    pub agent: Option<String>,
    pub agent_version: Option<String>,
    pub model: Option<String>,
    pub branch: Option<String>,
    /// A URL, or `none`.
    pub pull_request: Option<String>,
    pub test_command: Option<String>,
    pub test_command_exit: Option<String>,
    pub agent_exit: Option<String>,
    pub wall_seconds: Option<u64>,
    pub agent_final_message: Option<String>,
    pub extra: BTreeMap<String, String>,
}

impl DevJobReport {
    pub fn ok(&self) -> bool {
        self.status == "ok"
    }

    /// The pull request URL, when one was opened.
    pub fn pull_request_url(&self) -> Option<&str> {
        self.pull_request
            .as_deref()
            .filter(|p| p.starts_with("https://") || p.starts_with("http://"))
    }

    /// The report a log with no complete block stands for: a failed run whose report is missing.
    pub fn missing(why: &str) -> DevJobReport {
        DevJobReport {
            status: "failed".into(),
            failure: Some(why.to_string()),
            ..DevJobReport::default()
        }
    }

    /// `(key, value)` rows in the contract's order, for a table.
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let mut rows = vec![("status", self.status.clone())];
        let opt = |k: &'static str, v: &Option<String>, rows: &mut Vec<(&'static str, String)>| {
            if let Some(v) = v {
                rows.push((k, v.clone()));
            }
        };
        opt("failure", &self.failure, &mut rows);
        opt("repo", &self.repo, &mut rows);
        opt("agent", &self.agent, &mut rows);
        opt("agent_version", &self.agent_version, &mut rows);
        opt("model", &self.model, &mut rows);
        opt("branch", &self.branch, &mut rows);
        opt("pull_request", &self.pull_request, &mut rows);
        opt("test_command", &self.test_command, &mut rows);
        opt("test_command_exit", &self.test_command_exit, &mut rows);
        opt("agent_exit", &self.agent_exit, &mut rows);
        if let Some(w) = self.wall_seconds {
            rows.push(("wall_seconds", w.to_string()));
        }
        rows
    }
}

/// What a run came to, for a push title and the Job's [`ANN_OUTCOME`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    PullRequestOpened,
    NoChanges,
    Failed,
}

impl RunOutcome {
    pub fn of(report: &DevJobReport) -> RunOutcome {
        if !report.ok() {
            RunOutcome::Failed
        } else if report.pull_request_url().is_some() {
            RunOutcome::PullRequestOpened
        } else {
            RunOutcome::NoChanges
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RunOutcome::PullRequestOpened => "pull request opened",
            RunOutcome::NoChanges => "no changes",
            RunOutcome::Failed => "failed",
        }
    }

    /// The push title.
    pub fn title(self) -> &'static str {
        match self {
            RunOutcome::PullRequestOpened => "Pull request opened",
            RunOutcome::NoChanges => "No changes",
            RunOutcome::Failed => "Failed",
        }
    }
}

/// Parse the report block out of a pod log.
///
/// **ONLY THE LAST BLOCK, AND ONLY BETWEEN ITS TWO MARKER LINES.** The agent's own output comes
/// earlier in the log and can contain anything, a marker line included, so the block is the one
/// that ends at the LAST [`REPORT_END`] line and starts at the last [`REPORT_START`] line before
/// it. A log with no end marker is a failed run whose report is missing.
pub fn parse_report(log: &str) -> Result<DevJobReport, String> {
    let lines: Vec<&str> = log.lines().map(|l| l.trim_end_matches('\r')).collect();
    let Some(end) = lines.iter().rposition(|l| *l == REPORT_END) else {
        return Err(format!(
            "the log has no {REPORT_END} line: the run's report is missing"
        ));
    };
    let Some(start) = lines[..end].iter().rposition(|l| *l == REPORT_START) else {
        return Err(format!(
            "the log has a {REPORT_END} line with no {REPORT_START} line before it"
        ));
    };
    let body = &lines[start + 1..end];
    let mut r = DevJobReport::default();
    let mut i = 0;
    while i < body.len() {
        let line = body[i];
        i += 1;
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key == "agent_final_message" && value == "|" {
            let mut msg = Vec::new();
            while i < body.len() && (body[i].starts_with(' ') || body[i].is_empty()) {
                msg.push(body[i]);
                i += 1;
            }
            let indent = msg
                .iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.len() - l.trim_start().len())
                .min()
                .unwrap_or(0);
            let text: Vec<&str> = msg
                .iter()
                .map(|l| if l.len() >= indent { &l[indent..] } else { "" })
                .collect();
            r.agent_final_message = Some(text.join("\n").trim_end().to_string());
            continue;
        }
        let v = Some(value.to_string());
        match key {
            "status" => r.status = value.to_string(),
            "failure" => r.failure = v,
            "repo" => r.repo = v,
            "agent" => r.agent = v,
            "agent_version" => r.agent_version = v,
            "model" => r.model = v,
            "branch" => r.branch = v,
            "pull_request" => r.pull_request = v,
            "test_command" => r.test_command = v,
            "test_command_exit" => r.test_command_exit = v,
            "agent_exit" => r.agent_exit = v,
            "wall_seconds" => r.wall_seconds = value.parse().ok(),
            "agent_final_message" => r.agent_final_message = v,
            other => {
                r.extra.insert(other.to_string(), value.to_string());
            }
        }
    }
    if r.status != "ok" && r.status != "failed" {
        let seen = std::mem::take(&mut r.status);
        r.status = "failed".into();
        if r.failure.is_none() {
            r.failure = Some(format!(
                "the report's status is {seen:?}, which is neither ok nor failed"
            ));
        }
    }
    Ok(r)
}

/// The last `n` lines of `log`.
pub fn tail_lines(log: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.to_string())
        .collect()
}

// ---- THE OBJECTS ---------------------------------------------------------------------

/// The names one run's objects take: `agent-<stem>` and `prompt-<stem>`, where the stem is the
/// slug (cut to 50) and a five-character suffix, so two runs with one slug never collide and the
/// Job name stays within the 63 characters a label value may hold.
pub fn object_names(slug: &str, suffix: &str) -> (String, String) {
    let stem: String = slug.chars().take(50).collect();
    let stem = stem.trim_end_matches('-');
    (
        format!("agent-{stem}-{suffix}"),
        format!("prompt-{stem}-{suffix}"),
    )
}

/// A five-character lowercase suffix from the clock and the random source.
fn name_suffix() -> String {
    let hex = random_hex();
    let n = u64::from_str_radix(&hex[..12.min(hex.len())], 16).unwrap_or(0)
        ^ system_time_to_ms(SystemTime::now());
    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789";
    (0..5)
        .map(|i| alphabet[((n >> (i * 6)) % 36) as usize] as char)
        .collect()
}

fn run_labels(slug: &str) -> Value {
    json!({ LABEL_DEVJOB: "true", LABEL_SLUG: slug })
}

/// The prompt ConfigMap: the template's first document.
pub fn build_config_map(namespace: &str, name: &str, spec: &DevJobSpec) -> ConfigMap {
    serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": run_labels(&spec.branch_slug),
        },
        "data": { "prompt.md": spec.prompt },
    }))
    .expect("a ConfigMap literal deserialises")
}

/// The Job: `apps/jesse-dev/job-template.yaml` in the k3s repository at `b8b6c00`, field for
/// field, with the run's values filled in. The image is the configured digest; the deadline is
/// [`DevJobSpec::active_deadline_secs`]; `suspend` queues it. The labels and annotations are the
/// bridge's own. No toleration and no node selector beyond the template's (it has none).
pub fn build_job(
    namespace: &str,
    job_name: &str,
    config_map: &str,
    image: &str,
    spec: &DevJobSpec,
    suspend: bool,
    origin: &str,
) -> Job {
    let annotation = serde_json::to_string(&SpecAnnotation::from(spec)).expect("serialises");
    serde_json::from_value(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": job_name,
            "namespace": namespace,
            "labels": run_labels(&spec.branch_slug),
            "annotations": { ANN_SPEC: annotation, ANN_ORIGIN: origin },
        },
        "spec": {
            "suspend": suspend,
            "backoffLimit": 0,
            "activeDeadlineSeconds": spec.active_deadline_secs(),
            "ttlSecondsAfterFinished": 172800,
            "template": {
                "metadata": { "labels": run_labels(&spec.branch_slug) },
                "spec": {
                    "restartPolicy": "Never",
                    "securityContext": {
                        "runAsUser": 1000,
                        "runAsGroup": 1000,
                        "runAsNonRoot": true,
                        "fsGroup": 1000,
                    },
                    "containers": [{
                        "name": "agent",
                        "image": image,
                        "imagePullPolicy": "Always",
                        "securityContext": { "allowPrivilegeEscalation": false },
                        "env": [
                            { "name": "REPO", "value": spec.repo },
                            { "name": "AGENT", "value": spec.agent.label() },
                            { "name": "MODEL", "value": spec.model.clone().unwrap_or_default() },
                            { "name": "BRANCH_SLUG", "value": spec.branch_slug },
                            { "name": "PR_TITLE", "value": spec.branch_slug },
                            { "name": "TEST_COMMAND", "value": spec.test_command.clone().unwrap_or_default() },
                            { "name": "PROMPT_FILE", "value": "/prompt/prompt.md" },
                            { "name": "AGENT_TIMEOUT_SECONDS", "value": spec.agent_timeout_secs().to_string() },
                        ],
                        "envFrom": [{ "secretRef": { "name": "jesse-dev-agent-keys" } }],
                        "resources": {
                            "requests": { "cpu": "4", "memory": "16Gi" },
                            "limits": { "cpu": "8", "memory": "32Gi" },
                        },
                        "volumeMounts": [
                            { "name": "prompt", "mountPath": "/prompt", "readOnly": true },
                            { "name": "work", "mountPath": "/home/agent/work" },
                            { "name": "cache", "mountPath": "/home/agent/.cargo/registry", "subPath": "cargo-registry" },
                            { "name": "cache", "mountPath": "/home/agent/.npm", "subPath": "npm" },
                        ],
                    }],
                    "volumes": [
                        { "name": "prompt", "configMap": { "name": config_map } },
                        { "name": "work", "emptyDir": { "sizeLimit": "40Gi" } },
                        { "name": "cache", "persistentVolumeClaim": { "claimName": "jesse-dev-cache" } },
                    ],
                },
            },
        },
    }))
    .expect("a Job literal deserialises")
}

// ---- A JOB, AS THE WATCHER SEES IT ---------------------------------------------------

/// How a run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Suspended by the queue.
    Queued,
    /// Released, with no running pod yet (being scheduled, pulling, or refused a pod).
    Pending,
    Running,
    /// The Job completed or failed; its report may not be written yet.
    Finished,
}

/// One labelled Job, reduced to what the queue and the reports need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobView {
    pub name: String,
    pub slug: String,
    pub created_ms: u64,
    pub started_ms: Option<u64>,
    pub suspended: bool,
    /// `Some(succeeded)` once the Job has a `Complete` or `Failed` condition.
    pub finished: Option<bool>,
    pub finished_ms: Option<u64>,
    pub reported: bool,
    pub outcome: Option<String>,
    pub report_path: Option<String>,
    pub pull_request: Option<String>,
    pub origin: String,
    pub spec: SpecAnnotation,
    pub config_map: Option<String>,
    pub active_deadline_secs: Option<i64>,
    /// The `Failed` condition's reason and message (`DeadlineExceeded: …`), when it failed.
    pub failure_reason: Option<String>,
}

fn time_ms(t: &k8s_openapi::apimachinery::pkg::apis::meta::v1::Time) -> u64 {
    t.0.as_millisecond().max(0) as u64
}

impl JobView {
    pub fn of(job: &Job) -> JobView {
        let meta = &job.metadata;
        let ann = meta.annotations.clone().unwrap_or_default();
        let labels = meta.labels.clone().unwrap_or_default();
        let spec = job.spec.as_ref();
        let status = job.status.as_ref();
        let terminal = status.and_then(|s| s.conditions.as_ref()).and_then(|cs| {
            cs.iter()
                .find(|c| (c.type_ == "Complete" || c.type_ == "Failed") && c.status == "True")
        });
        let finished = terminal.map(|c| c.type_ == "Complete");
        let finished_ms = status
            .and_then(|s| s.completion_time.as_ref())
            .map(time_ms)
            .or_else(|| terminal.and_then(|c| c.last_transition_time.as_ref().map(time_ms)));
        let config_map = spec
            .and_then(|s| s.template.spec.as_ref())
            .and_then(|p| p.volumes.as_ref())
            .and_then(|vs| vs.iter().find(|v| v.name == "prompt"))
            .and_then(|v| v.config_map.as_ref())
            .map(|c| c.name.clone());
        JobView {
            name: meta.name.clone().unwrap_or_default(),
            slug: labels.get(LABEL_SLUG).cloned().unwrap_or_default(),
            created_ms: meta.creation_timestamp.as_ref().map(time_ms).unwrap_or(0),
            started_ms: status.and_then(|s| s.start_time.as_ref()).map(time_ms),
            suspended: spec.and_then(|s| s.suspend).unwrap_or(false),
            finished,
            finished_ms,
            reported: ann.get(ANN_REPORTED).map(String::as_str) == Some("true"),
            outcome: ann.get(ANN_OUTCOME).cloned(),
            report_path: ann.get(ANN_REPORT_PATH).cloned(),
            pull_request: ann.get(ANN_PULL_REQUEST).cloned(),
            origin: ann.get(ANN_ORIGIN).cloned().unwrap_or_default(),
            spec: ann
                .get(ANN_SPEC)
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default(),
            config_map,
            active_deadline_secs: spec.and_then(|s| s.active_deadline_seconds),
            failure_reason: terminal.filter(|c| c.type_ == "Failed").map(|c| {
                match (c.reason.as_deref(), c.message.as_deref()) {
                    (Some(r), Some(m)) => format!("{r}: {m}"),
                    (Some(r), None) => r.to_string(),
                    (None, Some(m)) => m.to_string(),
                    (None, None) => "Failed".to_string(),
                }
            }),
        }
    }

    /// Released and not finished: it holds (or is about to hold) one of the slots.
    pub fn is_active(&self) -> bool {
        self.finished.is_none() && !self.suspended
    }

    pub fn is_queued(&self) -> bool {
        self.finished.is_none() && self.suspended
    }
}

/// Whether a new run must be queued: the cap is reached, or something is already waiting (first
/// come, first served).
pub fn must_queue(views: &[JobView], max_concurrent: usize) -> bool {
    let active = views.iter().filter(|v| v.is_active()).count();
    active >= max_concurrent || views.iter().any(JobView::is_queued)
}

/// The queued Jobs to resume now, oldest first, as many as there are free slots.
pub fn to_resume(views: &[JobView], max_concurrent: usize) -> Vec<String> {
    let active = views.iter().filter(|v| v.is_active()).count();
    let free = max_concurrent.saturating_sub(active);
    let mut queued: Vec<&JobView> = views.iter().filter(|v| v.is_queued()).collect();
    queued.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then(a.name.cmp(&b.name)));
    queued
        .into_iter()
        .take(free)
        .map(|v| v.name.clone())
        .collect()
}

/// Why a pod is not running, from its status: the scheduler's reason while it is unscheduled (a
/// taint, the quota, no fitting node), else a waiting container's reason (an image pull).
pub fn pod_pending_reason(pod: &Pod) -> Option<String> {
    let status = pod.status.as_ref()?;
    if status.phase.as_deref() != Some("Pending") {
        return None;
    }
    if let Some(c) = status.conditions.as_ref().and_then(|cs| {
        cs.iter()
            .find(|c| c.type_ == "PodScheduled" && c.status == "False")
    }) {
        let reason = c.reason.clone().unwrap_or_else(|| "Unschedulable".into());
        return Some(match &c.message {
            Some(m) => format!("{reason}: {m}"),
            None => reason,
        });
    }
    let waiting = status
        .container_statuses
        .as_ref()
        .and_then(|cs| cs.iter().find_map(|c| c.state.as_ref()?.waiting.clone()));
    if let Some(w) = waiting {
        let reason = w.reason.unwrap_or_else(|| "Waiting".into());
        return Some(match w.message {
            Some(m) => format!("{reason}: {m}"),
            None => reason,
        });
    }
    Some("Pending".into())
}

// ---- THE VAULT REPORT ----------------------------------------------------------------

/// Everything [`render_report`] writes.
#[derive(Debug, Clone)]
pub struct ReportInput<'a> {
    pub job_name: &'a str,
    pub slug: &'a str,
    pub spec: &'a SpecAnnotation,
    /// `None` when the prompt ConfigMap was gone.
    pub prompt: Option<&'a str>,
    pub report: &'a DevJobReport,
    pub log_tail: &'a [String],
    pub queued_ms: u64,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
}

/// A fence longer than any run of backticks in `text`, so a prompt or a log that contains a
/// fence cannot close the block early.
fn fence_for(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

/// A table cell: pipes escaped, newlines flattened.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn local_stamp(ms: Option<u64>) -> String {
    use chrono::TimeZone;
    match ms.and_then(|m| chrono::Local.timestamp_millis_opt(m as i64).single()) {
        Some(t) => t.format("%Y-%m-%d %H:%M:%S %Z").to_string(),
        None => "—".into(),
    }
}

/// The wall time of a run: the report's own `wall_seconds`, else finish minus start.
pub fn wall_secs(
    report: &DevJobReport,
    started_ms: Option<u64>,
    finished_ms: Option<u64>,
) -> Option<u64> {
    report.wall_seconds.or(match (started_ms, finished_ms) {
        (Some(s), Some(f)) if f >= s => Some((f - s) / 1000),
        _ => None,
    })
}

/// `3m 34s`, `1h 02m`.
pub fn human_secs(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// The vault note for one finished run. Its front matter names the Job, which is how
/// [`find_existing_report`] knows the run was already reported.
pub fn render_report(r: &ReportInput) -> String {
    let outcome = RunOutcome::of(r.report);
    let mut s = String::new();
    s.push_str(&format!(
        "---\ndevjob: {}\nrepo: {}\noutcome: {}\n---\n\n",
        r.job_name,
        r.spec.repo,
        outcome.label()
    ));
    s.push_str(&format!("# Dev job {}: {}\n\n", r.slug, outcome.label()));
    if let Some(url) = r.report.pull_request_url() {
        s.push_str(&format!("Pull request: [{url}]({url})\n\n"));
    }
    s.push_str("## Spec\n\n| | |\n| --- | --- |\n");
    let model = r
        .spec
        .model
        .clone()
        .unwrap_or_else(|| "(agent default)".into());
    let test = r
        .spec
        .test_command
        .clone()
        .unwrap_or_else(|| "(none)".into());
    for (k, v) in [
        ("Repository", r.spec.repo.clone()),
        ("Agent", r.spec.agent.clone()),
        ("Model", model),
        ("Branch", format!("jesse/{}", r.spec.branch_slug)),
        ("Test command", test),
        ("Timeout", format!("{} min", r.spec.timeout_minutes)),
        ("Job", r.job_name.to_string()),
        ("Queued", local_stamp(Some(r.queued_ms))),
        ("Started", local_stamp(r.started_ms)),
        ("Finished", local_stamp(r.finished_ms)),
    ] {
        s.push_str(&format!("| {k} | {} |\n", cell(&v)));
    }
    s.push_str("\n### Prompt\n\n");
    match r.prompt {
        Some(p) => {
            let f = fence_for(p);
            s.push_str(&format!("{f}text\n{}\n{f}\n\n", p.trim_end()));
        }
        None => s.push_str("(the prompt ConfigMap was already gone)\n\n"),
    }
    s.push_str("## Report\n\n| Key | Value |\n| --- | --- |\n");
    for (k, v) in r.report.rows() {
        let v = if k == "pull_request" && r.report.pull_request_url().is_some() {
            format!("[{v}]({v})")
        } else {
            cell(&v)
        };
        s.push_str(&format!("| {k} | {v} |\n"));
    }
    for (k, v) in &r.report.extra {
        s.push_str(&format!("| {} | {} |\n", cell(k), cell(v)));
    }
    if let Some(m) = &r.report.agent_final_message {
        let f = fence_for(m);
        s.push_str(&format!("\n### Agent final message\n\n{f}text\n{m}\n{f}\n"));
    }
    let log = r.log_tail.join("\n");
    let f = fence_for(&log);
    s.push_str(&format!(
        "\n## Log (last {} lines)\n\n{f}text\n{log}\n{f}\n",
        r.log_tail.len()
    ));
    s
}

/// `Inbox/<date>-dev-job-<slug>.md`, or `-2`, `-3`, … when a run with the same slug was reported
/// the same day. `exists` answers for a path under the notes directory.
pub fn report_rel_path(date: &str, slug: &str, exists: impl Fn(&str) -> bool) -> String {
    let base = format!("{}/{date}-dev-job-{slug}", crate::vaultwrites::INBOX_DIR);
    let first = format!("{base}.md");
    if !exists(&first) {
        return first;
    }
    (2..)
        .map(|n| format!("{base}-{n}.md"))
        .find(|p| !exists(p))
        .expect("an unbounded range finds a free name")
}

/// The report already written for `job_name`, if any: a `*-dev-job-<slug>*.md` in the Inbox whose
/// front matter names the Job.
pub fn find_existing_report(notes_root: &Path, slug: &str, job_name: &str) -> Option<String> {
    let inbox = notes_root.join(crate::vaultwrites::INBOX_DIR);
    let marker = format!("-dev-job-{slug}");
    let needle = format!("\ndevjob: {job_name}\n");
    for e in std::fs::read_dir(&inbox).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".md") || !name.contains(&marker) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let head: String = text.chars().take(512).collect();
        if format!("\n{head}").contains(&needle) {
            return Some(format!("{}/{name}", crate::vaultwrites::INBOX_DIR));
        }
    }
    None
}

// ---- WHERE REPORTS GO ----------------------------------------------------------------

/// Tells the owner a run finished. The bridge's push, or a test's recorder.
pub trait Notifier: Send + Sync {
    fn notify(
        &self,
        title: String,
        body: String,
        job: String,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// Where the watcher writes and whom it tells. Attached once the app state exists.
#[derive(Clone)]
pub struct ReportSink {
    /// The vault's notes directory (`<vault>/vault`).
    pub notes_root: PathBuf,
    pub broker: Arc<LockBroker>,
    pub notifier: Option<Arc<dyn Notifier>>,
}

/// The bridge's push, as a [`Notifier`]: the same client, device and badge every other push uses.
pub struct AppNotifier {
    pub apns: Option<Arc<ApnsClient>>,
    pub devices: Arc<DeviceStore>,
    pub conversations: Arc<ConversationStore>,
    pub flags: Arc<FlagStore>,
}

impl AppNotifier {
    pub fn of(st: &AppState) -> AppNotifier {
        AppNotifier {
            apns: st.apns.clone(),
            devices: st.devices.clone(),
            conversations: st.conversations.clone(),
            flags: st.flags.clone(),
        }
    }
}

/// The push for a finished run: the outcome as the title, the slug and the wall time as the body.
pub fn build_devjob_payload(title: &str, body: &str, job: &str) -> Vec<u8> {
    json!({
        "aps": {
            "alert": { "title": title, "body": body },
            "sound": "default",
            "content-available": 1
        },
        "devjob": job,
    })
    .to_string()
    .into_bytes()
}

impl Notifier for AppNotifier {
    fn notify(
        &self,
        title: String,
        body: String,
        job: String,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let apns = self.apns.clone();
        let devices = self.devices.clone();
        let badge = unread_conversation_count(&self.conversations, &self.flags);
        Box::pin(async move {
            let Some(apns) = apns else {
                eprintln!("jesse-bridge: devjob PUSH {job} — push is not configured, nothing sent");
                return;
            };
            let Some(token) = devices.get() else {
                eprintln!("jesse-bridge: devjob PUSH {job} — no device registered, nothing sent");
                return;
            };
            match apns
                .push_payload(
                    &token,
                    build_devjob_payload(&title, &body, &job),
                    Some(badge),
                )
                .await
            {
                PushOutcome::Sent => eprintln!("jesse-bridge: devjob PUSH {job} sent"),
                PushOutcome::DeadToken => {
                    devices.clear();
                    eprintln!("jesse-bridge: devjob PUSH {job} — device token rejected, cleared");
                }
                PushOutcome::Failed(e) => {
                    eprintln!("jesse-bridge: devjob PUSH {job} failed: {e} — swallowed")
                }
            }
        })
    }
}

// ---- THE SERVICE -------------------------------------------------------------------

/// What a start returns.
#[derive(Debug, Clone, Serialize)]
pub struct Started {
    pub job_name: String,
    pub queued: bool,
    pub branch_slug: String,
    pub timeout_minutes: u64,
}

/// One run, as `devjob_status` reports it.
#[derive(Debug, Clone, Serialize)]
pub struct DevJobRun {
    pub job_name: String,
    pub pod_name: Option<String>,
    pub phase: Phase,
    pub slug: String,
    pub repo: String,
    pub agent: String,
    pub queued_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub elapsed_secs: Option<u64>,
    /// Why it is not running: queued behind the cap, or the scheduler's or the kubelet's reason.
    pub waiting_reason: Option<String>,
    pub reported: bool,
    pub outcome: Option<String>,
    pub report_path: Option<String>,
    pub report: Option<DevJobReport>,
    pub log_tail: Vec<String>,
    #[serde(skip)]
    pub queued_ms: u64,
    #[serde(skip)]
    pub started_ms: Option<u64>,
    #[serde(skip)]
    pub finished_ms: Option<u64>,
}

fn rfc3339(ms: Option<u64>) -> Option<String> {
    use chrono::TimeZone;
    ms.and_then(|m| chrono::Utc.timestamp_millis_opt(m as i64).single())
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// The `devjob` server: the cluster client, the queue and the watcher. One per process.
pub struct DevJobs {
    settings: DevJobSettings,
    default_agent: Agent,
    client: Client,
    sink: OnceLock<ReportSink>,
    /// The last [`REPORT_LOG_LINES`] of each running pod's log, refreshed every pass. The
    /// fallback for a report whose pod is gone by the time its Job finishes (Kubernetes deletes a
    /// pod that outlives `activeDeadlineSeconds`). Memory only: a restart loses it, and the
    /// report then says the log was gone.
    tails: Mutex<HashMap<String, Vec<String>>>,
    wake: tokio::sync::Notify,
    /// Serialises every queue decision (a start, a resume) against every other.
    gate: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for DevJobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevJobs")
            .field("namespace", &self.settings.namespace)
            .field("max_concurrent", &self.settings.max_concurrent)
            .finish_non_exhaustive()
    }
}

impl DevJobs {
    /// Build the client from the kubeconfig. Connects to nothing.
    pub async fn connect(settings: DevJobSettings) -> Result<DevJobs, String> {
        let kc = read_kubeconfig(&settings.kubeconfig)?;
        let config =
            kube::Config::from_custom_kubeconfig(kc, &kube::config::KubeConfigOptions::default())
                .await
                .map_err(|e| format!("the kubeconfig does not configure a client: {e}"))?;
        let client = Client::try_from(config).map_err(|e| format!("no Kubernetes client: {e}"))?;
        let default_agent = settings.default_agent.clone().unwrap_or(Agent::Claude);
        Ok(DevJobs {
            settings,
            default_agent,
            client,
            sink: OnceLock::new(),
            tails: Mutex::new(HashMap::new()),
            wake: tokio::sync::Notify::new(),
            gate: tokio::sync::Mutex::new(()),
        })
    }

    /// Where reports go. Until this is called the watcher keeps the queue moving but reports
    /// nothing, and a finished run waits, unreported, for the next pass after it.
    pub fn attach(&self, sink: ReportSink) {
        let _ = self.sink.set(sink);
        self.wake.notify_one();
    }

    pub fn default_agent(&self) -> Agent {
        self.default_agent
    }

    fn jobs(&self) -> Api<Job> {
        Api::namespaced(self.client.clone(), &self.settings.namespace)
    }

    fn pods(&self) -> Api<Pod> {
        Api::namespaced(self.client.clone(), &self.settings.namespace)
    }

    fn config_maps(&self) -> Api<ConfigMap> {
        Api::namespaced(self.client.clone(), &self.settings.namespace)
    }

    fn selector() -> String {
        format!("{LABEL_DEVJOB}=true")
    }

    /// Every labelled Job, as the watcher sees it.
    pub async fn views(&self) -> Result<Vec<(Job, JobView)>, String> {
        let list = self
            .jobs()
            .list(&ListParams::default().labels(&Self::selector()))
            .await
            .map_err(|e| format!("could not list the dev jobs: {e}"))?;
        Ok(list
            .items
            .into_iter()
            .map(|j| {
                let v = JobView::of(&j);
                (j, v)
            })
            .collect())
    }

    /// Start one run: the prompt ConfigMap, then the Job, suspended when the queue says so.
    /// `origin` is `turn` or `schedule:<id>`.
    pub async fn start(&self, spec: &DevJobSpec, origin: &str) -> Result<Started, String> {
        let _gate = self.gate.lock().await;
        let views: Vec<JobView> = self.views().await?.into_iter().map(|(_, v)| v).collect();
        let queued = must_queue(&views, self.settings.max_concurrent);
        let (job_name, cm_name) = object_names(&spec.branch_slug, &name_suffix());
        let ns = &self.settings.namespace;
        let cm = build_config_map(ns, &cm_name, spec);
        self.config_maps()
            .create(&PostParams::default(), &cm)
            .await
            .map_err(|e| format!("could not create the prompt ConfigMap: {e}"))?;
        let job = build_job(
            ns,
            &job_name,
            &cm_name,
            &self.settings.image,
            spec,
            queued,
            origin,
        );
        let created = match self.jobs().create(&PostParams::default(), &job).await {
            Ok(j) => j,
            Err(e) => {
                let _ = self
                    .config_maps()
                    .delete(&cm_name, &DeleteParams::default())
                    .await;
                return Err(format!("could not create the Job: {e}"));
            }
        };
        // The ConfigMap is owned by its Job, so the Job's TTL collects it even if the watcher
        // never reaches it. Best effort: the watcher deletes it at the end anyway.
        if let Some(uid) = created.metadata.uid.as_deref() {
            let owner = json!({ "metadata": { "ownerReferences": [{
                "apiVersion": "batch/v1", "kind": "Job", "name": job_name, "uid": uid,
            }]}});
            if let Err(e) = self
                .config_maps()
                .patch(&cm_name, &PatchParams::default(), &Patch::Merge(&owner))
                .await
            {
                eprintln!(
                    "jesse-bridge: devjob {job_name}: could not set the ConfigMap's owner: {e}"
                );
            }
        }
        eprintln!(
            "jesse-bridge: devjob START {job_name} repo={} agent={} slug={} queued={queued} origin={origin}",
            spec.repo,
            spec.agent.label(),
            spec.branch_slug
        );
        drop(_gate);
        self.wake.notify_one();
        Ok(Started {
            job_name,
            queued,
            branch_slug: spec.branch_slug.clone(),
            timeout_minutes: spec.timeout_minutes,
        })
    }

    /// The newest pod of `job`.
    async fn pod_of(&self, job: &str) -> Option<Pod> {
        let list = self
            .pods()
            .list(&ListParams::default().labels(&format!("{POD_JOB_LABEL}={job}")))
            .await
            .ok()?;
        list.items.into_iter().max_by_key(|p| {
            p.metadata
                .creation_timestamp
                .as_ref()
                .map(time_ms)
                .unwrap_or(0)
        })
    }

    async fn log_of(&self, pod: &str, tail: i64) -> Result<String, String> {
        self.pods()
            .logs(
                pod,
                &LogParams {
                    container: Some("agent".into()),
                    tail_lines: Some(tail),
                    ..LogParams::default()
                },
            )
            .await
            .map_err(|e| format!("could not read the log of {pod}: {e}"))
    }

    /// The newest Warning event on `job`, for a released Job that has no pod (the quota refused
    /// one, say).
    async fn job_warning(&self, job: &str) -> Option<String> {
        let events: Api<Event> = Api::namespaced(self.client.clone(), &self.settings.namespace);
        let list = events
            .list(&ListParams::default().fields(&format!(
                "involvedObject.kind=Job,involvedObject.name={job}"
            )))
            .await
            .ok()?;
        list.items
            .into_iter()
            .filter(|e| e.type_.as_deref() == Some("Warning"))
            .max_by_key(|e| {
                e.last_timestamp
                    .as_ref()
                    .map(time_ms)
                    .or_else(|| {
                        e.event_time
                            .as_ref()
                            .map(|t| t.0.as_millisecond().max(0) as u64)
                    })
                    .unwrap_or(0)
            })
            .map(|e| match (e.reason, e.message) {
                (Some(r), Some(m)) => format!("{r}: {m}"),
                (Some(r), None) => r,
                (None, Some(m)) => m,
                (None, None) => "a warning with no message".into(),
            })
    }

    /// One run's Job, reduced: no pod, no log. `Ok(None)` when no labelled Job has that name.
    pub async fn view(&self, name: &str) -> Result<Option<JobView>, String> {
        let job = self
            .jobs()
            .get_opt(name)
            .await
            .map_err(|e| format!("could not read the Job {name}: {e}"))?;
        Ok(job
            .filter(|j| {
                j.metadata
                    .labels
                    .as_ref()
                    .and_then(|l| l.get(LABEL_DEVJOB))
                    .is_some_and(|v| v == "true")
            })
            .map(|j| JobView::of(&j)))
    }

    /// One run, by Job name. `Ok(None)` when no labelled Job has that name.
    pub async fn run(&self, name: &str) -> Result<Option<DevJobRun>, String> {
        let Some(job) = self
            .jobs()
            .get_opt(name)
            .await
            .map_err(|e| format!("could not read the Job {name}: {e}"))?
        else {
            return Ok(None);
        };
        // Only a Job this server created: any other Job in the namespace is not a dev job.
        let ours = job
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get(LABEL_DEVJOB))
            .is_some_and(|v| v == "true");
        if !ours {
            return Ok(None);
        }
        Ok(Some(self.describe(&JobView::of(&job), true).await))
    }

    /// A view, with its pod's state, waiting reason, log tail and (once finished) report.
    async fn describe(&self, v: &JobView, with_log: bool) -> DevJobRun {
        let pod = if v.suspended {
            None
        } else {
            self.pod_of(&v.name).await
        };
        let pod_phase = pod
            .as_ref()
            .and_then(|p| p.status.as_ref())
            .and_then(|s| s.phase.clone());
        let phase = if v.finished.is_some() {
            Phase::Finished
        } else if v.suspended {
            Phase::Queued
        } else if pod_phase.as_deref() == Some("Running") {
            Phase::Running
        } else {
            Phase::Pending
        };
        let waiting_reason =
            match phase {
                Phase::Queued => Some(format!(
                    "queued: {} runs are already active ([devjob] max_concurrent)",
                    self.settings.max_concurrent
                )),
                Phase::Pending => match &pod {
                    Some(p) => pod_pending_reason(p),
                    None => Some(self.job_warning(&v.name).await.unwrap_or_else(|| {
                        "waiting for the Job controller to create a pod".into()
                    })),
                },
                _ => None,
            };
        let mut log_tail = Vec::new();
        let mut report = None;
        if let Some(p) = pod.as_ref().and_then(|p| p.metadata.name.clone()) {
            if with_log && pod_phase.as_deref() != Some("Pending") {
                if let Ok(log) = self.log_of(&p, LOG_TAIL_LINES).await {
                    log_tail = tail_lines(&log, STATUS_LOG_LINES);
                    if phase == Phase::Finished {
                        report =
                            Some(parse_report(&log).unwrap_or_else(|e| DevJobReport::missing(&e)));
                    }
                }
            }
        }
        let now = system_time_to_ms(SystemTime::now());
        let elapsed_secs = v
            .started_ms
            .map(|s| (v.finished_ms.unwrap_or(now).saturating_sub(s)) / 1000);
        DevJobRun {
            job_name: v.name.clone(),
            pod_name: pod.and_then(|p| p.metadata.name),
            phase,
            slug: v.slug.clone(),
            repo: v.spec.repo.clone(),
            agent: v.spec.agent.clone(),
            queued_at: rfc3339(Some(v.created_ms)).unwrap_or_default(),
            started_at: rfc3339(v.started_ms),
            finished_at: rfc3339(v.finished_ms),
            elapsed_secs,
            waiting_reason,
            reported: v.reported,
            outcome: v.outcome.clone(),
            report_path: v.report_path.clone(),
            report,
            log_tail,
            queued_ms: v.created_ms,
            started_ms: v.started_ms,
            finished_ms: v.finished_ms,
        }
    }

    /// Running, queued and the last [`LIST_FINISHED`] finished runs.
    pub async fn list(&self) -> Result<Value, String> {
        let views: Vec<JobView> = self.views().await?.into_iter().map(|(_, v)| v).collect();
        let mut running = Vec::new();
        let mut queued = Vec::new();
        let mut finished: Vec<&JobView> = Vec::new();
        for v in &views {
            if v.is_queued() {
                queued.push(v);
            } else if v.is_active() {
                running.push(v);
            } else {
                finished.push(v);
            }
        }
        queued.sort_by_key(|v| v.created_ms);
        running.sort_by_key(|v| v.created_ms);
        finished.sort_by_key(|v| std::cmp::Reverse(v.finished_ms.unwrap_or(v.created_ms)));
        finished.truncate(LIST_FINISHED);
        let brief = |v: &JobView| {
            json!({
                "job_name": v.name,
                "slug": v.slug,
                "repo": v.spec.repo,
                "agent": v.spec.agent,
                "queued_at": rfc3339(Some(v.created_ms)),
                "started_at": rfc3339(v.started_ms),
                "finished_at": rfc3339(v.finished_ms),
                "outcome": v.outcome,
                "reported": v.reported,
                "report_path": v.report_path,
            })
        };
        Ok(json!({
            "running": running.into_iter().map(brief).collect::<Vec<_>>(),
            "queued": queued.into_iter().map(brief).collect::<Vec<_>>(),
            "finished": finished.into_iter().map(brief).collect::<Vec<_>>(),
        }))
    }

    /// One watcher pass: report every finished, unreported run, then resume what the queue
    /// allows. Errors are logged and the next pass tries again.
    pub async fn reconcile(&self) {
        let views = match self.views().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("jesse-bridge: devjob watcher: {e}");
                return;
            }
        };
        // Follow every running pod: keep its log tail. A Job that no longer exists keeps none.
        {
            let names: std::collections::HashSet<&str> =
                views.iter().map(|(_, v)| v.name.as_str()).collect();
            self.tails
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|k, _| names.contains(k.as_str()));
        }
        for (_, v) in views.iter().filter(|(_, v)| v.is_active()) {
            if let Some(p) = self.pod_of(&v.name).await {
                let running = p.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running");
                if let (true, Some(name)) = (running, p.metadata.name.as_deref()) {
                    if let Ok(log) = self.log_of(name, REPORT_LOG_LINES as i64).await {
                        self.tails
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(v.name.clone(), tail_lines(&log, REPORT_LOG_LINES));
                    }
                }
            }
        }
        if let Some(sink) = self.sink.get() {
            for (_, v) in views
                .iter()
                .filter(|(_, v)| v.finished.is_some() && !v.reported)
            {
                if let Err(e) = self.finalize(v, sink).await {
                    eprintln!(
                        "jesse-bridge: devjob {}: report not written yet: {e}",
                        v.name
                    );
                }
            }
        }
        let _gate = self.gate.lock().await;
        // Re-listed under the gate, so a start that landed meanwhile is counted.
        let views: Vec<JobView> = match self.views().await {
            Ok(v) => v.into_iter().map(|(_, v)| v).collect(),
            Err(e) => {
                eprintln!("jesse-bridge: devjob watcher: {e}");
                return;
            }
        };
        for name in to_resume(&views, self.settings.max_concurrent) {
            let patch = json!({ "spec": { "suspend": false } });
            match self
                .jobs()
                .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
                .await
            {
                Ok(_) => eprintln!("jesse-bridge: devjob RESUME {name}"),
                Err(e) => eprintln!("jesse-bridge: devjob {name}: could not resume: {e}"),
            }
        }
    }

    /// Write one finished run's report, push, annotate its Job and delete its ConfigMap.
    ///
    /// Idempotent across restarts: a report already in the Inbox for this Job is not written
    /// (or pushed) again, only the annotation and the delete are redone.
    async fn finalize(&self, v: &JobView, sink: &ReportSink) -> Result<(), String> {
        let rel = match find_existing_report(&sink.notes_root, &v.slug, &v.name) {
            Some(rel) => {
                eprintln!("jesse-bridge: devjob {}: already reported at {rel}", v.name);
                (rel, None)
            }
            None => {
                let pod = self.pod_of(&v.name).await;
                let log = match pod.as_ref().and_then(|p| p.metadata.name.clone()) {
                    Some(p) => self.log_of(&p, LOG_TAIL_LINES).await,
                    None => Err("the Job has no pod left to read a log from".into()),
                };
                let kept = self
                    .tails
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&v.name)
                    .cloned();
                let (mut report, tail) = match &log {
                    Ok(text) => (
                        parse_report(text).unwrap_or_else(|e| DevJobReport::missing(&e)),
                        tail_lines(text, REPORT_LOG_LINES),
                    ),
                    // The pod is gone: the tail the watcher kept while it ran, if any.
                    Err(e) => (
                        DevJobReport::missing(&match &kept {
                            Some(_) => format!(
                                "{e}; the log below is the tail the bridge kept while it ran"
                            ),
                            None => e.clone(),
                        }),
                        kept.clone().unwrap_or_default(),
                    ),
                };
                if report.failure.is_some() && report.agent_exit.is_none() {
                    if let Some(why) = &v.failure_reason {
                        let f = report.failure.take().unwrap_or_default();
                        report.failure = Some(format!("the Job failed ({why}); {f}"));
                    }
                }
                let prompt = match &v.config_map {
                    Some(cm) => self
                        .config_maps()
                        .get_opt(cm)
                        .await
                        .ok()
                        .flatten()
                        .and_then(|c| c.data)
                        .and_then(|d| d.get("prompt.md").cloned()),
                    None => None,
                };
                let text = render_report(&ReportInput {
                    job_name: &v.name,
                    slug: &v.slug,
                    spec: &v.spec,
                    prompt: prompt.as_deref(),
                    report: &report,
                    log_tail: &tail,
                    queued_ms: v.created_ms,
                    started_ms: v.started_ms,
                    finished_ms: v.finished_ms,
                });
                let date = chrono::Local::now().format("%Y-%m-%d").to_string();
                let rel = write_report(sink, &v.name, &date, &v.slug, &text).await?;
                eprintln!(
                    "jesse-bridge: devjob REPORT {} outcome={} at {rel}",
                    v.name,
                    RunOutcome::of(&report).label()
                );
                (rel, Some(report))
            }
        };
        let (rel, report) = rel;
        let outcome = report.as_ref().map(RunOutcome::of);
        // A scheduled run is pushed by the scheduler, under its `notify`; only a turn's run pushes
        // here, so a scheduled run never pushes twice.
        if let (Some(report), Some(outcome), Some(n)) = (&report, outcome, &sink.notifier) {
            if !v.origin.starts_with("schedule:") {
                let wall = wall_secs(report, v.started_ms, v.finished_ms)
                    .map(human_secs)
                    .unwrap_or_else(|| "unknown time".into());
                n.notify(
                    outcome.title().to_string(),
                    format!("{} · {wall}", v.slug),
                    v.name.clone(),
                )
                .await;
            }
        }
        let mut ann = json!({ ANN_REPORTED: "true", ANN_REPORT_PATH: rel });
        if let Some(o) = outcome {
            ann[ANN_OUTCOME] = json!(o.label());
        }
        if let Some(url) = report.as_ref().and_then(|r| r.pull_request_url()) {
            ann[ANN_PULL_REQUEST] = json!(url);
        }
        let patch = json!({ "metadata": { "annotations": ann } });
        self.jobs()
            .patch(&v.name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| format!("could not annotate the Job: {e}"))?;
        self.tails
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&v.name);
        if let Some(cm) = &v.config_map {
            match self
                .config_maps()
                .delete(cm, &DeleteParams::default())
                .await
            {
                Ok(_) => {}
                Err(kube::Error::Api(s)) if s.code == 404 => {}
                Err(e) => eprintln!(
                    "jesse-bridge: devjob {}: could not delete {cm}: {e}",
                    v.name
                ),
            }
        }
        Ok(())
    }

    /// The watcher: a pass now, then one every [`WATCH_INTERVAL`] or whenever a start wakes it.
    /// Owned by no turn; runs for the life of the process.
    pub async fn run_watcher(self: Arc<Self>) {
        loop {
            self.reconcile().await;
            tokio::select! {
                _ = tokio::time::sleep(WATCH_INTERVAL) => {}
                _ = self.wake.notified() => {}
            }
        }
    }

    /// Start the watcher on the runtime.
    pub fn spawn_watcher(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(self.clone().run_watcher())
    }
}

/// Write `text` as the run's report, under the write lock, at a name no other report has.
async fn write_report(
    sink: &ReportSink,
    job: &str,
    date: &str,
    slug: &str,
    text: &str,
) -> Result<String, String> {
    let turn = format!("devjob:{job}");
    loop {
        let rel = report_rel_path(date, slug, |p| sink.notes_root.join(p).exists());
        let abs = sink.notes_root.join(&rel);
        let key = LockKey::Path(resolve_lock_path(&abs, "/"));
        if !sink.broker.acquire_direct(&key, &turn, "report").await {
            return Err(format!("the write lock on {rel} was not free"));
        }
        // Taken between choosing the name and locking it: choose again.
        if abs.exists() {
            sink.broker.release_call_direct(&turn, "report");
            continue;
        }
        let written = write_atomic(&abs, text.as_bytes());
        sink.broker.release_call_direct(&turn, "report");
        return written
            .map(|_| rel.clone())
            .map_err(|e| format!("could not write {rel}: {e}"));
    }
}

// ---- THE MCP FACE --------------------------------------------------------------------

/// The three tools, as `tools/list` entries.
pub fn tool_descriptors() -> Vec<Value> {
    vec![
        json!({
            "name": TOOL_START,
            "description": "Start a coding agent on the k3s cluster against a GitHub repository. \
                It clones the repository, runs the agent on the prompt and the test command, \
                pushes the branch jesse/<branch_slug> and opens a pull request if anything \
                changed; it never merges and never pushes to main. Returns the job name in \
                seconds and whether the run is queued. The bridge watches the run and writes \
                its report to Inbox/<date>-dev-job-<branch_slug>.md, then pushes a notification.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["repo", "prompt", "branch_slug"],
                "properties": {
                    "repo": { "type": "string", "description": "owner/name on GitHub" },
                    "prompt": { "type": "string", "description": "the task, at most 64 KiB" },
                    "agent": { "type": "string", "enum": ["claude", "codex", "gemini"],
                               "description": "default: the bridge's [devjob] default_agent" },
                    "model": { "type": "string", "description": "the agent's own model id; default: the agent's default" },
                    "branch_slug": { "type": "string",
                                     "description": "lowercase letters, digits and hyphens, at most 60" },
                    "test_command": { "type": "string", "description": "run after the agent, at most 1 KiB" },
                    "timeout_minutes": { "type": "integer", "minimum": 1,
                                         "description": "the agent's time limit; default 120, capped at 240" },
                },
            },
        }),
        json!({
            "name": TOOL_STATUS,
            "description": "One dev job: its phase (queued, pending, running, finished), why it \
                is waiting if it is, its elapsed time, the last 40 log lines, and the parsed \
                report once it has finished.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["job_name"],
                "properties": { "job_name": { "type": "string" } },
            },
        }),
        json!({
            "name": TOOL_LIST,
            "description": "The dev jobs running, queued, and the last 20 finished.",
            "inputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
        }),
    ]
}

fn text_result(v: &Value, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(v).unwrap_or_default() }],
        "isError": is_error,
    })
}

fn error_result(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusArgs {
    job_name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {}

impl crate::gateway::InProcessServer for DevJobs {
    fn tools(&self) -> Vec<Value> {
        tool_descriptors()
    }

    fn call<'a>(
        &'a self,
        tool: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>> {
        Box::pin(async move {
            match tool {
                TOOL_START => {
                    let args: DevJobArgs = match serde_json::from_value(arguments) {
                        Ok(a) => a,
                        Err(e) => return Ok(error_result(&format!("bad arguments: {e}"))),
                    };
                    let spec = match DevJobSpec::validate(args, self.default_agent) {
                        Ok(s) => s,
                        Err(e) => return Ok(error_result(&e)),
                    };
                    match self.start(&spec, "turn").await {
                        Ok(s) => Ok(text_result(&json!(s), false)),
                        Err(e) => Ok(error_result(&e)),
                    }
                }
                TOOL_STATUS => {
                    let args: StatusArgs = match serde_json::from_value(arguments) {
                        Ok(a) => a,
                        Err(e) => return Ok(error_result(&format!("bad arguments: {e}"))),
                    };
                    match self.run(args.job_name.trim()).await {
                        Ok(Some(r)) => Ok(text_result(&json!(r), false)),
                        Ok(None) => Ok(error_result(&format!(
                            "no dev job is named {:?}",
                            args.job_name
                        ))),
                        Err(e) => Ok(error_result(&e)),
                    }
                }
                TOOL_LIST => {
                    if let Err(e) = serde_json::from_value::<ListArgs>(arguments) {
                        return Ok(error_result(&format!("bad arguments: {e}")));
                    }
                    match self.list().await {
                        Ok(v) => Ok(text_result(&v, false)),
                        Err(e) => Ok(error_result(&e)),
                    }
                }
                other => Err(format!("devjob has no tool {other:?}")),
            }
        })
    }
}

/// The containment battery's `devjob`: the same three tools, listed exactly as the real server
/// lists them, and every call refused. Only in a `containment-probe` build.
#[cfg(feature = "containment-probe")]
pub struct BatteryStandIn;

#[cfg(feature = "containment-probe")]
impl crate::gateway::InProcessServer for BatteryStandIn {
    fn tools(&self) -> Vec<Value> {
        tool_descriptors()
    }

    fn call<'a>(
        &'a self,
        tool: &'a str,
        _arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>> {
        Box::pin(async move {
            eprintln!("containment: a child called devjob::{tool}; the stand-in started nothing");
            Ok(error_result(
                "the containment battery's devjob server starts no dev jobs",
            ))
        })
    }
}

#[cfg(test)]
mod tests;
