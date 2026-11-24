//! **DEV JOBS ON KC1** (`host: "cloud"`): the settings, the decisions and the burst workers.
//!
//! The design is `docs/devjob-cloud.md`. A cloud run is a [`super::DevJobs`] of its own, built
//! from `[devjob.cloud]` ([`CloudSettings::cluster_settings`]): the same Jobs, queue and reports
//! as the home cluster, in namespace `jesse-dev` on kc1, with these differences, each decided
//! here so it is tested without a cluster:
//!
//! - **Which drafts may go** ([`DraftHost`]): only one whose `devjob-host` is `cloud` or `any`.
//! - **When to merge** ([`merge_decision`]): asked for, and the repository on no
//!   [`DEFAULT_MERGE_DENY`] or `merge_deny` pattern. The Job does the merge, at green.
//! - **When a queued Job may start** ([`cloud_resumable`]): onto an idle, ready dev job burst
//!   worker only, because Kubernetes starts `activeDeadlineSeconds` on resume.
//! - **The workers** ([`plan_burst`], [`IdleClock`], [`Ledger`]): started on demand with
//!   `kc-burst up`, stopped once idle, extended while busy, under a hard daily cost cap.
//! - **What the agent is told** ([`cloud_paragraph`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use k8s_openapi::api::core::v1::{Node, Pod};
use kube::api::{DeleteParams, ListParams, Patch, PatchParams, PropagationPolicy};
use kube::Api;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    valid_label_value, CloudJobExtras, CloudKind, CloudStart, DevJobArgs, DevJobSettings,
    DevJobSlot, DevJobSpec, DevJobs, JobView, Started, StrandRef,
};

// ---- DEFAULTS ------------------------------------------------------------------------

/// The default `[devjob.cloud] namespace` on kc1.
pub const DEFAULT_NAMESPACE: &str = "jesse-dev";
/// The default `[devjob.cloud] kubeconfig`: the `jesse:devjob` ServiceAccount's token.
pub const DEFAULT_KUBECONFIG: &str = ".kube/kc1-devjob.yaml";
/// The default `[devjob.cloud] max_concurrent`: the namespace quota's two pods.
pub const DEFAULT_MAX_CONCURRENT: usize = 2;
/// Where `kc-burst` is installed on the Studio.
pub const DEFAULT_KC_BURST: &str = "/usr/local/bin/kc-burst";
/// DigitalOcean's 4 vCPU, 16 GB general purpose worker: one dev job per worker, with room for
/// a Rust workspace the size of trovato to build and test ([`DEFAULT_JOB_MEMORY_GI`]).
pub const DEFAULT_BURST_SIZE: &str = "g-4vcpu-16gb";
/// A worker's first TTL. Extended while it is busy, at most [`MAX_BURST_LIFE_MINUTES`].
pub const DEFAULT_BURST_TTL_MINUTES: u64 = 240;
/// The longest a worker lives, extensions included: kc-burst's own 8 hours.
pub const MAX_BURST_LIFE_MINUTES: u64 = 480;
/// The default `[devjob.cloud] job_memory_gi`: the pod's memory limit, sized for a 16 GB worker
/// less what the node itself keeps.
pub const DEFAULT_JOB_MEMORY_GI: u32 = 14;
/// The default `[devjob.cloud] job_memory_request_gi`.
pub const DEFAULT_JOB_MEMORY_REQUEST_GI: u32 = 12;
/// The default `[devjob.cloud] job_cpu`: the pod's CPU limit, and `CARGO_BUILD_JOBS`.
pub const DEFAULT_JOB_CPU: u32 = 4;
/// The default `[devjob.cloud] job_cpu_request`.
pub const DEFAULT_JOB_CPU_REQUEST: u32 = 3;
/// The resources a Job gets on DigitalOcean's 8 GB, 4 vCPU workers when the config names one as
/// its `burst_size` and sets no resources of its own: what every cloud Job had before the
/// resources came from config, so a config pinned to that size still schedules.
pub const SMALL_WORKER_RESOURCES: JobResources = JobResources {
    memory_gi: 7,
    memory_request_gi: 6,
    cpu: 4,
    cpu_request: 3,
};

/// A size slug's memory in GB and its vCPUs, read from its name (`g-4vcpu-16gb` is 16 and 4;
/// `s-4vcpu-8gb-intel` is 8 and 4). `None` for either part the name does not carry.
pub fn size_shape(slug: &str) -> (Option<u32>, Option<u32>) {
    let mut gb = None;
    let mut vcpu = None;
    for part in slug.split('-') {
        if let Some(n) = part.strip_suffix("gb").and_then(|n| n.parse().ok()) {
            gb = Some(n);
        } else if let Some(n) = part.strip_suffix("vcpu").and_then(|n| n.parse().ok()) {
            vcpu = Some(n);
        }
    }
    (gb, vcpu)
}

/// The Job's default resources for a worker size: [`SMALL_WORKER_RESOURCES`] on an 8 GB worker
/// or smaller, else the defaults sized for [`DEFAULT_BURST_SIZE`].
pub fn default_resources_for(slug: &str) -> JobResources {
    match size_shape(slug).0 {
        Some(gb) if gb < 16 => SMALL_WORKER_RESOURCES,
        _ => JobResources {
            memory_gi: DEFAULT_JOB_MEMORY_GI,
            memory_request_gi: DEFAULT_JOB_MEMORY_REQUEST_GI,
            cpu: DEFAULT_JOB_CPU,
            cpu_request: DEFAULT_JOB_CPU_REQUEST,
        },
    }
}

/// A worker with no dev job pod for this long is stopped, unless a queued run wants it.
pub const DEFAULT_IDLE_MINUTES: u64 = 10;
/// The hard daily cap on what the workers this bridge starts may cost.
pub const DEFAULT_DAILY_COST_CAP_EUR: f64 = 5.0;
/// `s-4vcpu-8gb-intel` in fra1: $0.08333 an hour on 2026-10-28, at kc-burst's 0.92. Not yet
/// re-read for [`DEFAULT_BURST_SIZE`] (`g-4vcpu-16gb` costs more): set `hourly_cost_eur` from
/// `doctl compute size list -o json` so the daily cap counts the bigger worker's price.
pub const DEFAULT_HOURLY_COST_EUR: f64 = 0.0767;
/// The ledger of committed cost, under the bridge's state directory.
pub const DEFAULT_LEDGER: &str = ".jesse-bridge/devjob-cloud-ledger.json";
/// Repositories that never merge automatically, whatever the config says.
pub const DEFAULT_MERGE_DENY: [&str; 2] = ["tag1consulting/scolta*", "tag1consulting/goose"];
/// How long the entrypoint waits for CI on one head before it gives up merging.
pub const MERGE_WAIT_SECONDS: u64 = 2400;
/// The entrypoint's rebase-and-wait attempts before a merge.
pub const MERGE_ATTEMPTS: u64 = 3;
/// The merge path's full wait, in minutes: [`MERGE_WAIT_SECONDS`] times [`MERGE_ATTEMPTS`].
pub const MERGE_WAITS_MINUTES: u64 = MERGE_WAIT_SECONDS * MERGE_ATTEMPTS / 60;
/// What the entrypoint needs after the agent exits (rebase onto `origin/main`, the test
/// command, push, open the pull request) before the merge wait even starts. Not sized from the
/// entrypoint's own code: it lives in `cloud/infrastructure/jesse-dev/` in `jeremyandrews/k3s`,
/// a separate repository this worker has no access to; 30 minutes is a conservative estimate for
/// that handful of steps on a 3 CPU worker.
pub const FINISH_ALLOWANCE_SECS: u64 = 30 * 60;
/// The default `[devjob.cloud] max_timeout_minutes`: the largest `timeout_minutes` a cloud
/// start may be granted. Sized so the Job's own deadline ([`cloud_deadline_secs`]) plus
/// [`BOOT_ALLOWANCE_MINUTES`] exactly fits [`DEFAULT_WORKER_LIFE_MINUTES`]: 480 - 15 - 120 - 30.
pub const DEFAULT_MAX_TIMEOUT_MINUTES: u64 = 315;
/// The default `[devjob.cloud] worker_life_minutes`: kc-burst's own hard cap on a worker's age
/// (`max_ttl`, 8 hours as of 2026-11-23). [`CloudSettings::problems`] refuses more.
pub const DEFAULT_WORKER_LIFE_MINUTES: u64 = 480;
/// Time from `kc-burst up` to a worker being ready and idle, before any Job can resume onto it
/// and start its own `activeDeadlineSeconds` clock. [`CloudSettings::problems`]'s margin.
pub const BOOT_ALLOWANCE_MINUTES: u64 = 15;

