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

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{ConfigMap, Event, Pod};
use kube::api::{DeleteParams, ListParams, LogParams, Patch, PatchParams, PostParams};
use kube::{Api, Client};
use serde::{Deserialize, Serialize};

use crate::hostrun::sys::Forge as _;
use crate::*;

pub mod cloud;

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
/// Answer a Studio run (`host: "studio"`): a new round in the same session. Classed `write`.
pub const TOOL_REPLY: &str = "devjob_reply";
/// Cancel a queued or running Studio run. Classed `write`.
pub const TOOL_CANCEL: &str = "devjob_cancel";
/// Close a Studio run that needs you, was interrupted or failed. Classed `write`.
pub const TOOL_CLOSE: &str = "devjob_close";
/// Have a registry model review a finished Studio run, read only, and (by default) hand its
/// findings to the run's session as the next round. Classed `write`, beside `devjob_reply`.
pub const TOOL_REVIEW: &str = "devjob_review";
/// Hand off a review that is waiting (`auto_reply: false`). Classed `write`.
pub const TOOL_REVIEW_REPLY: &str = "devjob_review_reply";
/// Change a queued Studio run's priority. Classed `write`, beside `devjob_cancel`.
pub const TOOL_PRIORITY: &str = "devjob_priority";
/// The nine tools the server lists once `[hostrun]` is attached ([`DevJobs::attach_studio`]).
pub const STUDIO_TOOLS: [&str; 9] = [
    TOOL_START,
    TOOL_STATUS,
    TOOL_LIST,
    TOOL_REPLY,
    TOOL_CANCEL,
    TOOL_CLOSE,
    TOOL_REVIEW,
    TOOL_REVIEW_REPLY,
    TOOL_PRIORITY,
];

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
/// On a Job a strand entry started: the strand note's slug (`Strands/<slug>.md`).
pub const LABEL_STRAND: &str = "jesse.dev/strand";
/// On a Job a strand entry started: the id of the queue line it runs.
pub const LABEL_STRAND_ID: &str = "jesse.dev/strand-id";
/// On a Job a strand entry started: the prompt file's path under the notes directory.
pub const ANN_PROMPT: &str = "jesse.dev/prompt";
/// The label Kubernetes puts on a Job's pods.
const POD_JOB_LABEL: &str = "batch.kubernetes.io/job-name";

/// The default timeout of a run, in minutes: the template's `AGENT_TIMEOUT_SECONDS`.
pub const DEFAULT_TIMEOUT_MINUTES: u64 = 120;
/// The longest timeout a cluster or Studio run may ask for, in minutes: larger values are
/// capped to this (silently in [`DevJobSpec::validate`]; [`timeout_reduced_note`] is what tells
/// a `devjob_start` caller it happened). A cloud run's own, usually higher, cap is
/// `[devjob.cloud] max_timeout_minutes` ([`cloud::CloudSettings::max_timeout_minutes`]).
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
///
/// `host` picks where the run goes: absent or `cluster` is the cluster, exactly as before;
/// `studio` is a [`crate::hostrun`] run on this Mac. `strand`, `item_id` and `again` apply only
/// to a Studio run ([`plan_start`]). [`DevJobSpec::validate`] reads none of the four.
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
    pub host: Option<String>,
    pub strand: Option<String>,
    pub item_id: Option<String>,
    /// `host: "studio"`: a draft by its path, run as a free run `s-<file stem>`.
    pub draft: Option<String>,
    pub again: Option<bool>,
    /// `host: "studio"`: who reviews the run before it finishes: a registry id, a list of at
    /// most three, or `"none"`. Absent: the draft's, else `[hostrun] default_reviewers`.
    pub reviewer: Option<crate::hostrun::review::ReviewerArg>,
    /// `host: "studio"`: `reviewer`'s list spelling; refused beside it.
    pub reviewers: Option<Vec<String>>,
    /// `host: "studio"`, with `reviewer`: what the reviewer should look at most closely.
    pub review_focus: Option<String>,
    /// `host: "cloud"`: merge the pull request once CI on its head is green. Absent: the
    /// draft's `devjob-merge`, else no.
    pub merge: Option<bool>,
    /// `host: "studio"`: how soon the run wants a slot: `low`, `normal`, `high` or `urgent`.
    /// Absent: the draft's `devjob-priority:`, else `normal`. Anything else is refused.
    pub priority: Option<String>,
}

/// Where a `devjob_start` goes, decided from its arguments alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartPlan {
    /// No `host`, or `cluster`: the cluster, with the spec [`DevJobSpec::validate`] builds.
    Cluster(DevJobSpec),
    /// `host: "studio"`: a run on this Mac.
    Studio {
        kind: crate::hostrun::StartKind,
        again: bool,
        /// Who reviews the run (ids, or `none`), checked when the run is created; `None` is the
        /// draft's, else the bridge's default.
        reviewer: Option<crate::hostrun::review::ReviewerArg>,
        review_focus: Option<String>,
        /// How soon the run wants a slot; `None` is the draft's `devjob-priority:`, else `normal`.
        priority: Option<crate::hostrun::Priority>,
    },
    /// `host: "cloud"`: a Job on kc1 ([`cloud`]).
    Cloud(CloudStart),
}

/// What a cloud start names: a strand item, a draft or a free prompt, as on the Studio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudKind {
    Item {
        strand: String,
        item_id: String,
        repo: Option<String>,
    },
    Draft {
        draft: String,
        repo: Option<String>,
    },
    /// Checked already: the prompt is the spec's, the run id `s-<branch_slug>`.
    Free(DevJobSpec),
}

/// A `devjob_start` with `host: "cloud"`, decided from its arguments alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudStart {
    pub kind: CloudKind,
    /// The tool's `merge`; `None` defers to the draft's `devjob-merge`.
    pub merge: Option<bool>,
    /// `agent`, `test_command` and `timeout_minutes`, applied when a draft is resolved; a free
    /// prompt's are in its spec already.
    pub agent: Option<String>,
    pub test_command: Option<String>,
    pub timeout_minutes: Option<u64>,
    /// How soon the run wants a slot; `None` is the draft's `devjob-priority:`, else `normal`.
    pub priority: Option<crate::hostrun::Priority>,
    /// Who reviews the run, as on the Studio: `None` is the draft's, else the bridge's default.
    pub reviewer: Option<crate::hostrun::review::ReviewerArg>,
    pub review_focus: Option<String>,
}

/// Decide a start. Without `host` (or with `cluster`) this is [`DevJobSpec::validate`] and
/// nothing else, so the cluster path is unchanged; `strand`, `item_id` and `again` are refused
/// there. With `studio`, a strand item (`strand` and `item_id`, optional `repo`; its prompt and
/// model come from its draft, its repository from the draft, else `repo`, else the strand's one
/// `repos:` entry), a draft by its path (`draft`, optional `repo`), or a free prompt (`prompt`,
/// `branch_slug`, optional `repo` and `model`). Where nothing names a repository, a Studio run
/// goes to a Studio only `local/<name>` one ([`crate::hostrun`]); the cluster refuses
/// `local/<name>` and a start with no repository ([`LOCAL_ONLY_STUDIO`]). A Studio run has no clock, so `timeout_minutes` is refused, and it runs Claude Code,
/// so `agent` may only be `claude`.
pub fn plan_start(args: DevJobArgs, default_agent: Agent) -> Result<StartPlan, String> {
    match args.host.as_deref().map(str::trim) {
        None | Some("cluster") | Some("studio") if args.merge.is_some() => Err(
            "merge applies only with host \"cloud\": a Studio run merges as its prompt \
                 says, and the cluster never merges"
                .into(),
        ),
        // The cluster's own queue has no priority: a priority there would be silently ignored,
        // so it is refused. A cloud run waits in the Studio's queue, priority and all.
        None | Some("cluster") if args.priority.is_some() => Err(
            "priority applies only with host \"studio\" or \"cloud\": the cluster starts its \
             runs in the order they were asked for"
                .into(),
        ),
        None | Some("cluster") => {
            if args.strand.is_some()
                || args.item_id.is_some()
                || args.draft.is_some()
                || args.again.is_some()
            {
                return Err(
                    "strand, item_id, draft and again apply only with host \"studio\"".into(),
                );
            }
            // The cluster has no review path: `[hostrun] default_reviewers` does not apply to
            // it, `none` is accepted and changes nothing, and a reviewer by name is refused.
            let reviewer = crate::hostrun::review::merge_spellings(
                args.reviewer.clone(),
                args.reviewers.clone(),
            )?;
            let off = matches!(
                crate::hostrun::review::choose(reviewer.as_ref()),
                Ok(crate::hostrun::review::ReviewerChoice::Off)
            );
            if (reviewer.is_some() && !off) || args.review_focus.is_some() {
                return Err(
                    "reviewer and review_focus apply only with host \"studio\": the cluster's \
                     runs are not reviewed"
                        .into(),
                );
            }
            match args.repo.as_deref().map(str::trim) {
                None | Some("") => return Err(format!("repo is missing: {LOCAL_ONLY_STUDIO}")),
                Some(r) if is_local_repo(r) => return Err(LOCAL_ONLY_STUDIO.into()),
                Some(_) => {}
            }
            DevJobSpec::validate(args, default_agent).map(StartPlan::Cluster)
        }
        Some("studio") => {
            if args.timeout_minutes.is_some() {
                return Err(
                    "timeout_minutes does not apply on the Studio: a Studio run has no clock"
                        .into(),
                );
            }
            if args.test_command.is_some() {
                return Err(
                    "test_command does not apply on the Studio: the prompt names its checks".into(),
                );
            }
            if let Some(a) = args.agent.as_deref() {
                if Agent::parse(a)? != Agent::Claude {
                    return Err(format!("agent {a:?}: the Studio runs claude only"));
                }
            }
            let again = args.again.unwrap_or(false);
            let (reviewer, review_focus) = start_reviewer(&args)?;
            let priority = args
                .priority
                .as_deref()
                .map(crate::hostrun::Priority::parse)
                .transpose()?;
            let repo = match args.repo.as_deref().map(str::trim) {
                None => None,
                Some(r) if valid_repo(r) => Some(r.to_string()),
                Some(r) => return Err(format!("repo {r:?} is not owner/name")),
            };
            if let Some(draft) = args.draft {
                if args.strand.is_some()
                    || args.item_id.is_some()
                    || args.prompt.is_some()
                    || args.branch_slug.is_some()
                    || args.model.is_some()
                {
                    return Err(
                        "a draft takes its prompt and model from the file: pass draft \
                                alone, and repo if it names none"
                            .into(),
                    );
                }
                return Ok(StartPlan::Studio {
                    kind: crate::hostrun::StartKind::Draft {
                        draft: draft.trim().to_string(),
                        repo,
                    },
                    again,
                    reviewer: reviewer.clone(),
                    review_focus: review_focus.clone(),
                    priority,
                });
            }
            match (args.strand, args.item_id) {
                (Some(strand), Some(item_id)) => {
                    if args.prompt.is_some() || args.branch_slug.is_some() || args.model.is_some() {
                        return Err(
                            "a strand item takes its prompt and model from its draft: pass \
                             strand and item_id, and repo if its draft names none"
                                .into(),
                        );
                    }
                    Ok(StartPlan::Studio {
                        kind: crate::hostrun::StartKind::Item {
                            strand: strand.trim().to_string(),
                            item_id: item_id.trim().to_string(),
                            repo,
                            scheduled: false,
                        },
                        again,
                        reviewer: reviewer.clone(),
                        review_focus: review_focus.clone(),
                        priority,
                    })
                }
                (None, None) => {
                    let model = match args.model.as_deref().map(str::trim) {
                        None | Some("") => None,
                        Some(m) if valid_model(m) => Some(m.to_string()),
                        Some(m) => return Err(format!("model {m:?} is not a model id")),
                    };
                    Ok(StartPlan::Studio {
                        kind: crate::hostrun::StartKind::Free {
                            repo: args.repo.unwrap_or_default().trim().to_string(),
                            prompt: args.prompt.unwrap_or_default(),
                            slug: args.branch_slug.unwrap_or_default().trim().to_string(),
                            model,
                        },
                        again,
                        reviewer: reviewer.clone(),
                        review_focus: review_focus.clone(),
                        priority,
                    })
                }
                _ => Err("strand and item_id go together".into()),
            }
        }
        Some("cloud") => plan_cloud(args, default_agent).map(StartPlan::Cloud),
        Some(other) => Err(format!("host {other:?} is not cluster, studio or cloud")),
    }
}

/// The reviewers a Studio or cloud start names (`reviewer`, or its list spelling `reviewers`)
/// and its `review_focus`: one spelling, its shape checked now. Whether the registry knows each
/// id, the draft's `devjob-reviewer:` and `[hostrun] default_reviewers` are the run's creation's
/// ([`crate::hostrun::HostRuns::create_on`]), for both hosts.
fn start_reviewer(
    args: &DevJobArgs,
) -> Result<(Option<crate::hostrun::review::ReviewerArg>, Option<String>), String> {
    let reviewer =
        crate::hostrun::review::merge_spellings(args.reviewer.clone(), args.reviewers.clone())?;
    crate::hostrun::review::choose(reviewer.as_ref())?;
    Ok((reviewer, args.review_focus.clone()))
}