/// The node label `kc-burst up --label` puts on a dev job worker; the admission policy in
/// `jesse-dev` requires pods to select it.
pub const POOL_LABEL: &str = "kc.pozza/pool";
pub const POOL_VALUE: &str = "devjob";
/// The labels kc-burst puts on every worker.
pub const BURST_LABEL: &str = "kc.pozza/burst";
pub const RUN_LABEL: &str = "kc.pozza/run";
/// A busy worker this close to its expiry is extended by [`EXTEND_BY`].
pub const EXTEND_WITHIN: Duration = Duration::from_secs(30 * 60);
pub const EXTEND_BY: Duration = Duration::from_secs(60 * 60);
/// After a refused `kc-burst up`, the next is tried this much later.
pub const UP_RETRY: Duration = Duration::from_secs(5 * 60);

/// On a cloud Job: the run id its report is named by.
pub const LABEL_RUN_ID: &str = "jesse.dev/run-id";
/// On a cloud Job: what was decided about merging (`true`, `false`, or `denied: <pattern>`).
pub const ANN_MERGE: &str = "jesse.dev/merge";
/// On a cloud Job a person cancelled.
pub const ANN_CANCELLED: &str = "jesse.dev/cancelled";
/// The draft key that asks a cloud run to merge at green.
pub const MERGE_KEY: &str = "devjob-merge";
/// Where the agent may write the pull request's title, inside the Job.
pub const PR_TITLE_FILE: &str = "/home/agent/work/pr-title.txt";
/// Where the agent writes its report, inside the Job.
pub const RUN_REPORT_FILE: &str = "/home/agent/work/report.md";

// ---- SETTINGS ------------------------------------------------------------------------

/// `[devjob.cloud]` in the bridge TOML.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudToml {
    #[serde(default)]
    pub enabled: bool,
    pub kubeconfig: Option<PathBuf>,
    pub namespace: Option<String>,
    pub image: Option<String>,
    pub max_concurrent: Option<usize>,
    pub kc_burst: Option<PathBuf>,
    pub burst_size: Option<String>,
    pub burst_ttl_minutes: Option<u64>,
    pub idle_minutes: Option<u64>,
    pub daily_cost_cap_eur: Option<f64>,
    pub hourly_cost_eur: Option<f64>,
    pub job_memory_gi: Option<u32>,
    pub job_memory_request_gi: Option<u32>,
    pub job_cpu: Option<u32>,
    pub job_cpu_request: Option<u32>,
    /// Added to [`DEFAULT_MERGE_DENY`], never replacing it.
    #[serde(default)]
    pub merge_deny: Vec<String>,
    pub ledger: Option<PathBuf>,
    /// The largest `timeout_minutes` a cloud start may be granted. Bridge 0.219.0 named this
    /// cap `max_agent_minutes`; a config that still says so keeps booting.
    #[serde(alias = "max_agent_minutes")]
    pub max_timeout_minutes: Option<u64>,
    /// How long a burst worker may live, kc-burst's own `max_ttl`.
    pub worker_life_minutes: Option<u64>,
}

/// A cloud Job's container resources, from [`CloudSettings`]: what [`super::build_cloud_job`]
/// writes, never literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobResources {
    pub memory_gi: u32,
    pub memory_request_gi: u32,
    pub cpu: u32,
    pub cpu_request: u32,
}

impl Default for JobResources {
    fn default() -> Self {
        CloudSettings::default().job_resources()
    }
}

/// `[devjob.cloud]`, resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudSettings {
    pub enabled: bool,
    pub kubeconfig: PathBuf,
    pub namespace: String,
    /// Empty when unset: there is no default, an amd64 digest must be named.
    pub image: String,
    pub max_concurrent: usize,
    pub kc_burst: PathBuf,
    pub burst_size: String,
    pub burst_ttl_minutes: u64,
    pub idle_minutes: u64,
    pub daily_cost_cap_eur: f64,
    pub hourly_cost_eur: f64,
    /// The Job's memory limit and request, in GiB, and its CPU limit and request. Sized to
    /// [`Self::burst_size`]: the worker runs one dev job.
    pub job_memory_gi: u32,
    pub job_memory_request_gi: u32,
    pub job_cpu: u32,
    pub job_cpu_request: u32,
    /// [`DEFAULT_MERGE_DENY`] and then the config's own.
    pub merge_deny: Vec<String>,
    pub ledger: PathBuf,
    pub max_timeout_minutes: u64,
    pub worker_life_minutes: u64,
}

impl Default for CloudSettings {
    fn default() -> Self {
        CloudSettings::from_toml(None, "")
    }
}

fn expand(p: &Path, home: &str) -> PathBuf {
    match p.strip_prefix("~") {
        Ok(rest) if !home.is_empty() => Path::new(home).join(rest),
        _ => p.to_path_buf(),
    }
}

impl CloudSettings {
    pub fn from_toml(t: Option<CloudToml>, home: &str) -> CloudSettings {
        let t = t.unwrap_or_default();
        let mut merge_deny: Vec<String> =
            DEFAULT_MERGE_DENY.iter().map(|s| s.to_string()).collect();
        for p in t.merge_deny {
            let p = p.trim().to_string();
            if !p.is_empty() && !merge_deny.contains(&p) {
                merge_deny.push(p);
            }
        }
        let burst_size = t
            .burst_size
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| DEFAULT_BURST_SIZE.to_string());
        // Absent keys follow the worker: a config pinned to a smaller size keeps fitting it.
        let sized = default_resources_for(&burst_size);
        CloudSettings {
            enabled: t.enabled,
            kubeconfig: match t.kubeconfig {
                Some(p) => expand(&p, home),
                None => Path::new(home).join(DEFAULT_KUBECONFIG),
            },
            namespace: t
                .namespace
                .map(|n| n.trim().to_string())
                .unwrap_or_else(|| DEFAULT_NAMESPACE.to_string()),
            image: t.image.map(|i| i.trim().to_string()).unwrap_or_default(),
            max_concurrent: t.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT),
            kc_burst: t
                .kc_burst
                .map(|p| expand(&p, home))
                .unwrap_or_else(|| PathBuf::from(DEFAULT_KC_BURST)),
            burst_size,
            burst_ttl_minutes: t.burst_ttl_minutes.unwrap_or(DEFAULT_BURST_TTL_MINUTES),
            idle_minutes: t.idle_minutes.unwrap_or(DEFAULT_IDLE_MINUTES),
            daily_cost_cap_eur: t.daily_cost_cap_eur.unwrap_or(DEFAULT_DAILY_COST_CAP_EUR),
            hourly_cost_eur: t.hourly_cost_eur.unwrap_or(DEFAULT_HOURLY_COST_EUR),
            job_memory_gi: t.job_memory_gi.unwrap_or(sized.memory_gi),
            job_memory_request_gi: t.job_memory_request_gi.unwrap_or(sized.memory_request_gi),
            job_cpu: t.job_cpu.unwrap_or(sized.cpu),
            job_cpu_request: t.job_cpu_request.unwrap_or(sized.cpu_request),
            merge_deny,
            ledger: match t.ledger {
                Some(p) => expand(&p, home),
                None => Path::new(home).join(DEFAULT_LEDGER),
            },
            max_timeout_minutes: t.max_timeout_minutes.unwrap_or(DEFAULT_MAX_TIMEOUT_MINUTES),
            worker_life_minutes: t.worker_life_minutes.unwrap_or(DEFAULT_WORKER_LIFE_MINUTES),
        }
    }

    /// Why `[devjob.cloud]` may not boot. Empty when it is off.
    pub fn problems(&self, devjob_enabled: bool) -> Vec<String> {
        let mut out = Vec::new();
        if !self.enabled {
            return out;
        }
        if !devjob_enabled {
            out.push(
                "[devjob.cloud] enabled needs [devjob] enabled: the cloud host is part of the \
                 devjob server"
                    .into(),
            );
        }
        if let Err(e) = super::read_kubeconfig(&self.kubeconfig) {
            out.push(format!(
                "[devjob.cloud] kubeconfig {} is unreadable: {e}",
                self.kubeconfig.display()
            ));
        }
        if !super::is_digest_reference(&self.image) {
            out.push(format!(
                "[devjob.cloud] image {:?} is not pinned by digest (<repository>@sha256:<64 hex>); \
                 name the amd64 build's digest",
                self.image
            ));
        }
        if self.namespace.is_empty() || !super::valid_slug(&self.namespace) {
            out.push(format!(
                "[devjob.cloud] namespace {:?} is not a namespace name",
                self.namespace
            ));
        }
        if self.max_concurrent == 0 {
            out.push("[devjob.cloud] max_concurrent must be at least 1".into());
        }
        if !self.kc_burst.is_file() {
            out.push(format!(
                "[devjob.cloud] kc_burst {} is not a file",
                self.kc_burst.display()
            ));
        }
        if self.burst_size.is_empty() || self.burst_size.contains(char::is_whitespace) {
            out.push(format!(
                "[devjob.cloud] burst_size {:?} is not a size",
                self.burst_size
            ));
        }
        if self.burst_ttl_minutes < 30 || self.burst_ttl_minutes > MAX_BURST_LIFE_MINUTES {
            out.push("[devjob.cloud] burst_ttl_minutes must be 30 to 480 (kc-burst's 8h)".into());
        }
        if self.idle_minutes == 0 {
            out.push("[devjob.cloud] idle_minutes must be at least 1".into());
        }
        if !(self.daily_cost_cap_eur.is_finite() && self.daily_cost_cap_eur > 0.0) {
            out.push("[devjob.cloud] daily_cost_cap_eur must be above 0".into());
        }
        if !(self.hourly_cost_eur.is_finite() && self.hourly_cost_eur > 0.0) {
            out.push("[devjob.cloud] hourly_cost_eur must be above 0".into());
        }
        out.extend(self.resource_problems());
        for p in &self.merge_deny {
            if !valid_deny_pattern(p) {
                out.push(format!(
                    "[devjob.cloud] merge_deny {p:?} is not owner/name or owner/prefix*"
                ));
            }
        }
        if self.max_timeout_minutes == 0 {
            out.push("[devjob.cloud] max_timeout_minutes must be at least 1".into());
        }
        if self.worker_life_minutes > DEFAULT_WORKER_LIFE_MINUTES {
            out.push(format!(
                "[devjob.cloud] worker_life_minutes must be {DEFAULT_WORKER_LIFE_MINUTES} or \
                 less: raise kc-burst's max_ttl first"
            ));
        }
        let needed = self.max_timeout_minutes
            + FINISH_ALLOWANCE_SECS / 60
            + MERGE_WAITS_MINUTES
            + BOOT_ALLOWANCE_MINUTES;
        if needed > self.worker_life_minutes {
            out.push(format!(
                "[devjob.cloud] max_timeout_minutes ({}) + the finish allowance ({}) + the \
                 merge waits ({MERGE_WAITS_MINUTES}) + a {BOOT_ALLOWANCE_MINUTES} minute boot \
                 allowance = {needed} minutes, more than worker_life_minutes ({}); lower \
                 max_timeout_minutes or raise worker_life_minutes (to at most \
                 {DEFAULT_WORKER_LIFE_MINUTES}, kc-burst's max_ttl)",
                self.max_timeout_minutes,
                FINISH_ALLOWANCE_SECS / 60,
                self.worker_life_minutes
            ));
        }
        out
    }

    /// Why the Job's resources or time cap are refused: each at least 1, each request at most
    /// its limit, and no agent time past the longest a worker lives.
    pub fn resource_problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (key, v) in [
            ("job_memory_gi", self.job_memory_gi),
            ("job_memory_request_gi", self.job_memory_request_gi),
            ("job_cpu", self.job_cpu),
            ("job_cpu_request", self.job_cpu_request),
        ] {
            if v == 0 {
                out.push(format!("[devjob.cloud] {key} must be at least 1"));
            }
        }
        if self.job_memory_request_gi > self.job_memory_gi {
            out.push(format!(
                "[devjob.cloud] job_memory_request_gi {} is above job_memory_gi {}",
                self.job_memory_request_gi, self.job_memory_gi
            ));
        }
        if self.job_cpu_request > self.job_cpu {
            out.push(format!(
                "[devjob.cloud] job_cpu_request {} is above job_cpu {}",
                self.job_cpu_request, self.job_cpu
            ));
        }
        // A request the worker cannot hold leaves every Job pending: the node keeps some of
        // its memory, so the request must be below the worker's own.
        let (gb, vcpu) = size_shape(&self.burst_size);
        if let Some(gb) = gb {
            if self.job_memory_request_gi >= gb {
                out.push(format!(
                    "[devjob.cloud] job_memory_request_gi {} does not fit burst_size {:?} ({gb} GB)",
                    self.job_memory_request_gi, self.burst_size
                ));
            }
        }
        if let Some(vcpu) = vcpu {
            if self.job_cpu_request > vcpu {
                out.push(format!(
                    "[devjob.cloud] job_cpu_request {} does not fit burst_size {:?} ({vcpu} vCPU)",
                    self.job_cpu_request, self.burst_size
                ));
            }
        }
        out
    }

    /// The Job's resources.
    pub fn job_resources(&self) -> JobResources {
        JobResources {
            memory_gi: self.job_memory_gi,
            memory_request_gi: self.job_memory_request_gi,
            cpu: self.job_cpu,
            cpu_request: self.job_cpu_request,
        }
    }

    /// The `[devjob]` settings the cloud's own [`super::DevJobs`] runs with: kc1's kubeconfig,
    /// namespace, image and cap, the home table's default agent.
    pub fn cluster_settings(&self, home: &DevJobSettings) -> DevJobSettings {
        DevJobSettings {
            enabled: self.enabled,
            kubeconfig: self.kubeconfig.clone(),
            namespace: self.namespace.clone(),
            image: self.image.clone(),
            max_concurrent: self.max_concurrent,
            default_agent: home.default_agent.clone(),
            service: DevJobSlot::default(),
            cloud: CloudSettings::default(),
        }
    }

    /// What one `up` commits against the day: its whole TTL.
    pub fn up_cost(&self) -> f64 {
        self.hourly_cost_eur * self.burst_ttl_minutes as f64 / 60.0
    }

    /// What one `extend` commits: [`EXTEND_BY`].
    pub fn extend_cost(&self) -> f64 {
        self.hourly_cost_eur * EXTEND_BY.as_secs_f64() / 3600.0
    }

    /// `MERGE_DENY` for the Job: the patterns, space separated.
    pub fn merge_deny_env(&self) -> String {
        self.merge_deny.join(" ")
    }
}

/// The cloud Job's `activeDeadlineSeconds`: the agent's own timeout, [`FINISH_ALLOWANCE_SECS`]
/// for what the entrypoint does after the agent exits (rebase onto `origin/main`, the test
/// command, push, open the pull request), and the merge path's full wait. Unlike the home
/// cluster's [`DevJobSpec::active_deadline_secs`] (twice the timeout, so Kubernetes never kills
/// the pod before the entrypoint prints its report), a cloud worker cannot live past
/// [`CloudSettings::worker_life_minutes`], so doubling the timeout would already not fit one.
pub fn cloud_deadline_secs(spec: &DevJobSpec) -> u64 {
    spec.agent_timeout_secs() + FINISH_ALLOWANCE_SECS + MERGE_WAIT_SECONDS * MERGE_ATTEMPTS
}

/// A cloud start's granted `timeout_minutes`: the default when none was asked, else the request
/// capped at `max_timeout_minutes` — this host's own cap, not [`super::MAX_TIMEOUT_MINUTES`].
pub fn cloud_capped_timeout_minutes(requested: Option<u64>, max_timeout_minutes: u64) -> u64 {
    requested.map_or(super::DEFAULT_TIMEOUT_MINUTES, |m| {
        m.min(max_timeout_minutes)
    })
}

// ---- ROUTING ---------------------------------------------------------------------------

/// What a draft's `devjob-host` says about where it may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftHost {
    /// No key: the Studio only, as before the cloud existed.
    Unset,
    Studio,
    Cloud,
    /// Whichever of a Studio slot and a cloud slot frees first.
    Any,
    /// A value none of the above: treated as Studio only.
    Other,
}

impl DraftHost {
    pub fn of(src: &str) -> DraftHost {
        match crate::strandjobs::fm(src, crate::hostrun::HOST_KEY).as_deref() {
            None => DraftHost::Unset,
            Some("studio") => DraftHost::Studio,
            Some("cloud") => DraftHost::Cloud,
            Some("any") => DraftHost::Any,
            Some(_) => DraftHost::Other,
        }
    }

    /// Whether a cloud start of the draft is allowed.
    pub fn cloud_capable(self) -> bool {
        matches!(self, DraftHost::Cloud | DraftHost::Any)
    }

    /// Whether a queued Studio run of the draft may be taken by a free cloud slot.
    pub fn drains_to_cloud(self) -> bool {
        self == DraftHost::Any
    }
}

/// Why a draft may not go to the cloud, or `Ok`.
pub fn check_cloud_capable(path: &str, src: &str) -> Result<(), String> {
    if DraftHost::of(src).cloud_capable() {
        Ok(())
    } else {
        Err(format!(
            "{path} does not say {}: cloud or any; only a draft that says it can run without the \
             Studio goes to the cloud",
            crate::hostrun::HOST_KEY
        ))
    }
}