/// `host: "cloud"`: a strand item, a draft or a free prompt, like the Studio's three, with the
/// cluster's `agent`, `test_command` and `timeout_minutes`, reviewed as a Studio run is
/// ([`start_reviewer`]). No `again`, never a `local/<name>` repository.
fn plan_cloud(args: DevJobArgs, default_agent: Agent) -> Result<CloudStart, String> {
    if args.again.is_some() {
        return Err("again applies only with host \"studio\"".into());
    }
    let (reviewer, review_focus) = start_reviewer(&args)?;
    let repo = match args.repo.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(r) if is_local_repo(r) => return Err(LOCAL_ONLY_STUDIO.into()),
        Some(r) if valid_repo(r) => Some(r.to_string()),
        Some(r) => return Err(format!("repo {r:?} is not owner/name")),
    };
    if let Some(a) = args.agent.as_deref() {
        Agent::parse(a)?;
    }
    if args.timeout_minutes == Some(0) {
        return Err("timeout_minutes must be at least 1".into());
    }
    let merge = args.merge;
    let priority = args
        .priority
        .as_deref()
        .map(crate::hostrun::Priority::parse)
        .transpose()?;
    let from_draft = |kind: CloudKind, args: &DevJobArgs| -> Result<CloudStart, String> {
        if args.prompt.is_some() || args.branch_slug.is_some() || args.model.is_some() {
            return Err(
                "a draft takes its prompt and model from the file: pass strand and item_id, or \
                 draft, and repo if it names none"
                    .into(),
            );
        }
        Ok(CloudStart {
            kind,
            merge,
            agent: args.agent.clone(),
            test_command: args.test_command.clone(),
            timeout_minutes: args.timeout_minutes,
            priority,
            reviewer: reviewer.clone(),
            review_focus: review_focus.clone(),
        })
    };
    if let Some(draft) = args.draft.clone() {
        if args.strand.is_some() || args.item_id.is_some() {
            return Err("pass draft, or strand and item_id, not both".into());
        }
        return from_draft(
            CloudKind::Draft {
                draft: draft.trim().to_string(),
                repo,
            },
            &args,
        );
    }
    match (args.strand.clone(), args.item_id.clone()) {
        (Some(strand), Some(item_id)) => from_draft(
            CloudKind::Item {
                strand: strand.trim().to_string(),
                item_id: item_id.trim().to_string(),
                repo,
            },
            &args,
        ),
        (None, None) => {
            if repo.is_none() {
                return Err(
                    "repo is missing: a cloud run's free prompt names its repository".into(),
                );
            }
            let (agent, test_command, timeout_minutes) = (
                args.agent.clone(),
                args.test_command.clone(),
                args.timeout_minutes,
            );
            let spec = DevJobSpec::validate(args, default_agent)?;
            Ok(CloudStart {
                kind: CloudKind::Free(spec),
                merge,
                agent,
                test_command,
                timeout_minutes,
                priority,
                reviewer,
                review_focus,
            })
        }
        _ => Err("strand and item_id go together".into()),
    }
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

/// The reserved owner of a Studio only repository: `local/<name>` lives at
/// `<[hostrun] root>/local/<name>.git` and has no remote ([`is_local_repo`]).
pub const LOCAL_OWNER: &str = "local";

/// Why a cluster start of a local repository (or of no repository) is refused.
pub const LOCAL_ONLY_STUDIO: &str = "local repositories run only on the Studio (host: \"studio\")";

/// Whether `repo` is `local/<name>`, a repository that exists only on the Studio, `<name>`
/// following [`valid_repo`]'s name rules. Such a repository is never a GitHub one: no `gh`, no
/// `git push`, no clone URL.
pub fn is_local_repo(repo: &str) -> bool {
    valid_repo(repo)
        && repo
            .split_once('/')
            .is_some_and(|(owner, _)| owner == LOCAL_OWNER)
}

/// `local/<name>` for a name, when the name follows [`valid_repo`]'s rules.
pub fn local_repo(name: &str) -> Option<String> {
    let repo = format!("{LOCAL_OWNER}/{name}");
    is_local_repo(&repo).then_some(repo)
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

/// Whether `v` may be a Kubernetes label value: at most 63 of letters, digits, `-`, `_` and `.`,
/// starting and ending with a letter or digit. A strand's slug and a line's id become two.
pub fn valid_label_value(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 63
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && v.starts_with(|c: char| c.is_ascii_alphanumeric())
        && v.ends_with(|c: char| c.is_ascii_alphanumeric())
}

/// The strand step a run was started for: carried on its Job as [`LABEL_STRAND`],
/// [`LABEL_STRAND_ID`] and [`ANN_PROMPT`], and written into its report's front matter, so the
/// vault's morning routine can close the step. Never kept in memory: [`strand_of`] reads it
/// back from the Job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrandRef {
    /// The note's slug: `Strands/<note>.md`.
    pub note: String,
    /// The queue line's id.
    pub id: String,
    /// The prompt file, relative to the notes directory.
    pub prompt: String,
}

/// The strand step a Job was started for, from its labels and annotation. `None` for a run
/// the tool or a fixed schedule entry started.
pub fn strand_of(job: &Job) -> Option<StrandRef> {
    let labels = job.metadata.labels.as_ref()?;
    let note = labels.get(LABEL_STRAND)?.clone();
    let id = labels.get(LABEL_STRAND_ID)?.clone();
    let prompt = job
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(ANN_PROMPT))
        .cloned()
        .unwrap_or_default();
    Some(StrandRef { note, id, prompt })
}

/// Whether a Job carries both strand labels for `note` and `id`, in any phase.
pub fn carries_strand_step(job: &Job, note: &str, id: &str) -> bool {
    job.metadata.labels.as_ref().is_some_and(|l| {
        l.get(LABEL_STRAND).map(String::as_str) == Some(note)
            && l.get(LABEL_STRAND_ID).map(String::as_str) == Some(id)
    })
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
        DevJobSpec::validate_on(args, default_agent, false)
    }

    /// [`DevJobSpec::validate`], with `studio` saying the run goes to the Studio, where a
    /// `local/<name>` repository is allowed; the cluster refuses one ([`LOCAL_ONLY_STUDIO`]).
    pub fn validate_on(
        args: DevJobArgs,
        default_agent: Agent,
        studio: bool,
    ) -> Result<DevJobSpec, String> {
        let repo = args.repo.unwrap_or_default().trim().to_string();
        if !valid_repo(&repo) {
            return Err(format!("repo {repo:?} is not owner/name"));
        }
        if !studio && is_local_repo(&repo) {
            return Err(LOCAL_ONLY_STUDIO.into());
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

/// What `devjob_start`'s reply adds when a requested `timeout_minutes` was reduced to a host's
/// cap, naming both numbers and why. `None` when nothing was asked, or nothing was reduced
/// (`requested` is at or under `granted`, which by construction is also true whenever `granted`
/// is the cap itself: a reduction always lands exactly on it).
pub fn timeout_reduced_note(requested: Option<u64>, granted: u64, host: &str) -> Option<String> {
    match requested {
        Some(r) if r > granted => Some(format!(
            "timeout_minutes {r} was reduced to {granted}, {host}'s maximum"
        )),
        _ => None,
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
    /// `[devjob.cloud]`: the second cluster, kc1 ([`cloud`]).
    pub cloud: Option<cloud::CloudToml>,
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
    /// `[devjob.cloud]`, resolved; off unless it says `enabled = true`.
    pub cloud: cloud::CloudSettings,
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
            cloud: cloud::CloudSettings::from_toml(t.cloud, home),
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
            // Only the cloud table can be wrong now: it needs this one on.
            return self.cloud.problems(false);
        }
        out.extend(self.cloud.problems(true));
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
    /// A URL, or `none`: the run's own pull request.
    pub pull_request: Option<String>,
    /// Every pull request the run opened, in order, the run's own last: `<url> <squash commit>`
    /// or `<url>` alone, comma separated. Only a run that shipped a part or a prerequisite
    /// before its own pull request prints it ([`DevJobReport::pull_requests`]).
    pub pull_requests: Option<String>,
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

    /// Every pull request the run opened, in order: the report's `pull_requests` when it has
    /// one, else its one `pull_request` (an older image, or a run that shipped nothing first).
    /// Only `http(s)://` URLs; the squash commit when the entry names one.
    pub fn pull_requests(&self) -> Vec<ReportPullRequest> {
        let listed: Vec<ReportPullRequest> = self
            .pull_requests
            .as_deref()
            .unwrap_or("")
            .split(',')
            .filter_map(|entry| {
                let mut words = entry.split_whitespace();
                let url = words.next()?;
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return None;
                }
                let squash = words
                    .next()
                    .filter(|w| w.len() >= 7 && w.chars().all(|c| c.is_ascii_hexdigit()));
                Some(ReportPullRequest {
                    url: url.to_string(),
                    squash: squash.map(str::to_string),
                })
            })
            .collect();
        if !listed.is_empty() {
            return listed;
        }
        self.pull_request_url()
            .map(|url| {
                vec![ReportPullRequest {
                    url: url.to_string(),
                    squash: None,
                }]
            })
            .unwrap_or_default()
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
        opt("pull_requests", &self.pull_requests, &mut rows);
        opt("test_command", &self.test_command, &mut rows);
        opt("test_command_exit", &self.test_command_exit, &mut rows);
        opt("agent_exit", &self.agent_exit, &mut rows);
        if let Some(w) = self.wall_seconds {
            rows.push(("wall_seconds", w.to_string()));
        }
        rows
    }
}

/// One pull request a run opened ([`DevJobReport::pull_requests`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportPullRequest {
    pub url: String,
    /// Its squash commit on the default branch, when it merged and the report names it.
    pub squash: Option<String>,
}

/// The pull requests a run opened, for a push body: ` · pull requests #4, #5, #6` in order, or
/// nothing when it opened fewer than two (the push already says a pull request was opened).
pub fn pull_requests_push_suffix(report: &DevJobReport) -> String {
    let prs = report.pull_requests();
    if prs.len() < 2 {
        return String::new();
    }
    let names: Vec<String> = prs
        .iter()
        .map(|p| match p.url.rsplit_once("/pull/") {
            Some((_, n)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => {
                format!("#{n}")
            }
            _ => p.url.clone(),
        })
        .collect();
    format!(" · pull requests {}", names.join(", "))
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
            "pull_requests" => r.pull_requests = v,
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

/// What a cloud Job carries beyond the spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudJobExtras {
    pub run_id: String,
    pub merge: cloud::MergeDecision,
    /// `MERGE_DENY`: the deny list again, which the entrypoint checks itself.
    pub merge_deny: String,
    /// The pod's memory and CPU, from `[devjob.cloud]`.
    pub resources: cloud::JobResources,
    /// `[devjob.cloud] services`: Postgres and Redis sidecars, their share out of the agent's.
    pub services: bool,
    /// Set only for a continuation: its `RUN_ID` and `CONTINUATION` envs, and its chain
    /// annotations, come from here instead of `run_id` and link 0.
    pub chain: Option<ChainExtras>,
    /// A reviewed run's Job: its `REVIEW` env and [`cloud::ANN_REVIEW`].
    pub review: Option<cloud::ReviewMode>,
}

/// One earlier link of a continuation chain, as [`cloud::ANN_CHAIN`] keeps it (JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainLink {
    pub run_id: String,
    pub report_path: String,
    /// A short, human line: how this link ended (its failure, or that it hit its time limit).
    pub outcome: String,
    pub wall_seconds: Option<u64>,
    pub checkpoint_sha: Option<String>,
    /// The checkpoint's tree, for the "no progress" rule ([`cloud::continuation_decision`]).
    pub checkpoint_tree: Option<String>,
}

/// A Job's place in a continuation chain, read from its annotations
/// ([`cloud::ANN_CHAIN_ROOT`], [`cloud::ANN_CONTINUATION`], [`cloud::ANN_CONTINUATION_OF`],
/// [`cloud::ANN_CHAIN`]). A Job with none of them is link 0 of a chain rooted at its own run id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChainInfo {
    pub root: String,
    pub link: u32,
    pub continuation_of: Option<String>,
    /// Every link before this one, oldest first.
    pub chain: Vec<ChainLink>,
}

impl ChainInfo {
    pub fn of(job: &Job, own_run_id: &str) -> ChainInfo {
        let ann = job.metadata.annotations.clone().unwrap_or_default();
        let root = ann
            .get(cloud::ANN_CHAIN_ROOT)
            .cloned()
            .unwrap_or_else(|| own_run_id.to_string());
        let link = ann
            .get(cloud::ANN_CONTINUATION)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let continuation_of = ann.get(cloud::ANN_CONTINUATION_OF).cloned();
        let chain = ann
            .get(cloud::ANN_CHAIN)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        ChainInfo {
            root,
            link,
            continuation_of,
            chain,
        }
    }
}

/// What a continuation Job's [`CloudJobExtras::chain`] carries: the chain's root, this new
/// link's own number, the previous link's run id, and every earlier link, for
/// [`build_cloud_job`] to annotate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainExtras {
    pub root: String,
    pub link: u32,
    pub continuation_of: String,
    /// [`ChainInfo::chain`] (every link before this new one) serialised for [`cloud::ANN_CHAIN`].
    pub chain_json: String,
}