/// A draft's `devjob-merge`, as a yes or no. Anything but `true` is no.
pub fn draft_merge(src: &str) -> bool {
    crate::strandjobs::fm(src, MERGE_KEY).as_deref() == Some("true")
}

// ---- MERGING -----------------------------------------------------------------------------

/// Whether `p` is `owner/name` or `owner/prefix*`.
pub fn valid_deny_pattern(p: &str) -> bool {
    let body = p.strip_suffix('*').unwrap_or(p);
    match body.split_once('/') {
        Some((owner, name)) => {
            !owner.is_empty()
                && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                && (!name.is_empty() || p.ends_with('*'))
        }
        None => false,
    }
}

/// The first deny pattern `repo` matches: exact, or a prefix before a trailing `*`. Compared
/// without case, as GitHub names are.
pub fn denied_by<'a>(repo: &str, deny: &'a [String]) -> Option<&'a str> {
    let repo = repo.to_ascii_lowercase();
    deny.iter().map(String::as_str).find(|p| {
        let p = p.to_ascii_lowercase();
        match p.strip_suffix('*') {
            Some(prefix) => repo.starts_with(prefix),
            None => repo == p,
        }
    })
}

/// What a cloud run does about merging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeDecision {
    /// Asked for and allowed: the Job merges at green (`AUTO_MERGE=true`).
    AtGreen,
    /// Not asked for: the pull request is left open.
    No,
    /// Asked for, and the repository is on the deny list: left open.
    Denied(String),
}

impl MergeDecision {
    /// `AUTO_MERGE` for the Job.
    pub fn auto_merge_env(&self) -> &'static str {
        match self {
            MergeDecision::AtGreen => "true",
            _ => "",
        }
    }

    /// The Job's [`ANN_MERGE`].
    pub fn annotation(&self) -> String {
        match self {
            MergeDecision::AtGreen => "true".into(),
            MergeDecision::No => "false".into(),
            MergeDecision::Denied(p) => format!("denied: {p}"),
        }
    }
}

/// The merge decision: `asked` (the tool's `merge`, else the draft's `devjob-merge`) and the deny
/// list. A repository on the list never merges, however it was asked.
pub fn merge_decision(repo: &str, asked: bool, deny: &[String]) -> MergeDecision {
    if !asked {
        return MergeDecision::No;
    }
    match denied_by(repo, deny) {
        Some(p) => MergeDecision::Denied(p.to_string()),
        None => MergeDecision::AtGreen,
    }
}

// ---- WHAT THE AGENT IS TOLD ---------------------------------------------------------------

/// The section appended to a cloud run's prompt: where it runs, what it may not do, and that the
/// Job, not it, pushes, opens the pull request and merges. Exactly this text, so the tests pin it.
pub fn cloud_paragraph(run_id: &str, repo: &str, branch: &str, merge: &MergeDecision) -> String {
    let merge_sentence = match merge {
        MergeDecision::AtGreen => {
            "and merges it once every CI run on its exact head commit has passed: never while one \
             is pending or has failed, and never by pushing to main"
                .to_string()
        }
        MergeDecision::No => "and leaves it open for Jeremy: this run does not merge".to_string(),
        MergeDecision::Denied(p) => format!(
            "and leaves it open for Jeremy: {repo} is on the merge deny list ({p}), so it never \
             merges automatically"
        ),
    };
    format!(
        "### Run ID and report\n\n\
         You are running in the cloud as dev job {run_id} on kc1, in a fresh clone of {repo} on \
         branch {branch}, cut from a freshly fetched origin/main. Wherever the prompt above names \
         another machine, host, user or directory to work in, this clone replaces it. Nothing \
         that exists only on the Mac Studio is here: not its LAN, its keychain, kc-burst, a \
         local/ repository or another checkout. If the work needs one of those, stop and say so \
         in your report.\n\n\
         You have no GitHub credential. Do not push, open a pull request or merge, and do not \
         try: commit your work on this branch and exit. Wherever the prompt above asks you to \
         open a pull request, wait for CI or merge, that is done for you after you exit: the job \
         rebases your branch onto a freshly fetched origin/main (other runs may be working on \
         this repository at the same time, and one may have merged since you started), runs its \
         test command, pushes the branch, opens a pull request titled [{run_id}] and the first \
         line of {PR_TITLE_FILE} if you write one, {merge_sentence}. So run every check the \
         prompt names yourself before you exit, and keep everything you create inside this clone \
         or the system temporary directory.\n\n\
         When you finish, or stop early, write your complete report, the same text you would \
         answer with, to the file named in the environment variable JESSE_RUN_REPORT ({RUN_REPORT_FILE}), \
         then answer as usual. Nobody can reply to this run: if you need a decision from Jeremy \
         to continue, stop, make the first line of that report NEEDS YOU: followed by the one \
         question, and end the run.\n"
    )
}

/// The prompt a cloud Job receives: the draft's section (or the free prompt) and then
/// [`cloud_paragraph`].
pub fn cloud_prompt(
    prompt: &str,
    run_id: &str,
    repo: &str,
    branch: &str,
    merge: &MergeDecision,
) -> String {
    format!(
        "{}\n\n{}",
        prompt.trim_end(),
        cloud_paragraph(run_id, repo, branch, merge)
    )
}

/// A run id a Job label can carry: [`valid_label_value`].
pub fn valid_run_id(id: &str) -> bool {
    valid_label_value(id)
}

/// `base`, or `base-2`, `base-3`, … : the first that `taken` refuses. A cloud run's id must not be
/// any live Studio run's or cloud Job's, so its report has a name of its own.
pub fn unique_run_id(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken(id))
        .expect("an unbounded range finds a free id")
}

// ---- SLOTS -----------------------------------------------------------------------------

/// Whether a new cloud run may be added at all: active plus queued under `max`. Used by the drain,
/// which must not pile Studio runs into the cloud's own queue.
pub fn cloud_slot_free(views: &[super::JobView], max: usize) -> bool {
    let live = views
        .iter()
        .filter(|v| v.is_active() || v.is_queued())
        .count();
    live < max
}

/// The queued cloud Jobs to resume now, oldest first: as many as [`super::to_resume`] allows
/// under the cap, each matched to an idle ready worker whose [`remaining_life`] covers the
/// Job's own `activeDeadlineSeconds` — because Kubernetes starts that clock on resume, a worker
/// that cannot live that long would be reaped mid run. Every released Job without a worker yet
/// (`unplaced`) takes one of `idle_ready` first, by count. A Job no idle worker's remaining life
/// covers stays queued: `plan_burst` starts it a fresh worker, and the too-old idle one is
/// stopped by the existing idle rule, same as ever.
pub fn cloud_resumable(
    views: &[super::JobView],
    max: usize,
    idle_ready: &[PoolWorker],
    unplaced: usize,
    worker_life_minutes: u64,
) -> Vec<String> {
    let mut lives: Vec<Duration> = idle_ready
        .iter()
        .skip(unplaced)
        .map(|w| remaining_life(w, worker_life_minutes))
        .collect();
    let mut out = Vec::new();
    for name in super::to_resume(views, max) {
        let Some(view) = views.iter().find(|v| v.name == name) else {
            continue;
        };
        let deadline = Duration::from_secs(view.active_deadline_secs.unwrap_or(0).max(0) as u64);
        let Some(pos) = lives.iter().position(|life| *life >= deadline) else {
            continue;
        };
        lives.remove(pos);
        out.push(name);
    }
    out
}

// ---- BURST WORKERS -------------------------------------------------------------------------

/// One dev job worker, as a pass sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolWorker {
    /// The node (and droplet) name, `kcb-<run>-1`.
    pub name: String,
    /// kc-burst's run id, which `down` and `extend` take.
    pub run: String,
    pub ready: bool,
    /// An unfinished dev job pod is bound to it.
    pub busy: bool,
    /// How long it has had no dev job pod ([`IdleClock`]); zero while busy.
    pub idle_for: Duration,
    /// Time to its expiry, when kc-burst knows it.
    pub expires_in: Option<Duration>,
    /// How long since `kc-burst up` created it, when kc-burst knows it (`age_seconds`).
    pub age: Option<Duration>,
}

/// A worker's life left to give a new Job: [`CloudSettings::worker_life_minutes`] minus its
/// [`PoolWorker::age`]. When kc-burst did not report an age, the worker cannot be assumed to
/// outlive what it has already been granted, so its current `expires` stands in.
pub fn remaining_life(w: &PoolWorker, worker_life_minutes: u64) -> Duration {
    match w.age {
        Some(age) => Duration::from_secs(worker_life_minutes * 60).saturating_sub(age),
        None => w.expires_in.unwrap_or(Duration::ZERO),
    }
}

/// What one pass asks kc-burst for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BurstPlan {
    /// Workers to start, one `up 1` each.
    pub up: usize,
    /// kc-burst runs to stop.
    pub down: Vec<String>,
    /// kc-burst runs to extend by [`EXTEND_BY`].
    pub extend: Vec<String>,
}

/// The pool's plan. `demand` is the cloud Jobs that are active or queued; `starting` the `up`s
/// already in flight. One worker per slot, never more than `max`:
///
/// - start until workers plus starting meet `min(demand, max)`;
/// - stop an idle worker once it has been idle `idle_after` and there are more workers than
///   wanted (a queued run keeps one, since it counts in `demand`);
/// - extend a busy worker within [`EXTEND_WITHIN`] of its expiry.
pub fn plan_burst(
    workers: &[PoolWorker],
    starting: usize,
    demand: usize,
    max: usize,
    idle_after: Duration,
) -> BurstPlan {
    let wanted = demand.min(max);
    let have = workers.len() + starting;
    let up = wanted.saturating_sub(have);
    let mut surplus = have.saturating_sub(wanted);
    let mut down = Vec::new();
    // Longest idle first, so the plan is the same whatever order the nodes were listed in.
    let mut idle: Vec<&PoolWorker> = workers
        .iter()
        .filter(|w| !w.busy && w.idle_for >= idle_after)
        .collect();
    idle.sort_by(|a, b| b.idle_for.cmp(&a.idle_for).then(a.name.cmp(&b.name)));
    for w in idle {
        if surplus == 0 {
            break;
        }
        down.push(w.run.clone());
        surplus -= 1;
    }
    let extend = workers
        .iter()
        .filter(|w| w.busy && w.expires_in.is_some_and(|e| e <= EXTEND_WITHIN))
        .map(|w| w.run.clone())
        .collect();
    BurstPlan { up, down, extend }
}

/// How long each worker has had no dev job pod, on tokio's clock (so a test pauses it). Memory
/// only: a restarted bridge counts idleness from its first pass, which only delays a stop, and
/// kc-burst's own TTL ends a worker in any case.
#[derive(Debug, Default)]
pub struct IdleClock {
    since: HashMap<String, tokio::time::Instant>,
}

impl IdleClock {
    /// Record a pass: a busy worker is not idle; an idle one keeps the instant it was first seen
    /// idle. Workers no longer listed are forgotten. Returns each worker's idle time.
    pub fn observe(&mut self, workers: &[(String, bool)]) -> BTreeMap<String, Duration> {
        let now = tokio::time::Instant::now();
        self.since
            .retain(|k, _| workers.iter().any(|(n, busy)| n == k && !busy));
        let mut out = BTreeMap::new();
        for (name, busy) in workers {
            if *busy {
                out.insert(name.clone(), Duration::ZERO);
            } else {
                let since = *self.since.entry(name.clone()).or_insert(now);
                out.insert(name.clone(), now.saturating_duration_since(since));
            }
        }
        out
    }
}

/// What the workers this bridge started have committed today, against the daily cap. Kept in a
/// file so a restart does not reset it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    /// `YYYY-MM-DD`, local time.
    pub day: String,
    pub committed_eur: f64,
}

impl Ledger {
    /// Commit `cost` for `day` if it keeps the day within `cap`. A new day starts at zero.
    pub fn try_commit(&mut self, day: &str, cost: f64, cap: f64) -> bool {
        if self.day != day {
            self.day = day.to_string();
            self.committed_eur = 0.0;
        }
        if self.committed_eur + cost > cap + 1e-9 {
            return false;
        }
        self.committed_eur += cost;
        true
    }

    /// Give back `cost` (an `up` that created nothing), on the same day only.
    pub fn refund(&mut self, day: &str, cost: f64) {
        if self.day == day {
            self.committed_eur = (self.committed_eur - cost).max(0.0);
        }
    }

    pub fn load(path: &Path) -> Ledger {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::write_atomic(path, &bytes)
            .map_err(|e| format!("could not write {}: {e}", path.display()))
    }
}

/// One worker as `kc-burst status --json` lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct BurstWorker {
    pub name: String,
    pub run: String,
    /// Unix seconds.
    pub expires: Option<u64>,
    /// Seconds since `kc-burst up` created it.
    pub age_seconds: Option<u64>,
    pub cost_so_far_eur: f64,
}

/// Parse `kc-burst --json status`: one object per worker, then a summary line with `workers`.
pub fn parse_status(out: &str) -> Vec<BurstWorker> {
    out.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("name").is_some() && v.get("run").is_some())
        .map(|v| BurstWorker {
            name: v["name"].as_str().unwrap_or_default().to_string(),
            run: v["run"].as_str().unwrap_or_default().to_string(),
            expires: v["expires"].as_u64(),
            age_seconds: v["age_seconds"].as_u64(),
            cost_so_far_eur: v["cost_so_far_eur"].as_f64().unwrap_or(0.0),
        })
        .collect()
}

/// The run id `kc-burst --json up` printed: the `run` of its worker lines.
pub fn parse_up(out: &str) -> Option<String> {
    out.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|v| v.get("run").and_then(|r| r.as_str()).map(str::to_string))
        .filter(|r| !r.is_empty())
}

type BurstFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// kc-burst, as the cloud uses it. The CLI in production, a recorder in tests.
pub trait Burst: Send + Sync {
    /// `up 1` in the dev job pool: the new run's id.
    fn up(&self, size: &str, ttl_minutes: u64) -> BurstFuture<'_, String>;
    fn down(&self, run: &str) -> BurstFuture<'_, ()>;
    fn extend(&self, run: &str, ttl: Duration) -> BurstFuture<'_, ()>;
    fn status(&self) -> BurstFuture<'_, Vec<BurstWorker>>;
}

/// The `kc-burst` CLI on the Studio, with its own config and credentials
/// (`~/.config/kc-burst/config.toml`). The bridge reads none of them.
pub struct KcBurstCli {
    pub path: PathBuf,
}

impl KcBurstCli {
    async fn run(&self, args: Vec<String>, limit: Duration) -> Result<String, String> {
        let mut cmd = tokio::process::Command::new(&self.path);
        cmd.arg("--json").args(&args).kill_on_drop(true);
        cmd.stdin(std::process::Stdio::null());
        let out = tokio::time::timeout(limit, cmd.output())
            .await
            .map_err(|_| format!("kc-burst {} took over {}s", args.join(" "), limit.as_secs()))?
            .map_err(|e| format!("kc-burst {}: {e}", args.join(" ")))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() {
            Ok(stdout)
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let last = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            Err(format!(
                "kc-burst {} exited {}: {last}",
                args.join(" "),
                out.status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "by a signal".into())
            ))
        }
    }
}

impl Burst for KcBurstCli {
    fn up(&self, size: &str, ttl_minutes: u64) -> BurstFuture<'_, String> {
        let args = vec![
            "up".into(),
            "1".into(),
            "--ttl".into(),
            format!("{ttl_minutes}m"),
            "--size".into(),
            size.to_string(),
            "--label".into(),
            format!("{POOL_LABEL}={POOL_VALUE}"),
        ];
        Box::pin(async move {
            let out = self.run(args, Duration::from_secs(20 * 60)).await?;
            parse_up(&out).ok_or_else(|| "kc-burst up printed no run id".to_string())
        })
    }

    fn down(&self, run: &str) -> BurstFuture<'_, ()> {
        let args = vec!["down".into(), "--run".into(), run.to_string()];
        Box::pin(async move {
            self.run(args, Duration::from_secs(10 * 60))
                .await
                .map(|_| ())
        })
    }

    fn extend(&self, run: &str, ttl: Duration) -> BurstFuture<'_, ()> {
        let args = vec![
            "extend".into(),
            "--run".into(),
            run.to_string(),
            "--ttl".into(),
            format!("{}m", ttl.as_secs() / 60),
        ];
        Box::pin(async move {
            self.run(args, Duration::from_secs(2 * 60))
                .await
                .map(|_| ())
        })
    }

    fn status(&self) -> BurstFuture<'_, Vec<BurstWorker>> {
        Box::pin(async move {
            let out = self
                .run(vec!["status".into()], Duration::from_secs(2 * 60))
                .await?;
            Ok(parse_status(&out))
        })
    }
}

// ---- THE RUNTIME ---------------------------------------------------------------------------