/// The Job for kc1: `cloud/infrastructure/jesse-dev/job-template.yaml` in the k3s repository,
/// field for field. Against [`build_job`]: the dev job pool's node selector and toleration, one
/// job per burst worker (a required pod anti-affinity on the hostname, whatever the requests),
/// no cache claim (the workers are disposable, so the caches live on the
/// run's 60Gi `emptyDir`), debug info and incremental builds off, and the merge path's variables.
/// Memory and CPU, request and limit, come from `extras.resources`: `[devjob.cloud]`
/// `job_memory_gi`, `job_memory_request_gi`, `job_cpu` and `job_cpu_request`, sized to its
/// `burst_size`. With `extras.services` the pod also runs Postgres 16 and Redis 7 as native
/// sidecars ([`service_sidecars`]), their fixed sizes taken out of the agent's so the pod's total
/// is still exactly the configured one, and the agent gets trovato CI's `DATABASE_URL` and
/// `REDIS_URL`. `CARGO_BUILD_JOBS` is the agent's CPU limit, rounded down, so cargo never runs
/// more rustc processes than the agent has CPUs: the parallel rustcs, not the CPU count, set a
/// build's peak memory.
/// cannot live past [`cloud::CloudSettings::effective_worker_life_minutes`], so doubling would
/// already not fit. The same deadline is also passed to the agent container as
/// `JOB_DEADLINE_SECONDS`, so the entrypoint knows its own wall-clock budget rather than
/// guessing one from `AGENT_TIMEOUT_SECONDS` alone. `terminationGracePeriodSeconds` is
/// [`cloud::CHECKPOINT_GRACE_SECS`], the entrypoint's time to push a SIGTERM checkpoint and print
/// its report before Kubernetes kills it. Always created suspended: it is released only onto an
/// idle worker whose remaining life covers it ([`cloud::cloud_resumable`]). With
/// `extras.chain` set (a continuation only) `RUN_ID` and `CONTINUATION` come from the chain's
/// root and this new link's own number, not this Job's own run id label, so the pull request
/// stays titled after the chain's first run; the chain annotations
/// ([`cloud::ANN_CHAIN_ROOT`] and siblings) are set only then.
pub fn build_cloud_job(
    namespace: &str,
    job_name: &str,
    config_map: &str,
    image: &str,
    spec: &DevJobSpec,
    extras: &CloudJobExtras,
    origin: &str,
) -> Job {
    let annotation = serde_json::to_string(&SpecAnnotation::from(spec)).expect("serialises");
    let mut labels = run_labels(&spec.branch_slug);
    labels[cloud::LABEL_RUN_ID] = json!(extras.run_id);
    let deadline = cloud::cloud_deadline_secs(spec);
    let agent = extras.resources.agent(extras.services);
    // A held Job never merges, even on an entrypoint that predates `REVIEW`: its `AUTO_MERGE`
    // says no, while the merge annotation keeps the run's decision for its continuation and
    // its answer.
    let auto_merge = match extras.review {
        Some(cloud::ReviewMode::Hold) => "",
        _ => extras.merge.auto_merge_env(),
    };
    let (run_id_env, continuation_env) = match &extras.chain {
        Some(c) => (c.root.clone(), c.link),
        None => (extras.run_id.clone(), 0u32),
    };
    let mut job = json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": job_name,
            "namespace": namespace,
            "labels": labels,
            "annotations": {
                ANN_SPEC: annotation,
                ANN_ORIGIN: origin,
                cloud::ANN_MERGE: extras.merge.annotation(),
            },
        },
        "spec": {
            "suspend": true,
            "backoffLimit": 0,
            "activeDeadlineSeconds": deadline,
            "ttlSecondsAfterFinished": 172800,
            "template": {
                "metadata": { "labels": labels },
                "spec": {
                    "restartPolicy": "Never",
                    // The entrypoint's time, between the deadline's SIGTERM and the SIGKILL, to
                    // push its SIGTERM checkpoint and print its report (Kubernetes' own default
                    // of 30s is not enough).
                    "terminationGracePeriodSeconds": cloud::CHECKPOINT_GRACE_SECS,
                    "nodeSelector": {
                        cloud::BURST_LABEL: "true",
                        cloud::POOL_LABEL: cloud::POOL_VALUE,
                    },
                    // One dev job per worker whatever the requests: no two cloud Jobs' pods
                    // ever share a node, so a smaller configured request cannot pack them.
                    "affinity": {
                        "podAntiAffinity": {
                            "requiredDuringSchedulingIgnoredDuringExecution": [{
                                "labelSelector": {
                                    "matchExpressions": [{
                                        "key": cloud::LABEL_RUN_ID,
                                        "operator": "Exists",
                                    }],
                                },
                                "topologyKey": "kubernetes.io/hostname",
                            }],
                        },
                    },
                    "tolerations": [{
                        "key": cloud::BURST_LABEL,
                        "operator": "Equal",
                        "value": "true",
                        "effect": "NoSchedule",
                    }],
                    "securityContext": {
                        "runAsUser": 1000,
                        "runAsGroup": 1000,
                        "runAsNonRoot": true,
                        "fsGroup": 1000,
                    },
                    "containers": [{
                        "name": "agent",
                        "image": image,
                        "imagePullPolicy": "IfNotPresent",
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
                            { "name": "JOB_DEADLINE_SECONDS", "value": deadline.to_string() },
                            { "name": "RUN_ID", "value": run_id_env },
                            { "name": "CONTINUATION", "value": continuation_env.to_string() },
                            { "name": "JESSE_RUN_REPORT", "value": cloud::RUN_REPORT_FILE },
                            { "name": "PR_TITLE_FILE", "value": cloud::PR_TITLE_FILE },
                            { "name": "AGENT_GITHUB", "value": "none" },
                            { "name": "REBASE_ON_MAIN", "value": "true" },
                            { "name": "AUTO_MERGE", "value": auto_merge },
                            { "name": "MERGE_DENY", "value": extras.merge_deny },
                            { "name": "MERGE_WAIT_SECONDS", "value": cloud::MERGE_WAIT_SECONDS.to_string() },
                            { "name": "CARGO_PROFILE_DEV_DEBUG", "value": "0" },
                            { "name": "CARGO_PROFILE_TEST_DEBUG", "value": "0" },
                            { "name": "CARGO_INCREMENTAL", "value": "0" },
                            { "name": "CARGO_BUILD_JOBS", "value": agent.build_jobs().to_string() },
                        ],
                        "envFrom": [{ "secretRef": { "name": "jesse-dev-agent-keys" } }],
                        "resources": {
                            "requests": {
                                "cpu": cloud::cpu_quantity(agent.cpu_request_milli),
                                "memory": cloud::memory_quantity(agent.memory_request_mi),
                                "ephemeral-storage": "20Gi",
                            },
                            "limits": {
                                "cpu": cloud::cpu_quantity(agent.cpu_limit_milli),
                                "memory": cloud::memory_quantity(agent.memory_limit_mi),
                            },
                        },
                        "volumeMounts": [
                            { "name": "prompt", "mountPath": "/prompt", "readOnly": true },
                            { "name": "work", "mountPath": "/home/agent/work" },
                            { "name": "work", "mountPath": "/home/agent/.cargo/registry", "subPath": "cargo-registry" },
                            { "name": "work", "mountPath": "/home/agent/.npm", "subPath": "npm" },
                        ],
                    }],
                    "volumes": [
                        { "name": "prompt", "configMap": { "name": config_map } },
                        { "name": "work", "emptyDir": { "sizeLimit": "60Gi" } },
                    ],
                },
            },
        },
    });
    if extras.services {
        let pod = &mut job["spec"]["template"]["spec"];
        pod["initContainers"] = service_sidecars();
        let env = pod["containers"][0]["env"]
            .as_array_mut()
            .expect("the agent has an env");
        env.push(json!({ "name": "DATABASE_URL", "value": cloud::DATABASE_URL }));
        env.push(json!({ "name": "REDIS_URL", "value": cloud::REDIS_URL }));
        pod["volumes"]
            .as_array_mut()
            .expect("the pod has volumes")
            .push(json!({ "name": "pgdata", "emptyDir": { "sizeLimit": "4Gi" } }));
    }
    if let Some(r) = extras.review {
        job["spec"]["template"]["spec"]["containers"][0]["env"]
            .as_array_mut()
            .expect("the agent has an env")
            .push(json!({ "name": "REVIEW", "value": r.label() }));
        job["metadata"]["annotations"][cloud::ANN_REVIEW] = json!(r.label());
    }
    if let Some(c) = &extras.chain {
        let ann = job["metadata"]["annotations"]
            .as_object_mut()
            .expect("the Job has annotations");
        ann.insert(cloud::ANN_CHAIN_ROOT.into(), json!(c.root));
        ann.insert(cloud::ANN_CONTINUATION.into(), json!(c.link.to_string()));
        ann.insert(cloud::ANN_CONTINUATION_OF.into(), json!(c.continuation_of));
        ann.insert(cloud::ANN_CHAIN.into(), json!(c.chain_json));
    }
    serde_json::from_value(job).expect("a Job literal deserialises")
}

/// Postgres 16 and Redis 7 as native sidecars (`initContainers` with `restartPolicy: Always`,
/// Kubernetes 1.29 and later): each starts, and passes its startup probe, before the agent
/// starts, and each stops when the agent has exited. Trovato CI's services, field for field:
/// user, password and database `trovato` / `trovato` / `trovato_test`. Both run as the image's
/// own non-root user (uid and gid [`cloud::SERVICE_UID`]) without privilege escalation, and both
/// listen on the pod's loopback only: no Service, no port. The Postgres probe asks over TCP, not
/// the socket, because the image's first-boot server answers on the socket alone and then
/// restarts.
pub fn service_sidecars() -> serde_json::Value {
    let security = json!({
        "runAsUser": cloud::SERVICE_UID,
        "runAsGroup": cloud::SERVICE_UID,
        "runAsNonRoot": true,
        "allowPrivilegeEscalation": false,
        "capabilities": { "drop": ["ALL"] },
        "seccompProfile": { "type": "RuntimeDefault" },
    });
    let resources = |s: cloud::SidecarResources| {
        json!({
            "requests": {
                "cpu": cloud::cpu_quantity(s.cpu_milli),
                "memory": cloud::memory_quantity(s.memory_request_mi),
            },
            "limits": {
                "cpu": cloud::cpu_quantity(s.cpu_milli),
                "memory": cloud::memory_quantity(s.memory_limit_mi),
            },
        })
    };
    let probe = |command: &[&str]| {
        json!({
            "exec": { "command": command },
            "periodSeconds": 2,
            "timeoutSeconds": 5,
            "failureThreshold": 150,
        })
    };
    json!([
        {
            "name": "postgres",
            "image": cloud::POSTGRES_IMAGE,
            "imagePullPolicy": "IfNotPresent",
            "restartPolicy": "Always",
            "args": ["postgres", "-c", "listen_addresses=localhost"],
            "env": [
                { "name": "POSTGRES_USER", "value": "trovato" },
                { "name": "POSTGRES_PASSWORD", "value": "trovato" },
                { "name": "POSTGRES_DB", "value": "trovato_test" },
                // A directory under the mount, so the entrypoint can create it as its own.
                { "name": "PGDATA", "value": "/var/lib/postgresql/data/pgdata" },
            ],
            "securityContext": security,
            "resources": resources(cloud::POSTGRES_SIDECAR),
            "startupProbe": probe(&[
                "pg_isready", "-h", "127.0.0.1", "-U", "trovato", "-d", "trovato_test",
            ]),
            "volumeMounts": [{ "name": "pgdata", "mountPath": "/var/lib/postgresql/data" }],
        },
        {
            "name": "redis",
            "image": cloud::REDIS_IMAGE,
            "imagePullPolicy": "IfNotPresent",
            "restartPolicy": "Always",
            "args": ["--save", "", "--appendonly", "no", "--bind", "127.0.0.1", "-::1"],
            "securityContext": security,
            "resources": resources(cloud::REDIS_SIDECAR),
            "startupProbe": probe(&["redis-cli", "-h", "127.0.0.1", "ping"]),
        },
    ])
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
    /// A cloud run's id ([`cloud::LABEL_RUN_ID`]); `None` on the home cluster.
    pub run_id: Option<String>,
    /// A cloud run a person cancelled ([`cloud::ANN_CANCELLED`]).
    pub cancelled: bool,
    /// The strand note and line a strand entry started it from ([`LABEL_STRAND`],
    /// [`LABEL_STRAND_ID`]); `None` for a draft or a free prompt.
    pub strand: Option<String>,
    pub strand_id: Option<String>,
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
            run_id: labels.get(cloud::LABEL_RUN_ID).cloned(),
            cancelled: ann.get(cloud::ANN_CANCELLED).map(String::as_str) == Some("true"),
            strand: labels.get(LABEL_STRAND).cloned(),
            strand_id: labels.get(LABEL_STRAND_ID).cloned(),
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

/// What `devjob_list` lists: the running and queued runs, oldest first, and the last
/// [`LIST_FINISHED`] finished ones, newest first.
pub struct ListedViews<'a> {
    pub running: Vec<&'a JobView>,
    pub queued: Vec<&'a JobView>,
    pub finished: Vec<&'a JobView>,
}