/// The cloud server's own state: settings, the kc-burst driver, and what a pass remembers. None
/// of it is run state (that is the cluster's); a restart loses only idle times and a backoff.
pub struct CloudRuntime {
    pub settings: CloudSettings,
    burst: Arc<dyn Burst>,
    idle: Mutex<IdleClock>,
    /// `up`s in flight.
    starting: Arc<AtomicUsize>,
    /// The last refused `up`, and when: no other is tried for [`UP_RETRY`].
    last_up_error: Arc<Mutex<Option<(tokio::time::Instant, String)>>>,
    /// Why queued runs wait, as the last pass saw it.
    blocked: Mutex<Option<String>>,
    /// Serialises the ledger's read, commit and write.
    ledger: Arc<Mutex<()>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Today, local time, as the ledger names a day.
pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

impl CloudRuntime {
    pub fn new(settings: CloudSettings, burst: Arc<dyn Burst>) -> CloudRuntime {
        CloudRuntime {
            settings,
            burst,
            idle: Mutex::new(IdleClock::default()),
            starting: Arc::new(AtomicUsize::new(0)),
            last_up_error: Arc::new(Mutex::new(None)),
            blocked: Mutex::new(None),
            ledger: Arc::new(Mutex::new(())),
        }
    }

    /// Commit `cost` against today's cap, in the ledger file. `Ok(false)` when it would pass it.
    pub fn commit(&self, cost: f64) -> Result<bool, String> {
        commit_in(&self.ledger, &self.settings, cost)
    }

    /// What `devjob_status` says a queued cloud run waits for.
    pub fn queued_reason(&self) -> String {
        match lock(&self.blocked).clone() {
            Some(why) => format!("queued: {why}"),
            None => "queued: waiting for an idle dev job burst worker on kc1".into(),
        }
    }

    fn up_backoff(&self) -> Option<String> {
        lock(&self.last_up_error)
            .as_ref()
            .filter(|(at, _)| at.elapsed() < UP_RETRY)
            .map(|(_, e)| e.clone())
    }

    /// Whether one more worker could be started now: no recent refusal and room under the cap.
    fn can_add_worker(&self) -> bool {
        if self.up_backoff().is_some() {
            return false;
        }
        let l = Ledger::load(&self.settings.ledger);
        let committed = if l.day == today() {
            l.committed_eur
        } else {
            0.0
        };
        committed + self.settings.up_cost() <= self.settings.daily_cost_cap_eur + 1e-9
    }
}

fn commit_in(m: &Mutex<()>, settings: &CloudSettings, cost: f64) -> Result<bool, String> {
    let _held = lock(m);
    let mut l = Ledger::load(&settings.ledger);
    if !l.try_commit(&today(), cost, settings.daily_cost_cap_eur) {
        return Ok(false);
    }
    l.save(&settings.ledger)?;
    Ok(true)
}

fn refund_in(m: &Mutex<()>, settings: &CloudSettings, cost: f64) {
    let _held = lock(m);
    let mut l = Ledger::load(&settings.ledger);
    l.refund(&today(), cost);
    if let Err(e) = l.save(&settings.ledger) {
        eprintln!("jesse-bridge: devjob cloud: {e}");
    }
}

/// A cloud run, resolved: the spec (its prompt already the full cloud prompt), the run id, the
/// merge decision and the strand step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudRun {
    pub spec: DevJobSpec,
    pub run_id: String,
    pub merge: MergeDecision,
    pub strand: Option<StrandRef>,
    /// Set when a requested `timeout_minutes` was reduced to [`CloudSettings::max_timeout_minutes`]
    /// ([`super::timeout_reduced_note`]): what `devjob_start`'s reply tells the caller.
    pub timeout_note: Option<String>,
}

/// The branch slug of a cloud run: the Studio's branch name for the same run, cut to the
/// longest slug.
pub fn cloud_slug(run_id: &str, item_text: Option<&str>) -> String {
    let b = crate::hostrun::branch_name(run_id, item_text);
    let cut: String = b.chars().take(super::MAX_SLUG_CHARS).collect();
    cut.trim_end_matches('-').to_string()
}

/// What a cloud run is made of before it has a unique id.
struct Resolved {
    base_id: String,
    item_text: Option<String>,
    repo: String,
    prompt: String,
    model: Option<String>,
    asked_merge: bool,
    strand: Option<StrandRef>,
}

impl DevJobs {
    fn runtime(&self) -> Result<&CloudRuntime, String> {
        self.rt
            .as_ref()
            .ok_or_else(|| "this is not the cloud host's server".to_string())
    }

    fn notes(&self) -> Result<PathBuf, String> {
        self.sink
            .get()
            .map(|s| s.notes_root.clone())
            .ok_or_else(|| "the bridge is still starting: no vault is attached yet".to_string())
    }

    /// Whether a run id is taken: by any Studio run, or any Job in the cloud namespace.
    async fn run_id_taken(&self) -> Result<HashSet<String>, String> {
        let mut taken: HashSet<String> = self
            .views()
            .await?
            .into_iter()
            .filter_map(|(_, v)| v.run_id)
            .collect();
        if let Some(studio) = self.studio.get() {
            let studio = studio.clone();
            let ids = tokio::task::spawn_blocking(move || studio.run_ids())
                .await
                .unwrap_or_default();
            taken.extend(ids);
        }
        Ok(taken)
    }

    /// Resolve a cloud start into a run: read its draft (refused unless it says `cloud` or
    /// `any`), choose its id and wrap its prompt.
    pub async fn resolve_cloud(&self, req: CloudStart) -> Result<CloudRun, String> {
        let rt = self.runtime()?;
        let r = match &req.kind {
            CloudKind::Free(spec) => Resolved {
                base_id: format!("s-{}", spec.branch_slug),
                item_text: None,
                repo: spec.repo.clone(),
                prompt: spec.prompt.clone(),
                model: spec.model.clone(),
                asked_merge: false,
                strand: None,
            },
            CloudKind::Item {
                strand,
                item_id,
                repo,
            } => {
                let notes = self.notes()?;
                let (strand, item_id, repo) = (strand.clone(), item_id.clone(), repo.clone());
                tokio::task::spawn_blocking(move || -> Result<Resolved, String> {
                    let it = crate::hostrun::studio_item(
                        &notes,
                        &strand,
                        &item_id,
                        crate::hostrun::OptIn::Request {
                            repo: repo.as_deref(),
                        },
                    )?;
                    let src = std::fs::read_to_string(notes.join(&it.prompt_path))
                        .map_err(|e| format!("{} is unreadable: {e}", it.prompt_path))?;
                    check_cloud_capable(&it.prompt_path, &src)?;
                    Ok(Resolved {
                        base_id: it.item_id.clone(),
                        item_text: Some(it.item_text.clone()),
                        repo: it.repo.clone(),
                        prompt: it.section.clone(),
                        model: it.model.clone(),
                        asked_merge: draft_merge(&src),
                        strand: Some(StrandRef {
                            note: it.strand.clone(),
                            id: it.item_id.clone(),
                            prompt: it.prompt_path.clone(),
                        }),
                    })
                })
                .await
                .map_err(|e| format!("the resolve task failed: {e}"))??
            }
            CloudKind::Draft { draft, repo } => {
                let notes = self.notes()?;
                let (draft, repo) = (draft.clone(), repo.clone());
                tokio::task::spawn_blocking(move || -> Result<Resolved, String> {
                    let d = crate::hostrun::studio_draft(&notes, &draft, repo.as_deref())?;
                    let src = std::fs::read_to_string(notes.join(&d.prompt_path))
                        .map_err(|e| format!("{} is unreadable: {e}", d.prompt_path))?;
                    check_cloud_capable(&d.prompt_path, &src)?;
                    Ok(Resolved {
                        base_id: format!("s-{}", d.slug),
                        item_text: None,
                        repo: d.repo.clone(),
                        prompt: d.section.clone(),
                        model: d.model.clone(),
                        asked_merge: draft_merge(&src),
                        strand: None,
                    })
                })
                .await
                .map_err(|e| format!("the resolve task failed: {e}"))??
            }
        };
        let taken = self.run_id_taken().await?;
        let run_id = unique_run_id(&r.base_id, |id| taken.contains(id));
        self.finish_resolve(rt, req, r, run_id)
    }