impl<'a> ListedViews<'a> {
    pub fn of(views: &'a [JobView]) -> ListedViews<'a> {
        let mut running = Vec::new();
        let mut queued = Vec::new();
        let mut finished: Vec<&JobView> = Vec::new();
        for v in views {
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
        ListedViews {
            running,
            queued,
            finished,
        }
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

/// When a pod's container was killed for memory (`OOMKilled`, in its terminated state or its
/// last one), the failure line that says so: `killed at the 14 Gi memory limit (OOMKilled)`,
/// the limit read from that container's spec. `None` when nothing was killed for memory.
pub fn oom_killed_line(pod: &Pod) -> Option<String> {
    let statuses = pod.status.as_ref()?.container_statuses.as_ref()?;
    let killed = statuses.iter().find(|c| {
        [c.state.as_ref(), c.last_state.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|s| s.terminated.as_ref())
            .any(|t| t.reason.as_deref() == Some("OOMKilled"))
    })?;
    let limit = pod
        .spec
        .as_ref()
        .and_then(|s| s.containers.iter().find(|c| c.name == killed.name))
        .and_then(|c| c.resources.as_ref())
        .and_then(|r| r.limits.as_ref())
        .and_then(|l| l.get("memory"))
        .map(|q| q.0.clone());
    Some(match limit {
        Some(q) => {
            let shown = match (q.strip_suffix("Gi"), q.strip_suffix("Mi")) {
                (Some(n), _) => format!("{n} Gi"),
                // The agent's share beside the sidecars: 27392Mi is 26.75 Gi.
                (_, Some(mi)) => match mi.parse::<f64>() {
                    Ok(mi) => {
                        let gi = format!("{:.2}", mi / 1024.0);
                        format!("{} Gi", gi.trim_end_matches('0').trim_end_matches('.'))
                    }
                    Err(_) => q,
                },
                _ => q,
            };
            format!("killed at the {shown} memory limit (OOMKilled)")
        }
        None => "killed at its memory limit (OOMKilled)".to_string(),
    })
}

/// Put what Kubernetes knows into a finished run's failure line: the memory kill first, when
/// there was one ([`oom_killed_line`]), then the Job's `Failed` reason when the agent never
/// reported an exit. A run killed for memory is failed whatever its log said.
pub fn explain_failure(report: &mut DevJobReport, failure_reason: Option<&str>, oom: Option<&str>) {
    if report.failure.is_some() && report.agent_exit.is_none() {
        if let Some(why) = failure_reason {
            let f = report.failure.take().unwrap_or_default();
            report.failure = Some(format!("the Job failed ({why}); {f}"));
        }
    }
    if let Some(oom) = oom {
        report.status = "failed".into();
        report.failure = Some(match report.failure.take() {
            Some(f) => format!("{oom}; {f}"),
            None => oom.to_string(),
        });
    }
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
    // A sidecar still pulling holds the agent at `PodInitializing`: its own reason comes first.
    let waiting = [&status.init_container_statuses, &status.container_statuses]
        .into_iter()
        .flatten()
        .flatten()
        .find_map(|c| c.state.as_ref()?.waiting.clone());
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
    render_report_for(r, None)
}

/// [`render_report`], for a run a strand entry started: the front matter adds `strand`,
/// `strand-id` and `prompt` after the existing keys, and the Spec table adds a Strand row.
/// With `None` it is byte for byte the report of a run the tool or a fixed entry started.
/// Set to `"found"` on a Job whose pull request or commits carried AI attribution.
pub const ANN_ATTRIBUTION: &str = "jesse.dev/attribution";

/// The post-run attribution check of a cluster run that pushed: the pull request the report
/// names and every pull request on `jesse/<slug>` (title and body), and the commits of each and
/// of the branch. The `ATTRIBUTION:` line, or `None` when nothing was pushed or nothing was
/// found.
pub fn cluster_attribution(
    gh: &dyn crate::attribution::GitHubView,
    spec: &SpecAnnotation,
    report: &DevJobReport,
) -> Option<String> {
    let branch = report
        .branch
        .clone()
        .unwrap_or_else(|| format!("jesse/{}", spec.branch_slug));
    let repo = report.repo.as_deref().unwrap_or(&spec.repo);
    let hits =
        crate::attribution::scan_github(gh, repo, &branch, "main", report.pull_request_url());
    crate::attribution::report_line(&hits)
}

/// `text` (a report with front matter) with `line` as the first line of its body.
pub fn lead_with(text: &str, line: &str) -> String {
    if let Some(rest) = text.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            let cut = 4 + end + 5;
            return format!("{}\n{line}\n{}", &text[..cut], &text[cut..]);
        }
    }
    format!("{line}\n\n{text}")
}

pub fn render_report_for(r: &ReportInput, strand: Option<&StrandRef>) -> String {
    render_report_with(r, strand, None)
}

/// [`render_report_for`], and for a cloud run (`cloud_run`, its run id) `host: cloud` and
/// `run: <run_id>` in the front matter and the run id in the title. With `None` it is byte for
/// byte the home cluster's report.
pub fn render_report_with(
    r: &ReportInput,
    strand: Option<&StrandRef>,
    cloud_run: Option<&str>,
) -> String {
    let outcome = RunOutcome::of(r.report);
    let mut s = String::new();
    s.push_str(&format!(
        "---\ndevjob: {}\nrepo: {}\noutcome: {}\n",
        r.job_name,
        r.spec.repo,
        outcome.label()
    ));
    if let Some(id) = cloud_run {
        s.push_str(&format!("host: cloud\nrun: {id}\n"));
    }
    if let Some(step) = strand {
        s.push_str(&format!(
            "strand: {}\nstrand-id: {}\nprompt: {}\n",
            step.note, step.id, step.prompt
        ));
    }
    s.push_str("---\n\n");
    match cloud_run {
        Some(id) => s.push_str(&format!("# Cloud run {id}: {}\n\n", outcome.label())),
        None => s.push_str(&format!("# Dev job {}: {}\n\n", r.slug, outcome.label())),
    }
    let prs = r.report.pull_requests();
    if prs.len() > 1 {
        s.push_str("Pull requests, in order, the run's own last:\n\n");
        for (i, p) in prs.iter().enumerate() {
            let url = &p.url;
            match &p.squash {
                Some(sha) => s.push_str(&format!(
                    "{}. [{url}]({url}), squash commit `{sha}`\n",
                    i + 1
                )),
                None => s.push_str(&format!("{}. [{url}]({url})\n", i + 1)),
            }
        }
        s.push('\n');
    } else if let Some(url) = r.report.pull_request_url() {
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
    if let Some(step) = strand {
        s.push_str(&format!(
            "| Strand | {} |\n",
            cell(&format!("{} {}", step.note, step.id))
        ));
    }
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

/// A capped link's report, when it continues: the line it starts with ([`lead_with`]).
pub fn capped_continued_line(
    minutes: u64,
    slug: &str,
    sha: &str,
    continued_as: &str,
    link: u32,
    max: u32,
) -> String {
    format!(
        "Hit its {minutes} minute time limit; its work is saved on jesse/{slug}-wip at {sha} \
         and continues as {continued_as} (continuation {link} of {max})."
    )
}

/// A capped link's report, when it does not continue: the line it starts with ([`lead_with`]).
pub fn capped_not_continued_line(minutes: u64, reason: &str) -> String {
    format!("Hit its {minutes} minute time limit and was not continued: {reason}.")
}

/// Any other failed cloud run whose report does not already name a checkpoint, when one was
/// found anyway: the line it starts with, so the work a run left is never invisible.
pub fn checkpoint_visibility_line(slug: &str, sha: &str) -> String {
    format!("Its work is saved on jesse/{slug}-wip at {sha}.")
}

/// A continuation's `## Chain` section: every link in order, run id, report file name, how it
/// ended, wall time and checkpoint sha.
pub fn render_chain_section(chain: &[ChainLink]) -> String {
    let mut s = String::from(
        "## Chain\n\n| Run | Report | Outcome | Wall | Checkpoint |\n\
         | --- | --- | --- | --- | --- |\n",
    );
    for l in chain {
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            cell(&l.run_id),
            cell(&l.report_path),
            cell(&l.outcome),
            l.wall_seconds.map(human_secs).unwrap_or_else(|| "—".into()),
            cell(l.checkpoint_sha.as_deref().unwrap_or("—")),
        ));
    }
    s.push('\n');
    s
}

/// `text` (a rendered report) with `section` inserted right before its `## Log` section.
pub fn insert_chain_section(text: &str, section: &str) -> String {
    match text.find("\n## Log (") {
        Some(i) => format!("{}{section}{}", &text[..i + 1], &text[i + 1..]),
        None => format!("{text}\n{section}"),
    }
}

/// `Inbox/<date>-dev-job-<slug>.md`, or `-2`, `-3`, … when a run with the same slug was reported
/// the same day. `exists` answers for a path under the notes directory.
pub fn report_rel_path(date: &str, slug: &str, exists: impl Fn(&str) -> bool) -> String {
    report_rel_path_stem(date, &format!("dev-job-{slug}"), exists)
}

/// `Inbox/<date>-<stem>.md`, or `-2`, `-3`, … : [`report_rel_path`] with the stem named. A cloud
/// run's stem is `run-<run_id>`, the Studio's name for a run's report.
pub fn report_rel_path_stem(date: &str, stem: &str, exists: impl Fn(&str) -> bool) -> String {
    let base = format!("{}/{date}-{stem}", crate::vaultwrites::INBOX_DIR);
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
    find_existing_report_stem(notes_root, &format!("dev-job-{slug}"), job_name)
}

/// [`find_existing_report`] with the stem named.
pub fn find_existing_report_stem(notes_root: &Path, stem: &str, job_name: &str) -> Option<String> {
    let inbox = notes_root.join(crate::vaultwrites::INBOX_DIR);
    let marker = format!("-{stem}");
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
            // Every paired device, each as its own topic: one line per device, then the sum.
            let tally = push_to_devices(
                &apns,
                &devices,
                "devjob",
                &job,
                build_devjob_payload(&title, &body, &job),
                Some(badge),
            )
            .await;
            eprintln!(
                "jesse-bridge: devjob PUSH {job} sent to {} of {} device(s)",
                tally.sent,
                tally.devices()
            );
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
    wake: Arc<tokio::sync::Notify>,
    /// Serialises every queue decision (a start, a resume) against every other.
    gate: tokio::sync::Mutex<()>,
    /// Studio runs (`[hostrun]`), when enabled: `host: "studio"` and the two Studio tools.
    studio: OnceLock<Arc<crate::hostrun::HostRuns>>,
    /// The cloud host's own server, when `[devjob.cloud]` is on ([`DevJobs::attach_cloud`]).
    cloud: OnceLock<Arc<DevJobs>>,
    /// On the cloud host's own server only: its workers, its ledger and its settings. `None` is
    /// the home cluster, whose every path is exactly what it was before the cloud existed.
    rt: Option<cloud::CloudRuntime>,
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
        DevJobs::connect_with(settings, None).await
    }

    /// The cloud host's server, from `[devjob.cloud]`: its own client, namespace, image and
    /// cap, and kc-burst for its workers. Connects to nothing. Reads kc-burst's own config once
    /// for its `max_ttl` ([`cloud::read_kc_burst_max_ttl`]), so [`cloud::CloudSettings`]'s
    /// effective caps follow whatever the burst system actually allows.
    pub async fn connect_cloud(home: &DevJobSettings) -> Result<DevJobs, String> {
        let mut home = home.clone();
        home.cloud.kc_burst_max_ttl_minutes =
            cloud::read_kc_burst_max_ttl(&home.cloud.kc_burst_config);
        home.cloud.log_effective_caps();
        let burst: Arc<dyn cloud::Burst> = Arc::new(cloud::KcBurstCli {
            path: home.cloud.kc_burst.clone(),
        });
        DevJobs::connect_cloud_with(&home, burst).await
    }

    /// [`DevJobs::connect_cloud`] with the workers' driver named (a recorder in tests).
    pub async fn connect_cloud_with(
        home: &DevJobSettings,
        burst: Arc<dyn cloud::Burst>,
    ) -> Result<DevJobs, String> {
        let rt = cloud::CloudRuntime::new(home.cloud.clone(), burst);
        DevJobs::connect_with(home.cloud.cluster_settings(home), Some(rt)).await
    }

    async fn connect_with(
        settings: DevJobSettings,
        rt: Option<cloud::CloudRuntime>,
    ) -> Result<DevJobs, String> {
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
            wake: Arc::new(tokio::sync::Notify::new()),
            gate: tokio::sync::Mutex::new(()),
            studio: OnceLock::new(),
            cloud: OnceLock::new(),
            rt,
        })
    }

    /// Serve `host: "cloud"` through `cloud`, which shares this server's report sink and
    /// Studio. Its watcher starts with this one's ([`DevJobs::spawn_watcher`]).
    pub fn attach_cloud(&self, cloud: Arc<DevJobs>) {
        if let Some(s) = self.studio.get() {
            cloud.attach_studio(s.clone());
            s.attach_cloud(cloud.clone());
        }
        if let Some(sink) = self.sink.get() {
            cloud.attach(sink.clone());
        }
        let _ = self.cloud.set(cloud);
    }

    /// The cloud host's server, when attached.
    pub fn cloud(&self) -> Option<&Arc<DevJobs>> {
        self.cloud.get()
    }

    /// The cloud runtime, on the cloud host's own server.
    pub fn cloud_runtime(&self) -> Option<&cloud::CloudRuntime> {
        self.rt.as_ref()
    }

    /// Serve Studio runs too: `host: "studio"` on `devjob_start`, Studio runs in
    /// `devjob_status` and `devjob_list`, and `devjob_reply`, `devjob_cancel` and
    /// `devjob_close`.
    pub fn attach_studio(&self, studio: Arc<crate::hostrun::HostRuns>) {
        if let Some(c) = self.cloud.get() {
            c.attach_studio(studio.clone());
            studio.attach_cloud(c.clone());
        }
        let _ = self.studio.set(studio);
    }

    /// Where reports go. Until this is called the watcher keeps the queue moving but reports
    /// nothing, and a finished run waits, unreported, for the next pass after it.
    pub fn attach(&self, sink: ReportSink) {
        if let Some(c) = self.cloud.get() {
            c.attach(sink.clone());
        }
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
        self.start_run(spec, origin, None).await
    }

    /// [`DevJobs::start`], for every caller: the tool and a fixed schedule entry pass no strand;
    /// a strand entry passes the step, which the Job then carries as two labels and an
    /// annotation ([`strand_of`]).
    pub async fn start_run(
        &self,
        spec: &DevJobSpec,
        origin: &str,
        strand: Option<&StrandRef>,
    ) -> Result<Started, String> {
        self.create_run(spec, origin, strand, None).await
    }

    /// The prompt ConfigMap and the Job, for the home cluster (`cloud` `None`: [`build_job`],
    /// suspended when the queue says so) or the cloud ([`build_cloud_job`], always suspended).
    async fn create_run(
        &self,
        spec: &DevJobSpec,
        origin: &str,
        strand: Option<&StrandRef>,
        cloud: Option<&CloudJobExtras>,
    ) -> Result<Started, String> {
        let _gate = self.gate.lock().await;
        let views: Vec<JobView> = self.views().await?.into_iter().map(|(_, v)| v).collect();
        let queued = cloud.is_some() || must_queue(&views, self.settings.max_concurrent);
        let (job_name, cm_name) = object_names(&spec.branch_slug, &name_suffix());
        let ns = &self.settings.namespace;
        let cm = build_config_map(ns, &cm_name, spec);
        self.config_maps()
            .create(&PostParams::default(), &cm)
            .await
            .map_err(|e| format!("could not create the prompt ConfigMap: {e}"))?;
        let mut job = match cloud {
            None => build_job(
                ns,
                &job_name,
                &cm_name,
                &self.settings.image,
                spec,
                queued,
                origin,
            ),
            Some(x) => build_cloud_job(
                ns,
                &job_name,
                &cm_name,
                &self.settings.image,
                spec,
                x,
                origin,
            ),
        };
        if let Some(step) = strand {
            let meta = &mut job.metadata;
            let labels = meta.labels.get_or_insert_with(Default::default);
            labels.insert(LABEL_STRAND.into(), step.note.clone());
            labels.insert(LABEL_STRAND_ID.into(), step.id.clone());
            meta.annotations
                .get_or_insert_with(Default::default)
                .insert(ANN_PROMPT.into(), step.prompt.clone());
        }
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
            "jesse-bridge: devjob START {job_name} repo={} agent={} slug={} queued={queued} origin={origin}{}{}",
            spec.repo,
            spec.agent.label(),
            spec.branch_slug,
            strand
                .map(|s| format!(" strand={} step={}", s.note, s.id))
                .unwrap_or_default(),
            cloud
                .map(|x| format!(" host=cloud run={} merge={}", x.run_id, x.merge.annotation()))
                .unwrap_or_default()
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

    /// Every Job in the namespace labelled with strand `note`, in any phase, suspended included.
    /// Listed by the strand label alone, not the dev job label, so the in-flight guard sees any
    /// Job that claims the step.
    pub async fn strand_jobs(&self, note: &str) -> Result<Vec<Job>, String> {
        let list = self
            .jobs()
            .list(&ListParams::default().labels(&format!("{LABEL_STRAND}={note}")))
            .await
            .map_err(|e| format!("could not list the dev jobs of strand {note}: {e}"))?;
        Ok(list.items)
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
                Phase::Queued => Some(match &self.rt {
                    None => format!(
                        "queued: {} runs are already active ([devjob] max_concurrent)",
                        self.settings.max_concurrent
                    ),
                    Some(rt) => rt.queued_reason(),
                }),
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

    /// Every run `devjob_list` lists ([`ListedViews::of`]), as views. What the app's Runs list
    /// shows of the cloud (`GET /jesse/runs`), so the two keep the same window.
    pub async fn listed(&self) -> Result<Vec<JobView>, String> {
        let views: Vec<JobView> = self.views().await?.into_iter().map(|(_, v)| v).collect();
        let l = ListedViews::of(&views);
        Ok(l.running
            .into_iter()
            .chain(l.queued)
            .chain(l.finished)
            .cloned()
            .collect())
    }

    /// The last [`REPORT_LOG_LINES`] of a running Job's log, as the watcher last kept them.
    pub fn kept_tail(&self, job: &str) -> Option<Vec<String>> {
        self.tails
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(job)
            .cloned()
    }

    /// Running, queued and the last [`LIST_FINISHED`] finished runs.
    pub async fn list(&self) -> Result<Value, String> {
        let views: Vec<JobView> = self.views().await?.into_iter().map(|(_, v)| v).collect();
        let ListedViews {
            running,
            queued,
            finished,
        } = ListedViews::of(&views);
        let cloud_host = self.rt.is_some();
        let brief = |v: &JobView| {
            let mut b = json!({
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
            });
            if cloud_host {
                b["host"] = json!("cloud");
                b["run_id"] = json!(v.run_id);
            }
            b
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
            for (job, v) in views
                .iter()
                .filter(|(_, v)| v.finished.is_some() && !v.reported)
            {
                if let Err(e) = self.finalize(job, v, strand_of(job).as_ref(), sink).await {
                    eprintln!(
                        "jesse-bridge: devjob {}: report not written yet: {e}",
                        v.name
                    );
                }
            }
        }
        if let Some(rt) = &self.rt {
            self.cloud_pass(rt).await;
            return;
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
    /// (or pushed) again, only the annotation and the delete are redone. `strand` is read from
    /// the Job ([`strand_of`]), never from memory, so a restart mid-run reports the same keys.
    /// For a cloud run only, before the report is written: reads a capped run's checkpoint and
    /// decides whether to continue it ([`DevJobs::plan_continuation`]), so the written report
    /// and the push can both say what happened.
    async fn finalize(
        &self,
        job: &Job,
        v: &JobView,
        strand: Option<&StrandRef>,
        sink: &ReportSink,
    ) -> Result<(), String> {
        let cloud_run = self.rt.as_ref().and(v.run_id.as_deref());
        let existing = match cloud_run {
            None => find_existing_report(&sink.notes_root, &v.slug, &v.name),
            Some(id) => find_existing_report_stem(&sink.notes_root, &format!("run-{id}"), &v.name),
        };
        let rel = match existing {
            Some(rel) => {
                eprintln!("jesse-bridge: devjob {}: already reported at {rel}", v.name);
                (rel, None, None, None, Vec::new())
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
                // A cloud run's pod is sized from config: say when it ran out of memory.
                let oom = match cloud_run {
                    Some(_) => pod.as_ref().and_then(oom_killed_line),
                    None => None,
                };
                explain_failure(&mut report, v.failure_reason.as_deref(), oom.as_deref());
                if v.cancelled {
                    let f = report.failure.take().unwrap_or_default();
                    report.status = "failed".into();
                    report.failure = Some(format!("cancelled by Jeremy; {f}"));
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
                let date = chrono::Local::now().format("%Y-%m-%d").to_string();
                let stem = match cloud_run {
                    None => format!("dev-job-{}", v.slug),
                    Some(id) => format!("run-{id}"),
                };
                // Where this run's own report will land, predicted (not locked): good enough to
                // name in a continuation's chain, and to read back below for this run's own.
                let predicted_rel =
                    report_rel_path_stem(&date, &stem, |p| sink.notes_root.join(p).exists());
                // A capped cloud run only: reads the checkpoint and decides whether to continue,
                // before the report below is rendered, so its lead line can say what happened.
                let (lead_line, continued_as) = match cloud_run {
                    Some(id) => {
                        self.plan_continuation(
                            job,
                            v,
                            &report,
                            oom.is_some(),
                            prompt.as_deref(),
                            id,
                            &predicted_rel,
                        )
                        .await
                    }
                    None => (None, None),
                };
                // NO AI ATTRIBUTION, the backstop: the pull request and commits the run pushed,
                // read back from GitHub before the run is reported.
                let attribution = {
                    let (spec, rep) = (v.spec.clone(), report.clone());
                    tokio::task::spawn_blocking(move || {
                        cluster_attribution(&crate::hostrun::sys::GhForge, &spec, &rep)
                    })
                    .await
                    .ok()
                    .flatten()
                };
                let text = render_report_with(
                    &ReportInput {
                        job_name: &v.name,
                        slug: &v.slug,
                        spec: &v.spec,
                        prompt: prompt.as_deref(),
                        report: &report,
                        log_tail: &tail,
                        queued_ms: v.created_ms,
                        started_ms: v.started_ms,
                        finished_ms: v.finished_ms,
                    },
                    strand,
                    cloud_run,
                );
                let text = match &attribution {
                    Some(line) => {
                        eprintln!("jesse-bridge: devjob {}: {line}", v.name);
                        lead_with(&text, line)
                    }
                    None => text,
                };
                // The lead line (capped, continuing or not, or a generic checkpoint pointer)
                // comes first: applied last, since `lead_with` always inserts at the very top.
                let text = match &lead_line {
                    Some(line) => lead_with(&text, line),
                    None => text,
                };
                // This run's own place in a chain (link 1 or later): its ancestors, plus itself.
                let text = match cloud_run.map(|id| (id, ChainInfo::of(job, id))) {
                    Some((id, ci)) if ci.link > 0 => {
                        let mut chain = ci.chain;
                        chain.push(ChainLink {
                            run_id: id.to_string(),
                            report_path: predicted_rel.clone(),
                            outcome: report
                                .failure
                                .clone()
                                .unwrap_or_else(|| report.status.clone()),
                            wall_seconds: wall_secs(&report, v.started_ms, v.finished_ms),
                            checkpoint_sha: None,
                            checkpoint_tree: None,
                        });
                        insert_chain_section(&text, &render_chain_section(&chain))
                    }
                    _ => text,
                };
                // A cloud run's Job that ended its round: its report says what the Studio's would
                // of the run's review (the answer's leak check, a cycle that could not run).
                let (text, leaks) = match (cloud_run, self.studio.get(), &continued_as) {
                    (Some(_), Some(studio), None) => {
                        studio
                            .cloud_report_marks(
                                &v.name,
                                text,
                                cloud_end_status(v, Some(RunOutcome::of(&report))),
                                report.pull_request_url(),
                            )
                            .await
                    }
                    _ => (text, Vec::new()),
                };
                let rel = write_report(sink, &v.name, &date, &stem, &text).await?;
                eprintln!(
                    "jesse-bridge: devjob REPORT {} outcome={} at {rel}",
                    v.name,
                    RunOutcome::of(&report).label()
                );
                (rel, Some(report), attribution, continued_as, leaks)
            }
        };
        let (rel, report, attribution, continued_as, leaks) = rel;
        // A reviewed cloud run's implementing Job that opened its pull request: nothing is sent,
        // as for a Studio run's implementing round; the push is the answer's.
        let held = cloud::review_mode(job) == Some(cloud::ReviewMode::Hold)
            && continued_as.is_none()
            && report
                .as_ref()
                .is_some_and(|r| RunOutcome::of(r) == RunOutcome::PullRequestOpened);
        let outcome = report.as_ref().map(RunOutcome::of);
        // A scheduled run is pushed by the scheduler, under its `notify`; only a turn's run pushes
        // here, so a scheduled run never pushes twice.
        if let (Some(report), Some(outcome), Some(n)) = (&report, outcome, &sink.notifier) {
            if !v.origin.starts_with("schedule:") && attribution.is_none() && !held {
                if let Some(continued_as) = &continued_as {
                    let run_id = v.run_id.as_deref().unwrap_or(&v.name);
                    n.notify(
                        "Cloud run continuing".to_string(),
                        format!(
                            "{run_id} hit its {} min limit · continuing as {continued_as}",
                            v.spec.timeout_minutes
                        ),
                        v.name.clone(),
                    )
                    .await;
                } else {
                    let wall = wall_secs(report, v.started_ms, v.finished_ms)
                        .map(human_secs)
                        .unwrap_or_else(|| "unknown time".into());
                    let prs = pull_requests_push_suffix(report);
                    let body = match cloud_run {
                        None => format!("{} · {wall}{prs}", v.slug),
                        Some(id) => format!("{id} in the cloud · {wall}{prs}"),
                    };
                    n.notify(outcome.title().to_string(), body, v.name.clone())
                        .await;
                }
            }
        }
        // A hit is pushed whoever started the run, and says what it found.
        if let (Some(line), Some(n)) = (&attribution, &sink.notifier) {
            n.notify(
                "Dev job needs you".to_string(),
                format!(
                    "{} · {}",
                    v.slug,
                    line.chars().take(160).collect::<String>()
                ),
                v.name.clone(),
            )
            .await;
        }
        // A cloud run's record ends its round with this report, or moves to its continuation.
        if let (Some(_), Some(studio)) = (cloud_run, self.studio.get()) {
            // The Job this link continues as, read from kc1 every time (also on a retried pass,
            // which does not know `continued_as`). A lookup that fails, or misses the Job just
            // created, leaves the Job unannotated so the next pass ends the record rightly.
            let link = v.run_id.as_deref().unwrap_or(&v.name);
            let continued_job = match self.views().await {
                Ok(jobs) => cloud::continuation_job(&jobs, link),
                Err(e) => return Err(format!("its continuation could not be looked up: {e}")),
            };
            if let (Some(id), None) = (&continued_as, &continued_job) {
                return Err(format!("its continuation {id} is not listed yet"));
            }
            let end = crate::hostrun::CloudEnd {
                job: v.name.clone(),
                status: cloud_end_status(v, outcome),
                report_path: Some(rel.clone()),
                continued_as: continued_job,
                error: report.as_ref().and_then(|r| r.failure.clone()),
                leaks,
            };
            let s = studio.clone();
            let _ = tokio::task::spawn_blocking(move || s.cloud_ended(&end)).await;
        }
        let mut ann = json!({ ANN_REPORTED: "true", ANN_REPORT_PATH: rel });
        if attribution.is_some() {
            ann[ANN_ATTRIBUTION] = json!("found");
        }
        if let Some(o) = outcome {
            ann[ANN_OUTCOME] = json!(o.label());
        }
        if let Some(url) = report.as_ref().and_then(|r| r.pull_request_url()) {
            ann[ANN_PULL_REQUEST] = json!(url);
        }
        if let Some(id) = &continued_as {
            ann[cloud::ANN_CONTINUED_AS] = json!(id);
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

    /// For a cloud run only, in [`DevJobs::finalize`]'s `None` branch: whether a capped run
    /// continues, and the line to lead its own report with. `(None, None)` for a run that is
    /// not capped and has no checkpoint worth pointing at. Reads `jesse/<slug>-wip` fresh from
    /// GitHub every time; there is no cache, since GitHub is the only truth here.
    #[allow(clippy::too_many_arguments)]
    async fn plan_continuation(
        &self,
        job: &Job,
        v: &JobView,
        report: &DevJobReport,
        oom_killed: bool,
        prompt_md: Option<&str>,
        run_id: &str,
        own_report_rel: &str,
    ) -> (Option<String>, Option<String>) {
        let Some(rt) = self.cloud_runtime() else {
            return (None, None);
        };
        if report.ok() {
            return (None, None);
        }
        let slug = v.spec.branch_slug.clone();
        let repo = v.spec.repo.clone();
        let stopped = cloud::stopped_at_time_limit(report, v.failure_reason.as_deref());
        let read = {
            let (repo, slug) = (repo.clone(), slug.clone());
            tokio::task::spawn_blocking(move || {
                crate::hostrun::sys::GhForge.checkpoint(&repo, &slug)
            })
            .await
            .unwrap_or_else(|e| Err(format!("the checkpoint read task failed: {e}")))
        };
        let checkpoint: Result<crate::hostrun::sys::Checkpoint, String> = match read {
            Ok(None) => Err(format!("jesse/{slug}-wip does not exist on origin")),
            Ok(Some(cp)) => match cp.invalid_reason(&slug) {
                Some(reason) => Err(reason),
                None => Ok(cp),
            },
            Err(e) => Err(format!("the checkpoint could not be read: {e}")),
        };
        if !stopped {
            // Not a capped run: a generic pointer at the work it saved, never a continuation.
            return match &checkpoint {
                Ok(cp) if !report.extra.contains_key("checkpoint") => {
                    (Some(checkpoint_visibility_line(&slug, &cp.sha)), None)
                }
                _ => (None, None),
            };
        }
        let minutes = v.spec.timeout_minutes;
        let chain = ChainInfo::of(job, run_id);
        let needs_you = report
            .agent_final_message
            .as_deref()
            .and_then(|m| m.lines().next())
            .is_some_and(|l| l.trim_start().starts_with("NEEDS YOU"));
        let previous = chain
            .chain
            .last()
            .and_then(|l| l.checkpoint_tree.as_deref().map(|t| (l.run_id.as_str(), t)));
        let decision = cloud::continuation_decision(
            v.cancelled,
            oom_killed,
            report.pull_request_url().is_some(),
            stopped,
            needs_you,
            checkpoint.as_ref().map_err(String::as_str),
            chain.link,
            rt.settings.max_continuations,
            previous,
        );
        match decision {
            cloud::ContinuationDecision::Stop(reason) => {
                (Some(capped_not_continued_line(minutes, &reason)), None)
            }
            cloud::ContinuationDecision::Continue { link } => {
                let cp = checkpoint.expect("Continue only follows a usable checkpoint");
                let new_run_id = format!("{}-c{}", chain.root, link + 1);
                let mut new_chain = chain.chain.clone();
                new_chain.push(ChainLink {
                    run_id: run_id.to_string(),
                    report_path: own_report_rel.to_string(),
                    outcome: format!("hit its {minutes} minute limit"),
                    wall_seconds: wall_secs(report, v.started_ms, v.finished_ms),
                    checkpoint_sha: Some(cp.sha.clone()),
                    checkpoint_tree: Some(cp.tree.clone()),
                });
                let max = rt.settings.max_continuations;
                match self
                    .create_continuation(
                        job,
                        v,
                        &chain,
                        link,
                        &new_run_id,
                        &cp,
                        prompt_md,
                        new_chain,
                    )
                    .await
                {
                    Ok(()) => (
                        Some(capped_continued_line(
                            minutes,
                            &slug,
                            &cp.sha,
                            &new_run_id,
                            link,
                            max,
                        )),
                        Some(new_run_id),
                    ),
                    Err(e) => (
                        Some(capped_not_continued_line(
                            minutes,
                            &format!("could not create the continuation: {e}"),
                        )),
                        None,
                    ),
                }
            }
        }
    }

    /// Create (or, on a retried pass, reuse — [`cloud::continuation_create_or_reuse`]) a capped
    /// run's continuation: the next link, suspended, through [`DevJobs::create_cloud_run`] (so
    /// the slots, the worker life check and the daily cost cap all apply unchanged), whose
    /// prompt resumes from `cp`'s checkpoint.
    #[allow(clippy::too_many_arguments)]
    async fn create_continuation(
        &self,
        job: &Job,
        v: &JobView,
        chain: &ChainInfo,
        link: u32,
        new_run_id: &str,
        cp: &crate::hostrun::sys::Checkpoint,
        prompt_md: Option<&str>,
        new_chain: Vec<ChainLink>,
    ) -> Result<(), String> {
        let rt = self
            .cloud_runtime()
            .ok_or_else(|| "this is not the cloud host's server".to_string())?;
        let existing_ids: Vec<String> = self
            .views()
            .await?
            .into_iter()
            .filter_map(|(_, vv)| vv.run_id)
            .collect();
        if let cloud::ContinuationCreate::Reuse =
            cloud::continuation_create_or_reuse(&existing_ids, new_run_id)
        {
            return Ok(());
        }
        let prompt_md =
            prompt_md.ok_or_else(|| "the prompt ConfigMap was already gone".to_string())?;
        let minutes = v.spec.timeout_minutes;
        let branch = format!("jesse/{}", v.spec.branch_slug);
        let previous = v.run_id.as_deref().unwrap_or(&v.name);
        let prompt = cloud::continuation_prompt(
            prompt_md,
            &cloud::ContinuationPromptInput {
                link,
                max: rt.settings.max_continuations,
                root: &chain.root,
                previous,
                minutes,
                branch: &branch,
                sha: &cp.sha,
            },
        );
        let args = DevJobArgs {
            repo: Some(v.spec.repo.clone()),
            prompt: Some(prompt),
            agent: Some(v.spec.agent.clone()),
            model: v.spec.model.clone(),
            branch_slug: Some(v.spec.branch_slug.clone()),
            test_command: v.spec.test_command.clone(),
            timeout_minutes: Some(minutes),
            ..DevJobArgs::default()
        };
        let mut spec = DevJobSpec::validate(args, self.default_agent)?;
        spec.timeout_minutes = cloud::cloud_capped_timeout_minutes(
            Some(minutes),
            rt.settings.effective_max_timeout_minutes(),
        );
        let merge = cloud::MergeDecision::from_annotation(
            job.metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get(cloud::ANN_MERGE))
                .map(String::as_str)
                .unwrap_or(""),
        );
        let chain_json = serde_json::to_string(&new_chain).map_err(|e| e.to_string())?;
        let run = cloud::CloudRun {
            spec,
            run_id: new_run_id.to_string(),
            merge,
            strand: strand_of(job),
            timeout_note: None,
            chain: Some(ChainExtras {
                root: chain.root.clone(),
                link,
                continuation_of: previous.to_string(),
                chain_json,
            }),
            // A capped held Job's continuation is still held; a capped answer still answers.
            review: cloud::review_mode(job),
        };
        self.create_cloud_run(&run, &v.origin).await.map(|_| ())
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
        if let Some(c) = self.cloud.get() {
            c.spawn_watcher();
        }
        tokio::spawn(self.clone().run_watcher())
    }

    /// Wake the watcher for a pass now.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// The watcher's wake, for a task that outlives the call that spawned it.
    fn self_wake(&self) -> Arc<tokio::sync::Notify> {
        self.wake.clone()
    }
}

/// The status a cloud run's record ends with once its Job is reported: the Runs page's own
/// mapping ([`crate::hostrun::cloud::cloud_status`]) of the Job and the report's outcome (`None`
/// when an earlier pass wrote the report). A Job that completed with no outcome known is
/// `reported`, one that failed `failed`.
pub fn cloud_end_status(v: &JobView, outcome: Option<RunOutcome>) -> crate::hostrun::RunStatus {
    let mut seen = v.clone();
    if let Some(o) = outcome {
        seen.outcome = Some(o.label().to_string());
    }
    match crate::hostrun::cloud::cloud_status(&seen) {
        crate::hostrun::RunStatus::Queued | crate::hostrun::RunStatus::Running => {
            if v.finished == Some(true) {
                crate::hostrun::RunStatus::Reported
            } else {
                crate::hostrun::RunStatus::Failed
            }
        }
        s => s,
    }
}

/// Write `text` as the run's report, under the write lock, at a name no other report has.
async fn write_report(
    sink: &ReportSink,
    job: &str,
    date: &str,
    stem: &str,
    text: &str,
) -> Result<String, String> {
    let turn = format!("devjob:{job}");
    loop {
        let rel = report_rel_path_stem(date, stem, |p| sink.notes_root.join(p).exists());
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
                                         "description": "the agent's time limit; default 120, \
                                             capped at 240; a reduced value is named in the \
                                             reply's timeout_note" },
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

/// The tools once `[hostrun]` is attached: `devjob_start` gains `host`, `strand`, `item_id` and
/// `again`, and `devjob_reply`, `devjob_cancel` and `devjob_close` join. [`tool_descriptors`] is
/// unchanged.
pub fn tool_descriptors_with_studio() -> Vec<Value> {
    let mut tools = tool_descriptors();
    if let Some(start) = tools.first_mut() {
        start["description"] = json!(
            "Start a coding agent. With no host (or host \"cluster\") it runs on the k3s \
             cluster against a GitHub repository: it clones the repository, runs the agent on the \
             prompt and the test command, pushes the branch jesse/<branch_slug> and opens a pull \
             request if anything changed; it never merges and never pushes to main. Returns the \
             job name in seconds and whether the run is queued. The bridge watches the run and \
             writes its report to Inbox/<date>-dev-job-<branch_slug>.md, then pushes a \
             notification. With host \"studio\" it runs Claude Code on Jeremy's Mac Studio \
             with no time limit, in its own worktree: pass strand and item_id to run any \
             unchecked Drafts item of a strand (\"run GD5 on the Studio\"); its repository is \
             the draft's devjob-repo, else repo, else the strand's one repos entry (a strand with \
             several needs repo). Or pass draft (a path under Projects/drafts/), and repo if it \
             names none, to run a draft outside any strand line, or prompt and branch_slug (and \
             repo) for a free prompt. On the Studio repo is optional: local/<name> names a \
             repository that exists only on the Studio, with no GitHub remote, no push, no pull \
             request and no CI (the agent merges into its main), created on first use. When \
             nothing names a repository, a strand with no repos uses local/<strand slug>, a \
             loose draft local/scratch, and a free prompt local/<branch_slug>. The cluster \
             refuses local/<name>. It returns a run_id; the report lands in \
             Inbox/<date>-run-<run_id>.md. A second start of an item that is still queued, \
             running, paused or needs Jeremy is refused unless again is true. On the Studio \
             every run is a reviewed run unless it says otherwise: when the run reports, a second \
             model reads its diff read only, the run's own session checks each finding and fixes \
             what holds, and only then is the pull request marked ready (and merged if the prompt \
             said so) and Jeremy told. With no reviewer named the bridge's default reviewers \
             (Codex) review it, and a default reviewer that is down does not stop the run: it \
             goes on unreviewed and its report says so. reviewer names the reviewer instead (a \
             model id from the model picker: a Codex, Fireworks, local or Claude model), or a \
             list of up to three that review at once and are answered in one round, or \"none\" \
             for no review. A draft may say the same as devjob-reviewer:. The cluster's runs \
             are never reviewed. priority (low, normal, high or urgent; a draft may say \
             devjob-priority:) decides which queued Studio run takes a free slot first; \
             devjob_priority changes it while the run waits."
        );
        if let Some(props) = start["inputSchema"]["properties"].as_object_mut() {
            props.insert(
                "repo".into(),
                json!({ "type": "string",
                        "description": "owner/name on GitHub; required on the cluster. Host \
                            studio: optional, and local/<name> names a Studio only repository \
                            with no remote; with none named it falls back to local/<strand slug>, \
                            local/scratch or local/<branch_slug>" }),
            );
            props.insert(
                "host".into(),
                json!({ "type": "string", "enum": ["cluster", "studio"],
                        "description": "where the run goes; default cluster" }),
            );
            props.insert(
                "strand".into(),
                json!({ "type": "string", "description": "host studio: the strand note's slug (Strands/<strand>.md)" }),
            );
            props.insert(
                "item_id".into(),
                json!({ "type": "string", "description": "host studio: the Drafts line's bold id, e.g. GD5" }),
            );
            props.insert(
                "draft".into(),
                json!({ "type": "string",
                        "description": "host studio: a draft's path under Projects/drafts/, run outside any strand line" }),
            );
            props.insert(
                "again".into(),
                json!({ "type": "boolean",
                        "description": "host studio: start a further run of an item whose run is still live" }),
            );
            props.insert(
                "reviewer".into(),
                json!({ "type": "string",
                        "description": "host studio: who reviews the run read only before it finishes: a model id exactly as the model picker names it, or \"none\" for no review (several: reviewers). Absent: the draft's devjob-reviewer:, else the bridge's default reviewers. A named one that is unknown or unhealthy is refused" }),
            );
            props.insert(
                "reviewers".into(),
                json!({ "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 3,
                        "description": "host studio: up to three model ids, no repeats, that review at once and are answered in one round; [\"none\"] is no review. Pass reviewer or reviewers, not both" }),
            );
            props.insert(
                "review_focus".into(),
                json!({ "type": "string",
                        "description": "host studio: what the reviewers should look at most closely, at most 4 KiB; refused when nothing reviews the run" }),
            );
            props.insert(
                "priority".into(),
                json!({ "type": "string", "enum": crate::hostrun::Priority::NAMES,
                        "description": "host studio: how soon the run wants a slot while queued; queued runs start highest first, oldest first within a level, and a priority never stops a running run or bypasses a cap. Absent: the draft's devjob-priority:, else normal. Change it while queued with devjob_priority" }),
            );
        }
        start["inputSchema"]["required"] = json!([]);
    }
    tools.push(json!({
        "name": TOOL_REPLY,
        "description": "Answer a Studio run that needs you, or continue a reported, failed, \
            interrupted or paused one: a new round in the same worktree and Claude Code session, with the \
            text as the next message. \"Continue where you left off.\" resumes it.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["run_id", "text"],
            "properties": {
                "run_id": { "type": "string" },
                "text": { "type": "string", "description": "the reply, at most 64 KiB" },
            },
        },
    }));
    tools.push(json!({
        "name": TOOL_CANCEL,
        "description": "Cancel a Studio run: a queued or paused one simply never starts again; a running one is \
            stopped (SIGTERM, then SIGKILL after 30 seconds) and its report is written from \
            whatever it has.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["run_id"],
            "properties": { "run_id": { "type": "string" } },
        },
    }));
    tools.push(json!({
        "name": TOOL_CLOSE,
        "description": "Close a Studio run whose question is settled, so it no longer counts as \
            needing Jeremy: a run that needs you, was interrupted or failed. Nothing is stopped and \
            nothing is deleted; a reply still continues it. Queued, running and paused runs are \
            refused (cancel those).",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["run_id"],
            "properties": {
                "run_id": { "type": "string" },
                "reason": { "type": "string", "description": "why; default \"closed by Jeremy\"" },
            },
        },
    }));
    tools.push(json!({
        "name": TOOL_REVIEW,
        "description": "Have a second model review a finished Studio run: the model (any id the \
            model picker names: Codex, a Fireworks model, a local model, another Claude) reads \
            the run's diff from its merge base with main to its head, read only, in a worktree \
            of its own, and the bridge writes its findings to \
            Inbox/<date>-run-<run_id>-review-<n>.md. reviewer may be a list of up to three: \
            they review at once, each writes its own note, and their findings are handed off \
            together. With auto_reply (the default) the findings then go to the run's own \
            session as its next round, one round however many reviewed, which checks each one, \
            fixes only what holds and reports a verdict table. Refused for a run that is \
            queued, running, paused or already being reviewed, one with no commits beyond its \
            base, and a model that is unknown or unhealthy.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["run_id"],
            "properties": {
                "run_id": { "type": "string" },
                "reviewer": { "type": "string", "description": "a model id, exactly as the model picker names it (several: reviewers)" },
                "reviewers": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 3,
                               "description": "up to three model ids, no repeats, reviewed at once and handed off together; pass reviewer or reviewers, not both" },
                "focus": { "type": "string", "description": "what to look at most closely, at most 4 KiB" },
                "auto_reply": { "type": "boolean", "description": "hand the findings to the run's session at once; default true" },
            },
        },
    }));
    tools.push(json!({
        "name": TOOL_REVIEW_REPLY,
        "description": "Hand a Studio run's waiting review (one asked for with auto_reply false) \
            to the run's session as its next round, with the same reply an automatic hand off \
            sends.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["run_id"],
            "properties": { "run_id": { "type": "string" } },
        },
    }));
    tools.push(json!({
        "name": TOOL_PRIORITY,
        "description": "Change the priority of a queued Studio run (one waiting for a slot, its \
            repository, the disk or the usage limit), then run the queue once. Queued runs start \
            highest priority first (urgent, high, normal, low), oldest first within a level; a \
            run the usage limit or a restart stopped still goes first. A priority never stops \
            or pauses a running run and never bypasses a cap: a high run in a busy repository \
            leaves a free slot to a lower run in an idle one. Refused for a run that is \
            running, finished, closed or cancelled.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["run_id", "priority"],
            "properties": {
                "run_id": { "type": "string" },
                "priority": { "type": "string", "enum": crate::hostrun::Priority::NAMES },
            },
        },
    }));
    tools
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyArgs {
    run_id: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelArgs {
    run_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PriorityArgs {
    run_id: String,
    priority: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    run_id: String,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewArgs {
    run_id: String,
    /// One id, or (also accepted here) a list.
    reviewer: Option<crate::hostrun::review::ReviewerArg>,
    reviewers: Option<Vec<String>>,
    focus: Option<String>,
    auto_reply: Option<bool>,
}

/// One Studio run, as `devjob_status` reports it: the record, `host`, and the readable log tail.
fn studio_status(svc: &crate::hostrun::HostRuns, r: &crate::hostrun::RunRecord) -> Value {
    let mut v = crate::hostrun::run_json_located(r, svc.notes_root());
    v["log_tail"] = json!(svc
        .log_lines(&r.run_id, STATUS_LOG_LINES)
        .unwrap_or_default());
    // The record leaves out a `normal` priority; the tool always says it.
    v["priority"] = json!(r.priority);
    v
}

/// Whether a Studio run was closed by a bridge before 0.226.0 when a cloud slot took it: its
/// id names that bridge's cloud Job, which has no record of its own.
fn moved_to_cloud(r: &crate::hostrun::RunRecord) -> bool {
    r.closed_reason
        .as_deref()
        .is_some_and(|c| c.starts_with(crate::hostrun::MOVED_TO_CLOUD))
}

/// One Studio run, as `devjob_list` names it.
fn studio_brief(r: &crate::hostrun::RunRecord) -> Value {
    let mut v = json!({
        "run_id": r.run_id,
        "host": r.served_host(),
        "repo": r.repo,
        "branch": r.branch,
        "status": r.status,
        "waiting_for": r.waiting_for,
        "priority": r.priority,
        "strand": r.strand,
        "item_id": r.item_id,
        "created_at": r.created_at,
        "finished_at": r.finished_at,
        "stalled_since": r.stalled_since,
        "resume_at": r.resume_at,
        "closed_reason": r.closed_reason,
        "reviewer": r.reviewer,
        "reviewers": r.reviewer_ids(),
        "reviewers_source": r.source_of_reviewers(),
        "reviews": crate::hostrun::reviews_json(r, None),
        "report_paths": r.rounds.iter().filter_map(|x| x.report_path.clone()).collect::<Vec<_>>(),
        "desk_command": r.desk_command,
    });
    // A cloud run names its Job too; a Studio run's brief is what it was.
    if let Some(job) = r.cloud.as_ref().and_then(|c| c.job_name.as_deref()) {
        v["job_name"] = json!(job);
    }
    v
}

/// `devjob_list`'s Studio `active`: the running, paused and reviewing runs, newest first as `runs` gives them, then the queued runs in the order the queue
/// pass considers them ([`crate::hostrun::start_order`]), each with `queue_position`, its
/// place among its repository's queued runs (from 1). Across repositories the caps still decide
/// which starts next.
fn studio_active(runs: &[crate::hostrun::RunRecord]) -> Vec<Value> {
    let mut out: Vec<Value> = runs
        .iter()
        .filter(|r| !r.status.is_finished() && r.status != crate::hostrun::RunStatus::Queued)
        .map(studio_brief)
        .collect();
    for (r, place) in crate::hostrun::start_order(runs) {
        let mut v = studio_brief(r);
        v["queue_position"] = json!(place);
        out.push(v);
    }
    out
}

impl DevJobs {
    /// `devjob_start` with `host: "studio"`.
    async fn start_studio(
        &self,
        kind: crate::hostrun::StartKind,
        again: bool,
        reviewer: Option<crate::hostrun::review::ReviewerArg>,
        review_focus: Option<String>,
        priority: Option<crate::hostrun::Priority>,
    ) -> Value {
        let Some(svc) = self.studio.get() else {
            return error_result("host \"studio\" needs [hostrun] enabled on this bridge");
        };
        match svc
            .start(crate::hostrun::StartRequest {
                kind,
                origin: "turn".into(),
                again,
                reviewer,
                review_focus,
                priority,
            })
            .await
        {
            Ok(s) => text_result(
                &json!({
                    "run_id": s.run_id,
                    "host": "studio",
                    "status": s.status,
                    "waiting_for": s.waiting_for,
                    "priority": svc.get(&s.run_id).map(|r| r.priority).unwrap_or_default(),
                }),
                false,
            ),
            Err(e) => error_result(e.message()),
        }
    }
}

impl DevJobs {
    /// `devjob_list`'s `cloud` key, when the cloud host is attached: its running, queued and
    /// finished Jobs, or the error that kept them from being listed. A Job a run record owns
    /// (archived or not) is left out: the run is listed once, as its record, under `studio`.
    async fn list_cloud(&self, v: &mut Value) {
        let Some(c) = self.cloud.get() else {
            return;
        };
        let mut listed = match c.list().await {
            Ok(l) => l,
            Err(e) => json!({ "running": [], "queued": [], "finished": [], "cloud_error": e }),
        };
        if let Some(svc) = self.studio.get().cloned() {
            let owned: HashSet<String> = tokio::task::spawn_blocking(move || {
                crate::hostrun::scan_runs(&svc.settings.runs_dir())
                    .0
                    .into_iter()
                    .filter_map(|(_, r)| r.cloud)
                    .flat_map(|c| c.jobs.into_iter().chain(c.job_name))
                    .collect()
            })
            .await
            .unwrap_or_default();
            drop_owned(&mut listed, &owned);
        }
        v["cloud"] = listed;
    }
}

/// `devjob_list`'s `cloud` key without the Jobs in `owned`: those of a run record, which the
/// list serves once, as the record, under `studio`.
pub fn drop_owned(listed: &mut Value, owned: &HashSet<String>) {
    for key in ["running", "queued", "finished"] {
        if let Some(a) = listed[key].as_array_mut() {
            a.retain(|j| j["job_name"].as_str().is_none_or(|n| !owned.contains(n)));
        }
    }
}

/// The tools with the cloud host: `host` gains `cloud`, `devjob_start` gains `merge`, and
/// `devjob_cancel` is listed (it cancels cloud runs as well as Studio ones). Without the cloud
/// the tools are exactly [`tool_descriptors`] or [`tool_descriptors_with_studio`].
/// `max_timeout_minutes` is the cloud's own cap on `timeout_minutes`: `[devjob.cloud]
/// max_timeout_minutes`, or less when kc-burst's own live `max_ttl` leaves less
/// ([`cloud::CloudSettings::effective_max_timeout_minutes`]).
pub fn with_cloud(mut tools: Vec<Value>, max_timeout_minutes: u64) -> Vec<Value> {
    if let Some(start) = tools.iter_mut().find(|t| t["name"] == TOOL_START) {
        let d = start["description"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        start["description"] = json!(format!(
            "{d} With host \"cloud\" it runs on kc1, two at a time, on burst workers the \
             bridge starts and stops: pass strand and item_id or draft (the draft must say \
             devjob-host: cloud or any) or prompt, branch_slug and repo, as on the Studio, with \
             agent and test_command as on the cluster, and timeout_minutes capped at \
             {max_timeout_minutes} by the bridge's [devjob.cloud] max_timeout_minutes config, \
             not the cluster's {MAX_TIMEOUT_MINUTES}; a reduced value is named in the reply's \
             timeout_note. The agent has no GitHub \
             credential; the job rebases onto origin/main, pushes, opens a pull request titled \
             [<run_id>] and, with merge true (or devjob-merge: true in the draft), merges it once \
             every CI run on its exact head is green; repositories on the deny list never \
             merge. A prompt that asks for several pull requests in order may ship several: \
             each finished part but the last is opened as [<run_id>] Part <n>: <title>, \
             merged once CI on it is green before the next part is cut, and the agent \
             carries on on the new main in the same run; the last part is the run's own pull \
             request. A part the run may not merge (no merge, the deny list, a reviewed run's \
             first job) stops the run with NEEDS YOU naming it, since the next part cannot \
             be cut on an unmerged one. The report and the push name every pull request, in \
             order. It returns a run_id; the report lands in Inbox/<date>-run-<run_id>.md. A \
             cloud run waits in the Studio's queue, as a run of its own on the Runs page: \
             priority and devjob_priority order it as they order Studio runs, and \
             devjob_cancel, devjob_close and devjob_status work on it by run id. A queued Studio \
             run whose draft says devjob-host: any is started by whichever of a Studio slot and \
             a cloud slot frees first, keeping its run id. A cloud run is reviewed as a Studio \
             run is (reviewer, reviewers, review_focus, the draft's devjob-reviewer:, else the \
             bridge's default reviewers): its job opens the pull request as a draft and does not \
             merge, the bridge reviews the pushed branch, and a further job on the same branch \
             and pull request answers the findings, then merges as above. devjob_reply and \
             devjob_review reach it the same way."
        ));
        let props = &mut start["inputSchema"]["properties"];
        let hosts = match props.get("host") {
            Some(_) => json!(["cluster", "studio", "cloud"]),
            None => json!(["cluster", "cloud"]),
        };
        props["host"] = json!({ "type": "string", "enum": hosts,
                                "description": "where the run goes; default cluster" });
        if props.get("priority").is_some() {
            props["priority"]["description"] = json!("host studio or cloud: how soon the run wants a slot while queued; queued runs start highest first, oldest first within a level, on either host, and a priority never stops a running run or bypasses a cap. Absent: the draft's devjob-priority:, else normal. Change it while queued with devjob_priority");
        }
        for key in ["reviewer", "reviewers", "review_focus"] {
            if let Some(d) = props
                .get(key)
                .and_then(|p| p["description"].as_str())
                .and_then(|d| d.strip_prefix("host studio: "))
                .map(str::to_string)
            {
                props[key]["description"] = json!(format!("host studio or cloud: {d}"));
            }
        }
        props["merge"] = json!({ "type": "boolean",
                                 "description": "host cloud: merge the pull request once every CI run on its head is green; default the draft's devjob-merge, else false" });
        if props.get("strand").is_none() {
            props["strand"] =
                json!({ "type": "string", "description": "host cloud: the strand note's slug" });
            props["item_id"] =
                json!({ "type": "string", "description": "host cloud: the Drafts line's bold id" });
            props["draft"] = json!({ "type": "string", "description": "host cloud: a draft's path under Projects/drafts/" });
        }
        props["timeout_minutes"] = json!({ "type": "integer", "minimum": 1,
            "description": format!(
                "the agent's time limit; default {DEFAULT_TIMEOUT_MINUTES}, capped at \
                 {MAX_TIMEOUT_MINUTES} on the cluster and, with host cloud, at \
                 {max_timeout_minutes} from [devjob.cloud] max_timeout_minutes; a reduced \
                 value is named in the reply's timeout_note"
            ) });
        start["inputSchema"]["required"] = json!([]);
    }
    if let Some(p) = tools.iter_mut().find(|t| t["name"] == TOOL_PRIORITY) {
        let d = p["description"].as_str().unwrap_or_default().to_string();
        p["description"] = json!(format!(
            "{d} A queued cloud run (host cloud) is in the same queue and changes the same way."
        ));
    }
    if !tools.iter().any(|t| t["name"] == TOOL_CANCEL) {
        tools.push(json!({
            "name": TOOL_CANCEL,
            "description": "Cancel a cloud run by run_id or job name: a queued one is deleted; a running one is stopped and reported.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["run_id"],
                "properties": { "run_id": { "type": "string" } },
            },
        }));
    }
    tools
}

impl crate::gateway::InProcessServer for DevJobs {
    fn tools(&self) -> Vec<Value> {
        let tools = if self.studio.get().is_some() {
            tool_descriptors_with_studio()
        } else {
            tool_descriptors()
        };
        if let Some(c) = self.cloud.get() {
            let max =
                c.rt.as_ref()
                    .map(|rt| rt.settings.effective_max_timeout_minutes())
                    .unwrap_or(cloud::DEFAULT_MAX_TIMEOUT_MINUTES);
            with_cloud(tools, max)
        } else {
            tools
        }
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
                    let requested_timeout = args.timeout_minutes;
                    let spec = match plan_start(args, self.default_agent) {
                        Ok(StartPlan::Cluster(s)) => s,
                        Ok(StartPlan::Studio {
                            kind,
                            again,
                            reviewer,
                            review_focus,
                            priority,
                        }) => {
                            return Ok(self
                                .start_studio(kind, again, reviewer, review_focus, priority)
                                .await)
                        }
                        Ok(StartPlan::Cloud(req)) => {
                            let Some(c) = self.cloud.get() else {
                                return Ok(error_result(
                                    "host \"cloud\" needs [devjob.cloud] enabled on this bridge",
                                ));
                            };
                            let Some(svc) = self.studio.get() else {
                                return Ok(error_result(
                                    "host \"cloud\" needs [hostrun] enabled on this bridge: a \
                                     cloud run waits in the Studio's queue",
                                ));
                            };
                            return Ok(match c.start_cloud(svc, req, "turn").await {
                                Ok(v) => text_result(&v, false),
                                Err(e) => error_result(&e),
                            });
                        }
                        Err(e) => return Ok(error_result(&e)),
                    };
                    match self.start(&spec, "turn").await {
                        Ok(s) => {
                            let mut v = json!(s);
                            v["timeout_note"] = json!(timeout_reduced_note(
                                requested_timeout,
                                s.timeout_minutes,
                                "the cluster"
                            ));
                            Ok(text_result(&v, false))
                        }
                        Err(e) => Ok(error_result(&e)),
                    }
                }
                TOOL_STATUS => {
                    let args: StatusArgs = match serde_json::from_value(arguments) {
                        Ok(a) => a,
                        Err(e) => return Ok(error_result(&format!("bad arguments: {e}"))),
                    };
                    if let Some(svc) = self.studio.get() {
                        if let Some(r) = svc.get(args.job_name.trim()) {
                            // A run an earlier bridge gave the cloud is answered by its Job.
                            if !(moved_to_cloud(&r) && self.cloud.get().is_some()) {
                                let mut out = studio_status(svc, &r);
                                // A run on the cloud: its Job's phase, wait and log.
                                let job = r.cloud.as_ref().and_then(|c| c.job_name.clone());
                                if let (Some(job), Some(c)) = (job, self.cloud.get()) {
                                    if let Ok(Some(run)) = c.run(&job).await {
                                        out["log_tail"] = json!(run.log_tail);
                                        out["job"] = json!(run);
                                    }
                                }
                                return Ok(text_result(&out, false));
                            }
                        }
                    }
                    if let Some(c) = self.cloud.get() {
                        if let Ok(Some(v)) = c.find_cloud_job(args.job_name.trim()).await {
                            if let Ok(Some(r)) = c.run(&v.name).await {
                                let mut out = json!(r);
                                out["host"] = json!("cloud");
                                out["run_id"] = json!(v.run_id);
                                return Ok(text_result(&out, false));
                            }
                        }
                    }
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
                    let Some(svc) = self.studio.get() else {
                        return match self.list().await {
                            Ok(mut v) => {
                                self.list_cloud(&mut v).await;
                                Ok(text_result(&v, false))
                            }
                            Err(e) => Ok(error_result(&e)),
                        };
                    };
                    // With Studio runs, a cluster that cannot be listed still lists them.
                    let mut v = match self.list().await {
                        Ok(v) => v,
                        Err(e) => {
                            json!({ "running": [], "queued": [], "finished": [], "cluster_error": e })
                        }
                    };
                    let svc = svc.clone();
                    let runs = tokio::task::spawn_blocking(move || {
                        let mut runs: Vec<crate::hostrun::RunRecord> =
                            crate::hostrun::scan_runs(&svc.settings.runs_dir())
                                .0
                                .into_iter()
                                .map(|(_, r)| r)
                                .collect();
                        runs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
                        runs
                    })
                    .await
                    .unwrap_or_default();
                    let finished: Vec<Value> = runs
                        .iter()
                        .filter(|r| r.status.is_finished())
                        .take(LIST_FINISHED)
                        .map(studio_brief)
                        .collect();
                    v["studio"] = json!({
                        "active": studio_active(&runs),
                        "finished": finished,
                    });
                    self.list_cloud(&mut v).await;
                    Ok(text_result(&v, false))
                }
                TOOL_REPLY => {
                    let Some(svc) = self.studio.get() else {
                        return Err(format!("devjob has no tool {tool:?}"));
                    };
                    let args: ReplyArgs = match serde_json::from_value(arguments) {
                        Ok(a) => a,
                        Err(e) => return Ok(error_result(&format!("bad arguments: {e}"))),
                    };
                    match svc.reply_and_pump(args.run_id.trim(), &args.text).await {
                        Ok(r) => Ok(text_result(&studio_brief(&r), false)),
                        Err(e) => Ok(error_result(e.message())),
                    }
                }
                TOOL_CANCEL => {
                    if self.studio.get().is_none() && self.cloud.get().is_none() {
                        return Err(format!("devjob has no tool {tool:?}"));
                    }
                    let args: CancelArgs = match serde_json::from_value(arguments) {
                        Ok(a) => a,
                        Err(e) => return Ok(error_result(&format!("bad arguments: {e}"))),
                    };
                    let id = args.run_id.trim();
                    let studio_has = self
                        .studio
                        .get()
                        .and_then(|s| s.get(id))
                        .is_some_and(|r| !moved_to_cloud(&r));
                    if !studio_has {
                        if let Some(c) = self.cloud.get() {
                            if let Ok(Some(_)) = c.find_cloud_job(id).await {
                                return Ok(match c.cancel_cloud(id).await {
                                    Ok(v) => text_result(&v, false),
                                    Err(e) => error_result(&e),
                                });
                            }
                        }
                    }
                    let Some(svc) = self.studio.get() else {
                        return Ok(error_result(&format!("no run is named {id:?}")));
                    };
                    match svc.cancel_and_wake(id).await {
                        Ok(r) => Ok(text_result(&studio_brief(&r), false)),
                        Err(e) => Ok(error_result(e.message())),
                    }
                }
                TOOL_CLOSE => {
                    let Some(svc) = self.studio.get() else {
                        return Err(format!("devjob has no tool {tool:?}"));
                    };
                    let args: CloseArgs = match serde_json::from_value(arguments) {
                        Ok(a) => a,
                        Err(e) => return Ok(error_result(&format!("bad arguments: {e}"))),
                    };
                    match svc.close_blocking(args.run_id.trim(), args.reason).await {
                        Ok(r) => Ok(text_result(&studio_brief(&r), false)),
                        Err(e) => Ok(error_result(e.message())),
                    }
                }
                TOOL_REVIEW => {
                    let Some(svc) = self.studio.get() else {
                        return Err(format!("devjob has no tool {tool:?}"));
                    };
                    let args: ReviewArgs = match serde_json::from_value(arguments) {
                        Ok(a) => a,
                        Err(e) => return Ok(error_result(&format!("bad arguments: {e}"))),
                    };
                    let who = match crate::hostrun::review::merge_spellings(
                        args.reviewer,
                        args.reviewers,
                    ) {
                        Ok(Some(w)) => w,
                        Ok(None) => {
                            return Ok(error_result("reviewer is missing: name a model id"))
                        }
                        Err(e) => return Ok(error_result(&e)),
                    };
                    match svc
                        .request_review_and_start(
                            args.run_id.trim(),
                            who,
                            args.focus,
                            args.auto_reply.unwrap_or(true),
                        )
                        .await
                    {
                        Ok(r) => Ok(text_result(&studio_brief(&r), false)),
                        Err(e) => Ok(error_result(e.message())),
                    }
                }
                TOOL_REVIEW_REPLY => {
                    let Some(svc) = self.studio.get() else {
                        return Err(format!("devjob has no tool {tool:?}"));
                    };
                    let args: CancelArgs = match serde_json::from_value(arguments) {
                        Ok(a) => a,
                        Err(e) => return Ok(error_result(&format!("bad arguments: {e}"))),
                    };
                    match svc.review_reply_and_pump(args.run_id.trim()).await {
                        Ok(r) => Ok(text_result(&studio_brief(&r), false)),
                        Err(e) => Ok(error_result(e.message())),
                    }
                }
                TOOL_PRIORITY => {
                    let Some(svc) = self.studio.get() else {
                        return Err(format!("devjob has no tool {tool:?}"));
                    };
                    let args: PriorityArgs = match serde_json::from_value(arguments) {
                        Ok(a) => a,
                        Err(e) => return Ok(error_result(&format!("bad arguments: {e}"))),
                    };
                    let priority = match crate::hostrun::Priority::parse(&args.priority) {
                        Ok(p) => p,
                        Err(e) => return Ok(error_result(&e)),
                    };
                    match svc
                        .set_priority_and_pump(args.run_id.trim(), priority)
                        .await
                    {
                        Ok(r) => Ok(text_result(&studio_brief(&r), false)),
                        Err(e) => Ok(error_result(e.message())),
                    }
                }
                other => Err(format!("devjob has no tool {other:?}")),
            }
        })
    }
}

/// The containment battery's `devjob`: the nine tools, listed exactly as the real server lists
/// them on the Studio (with `[hostrun]` attached), and every call refused. Only in a
/// `containment-probe` build.
#[cfg(feature = "containment-probe")]
pub struct BatteryStandIn;

#[cfg(feature = "containment-probe")]
impl crate::gateway::InProcessServer for BatteryStandIn {
    fn tools(&self) -> Vec<Value> {
        tool_descriptors_with_studio()
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