    fn finish_resolve(
        &self,
        rt: &CloudRuntime,
        req: CloudStart,
        r: Resolved,
        run_id: String,
    ) -> Result<CloudRun, String> {
        if super::is_local_repo(&r.repo) {
            return Err(super::LOCAL_ONLY_STUDIO.into());
        }
        if !valid_run_id(&run_id) {
            return Err(format!("{run_id:?} cannot name a run"));
        }
        let merge = merge_decision(
            &r.repo,
            req.merge.unwrap_or(r.asked_merge),
            &rt.settings.merge_deny,
        );
        let slug = match &req.kind {
            CloudKind::Free(spec) => spec.branch_slug.clone(),
            _ => cloud_slug(&run_id, r.item_text.as_deref()),
        };
        let branch = format!("jesse/{slug}");
        let prompt = cloud_prompt(&r.prompt, &run_id, &r.repo, &branch, &merge);
        let requested_timeout = req.timeout_minutes;
        let mut spec = match req.kind {
            CloudKind::Free(spec) => spec,
            _ => DevJobSpec::validate(
                DevJobArgs {
                    repo: Some(r.repo.clone()),
                    prompt: Some(r.prompt.clone()),
                    agent: req.agent.clone(),
                    model: r.model.clone(),
                    branch_slug: Some(slug),
                    test_command: req.test_command.clone(),
                    timeout_minutes: req.timeout_minutes,
                    ..DevJobArgs::default()
                },
                self.default_agent,
            )?,
        };
        spec.prompt = prompt;
        // The cluster's cap ([`super::MAX_TIMEOUT_MINUTES`]), already applied above (`Free`
        // directly, the other two kinds through `DevJobSpec::validate`), does not apply in the
        // cloud: its own, usually higher, cap does.
        spec.timeout_minutes =
            cloud_capped_timeout_minutes(requested_timeout, rt.settings.max_timeout_minutes);
        let timeout_note =
            super::timeout_reduced_note(requested_timeout, spec.timeout_minutes, "the cloud");
        Ok(CloudRun {
            spec,
            run_id,
            merge,
            strand: r.strand,
            timeout_note,
        })
    }

    /// Create a resolved cloud run's ConfigMap and Job, suspended until a worker is idle.
    pub async fn create_cloud_run(&self, run: &CloudRun, origin: &str) -> Result<Started, String> {
        let rt = self.runtime()?;
        let extras = CloudJobExtras {
            run_id: run.run_id.clone(),
            merge: run.merge.clone(),
            merge_deny: rt.settings.merge_deny_env(),
            resources: rt.settings.job_resources(),
        };
        self.create_run(&run.spec, origin, run.strand.as_ref(), Some(&extras))
            .await
    }

    /// `devjob_start` with `host: "cloud"`: resolve, create, answer.
    pub async fn start_cloud(&self, req: CloudStart, origin: &str) -> Result<Value, String> {
        let run = self.resolve_cloud(req).await?;
        let started = self.create_cloud_run(&run, origin).await?;
        Ok(json!({
            "run_id": run.run_id,
            "host": "cloud",
            "job_name": started.job_name,
            "queued": true,
            "waiting_for": self.runtime()?.queued_reason().trim_start_matches("queued: "),
            "branch": format!("jesse/{}", started.branch_slug),
            "merge": run.merge.annotation(),
            "timeout_minutes": started.timeout_minutes,
            "timeout_note": run.timeout_note,
        }))
    }

    /// The labelled Job whose name, or whose run id, is `key`.
    pub async fn find_cloud_job(&self, key: &str) -> Result<Option<JobView>, String> {
        let views = self.views().await?;
        let mut hits: Vec<JobView> = views
            .into_iter()
            .map(|(_, v)| v)
            .filter(|v| v.name == key || v.run_id.as_deref() == Some(key))
            .collect();
        hits.sort_by_key(|v| std::cmp::Reverse(v.created_ms));
        Ok(hits.into_iter().next())
    }

    /// `devjob_cancel` of a cloud run: a queued Job and its ConfigMap are deleted (nothing ran,
    /// nothing is reported); a released one is marked cancelled and ended through its deadline,
    /// and the watcher reports it from what it has.
    pub async fn cancel_cloud(&self, key: &str) -> Result<Value, String> {
        let Some(v) = self.find_cloud_job(key).await? else {
            return Err(format!("no cloud run is named {key:?}"));
        };
        if v.finished.is_some() {
            return Err(format!("{} has already finished", v.name));
        }
        let _gate = self.gate.lock().await;
        if v.suspended {
            let dp = DeleteParams {
                propagation_policy: Some(PropagationPolicy::Background),
                ..DeleteParams::default()
            };
            self.jobs()
                .delete(&v.name, &dp)
                .await
                .map_err(|e| format!("could not delete the Job {}: {e}", v.name))?;
            if let Some(cm) = &v.config_map {
                let _ = self
                    .config_maps()
                    .delete(cm, &DeleteParams::default())
                    .await;
            }
            eprintln!("jesse-bridge: devjob CANCEL {} (queued, deleted)", v.name);
            self.wake.notify_one();
            return Ok(
                json!({ "run_id": v.run_id, "job_name": v.name, "host": "cloud", "status": "cancelled", "reported": false }),
            );
        }
        let patch = json!({
            "metadata": { "annotations": { ANN_CANCELLED: "true" } },
            "spec": { "activeDeadlineSeconds": 1 },
        });
        self.jobs()
            .patch(&v.name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| format!("could not stop the Job {}: {e}", v.name))?;
        eprintln!("jesse-bridge: devjob CANCEL {} (running, stopped)", v.name);
        self.wake.notify_one();
        Ok(
            json!({ "run_id": v.run_id, "job_name": v.name, "host": "cloud", "status": "cancelling", "reported": "when the watcher next passes" }),
        )
    }

    /// Take queued Studio runs whose draft says `devjob-host: any` while a cloud slot is free and
    /// a worker can be had. A run whose Job cannot be made goes back to the Studio's queue.
    async fn drain_studio(&self, rt: &CloudRuntime, idle_ready: usize) {
        let Some(studio) = self.studio.get().cloned() else {
            return;
        };
        let Ok(notes) = self.notes() else {
            return;
        };
        loop {
            let views: Vec<JobView> = match self.views().await {
                Ok(v) => v.into_iter().map(|(_, v)| v).collect(),
                Err(_) => return,
            };
            if !cloud_slot_free(&views, rt.settings.max_concurrent) {
                return;
            }
            // An idle worker not already promised to a queued cloud run, or room for one more.
            let waiting = views.iter().filter(|v| v.is_queued()).count();
            if idle_ready.saturating_sub(waiting) == 0 && !rt.can_add_worker() {
                return;
            }
            let (s, n) = (studio.clone(), notes.clone());
            let Some(rec) = tokio::task::spawn_blocking(move || s.take_for_cloud(&n))
                .await
                .ok()
                .flatten()
            else {
                return;
            };
            let id = rec.run_id.clone();
            match self.moved_run(rt, &notes, &rec).await {
                Ok(run) => match self.create_cloud_run(&run, "studio-queue").await {
                    Ok(started) => {
                        let s = studio.clone();
                        let job = started.job_name.clone();
                        let _ = tokio::task::spawn_blocking(move || s.note_moved(&id, &job)).await;
                        eprintln!(
                            "jesse-bridge: devjob cloud took Studio run {} as {}",
                            run.run_id, started.job_name
                        );
                    }
                    Err(e) => {
                        let s = studio.clone();
                        let _ =
                            tokio::task::spawn_blocking(move || s.return_from_cloud(&id, &e)).await;
                        return;
                    }
                },
                Err(e) => {
                    let s = studio.clone();
                    let _ = tokio::task::spawn_blocking(move || s.return_from_cloud(&id, &e)).await;
                    return;
                }
            }
        }
    }

    /// A Studio run the cloud took, as a cloud run with the same id.
    async fn moved_run(
        &self,
        rt: &CloudRuntime,
        notes: &Path,
        rec: &crate::hostrun::RunRecord,
    ) -> Result<CloudRun, String> {
        let path = rec
            .prompt_path
            .clone()
            .ok_or_else(|| format!("{} has no draft", rec.run_id))?;
        let src = std::fs::read_to_string(notes.join(&path))
            .map_err(|e| format!("{path} is unreadable: {e}"))?;
        let prompt = crate::hostrun::prompt_section(&src)
            .ok_or_else(|| format!("{path} has no ## PROMPT section"))?;
        let strand = match (&rec.strand, &rec.item_id) {
            (Some(note), Some(id)) => Some(StrandRef {
                note: note.clone(),
                id: id.clone(),
                prompt: path.clone(),
            }),
            _ => None,
        };
        let r = Resolved {
            base_id: rec.run_id.clone(),
            item_text: rec.item_text.clone(),
            repo: rec.repo.clone(),
            prompt,
            model: rec.model.clone(),
            asked_merge: draft_merge(&src),
            strand,
        };
        let req = CloudStart {
            kind: CloudKind::Draft {
                draft: path,
                repo: None,
            },
            merge: None,
            agent: None,
            test_command: None,
            timeout_minutes: None,
        };
        self.finish_resolve(rt, req, r, rec.run_id.clone())
    }

    /// The dev job workers, as a pass sees them: the nodes in the pool, whether a dev job pod is
    /// on each, how long each has been idle, and its expiry from kc-burst.
    async fn pool(&self, rt: &CloudRuntime) -> Result<(Vec<PoolWorker>, usize), String> {
        let nodes: Api<Node> = Api::all(self.client.clone());
        let list = nodes
            .list(&ListParams::default().labels(&format!("{POOL_LABEL}={POOL_VALUE}")))
            .await
            .map_err(|e| format!("could not list the dev job workers: {e}"))?;
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &self.settings.namespace);
        let pods = pods
            .list(&ListParams::default())
            .await
            .map_err(|e| format!("could not list the dev job pods: {e}"))?;
        let live = |p: &&Pod| {
            !matches!(
                p.status.as_ref().and_then(|s| s.phase.as_deref()),
                Some("Succeeded") | Some("Failed")
            )
        };
        let busy: HashSet<String> = pods
            .items
            .iter()
            .filter(live)
            .filter_map(|p| p.spec.as_ref().and_then(|s| s.node_name.clone()))
            .collect();
        // Released Jobs whose pod is not on a node yet: each will take an idle worker.
        let placed_jobs: HashSet<String> = pods
            .items
            .iter()
            .filter(live)
            .filter(|p| p.spec.as_ref().and_then(|s| s.node_name.as_ref()).is_some())
            .filter_map(|p| {
                p.metadata
                    .labels
                    .as_ref()
                    .and_then(|l| l.get(super::POD_JOB_LABEL).cloned())
            })
            .collect();
        let unplaced = self
            .views()
            .await?
            .into_iter()
            .filter(|(_, v)| v.is_active() && !placed_jobs.contains(&v.name))
            .count();
        // Expiries matter for a busy worker (to extend it) and an age for an idle ready one (to
        // place a Job onto it), so kc-burst (a provider API call) is asked only while there is a
        // worker at all.
        let (expiries, ages): (HashMap<String, Option<u64>>, HashMap<String, Option<u64>>) =
            if list.items.is_empty() {
                (HashMap::new(), HashMap::new())
            } else {
                match rt.burst.status().await {
                    Ok(ws) => (
                        ws.iter().map(|w| (w.name.clone(), w.expires)).collect(),
                        ws.into_iter().map(|w| (w.name, w.age_seconds)).collect(),
                    ),
                    Err(e) => {
                        eprintln!("jesse-bridge: devjob cloud: {e}");
                        (HashMap::new(), HashMap::new())
                    }
                }
            };
        let now = crate::system_time_to_ms(std::time::SystemTime::now()) / 1000;
        let seen: Vec<(String, bool)> = list
            .items
            .iter()
            .filter_map(|n| n.metadata.name.clone())
            .map(|n| {
                let b = busy.contains(&n);
                (n, b)
            })
            .collect();
        let idle = lock(&rt.idle).observe(&seen);
        let workers = list
            .items
            .iter()
            .filter_map(|n| {
                let name = n.metadata.name.clone()?;
                let run = n.metadata.labels.as_ref()?.get(RUN_LABEL)?.clone();
                let ready = n
                    .status
                    .as_ref()
                    .and_then(|s| s.conditions.as_ref())
                    .is_some_and(|cs| cs.iter().any(|c| c.type_ == "Ready" && c.status == "True"));
                let unschedulable = n
                    .spec
                    .as_ref()
                    .and_then(|s| s.unschedulable)
                    .unwrap_or(false);
                Some(PoolWorker {
                    busy: busy.contains(&name),
                    idle_for: idle.get(&name).copied().unwrap_or_default(),
                    expires_in: expiries
                        .get(&name)
                        .copied()
                        .flatten()
                        .map(|e| Duration::from_secs(e.saturating_sub(now))),
                    age: ages.get(&name).copied().flatten().map(Duration::from_secs),
                    ready: ready && !unschedulable,
                    name,
                    run,
                })
            })
            .collect();
        Ok((workers, unplaced))
    }

    /// One cloud pass, after the reports: take what the Studio's queue offers, plan the workers,
    /// and release queued Jobs onto idle ones.
    pub(super) async fn cloud_pass(&self, rt: &CloudRuntime) {
        let (workers, _) = match self.pool(rt).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("jesse-bridge: devjob cloud: {e}");
                return;
            }
        };
        let idle_ready = workers.iter().filter(|w| w.ready && !w.busy).count();
        self.drain_studio(rt, idle_ready).await;
        let _gate = self.gate.lock().await;
        let (workers, unplaced) = match self.pool(rt).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("jesse-bridge: devjob cloud: {e}");
                return;
            }
        };
        let views: Vec<JobView> = match self.views().await {
            Ok(v) => v.into_iter().map(|(_, v)| v).collect(),
            Err(e) => {
                eprintln!("jesse-bridge: devjob cloud: {e}");
                return;
            }
        };
        let demand = views
            .iter()
            .filter(|v| v.is_active() || v.is_queued())
            .count();
        let plan = plan_burst(
            &workers,
            rt.starting.load(Ordering::SeqCst),
            demand,
            rt.settings.max_concurrent,
            Duration::from_secs(rt.settings.idle_minutes * 60),
        );
        self.carry_out(rt, &plan).await;
        let idle_ready: Vec<PoolWorker> = workers
            .iter()
            .filter(|w| w.ready && !w.busy)
            .cloned()
            .collect();
        for name in cloud_resumable(
            &views,
            rt.settings.max_concurrent,
            &idle_ready,
            unplaced,
            rt.settings.worker_life_minutes,
        ) {
            let patch = json!({ "spec": { "suspend": false } });
            match self
                .jobs()
                .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
                .await
            {
                Ok(_) => eprintln!("jesse-bridge: devjob RESUME {name} (cloud)"),
                Err(e) => eprintln!("jesse-bridge: devjob {name}: could not resume: {e}"),
            }
        }
    }

    /// Start, stop and extend workers as `plan` says, under the daily cap. `up` and `down` take
    /// minutes, so each runs as a task of its own and wakes the watcher when it is done.
    async fn carry_out(&self, rt: &CloudRuntime, plan: &BurstPlan) {
        let mut blocked = None;
        for _ in 0..plan.up {
            if let Some(e) = rt.up_backoff() {
                blocked = Some(format!(
                    "the last kc-burst up failed ({e}); retrying within 5 minutes"
                ));
                break;
            }
            let cost = rt.settings.up_cost();
            match rt.commit(cost) {
                Ok(true) => {}
                Ok(false) => {
                    blocked = Some(format!(
                        "the daily cost cap ({:.2} EUR, [devjob.cloud] daily_cost_cap_eur) is reached for today",
                        rt.settings.daily_cost_cap_eur
                    ));
                    break;
                }
                Err(e) => {
                    blocked = Some(format!("the cost ledger could not be written: {e}"));
                    break;
                }
            }
            rt.starting.fetch_add(1, Ordering::SeqCst);
            let burst = rt.burst.clone();
            let starting = rt.starting.clone();
            let last = rt.last_up_error.clone();
            let ledger = rt.ledger.clone();
            let settings = rt.settings.clone();
            let me = self.self_wake();
            tokio::spawn(async move {
                match burst
                    .up(&settings.burst_size, settings.burst_ttl_minutes)
                    .await
                {
                    Ok(run) => eprintln!("jesse-bridge: devjob cloud: worker run {run} is up"),
                    Err(e) => {
                        eprintln!("jesse-bridge: devjob cloud: {e}");
                        refund_in(&ledger, &settings, cost);
                        *lock(&last) = Some((tokio::time::Instant::now(), e));
                    }
                }
                starting.fetch_sub(1, Ordering::SeqCst);
                me.notify_one();
            });
        }
        if blocked.is_none() && rt.starting.load(Ordering::SeqCst) > 0 {
            blocked = Some("a dev job burst worker is starting (a few minutes)".into());
        }
        *lock(&rt.blocked) = blocked;
        for run in &plan.down {
            let burst = rt.burst.clone();
            let run = run.clone();
            let me = self.self_wake();
            tokio::spawn(async move {
                match burst.down(&run).await {
                    Ok(()) => {
                        eprintln!("jesse-bridge: devjob cloud: worker run {run} stopped (idle)")
                    }
                    Err(e) => eprintln!("jesse-bridge: devjob cloud: {e}"),
                }
                me.notify_one();
            });
        }
        for run in &plan.extend {
            match rt.commit(rt.settings.extend_cost()) {
                Ok(true) => match rt.burst.extend(run, EXTEND_BY).await {
                    Ok(()) => eprintln!("jesse-bridge: devjob cloud: worker run {run} extended"),
                    Err(e) => {
                        eprintln!("jesse-bridge: devjob cloud: {e}");
                        refund_in(&rt.ledger, &rt.settings, rt.settings.extend_cost());
                    }
                },
                Ok(false) => eprintln!(
                    "jesse-bridge: devjob cloud: worker run {run} not extended: the daily cost cap is reached"
                ),
                Err(e) => eprintln!("jesse-bridge: devjob cloud: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests;
