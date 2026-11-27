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
use kube::api::{DeleteParams, ListParams, LogParams, Patch, PatchParams, PropagationPolicy};
use kube::Api;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    valid_label_value, ChainExtras, ChainInfo, ChainLink, CloudJobExtras, CloudKind, CloudStart,
    DevJobArgs, DevJobSettings, DevJobSlot, DevJobSpec, DevJobs, JobView, Started, StrandRef,
};

// ---- DEFAULTS ------------------------------------------------------------------------

/// The default `[devjob.cloud] namespace` on kc1.
pub const DEFAULT_NAMESPACE: &str = "jesse-dev";
/// The default `[devjob.cloud] kubeconfig`: the `jesse:devjob` ServiceAccount's token.
pub const DEFAULT_KUBECONFIG: &str = ".kube/kc1-devjob.yaml";
/// The default `[devjob.cloud] kc_burst_config`: kc-burst's own config, read once at boot for
/// its `max_ttl` ([`read_kc_burst_max_ttl`]). Home relative, expanded like [`DEFAULT_KUBECONFIG`].
pub const DEFAULT_KC_BURST_CONFIG: &str = ".config/kc-burst/config.toml";
/// The default `[devjob.cloud] max_concurrent`: the namespace quota's two pods.
pub const DEFAULT_MAX_CONCURRENT: usize = 2;
/// Where `kc-burst` is installed on the Studio.
pub const DEFAULT_KC_BURST: &str = "/usr/local/bin/kc-burst";
/// DigitalOcean's 4 vCPU, 16 GB general purpose worker: one dev job per worker, with room for
/// a Rust workspace the size of trovato to build and test ([`DEFAULT_JOB_MEMORY_GI`]).
pub const DEFAULT_BURST_SIZE: &str = "g-4vcpu-16gb";
/// A worker's first TTL. Extended while it is busy, at most [`MAX_WORKER_LIFE_MINUTES`].
pub const DEFAULT_BURST_TTL_MINUTES: u64 = 240;
/// The longest a worker lives, extensions included: the burst system's own hard ceiling —
/// kc-burst's `max_ttl` and the reaper's `--max-age`, both 12 hours once a companion change in
/// `jeremyandrews/k3s` raises them from 8. [`CloudSettings::problems`] refuses a configured
/// `worker_life_minutes` above this; [`CloudSettings::effective_worker_life_minutes`] caps it
/// further, live, at whatever kc-burst's own config actually says.
pub const MAX_WORKER_LIFE_MINUTES: u64 = 720;
/// The default `[devjob.cloud] job_memory_gi`: the pod's memory limit, sized for a 16 GB worker
/// less what the node itself keeps.
pub const DEFAULT_JOB_MEMORY_GI: u32 = 14;
/// The default `[devjob.cloud] job_memory_request_gi`.
pub const DEFAULT_JOB_MEMORY_REQUEST_GI: u32 = 12;
/// The default `[devjob.cloud] job_cpu`: the pod's CPU limit. `CARGO_BUILD_JOBS` is the
/// agent's share of it ([`AgentResources::build_jobs`]).
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

// ---- THE SERVICES BESIDE THE AGENT ---------------------------------------------------

/// A service sidecar's fixed requests and limits. Taken out of the agent's share, so the pod's
/// total stays exactly what `[devjob.cloud]` configures and the namespace quota needs no edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidecarResources {
    pub memory_request_mi: u64,
    pub memory_limit_mi: u64,
    /// Request and limit alike.
    pub cpu_milli: u64,
}

/// `postgres:16`: trovato's integration tests on a small database.
pub const POSTGRES_SIDECAR: SidecarResources = SidecarResources {
    memory_request_mi: 512,
    memory_limit_mi: 1024,
    cpu_milli: 500,
};
/// `redis:7`, with no persistence.
pub const REDIS_SIDECAR: SidecarResources = SidecarResources {
    memory_request_mi: 128,
    memory_limit_mi: 256,
    cpu_milli: 250,
};
/// The sidecars' images, pinned by tag: the versions trovato's CI runs.
pub const POSTGRES_IMAGE: &str = "postgres:16";
pub const REDIS_IMAGE: &str = "redis:7";
/// The uid and gid both images run their server as (`postgres` and `redis`, 999 in each).
pub const SERVICE_UID: u64 = 999;
/// The agent's environment, exactly as trovato's CI sets it.
pub const DATABASE_URL: &str = "postgres://trovato:trovato@localhost:5432/trovato_test";
pub const REDIS_URL: &str = "redis://127.0.0.1:6379";
/// The least the agent may keep once the sidecars have theirs: below this the bridge refuses
/// to boot rather than start runs that cannot build.
pub const AGENT_MIN_MEMORY_MI: u64 = 2048;
pub const AGENT_MIN_CPU_MILLI: u64 = 1000;

/// The agent container's own requests and limits, in MiB and millicores. `u64`, so every
/// `u32` the config accepts converts exactly: `u32::MAX` GiB is a valid, if absurd, limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentResources {
    pub memory_request_mi: u64,
    pub memory_limit_mi: u64,
    pub cpu_request_milli: u64,
    pub cpu_limit_milli: u64,
}

impl AgentResources {
    /// `CARGO_BUILD_JOBS`: the agent's CPU limit, rounded down, at least 1.
    pub fn build_jobs(&self) -> u64 {
        (self.cpu_limit_milli / 1000).max(1)
    }
}

/// A memory quantity as Kubernetes reads it: whole GiB as `NGi`, else `NMi`.
pub fn memory_quantity(mi: u64) -> String {
    if mi.is_multiple_of(1024) {
        format!("{}Gi", mi / 1024)
    } else {
        format!("{mi}Mi")
    }
}

/// A CPU quantity: whole CPUs as `N`, else `Nm`.
pub fn cpu_quantity(milli: u64) -> String {
    if milli.is_multiple_of(1000) {
        (milli / 1000).to_string()
    } else {
        format!("{milli}m")
    }
}

impl JobResources {
    /// What the agent container keeps: the pod's configured totals, less the sidecars' when
    /// `services` is on. Saturates at zero; [`CloudSettings::resource_problems`] refuses a size
    /// that would get there.
    pub fn agent(&self, services: bool) -> AgentResources {
        let (mem_req, mem_lim, cpu) = if services {
            let s = [POSTGRES_SIDECAR, REDIS_SIDECAR];
            (
                s.iter().map(|x| x.memory_request_mi).sum(),
                s.iter().map(|x| x.memory_limit_mi).sum(),
                s.iter().map(|x| x.cpu_milli).sum(),
            )
        } else {
            (0, 0, 0)
        };
        AgentResources {
            memory_request_mi: (u64::from(self.memory_request_gi) * 1024).saturating_sub(mem_req),
            memory_limit_mi: (u64::from(self.memory_gi) * 1024).saturating_sub(mem_lim),
            cpu_request_milli: (u64::from(self.cpu_request) * 1000).saturating_sub(cpu),
            cpu_limit_milli: (u64::from(self.cpu) * 1000).saturating_sub(cpu),
        }
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
/// [`BOOT_ALLOWANCE_MINUTES`] exactly fits [`DEFAULT_WORKER_LIFE_MINUTES`]: 720 - 15 - 120 - 30.
pub const DEFAULT_MAX_TIMEOUT_MINUTES: u64 = 555;
/// The default `[devjob.cloud] worker_life_minutes`, and [`MAX_WORKER_LIFE_MINUTES`]: a kc1
/// burst worker may live a full 12 hours once the burst system's own limits are raised to match
/// (a companion change in `jeremyandrews/k3s`). [`CloudSettings::problems`] refuses more; a
/// shorter live `max_ttl` is enforced instead by
/// [`CloudSettings::effective_worker_life_minutes`].
pub const DEFAULT_WORKER_LIFE_MINUTES: u64 = 720;
/// Time from `kc-burst up` to a worker being ready and idle, before any Job can resume onto it
/// and start its own `activeDeadlineSeconds` clock. [`CloudSettings::problems`]'s margin.
pub const BOOT_ALLOWANCE_MINUTES: u64 = 15;
/// The pod's `terminationGracePeriodSeconds`: the entrypoint's time, between the deadline's
/// SIGTERM and the SIGKILL, to push its SIGTERM checkpoint to `jesse/<slug>-wip` and print its
/// report. Kubernetes' own default (30s) is not enough; the k3s Job template uses this value.
pub const CHECKPOINT_GRACE_SECS: u64 = 120;
/// The default `[devjob.cloud] max_continuations`: how many times a capped run may be continued
/// as `<run_id>-c2`, `-c3`, and so on before the chain stops on its own. `0` turns continuations off.
pub const DEFAULT_MAX_CONTINUATIONS: u32 = 3;
/// The most `max_continuations` may be set to ([`CloudSettings::problems`]).
pub const MAX_MAX_CONTINUATIONS: u32 = 10;

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
/// On a reviewed run's cloud Job: `hold` or `answer`, its `REVIEW` ([`ReviewMode`]).
pub const ANN_REVIEW: &str = "jesse.dev/review";
/// On a cloud Job a person cancelled.
pub const ANN_CANCELLED: &str = "jesse.dev/cancelled";
/// The draft key that asks a cloud run to merge at green.
pub const MERGE_KEY: &str = "devjob-merge";
/// Where the agent may write the pull request's title, inside the Job.
pub const PR_TITLE_FILE: &str = "/home/agent/work/pr-title.txt";
/// Where the agent writes its report, inside the Job.
pub const RUN_REPORT_FILE: &str = "/home/agent/work/report.md";
/// On a continuation Job only: the chain's first run's id. Absent on link 0 ([`ChainInfo::of`]).
pub const ANN_CHAIN_ROOT: &str = "jesse.dev/chain-root";
/// On a continuation Job only: its own link number (link 0 is the first run).
pub const ANN_CONTINUATION: &str = "jesse.dev/continuation";
/// On a continuation Job only: the previous link's run id.
pub const ANN_CONTINUATION_OF: &str = "jesse.dev/continuation-of";
/// On a continuation Job only: every earlier link, as JSON ([`ChainLink`]).
pub const ANN_CHAIN: &str = "jesse.dev/chain";
/// On a finished cloud Job that continued: the new run's id.
pub const ANN_CONTINUED_AS: &str = "jesse.dev/continued-as";

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
    /// Postgres 16 and Redis 7 beside the agent; default on.
    pub services: Option<bool>,
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
    /// How many times a capped run may continue as `<run_id>-c2`, `-c3`, and so on. `0` turns
    /// continuations off.
    pub max_continuations: Option<u32>,
    /// Where kc-burst's own config lives, for its `max_ttl` ([`read_kc_burst_max_ttl`]).
    pub kc_burst_config: Option<PathBuf>,
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
    /// Postgres 16 and Redis 7 run as sidecars in every cloud pod, their sizes taken out of the
    /// agent's ([`JobResources::agent`]). Off: the single container pod of 0.222.0.
    pub services: bool,
    /// [`DEFAULT_MERGE_DENY`] and then the config's own.
    pub merge_deny: Vec<String>,
    pub ledger: PathBuf,
    pub max_timeout_minutes: u64,
    pub worker_life_minutes: u64,
    pub max_continuations: u32,
    /// Where kc-burst's own config lives: its `max_ttl` is read from here, once, into
    /// [`Self::kc_burst_max_ttl_minutes`]. The bridge never opens it for anything else.
    pub kc_burst_config: PathBuf,
    /// kc-burst's own `max_ttl`, in minutes, read from [`Self::kc_burst_config`] once at boot
    /// ([`read_kc_burst_max_ttl`]). `None` until that read runs — so [`CloudSettings::default`]
    /// and every settings built straight from TOML carry the configured values alone — or when
    /// the file could not be read or parsed.
    pub kc_burst_max_ttl_minutes: Option<u64>,
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
            services: t.services.unwrap_or(true),
            merge_deny,
            ledger: match t.ledger {
                Some(p) => expand(&p, home),
                None => Path::new(home).join(DEFAULT_LEDGER),
            },
            max_timeout_minutes: t.max_timeout_minutes.unwrap_or(DEFAULT_MAX_TIMEOUT_MINUTES),
            worker_life_minutes: t.worker_life_minutes.unwrap_or(DEFAULT_WORKER_LIFE_MINUTES),
            max_continuations: t.max_continuations.unwrap_or(DEFAULT_MAX_CONTINUATIONS),
            kc_burst_config: match t.kc_burst_config {
                Some(p) => expand(&p, home),
                None => Path::new(home).join(DEFAULT_KC_BURST_CONFIG),
            },
            kc_burst_max_ttl_minutes: None,
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
        if self.burst_ttl_minutes < 30 || self.burst_ttl_minutes > self.worker_life_minutes {
            out.push(format!(
                "[devjob.cloud] burst_ttl_minutes must be 30 to worker_life_minutes ({}), not {}",
                self.worker_life_minutes, self.burst_ttl_minutes
            ));
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
        if self.max_continuations > MAX_MAX_CONTINUATIONS {
            out.push(format!(
                "[devjob.cloud] max_continuations must be at most {MAX_MAX_CONTINUATIONS}"
            ));
        }
        if self.worker_life_minutes > MAX_WORKER_LIFE_MINUTES {
            out.push(format!(
                "[devjob.cloud] worker_life_minutes must be {MAX_WORKER_LIFE_MINUTES} or less: \
                 raise the burst system's own limits first (kc-burst's max_ttl and the \
                 reaper's --max-age)"
            ));
        }
        let min_worker_life =
            BOOT_ALLOWANCE_MINUTES + FINISH_ALLOWANCE_SECS / 60 + MERGE_WAITS_MINUTES + 1;
        if self.worker_life_minutes < min_worker_life {
            out.push(format!(
                "[devjob.cloud] worker_life_minutes must be at least {min_worker_life} \
                 minutes: a {BOOT_ALLOWANCE_MINUTES} minute boot allowance + the finish \
                 allowance ({}) + the merge waits ({MERGE_WAITS_MINUTES}) + 1 minute left for \
                 the agent",
                FINISH_ALLOWANCE_SECS / 60
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
                 {MAX_WORKER_LIFE_MINUTES}, kc-burst's max_ttl)",
                self.max_timeout_minutes,
                FINISH_ALLOWANCE_SECS / 60,
                self.worker_life_minutes
            ));
        }
        out
    }

    /// Why the Job's resources or time cap are refused: each at least 1, each request at most
    /// its limit, a request the worker can hold, and with `services` on, at least
    /// [`AGENT_MIN_MEMORY_MI`] and [`AGENT_MIN_CPU_MILLI`] left to the agent.
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
        if self.services {
            let a = self.job_resources().agent(true);
            if a.memory_request_mi < AGENT_MIN_MEMORY_MI || a.memory_limit_mi < AGENT_MIN_MEMORY_MI
            {
                out.push(format!(
                    "[devjob.cloud] job_memory_request_gi {} / job_memory_gi {} leave the agent \
                     {} / {} once Postgres and Redis have theirs, less than {}; raise them or \
                     set services = false",
                    self.job_memory_request_gi,
                    self.job_memory_gi,
                    memory_quantity(a.memory_request_mi),
                    memory_quantity(a.memory_limit_mi),
                    memory_quantity(AGENT_MIN_MEMORY_MI),
                ));
            }
            if a.cpu_request_milli < AGENT_MIN_CPU_MILLI || a.cpu_limit_milli < AGENT_MIN_CPU_MILLI
            {
                out.push(format!(
                    "[devjob.cloud] job_cpu_request {} / job_cpu {} leave the agent {} / {} CPU \
                     once Postgres and Redis have theirs, less than {}; raise them or set \
                     services = false",
                    self.job_cpu_request,
                    self.job_cpu,
                    cpu_quantity(a.cpu_request_milli),
                    cpu_quantity(a.cpu_limit_milli),
                    cpu_quantity(AGENT_MIN_CPU_MILLI),
                ));
            }
            // The sidecars' memory requests sit below their limits, so the agent's request can
            // pass its limit when the configured request equals the configured limit.
            if a.memory_request_mi > a.memory_limit_mi {
                out.push(format!(
                    "[devjob.cloud] job_memory_request_gi {} must be below job_memory_gi {} \
                     with services on: the agent would request {} under a {} limit",
                    self.job_memory_request_gi,
                    self.job_memory_gi,
                    memory_quantity(a.memory_request_mi),
                    memory_quantity(a.memory_limit_mi),
                ));
            }
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

    /// [`Self::worker_life_minutes`], capped at kc-burst's own `max_ttl`
    /// ([`Self::kc_burst_max_ttl_minutes`]) when that was read: the bridge never promises a
    /// worker more life than the burst system itself will actually give it.
    pub fn effective_worker_life_minutes(&self) -> u64 {
        match self.kc_burst_max_ttl_minutes {
            Some(max_ttl) => self.worker_life_minutes.min(max_ttl),
            None => self.worker_life_minutes,
        }
    }

    /// [`Self::max_timeout_minutes`], capped at what [`Self::effective_worker_life_minutes`]
    /// leaves once the boot allowance, the finish allowance and the merge waits are taken out
    /// (saturating, at least 1 minute) — the same fit [`Self::problems`] checks at boot, applied
    /// live against whatever kc-burst's own `max_ttl` actually allows.
    pub fn effective_max_timeout_minutes(&self) -> u64 {
        let ceiling = self
            .effective_worker_life_minutes()
            .saturating_sub(BOOT_ALLOWANCE_MINUTES)
            .saturating_sub(MERGE_WAITS_MINUTES)
            .saturating_sub(FINISH_ALLOWANCE_SECS / 60)
            .max(1);
        self.max_timeout_minutes.min(ceiling)
    }

    /// One log line, when kc-burst's own `max_ttl` pulls either effective value below what is
    /// configured: for example, when the live config still says 8 hours while
    /// `worker_life_minutes` has been raised to [`DEFAULT_WORKER_LIFE_MINUTES`]. Meant to run
    /// once, at boot, after [`Self::kc_burst_max_ttl_minutes`] has been read.
    pub fn log_effective_caps(&self) {
        let life = self.effective_worker_life_minutes();
        let timeout = self.effective_max_timeout_minutes();
        if life < self.worker_life_minutes || timeout < self.max_timeout_minutes {
            eprintln!(
                "jesse-bridge: devjob cloud: kc-burst max_ttl is {} min (from {}), below \
                 worker_life_minutes {}: workers live {} min and agents get at most {} min",
                self.kc_burst_max_ttl_minutes.unwrap_or_default(),
                self.kc_burst_config.display(),
                self.worker_life_minutes,
                life,
                timeout,
            );
        }
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

/// The cloud Job's `activeDeadlineSeconds`, and what `build_cloud_job` also tells the entrypoint
/// as `JOB_DEADLINE_SECONDS` so it never starts a wait Kubernetes cannot let finish: the agent's
/// own timeout, [`FINISH_ALLOWANCE_SECS`] for what the entrypoint does after the agent exits
/// (rebase onto `origin/main`, the test command, push, open the pull request), and the merge
/// path's full wait. Unlike the home cluster's [`DevJobSpec::active_deadline_secs`] (twice the
/// timeout, so Kubernetes never kills the pod before the entrypoint prints its report), a cloud
/// worker cannot live past [`CloudSettings::effective_worker_life_minutes`], so doubling the
/// timeout would already not fit one.
pub fn cloud_deadline_secs(spec: &DevJobSpec) -> u64 {
    spec.agent_timeout_secs() + FINISH_ALLOWANCE_SECS + MERGE_WAIT_SECONDS * MERGE_ATTEMPTS
}

/// A cloud start's granted `timeout_minutes`: the request, or the default when none was asked,
/// capped at `max_timeout_minutes` — this host's own cap, not [`super::MAX_TIMEOUT_MINUTES`].
/// The default is capped too, so a cap below it holds for a start that names no timeout.
pub fn cloud_capped_timeout_minutes(requested: Option<u64>, max_timeout_minutes: u64) -> u64 {
    requested
        .unwrap_or(super::DEFAULT_TIMEOUT_MINUTES)
        .min(max_timeout_minutes)
}

/// The top level `max_ttl` string `[devjob.cloud] kc_burst_config` carries, when it has one.
#[derive(Debug, Clone, Default, Deserialize)]
struct KcBurstToml {
    max_ttl: Option<String>,
}

/// kc-burst's own `max_ttl`, in whole minutes rounded down, from its TOML config at `path` (the
/// same file kc-burst itself reads, at `~/.config/kc-burst/config.toml` by default; parsed with
/// the `humantime` crate kc-burst uses, so `"8h"`, `"12h"`, `"720m"` and `"11h 30m"` all read).
/// `None` when the file is missing, unreadable, not TOML, has no top level `max_ttl`, or that
/// value does not parse as a duration; each case logs one line naming the path and the reason,
/// never the file's contents — the file also names credential files.
pub fn read_kc_burst_max_ttl(path: &Path) -> Option<u64> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "jesse-bridge: devjob cloud: kc_burst_config {} is unreadable: {e}",
                path.display()
            );
            return None;
        }
    };
    let parsed: KcBurstToml = match toml::from_str(&text) {
        Ok(v) => v,
        Err(_) => {
            eprintln!(
                "jesse-bridge: devjob cloud: kc_burst_config {} is not valid TOML",
                path.display()
            );
            return None;
        }
    };
    let Some(max_ttl) = parsed.max_ttl else {
        eprintln!(
            "jesse-bridge: devjob cloud: kc_burst_config {} has no top level max_ttl",
            path.display()
        );
        return None;
    };
    match humantime::parse_duration(&max_ttl) {
        Ok(d) => Some(d.as_secs() / 60),
        Err(_) => {
            eprintln!(
                "jesse-bridge: devjob cloud: kc_burst_config {}: max_ttl does not parse as a \
                 duration",
                path.display()
            );
            None
        }
    }
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

    /// [`MergeDecision::annotation`], read back: a continuation copies the finished Job's own
    /// decision rather than deciding again (the deny list, or what was asked, might have since
    /// changed). An annotation this does not recognise is `No`, same as one never asked for.
    pub fn from_annotation(s: &str) -> MergeDecision {
        match s {
            "true" => MergeDecision::AtGreen,
            s if s.starts_with("denied: ") => {
                MergeDecision::Denied(s["denied: ".len()..].to_string())
            }
            _ => MergeDecision::No,
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

// ---- CONTINUATIONS ---------------------------------------------------------------------

/// Whether a capped run's continuation is made new, or one already exists to reuse: a crash or
/// a retried [`super::DevJobs::reconcile`] pass between creating the Job and annotating the
/// finished one as reported must not create two Jobs for one continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationCreate {
    Create,
    Reuse,
}

/// [`ContinuationCreate`]: whether `new_run_id` is already the run id of a live or finished Job.
pub fn continuation_create_or_reuse(
    existing_run_ids: &[String],
    new_run_id: &str,
) -> ContinuationCreate {
    if existing_run_ids.iter().any(|id| id == new_run_id) {
        ContinuationCreate::Reuse
    } else {
        ContinuationCreate::Create
    }
}

/// What a capped run's finalize decides about continuing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationDecision {
    /// Make (or reuse) the next link, numbered `link`.
    Continue { link: u32 },
    /// Do not continue, and why.
    Stop(String),
}

/// The rules, first match wins (`docs/devjob-cloud.md` and the DJ29 prompt's step 6):
///
/// 1. `cancelled`: a person cancelled the run.
/// 2. `oom_killed`: the pod was killed for memory.
/// 3. `has_pull_request`: the report already names a pull request — the work is on GitHub, and a
///    new Job would refuse to start (`jesse/<slug>` already exists).
/// 4. `!stopped_at_limit`: the run did not fail by hitting its time limit (a missing report, a
///    test failure, an unexplained `BackoffLimitExceeded`, out of memory already covered above).
/// 5. `needs_you`: the agent's final message began `NEEDS YOU`.
/// 6. `checkpoint`: `Err(reason)` when the checkpoint could not be read, or none was found, or
///    the one found does not look like the entrypoint's own.
/// 7. `current_link >= max_continuations`: this link is already at the cap (the first run is
///    link 0).
/// 8. `previous`: `Some((run id, tree))` the checkpoint resumed from; stop when its tree equals
///    the checkpoint just read (no progress was made).
///
/// Otherwise, continue as the next link (`current_link + 1`).
#[allow(clippy::too_many_arguments)]
pub fn continuation_decision(
    cancelled: bool,
    oom_killed: bool,
    has_pull_request: bool,
    stopped_at_limit: bool,
    needs_you: bool,
    checkpoint: Result<&crate::hostrun::sys::Checkpoint, &str>,
    current_link: u32,
    max_continuations: u32,
    previous: Option<(&str, &str)>,
) -> ContinuationDecision {
    use ContinuationDecision::{Continue, Stop};
    if cancelled {
        return Stop("the run was cancelled".into());
    }
    if oom_killed {
        return Stop("the pod was killed for memory".into());
    }
    if has_pull_request {
        return Stop(
            "the report already names a pull request: the work is already on GitHub".into(),
        );
    }
    if !stopped_at_limit {
        return Stop("did not stop at its time limit".into());
    }
    if needs_you {
        return Stop("the agent asked a question (NEEDS YOU)".into());
    }
    let cp = match checkpoint {
        Ok(cp) => cp,
        Err(reason) => return Stop(reason.to_string()),
    };
    if current_link >= max_continuations {
        return Stop(format!(
            "link {current_link} is already max_continuations ({max_continuations})"
        ));
    }
    if let Some((previous_id, previous_tree)) = previous {
        if previous_tree == cp.tree {
            return Stop(format!("no progress since {previous_id}"));
        }
    }
    Continue {
        link: current_link + 1,
    }
}

/// Whether `report` says the run stopped at its time limit: the companion entrypoint change's
/// own `stopped_at_limit` key, else the agent timeout's own failure wording, else a SIGTERM the
/// Job's own deadline explains (`DeadlineExceeded`).
pub fn stopped_at_time_limit(report: &super::DevJobReport, failure_reason: Option<&str>) -> bool {
    if report.extra.contains_key("stopped_at_limit") {
        return true;
    }
    let Some(f) = &report.failure else {
        return false;
    };
    if f.starts_with("agent hit the") && f.contains("wall clock limit") {
        return true;
    }
    f.contains("stopped (SIGTERM)")
        && failure_reason.is_some_and(|r| r.starts_with("DeadlineExceeded"))
}

/// `### Continuation`, as a whole line: where [`strip_continuation_section`] cuts.
pub const CONTINUATION_HEADER: &str = "### Continuation";

/// `prompt` with any `### Continuation` section (that line to the end) removed, so a resumed
/// prompt never carries two.
pub fn strip_continuation_section(prompt: &str) -> String {
    let mut offset = 0;
    for line in prompt.split_inclusive('\n') {
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        if trimmed == CONTINUATION_HEADER {
            return prompt[..offset].trim_end().to_string();
        }
        offset += line.len();
    }
    prompt.trim_end().to_string()
}

/// What [`continuation_prompt`] fills into its pinned text.
pub struct ContinuationPromptInput<'a> {
    /// The new Job's own link number (the next one after the run that capped).
    pub link: u32,
    pub max: u32,
    /// The chain's first run's id.
    pub root: &'a str,
    /// The run id of the attempt that just hit its limit.
    pub previous: &'a str,
    /// The previous attempt's own `timeout_minutes`.
    pub minutes: u64,
    pub branch: &'a str,
    /// The checkpoint commit's sha.
    pub sha: &'a str,
}

/// The continuation's prompt: `base_prompt` (the ConfigMap's `prompt.md`, already the full cloud
/// prompt) with any earlier `### Continuation` section stripped, then this one appended. Exactly
/// this text (the DJ29 prompt's step 9), so the tests pin it.
pub fn continuation_prompt(base_prompt: &str, p: &ContinuationPromptInput) -> String {
    let stripped = strip_continuation_section(base_prompt);
    format!(
        "{stripped}\n\n{CONTINUATION_HEADER}\n\n\
         This is continuation {link} of at most {max} of cloud run {root}. The attempt before \
         this one ({previous}) reached its {minutes} minute time limit before it finished, and \
         its work was saved: your branch {branch} starts from its checkpoint {sha}, with its \
         commits on the branch and its uncommitted changes in the work tree. If the environment \
         variable JESSE_PROGRESS_FILE names a file that exists, read it first: it says what the \
         earlier attempts finished and what they meant to do next. Continue the task above from \
         there. Do not start over, and do not redo work that is already committed and passing. \
         Keep that file up to date as you go (what is done, what is next), never put this \
         prompt's text or any secret in it, because it is pushed to the repository with your \
         checkpoints, and commit after each step you finish: you have the same time limit.\n",
        link = p.link,
        max = p.max,
        root = p.root,
        previous = p.previous,
        minutes = p.minutes,
        branch = p.branch,
        sha = p.sha,
    )
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
         then answer as usual. Nobody can answer you while this run lasts: if you need a \
         decision from Jeremy to continue, stop, make the first line of that report NEEDS YOU: \
         followed by the one question, and end the run. His answer comes to a further run of \
         this task, on the same branch and pull request.\n"
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

/// Where queued cloud Jobs go this pass ([`cloud_resumable`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placement {
    /// Each Job to resume with the worker (node name) it is pinned to, oldest Job first.
    pub resume: Vec<(String, String)>,
    /// Idle ready workers too old for every queued Job that found no worker: they do not count
    /// as capacity for the queue, so [`plan_pass`] starts fresh workers in their place.
    pub spent: Vec<String>,
}

/// The queued cloud Jobs to resume now, oldest first: as many as [`super::to_resume`] allows
/// under the cap, each matched to an idle ready worker whose [`remaining_life`] covers the
/// Job's own `activeDeadlineSeconds` — because Kubernetes starts that clock on resume, a worker
/// that cannot live that long would be reaped mid run. The resume pins the Job's pod to that
/// worker ([`pin_to_worker`]), since another idle worker, too old for it, is otherwise as
/// eligible to the scheduler. Every released Job without a worker yet (`unplaced`) takes one of
/// `idle_ready` first, by count. A Job no idle worker's remaining life covers stays queued, and
/// the idle workers left unmatched are named [`Placement::spent`].
pub fn cloud_resumable(
    views: &[super::JobView],
    max: usize,
    idle_ready: &[PoolWorker],
    unplaced: usize,
    worker_life_minutes: u64,
) -> Placement {
    let mut lives: Vec<(&str, Duration)> = idle_ready
        .iter()
        .skip(unplaced)
        .map(|w| (w.name.as_str(), remaining_life(w, worker_life_minutes)))
        .collect();
    let mut out = Placement::default();
    let mut short = false;
    for name in super::to_resume(views, max) {
        let Some(view) = views.iter().find(|v| v.name == name) else {
            continue;
        };
        let deadline = Duration::from_secs(view.active_deadline_secs.unwrap_or(0).max(0) as u64);
        let Some(pos) = lives.iter().position(|(_, life)| *life >= deadline) else {
            short = true;
            continue;
        };
        let (worker, _) = lives.remove(pos);
        out.resume.push((name, worker.to_string()));
    }
    // Any worker still unmatched covers none of the Jobs that found no worker: each of them
    // looked at every worker left.
    if short {
        out.spent = lives.into_iter().map(|(w, _)| w.to_string()).collect();
    }
    out
}

/// The merge patch that resumes a queued cloud Job onto `worker` alone: a required node affinity
/// on the node's name. Kubernetes lets a suspended Job that never started change its pod
/// template's node affinity, so the pin and the resume go in one patch.
pub fn pin_to_worker(worker: &str) -> Value {
    json!({ "spec": {
        "suspend": false,
        "template": { "spec": { "affinity": { "nodeAffinity": {
            "requiredDuringSchedulingIgnoredDuringExecution": { "nodeSelectorTerms": [
                { "matchFields": [
                    { "key": "metadata.name", "operator": "In", "values": [worker] }
                ] }
            ] }
        } } } }
    } })
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

/// [`plan_burst`] with the [`Placement::spent`] workers left out of the capacity it counts, so
/// a queued Job too long for every idle worker gets a fresh one rather than waiting for an old
/// one to expire. A spent worker is stopped once idle [`CloudSettings::idle_minutes`], like any
/// other idle worker no run wants.
pub fn plan_pass(
    workers: &[PoolWorker],
    spent: &[String],
    starting: usize,
    demand: usize,
    max: usize,
    idle_after: Duration,
) -> BurstPlan {
    let (spent, usable): (Vec<PoolWorker>, Vec<PoolWorker>) = workers
        .iter()
        .cloned()
        .partition(|w| spent.contains(&w.name));
    let mut plan = plan_burst(&usable, starting, demand, max, idle_after);
    plan.down.extend(
        spent
            .iter()
            .filter(|w| !w.busy && w.idle_for >= idle_after)
            .map(|w| w.run.clone()),
    );
    plan
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
    /// Set when this run is a further link of its chain: a capped run's continuation
    /// ([`DevJobs::finalize`]'s own creation path), or a run record's further round (a review's
    /// hand off, a reply) the drain makes ([`DevJobs::record_run`]). `None` for a run's first
    /// Job, link 0.
    pub chain: Option<ChainExtras>,
    /// A reviewed run's Job: [`ReviewMode`]. `None` for a run nothing reviews.
    pub review: Option<ReviewMode>,
}

/// What a reviewed run's Job does about its pull request: the entrypoint's `REVIEW`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
    /// The implementing Job: push and open the pull request as a draft, and stop there, never
    /// merging: the bridge reviews the pushed branch next.
    Hold,
    /// A further round (a review's findings, a reply): continue the run's own branch and open
    /// pull request, then wait, mark ready and merge as any Job does.
    Answer,
}

impl ReviewMode {
    pub fn label(self) -> &'static str {
        match self {
            ReviewMode::Hold => "hold",
            ReviewMode::Answer => "answer",
        }
    }

    pub fn parse(s: &str) -> Option<ReviewMode> {
        match s {
            "hold" => Some(ReviewMode::Hold),
            "answer" => Some(ReviewMode::Answer),
            _ => None,
        }
    }
}

/// The [`ReviewMode`] a Job was made with ([`ANN_REVIEW`]), carried to its continuation.
pub fn review_mode(job: &k8s_openapi::api::batch::v1::Job) -> Option<ReviewMode> {
    job.metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(ANN_REVIEW))
        .and_then(|s| ReviewMode::parse(s))
}

/// The heading a run record's further round goes under in its Job's prompt ([`round_prompt`]).
pub const ROUND_HEADER: &str = "### Next round";

/// The prompt of a run record's further round on the cloud: the run's own prompt, then the
/// round's text (a review's hand off, a reply) under [`ROUND_HEADER`]. A pod has no session to
/// resume, so the task comes again with what comes next.
pub fn round_prompt(section: &str, round: &str) -> String {
    format!(
        "{}\n\n{ROUND_HEADER}\n\nAn earlier run of the task above pushed its branch and opened \
         its pull request, still a draft; this run continues it, on the same branch and pull \
         request. What comes next:\n\n{}\n",
        section.trim_end(),
        round.trim()
    )
}

/// The run id of link `link` of the chain rooted at `root`: the root itself, then
/// `<root>-c2`, `<root>-c3`, … as DJ29's continuations are named.
pub fn link_run_id(root: &str, link: u32) -> String {
    if link == 0 {
        root.to_string()
    } else {
        format!("{root}-c{}", link + 1)
    }
}

/// The chain place of a run record's further round: the link after the record's last Job
/// (`last`, read from that Job when kc1 still has it), rooted at the record's run id, with the
/// last Job's own link added to the chain it carried.
pub fn next_link(rec: &crate::hostrun::RunRecord, last: Option<&ChainInfo>) -> ChainExtras {
    let root = rec.run_id.clone();
    let ran = rec.cloud.as_ref().map(|c| c.jobs.len() as u32).unwrap_or(0);
    let prev = ran.saturating_sub(1);
    let previous = link_run_id(&root, prev);
    let mut chain = last.map(|c| c.chain.clone()).unwrap_or_default();
    let ended = rec.rounds.iter().rev().find(|r| r.finished_at.is_some());
    chain.push(ChainLink {
        run_id: previous.clone(),
        report_path: ended
            .and_then(|r| r.report_path.clone())
            .unwrap_or_default(),
        outcome: ended
            .and_then(|r| r.outcome)
            .map(|o| o.label().to_string())
            .unwrap_or_else(|| "ended".into()),
        wall_seconds: None,
        checkpoint_sha: None,
        checkpoint_tree: None,
    });
    ChainExtras {
        root,
        link: ran.max(1),
        continuation_of: previous,
        chain_json: serde_json::to_string(&chain).unwrap_or_else(|_| "[]".into()),
    }
}

/// The Job a run record executes as: its prompt (`section`, what the record was created with)
/// wrapped for the cloud, its branch, its merge decision `merge` and its strand step. A reviewed
/// run's first Job holds its merge ([`ReviewMode::Hold`]); a further round of a run (a review's
/// hand off, a reply) is the chain's next link on the same branch and pull request
/// ([`ReviewMode::Answer`], [`next_link`]), its round's text after the run's prompt
/// ([`round_prompt`]). Refused for a `local/` repository.
pub fn record_job(
    rec: &crate::hostrun::RunRecord,
    section: &str,
    merge: MergeDecision,
    default_agent: super::Agent,
    max_timeout_minutes: u64,
    last: Option<&ChainInfo>,
) -> Result<CloudRun, String> {
    if super::is_local_repo(&rec.repo) {
        return Err(super::LOCAL_ONLY_STUDIO.into());
    }
    if !valid_run_id(&rec.run_id) {
        return Err(format!("{:?} cannot name a run", rec.run_id));
    }
    let part = rec.cloud.clone().unwrap_or_default();
    let slug = record_slug(rec);
    let branch = format!("jesse/{slug}");
    let mut spec = DevJobSpec::validate(
        DevJobArgs {
            repo: Some(rec.repo.clone()),
            prompt: Some(section.to_string()),
            agent: part.agent.clone(),
            model: rec.model.clone(),
            branch_slug: Some(slug),
            test_command: part.test_command.clone(),
            timeout_minutes: part.timeout_minutes,
            ..DevJobArgs::default()
        },
        default_agent,
    )?;
    // A further round of the run (its current round is not its first, nor a capped Job's
    // continuation, which `DevJobs::finalize` makes itself) continues its branch and pull
    // request as the chain's next link; the first Job of a reviewed run holds its merge.
    let round = rec.round();
    let further = rec.rounds.len() > 1
        && round.is_some_and(|r| r.auto.as_deref() != Some(crate::hostrun::AUTO_CONTINUATION));
    let review = if further {
        Some(ReviewMode::Answer)
    } else if !rec.reviewer_ids().is_empty() {
        Some(ReviewMode::Hold)
    } else {
        None
    };
    let prompt = match round.and_then(|r| r.text.as_deref()).filter(|_| further) {
        Some(text) => round_prompt(section, text),
        None => section.to_string(),
    };
    // The held Job itself never merges, whatever the run's decision; its Job keeps that
    // decision (`AUTO_MERGE`, the merge annotation) for its continuation and its answer.
    let told = match review {
        Some(ReviewMode::Hold) => MergeDecision::No,
        _ => merge.clone(),
    };
    spec.prompt = cloud_prompt(&prompt, &rec.run_id, &rec.repo, &branch, &told);
    let chain = further.then(|| next_link(rec, last));
    // The cluster's cap, applied by `validate`, does not apply in the cloud: its own does.
    spec.timeout_minutes = cloud_capped_timeout_minutes(part.timeout_minutes, max_timeout_minutes);
    let strand = match (&rec.strand, &rec.item_id, &rec.prompt_path) {
        (Some(note), Some(id), Some(prompt)) => Some(StrandRef {
            note: note.clone(),
            id: id.clone(),
            prompt: prompt.clone(),
        }),
        _ => None,
    };
    let run_id = chain
        .as_ref()
        .map(|c| link_run_id(&c.root, c.link))
        .unwrap_or_else(|| rec.run_id.clone());
    Ok(CloudRun {
        spec,
        run_id,
        merge,
        strand,
        timeout_note: None,
        chain,
        review,
    })
}

/// The branch slug of a cloud run: the Studio's branch name for the same run, cut to the
/// longest slug.
pub fn cloud_slug(run_id: &str, item_text: Option<&str>) -> String {
    let b = crate::hostrun::branch_name(run_id, item_text);
    let cut: String = b.chars().take(super::MAX_SLUG_CHARS).collect();
    cut.trim_end_matches('-').to_string()
}

/// Whether a cloud run asked to merge: its start's `merge`, else its draft's `devjob-merge`.
pub fn asked_merge(notes: &Path, rec: &crate::hostrun::RunRecord) -> bool {
    if let Some(m) = rec.cloud.as_ref().and_then(|c| c.merge) {
        return m;
    }
    rec.prompt_path
        .as_deref()
        .and_then(|p| std::fs::read_to_string(notes.join(p)).ok())
        .is_some_and(|src| draft_merge(&src))
}

/// The branch slug of a run on the cloud: its record's `jesse/<slug>` when it was asked for
/// the cloud, else (an `any` run the Studio named) the cloud's slug for its id and line.
pub fn record_slug(rec: &crate::hostrun::RunRecord) -> String {
    match rec.branch.strip_prefix("jesse/") {
        Some(slug) if !slug.is_empty() => slug.to_string(),
        _ => cloud_slug(&rec.run_id, rec.item_text.as_deref()),
    }
}

/// The Job that continues the capped link `link_run_id` (its `jesse.dev/continuation-of`), the
/// newest when a retried pass made two. `None` when the link did not continue.
pub fn continuation_job(
    jobs: &[(k8s_openapi::api::batch::v1::Job, JobView)],
    link_run_id: &str,
) -> Option<String> {
    jobs.iter()
        .filter(|(j, _)| {
            j.metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get(ANN_CONTINUATION_OF))
                .is_some_and(|of| of == link_run_id)
        })
        .max_by_key(|(_, v)| v.created_ms)
        .map(|(_, v)| v.name.clone())
}

/// How far a Job's creation time may read before the moment its run was taken, Kubernetes'
/// clock and this Mac's being two clocks with whole second stamps.
pub const ADOPT_SKEW_MS: u64 = 60_000;

/// The Job a run taken but never named ([`crate::hostrun::HostRuns::unnamed_on_cloud`]) was
/// made as before the bridge stopped: one carrying its run id (for a further round, the next
/// link's, [`next_link`]), created no earlier than it was taken, owned by no other run. `None`:
/// no Job was made, so it is queued again.
pub fn adopted_job<'a>(
    views: &'a [JobView],
    rec: &crate::hostrun::RunRecord,
) -> Option<&'a JobView> {
    let taken = rec
        .cloud
        .as_ref()
        .and_then(|c| c.taken_at.as_deref())
        .and_then(crate::hostrun::ms_of)?;
    let ran = rec.cloud.as_ref().map(|c| c.jobs.len() as u32).unwrap_or(0);
    let id = link_run_id(&rec.run_id, ran);
    views
        .iter()
        .filter(|v| v.run_id.as_deref() == Some(id.as_str()))
        .filter(|v| v.created_ms + ADOPT_SKEW_MS >= taken)
        .max_by_key(|v| v.created_ms)
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

    /// `devjob_start` with `host: "cloud"`: a run record in the Studio's queue, through the
    /// Studio's own start ([`crate::hostrun::HostRuns::start_on`]), which a cloud slot takes in
    /// queue order ([`DevJobs::drain_studio`]).
    pub async fn start_cloud(
        &self,
        studio: &Arc<crate::hostrun::HostRuns>,
        req: CloudStart,
        origin: &str,
    ) -> Result<Value, String> {
        let rt = self.runtime()?;
        let kind = match req.kind {
            CloudKind::Item {
                strand,
                item_id,
                repo,
            } => crate::hostrun::StartKind::Item {
                strand,
                item_id,
                repo,
                scheduled: false,
            },
            CloudKind::Draft { draft, repo } => crate::hostrun::StartKind::Draft { draft, repo },
            CloudKind::Free(spec) => crate::hostrun::StartKind::Free {
                repo: spec.repo,
                prompt: spec.prompt,
                slug: spec.branch_slug,
                model: spec.model,
            },
        };
        let part = crate::hostrun::CloudPart {
            merge: req.merge,
            agent: req.agent,
            test_command: req.test_command,
            timeout_minutes: req.timeout_minutes,
            ..crate::hostrun::CloudPart::default()
        };
        // A new run takes no id a cloud Job already carries.
        if let Ok(views) = self.views().await {
            studio.set_cloud_ids(views.into_iter().filter_map(|(_, v)| v.run_id).collect());
        }
        let started = studio
            .start_on(
                crate::hostrun::StartRequest {
                    kind,
                    origin: origin.to_string(),
                    again: false,
                    reviewer: req.reviewer,
                    review_focus: req.review_focus,
                    priority: req.priority,
                },
                Some(part),
            )
            .await
            .map_err(|e| e.message().to_string())?;
        self.wake();
        let rec = studio
            .get(&started.run_id)
            .ok_or_else(|| format!("{} was created but cannot be read", started.run_id))?;
        let notes = studio.notes_root().to_path_buf();
        let merge = merge_decision(
            &rec.repo,
            asked_merge(&notes, &rec),
            &rt.settings.merge_deny,
        );
        let granted = cloud_capped_timeout_minutes(
            req.timeout_minutes,
            rt.settings.effective_max_timeout_minutes(),
        );
        Ok(json!({
            "run_id": rec.run_id,
            "host": "cloud",
            "status": rec.status,
            "waiting_for": rec.waiting_for,
            "priority": rec.priority,
            "branch": rec.branch,
            "merge": merge.annotation(),
            "timeout_minutes": granted,
            "timeout_note": super::timeout_reduced_note(req.timeout_minutes, granted, "the cloud"),
        }))
    }

    /// The Job a run record executes as ([`record_job`]), with this bridge's merge deny list,
    /// default agent and time cap. Refused for a `local/` repository.
    pub fn record_run(
        &self,
        rt: &CloudRuntime,
        notes: &Path,
        rec: &crate::hostrun::RunRecord,
        section: &str,
        last: Option<&ChainInfo>,
    ) -> Result<CloudRun, String> {
        let merge = merge_decision(&rec.repo, asked_merge(notes, rec), &rt.settings.merge_deny);
        record_job(
            rec,
            section,
            merge,
            self.default_agent,
            rt.settings.effective_max_timeout_minutes(),
            last,
        )
    }

    /// Create a resolved cloud run's ConfigMap and Job, suspended until a worker is idle.
    pub async fn create_cloud_run(&self, run: &CloudRun, origin: &str) -> Result<Started, String> {
        let rt = self.runtime()?;
        let extras = CloudJobExtras {
            run_id: run.run_id.clone(),
            merge: run.merge.clone(),
            merge_deny: rt.settings.merge_deny_env(),
            resources: rt.settings.job_resources(),
            services: rt.settings.services,
            chain: run.chain.clone(),
            review: run.review,
        };
        self.create_run(&run.spec, origin, run.strand.as_ref(), Some(&extras))
            .await
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

    /// `devjob_cancel` of a cloud Job no run record owns (one an earlier bridge created): a
    /// queued Job and its ConfigMap are deleted (nothing ran, nothing is reported); a released one
    /// is marked cancelled and ended through its deadline, and the watcher reports it from what
    /// it has.
    pub async fn cancel_cloud(&self, key: &str) -> Result<Value, String> {
        let (v, how) = self.stop_job(key).await?;
        Ok(match how {
            crate::hostrun::CloudCancel::Deleted => {
                json!({ "run_id": v.run_id, "job_name": v.name, "host": "cloud", "status": "cancelled", "reported": false })
            }
            crate::hostrun::CloudCancel::Stopping => {
                json!({ "run_id": v.run_id, "job_name": v.name, "host": "cloud", "status": "cancelling", "reported": "when the watcher next passes" })
            }
        })
    }

    /// Stop the Job `key` (its name or run id): delete it while it is suspended, else end it
    /// through its deadline, marked cancelled. What [`DevJobs::cancel_cloud`] and a run record's
    /// own cancel ([`crate::hostrun::CloudJobs`]) both do.
    async fn stop_job(&self, key: &str) -> Result<(JobView, crate::hostrun::CloudCancel), String> {
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
            return Ok((v, crate::hostrun::CloudCancel::Deleted));
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
        Ok((v, crate::hostrun::CloudCancel::Stopping))
    }

    /// While a cloud slot is free and a worker can be had, take the first queued run whose host
    /// allows the cloud, in the one queue's order ([`crate::hostrun::HostRuns::take_for_cloud`]),
    /// and make its Job. The run keeps its record; its Job is named on it. A run whose Job
    /// cannot be made now goes back to the queue; one the cloud can never run is refused.
    async fn drain_studio(&self, rt: &CloudRuntime, idle_ready: usize) {
        let Some(studio) = self.studio.get().cloned() else {
            return;
        };
        let Ok(notes) = self.notes() else {
            return;
        };
        let Ok(all) = self.views().await else {
            return;
        };
        let views: Vec<JobView> = all.into_iter().map(|(_, v)| v).collect();
        studio.set_cloud_ids(views.iter().filter_map(|v| v.run_id.clone()).collect());
        // A run taken but never named: the bridge stopped in between. Nothing is being taken
        // now (this pass is the only taker), so each is adopted by its Job or queued again.
        let s = studio.clone();
        let unnamed = tokio::task::spawn_blocking(move || s.unnamed_on_cloud())
            .await
            .unwrap_or_default();
        for rec in unnamed {
            let s = studio.clone();
            let id = rec.run_id.clone();
            match adopted_job(&views, &rec) {
                Some(v) => {
                    let (job, branch) = (v.name.clone(), format!("jesse/{}", v.slug));
                    eprintln!("jesse-bridge: devjob cloud: run {id} adopts its Job {job}");
                    let _ =
                        tokio::task::spawn_blocking(move || s.note_job(&id, &job, &branch)).await;
                }
                None => {
                    let _ = tokio::task::spawn_blocking(move || {
                        s.return_from_cloud(&id, "the bridge stopped before its Job was made")
                    })
                    .await;
                }
            }
        }
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
            // A further round's chain goes on from the run's last Job, while kc1 keeps it.
            let last = match rec.cloud.as_ref().and_then(|c| c.jobs.last()) {
                Some(name) => {
                    let prev = link_run_id(
                        &id,
                        rec.cloud.as_ref().map_or(1, |c| c.jobs.len() as u32) - 1,
                    );
                    self.jobs()
                        .get_opt(name)
                        .await
                        .ok()
                        .flatten()
                        .map(|j| ChainInfo::of(&j, &prev))
                }
                None => None,
            };
            let run = studio
                .section_of(&id)
                .and_then(|section| self.record_run(rt, &notes, &rec, &section, last.as_ref()));
            let run = match run {
                Ok(run) => run,
                Err(e) => {
                    let s = studio.clone();
                    let (i, why) = (id.clone(), e.clone());
                    let after =
                        tokio::task::spawn_blocking(move || s.refused_by_cloud(&i, &why)).await;
                    let failed = after
                        .ok()
                        .flatten()
                        .is_some_and(|r| r.status == crate::hostrun::RunStatus::Failed);
                    if let (true, Some(n)) =
                        (failed, self.sink.get().and_then(|s| s.notifier.clone()))
                    {
                        n.notify(
                            "Failed".into(),
                            format!("{id} in the cloud · {e}"),
                            id.clone(),
                        )
                        .await;
                    }
                    continue;
                }
            };
            match self.make_record_job(&run, &id, &rec.origin).await {
                Ok(started) => {
                    let s = studio.clone();
                    let job = started.job_name.clone();
                    let branch = format!("jesse/{}", started.branch_slug);
                    let i = id.clone();
                    let after =
                        tokio::task::spawn_blocking(move || s.note_job(&i, &job, &branch)).await;
                    eprintln!(
                        "jesse-bridge: devjob cloud took run {id} as {}",
                        started.job_name
                    );
                    // Cancelled while its Job was being made: stop the Job it now has.
                    let cancelled = after.ok().flatten().is_some_and(|r| {
                        r.round().is_some_and(|x| x.cancel_requested_at.is_some())
                    });
                    if cancelled {
                        match self.stop_job(&started.job_name).await {
                            Ok((_, crate::hostrun::CloudCancel::Deleted)) => {
                                let s = studio.clone();
                                let end = crate::hostrun::CloudEnd {
                                    job: started.job_name.clone(),
                                    status: crate::hostrun::RunStatus::Cancelled,
                                    report_path: None,
                                    continued_as: None,
                                    error: None,
                                    leaks: Vec::new(),
                                };
                                let _ =
                                    tokio::task::spawn_blocking(move || s.cloud_ended(&end)).await;
                            }
                            Ok(_) => {}
                            Err(e) => eprintln!("jesse-bridge: devjob cloud {id}: {e}"),
                        }
                    }
                }
                Err(e) => {
                    let s = studio.clone();
                    let _ = tokio::task::spawn_blocking(move || s.return_from_cloud(&id, &e)).await;
                    return;
                }
            }
        }
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
        let idle_ready: Vec<PoolWorker> = workers
            .iter()
            .filter(|w| w.ready && !w.busy)
            .cloned()
            .collect();
        let placement = cloud_resumable(
            &views,
            rt.settings.max_concurrent,
            &idle_ready,
            unplaced,
            rt.settings.effective_worker_life_minutes(),
        );
        let plan = plan_pass(
            &workers,
            &placement.spent,
            rt.starting.load(Ordering::SeqCst),
            demand,
            rt.settings.max_concurrent,
            Duration::from_secs(rt.settings.idle_minutes * 60),
        );
        self.carry_out(rt, &plan).await;
        for (name, worker) in placement.resume {
            let patch = pin_to_worker(&worker);
            match self
                .jobs()
                .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
                .await
            {
                Ok(_) => eprintln!("jesse-bridge: devjob RESUME {name} (cloud, on {worker})"),
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

// ---- THE MAILBOX ConfigMap ----------------------------------------------------------------

/// The run's mailbox ConfigMap ([`crate::hostrun::mailbox`]): `steer-<run id>`, one key per
/// message, labelled with the run.
pub fn steer_config_map(namespace: &str, run_id: &str, data: &BTreeMap<String, String>) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": crate::hostrun::mailbox::config_map_name(run_id),
            "namespace": namespace,
            "labels": {
                super::LABEL_DEVJOB: "true",
                LABEL_RUN_ID: if valid_label_value(run_id) { run_id } else { "" },
            },
        },
        "data": data,
    })
}

/// The keys and bodies of a run's messages still waiting for the hook: what its ConfigMap holds
/// when a Job of the run is made.
pub fn waiting_steer(rec: &crate::hostrun::RunRecord) -> BTreeMap<String, String> {
    rec.messages
        .iter()
        .filter(|m| m.waiting() && m.channel == crate::hostrun::mailbox::CHANNEL_TOOL_CALL)
        .map(|m| (m.file_name(), crate::hostrun::mailbox::body(&m.mail())))
        .collect()
}

/// The keys of a run's messages that no longer wait (delivered, or carried into a round): what
/// comes out of its ConfigMap so a later Job of the run never hands them over again.
pub fn settled_steer(rec: &crate::hostrun::RunRecord) -> Vec<String> {
    rec.messages
        .iter()
        .filter(|m| !m.waiting())
        .map(|m| m.file_name())
        .collect()
}

/// How far before the oldest waiting message a delivery read starts: clock skew between the
/// Studio and the worker, with room to spare.
pub const STEER_LOG_SKEW_SECS: i64 = 120;
/// The most of a pod's log one delivery read takes, from the start of its window. A delivery
/// line comes soon after its message reaches the pod, so it is near the start.
pub const STEER_LOG_BYTES: i64 = 64 * 1024 * 1024;

/// The log read that finds a run's deliveries: the agent's log since its oldest message still
/// waiting for the hook was queued (less [`STEER_LOG_SKEW_SECS`]), from the start of that window
/// and not a fixed tail, so no amount of output after a delivery line pushes it out of reach.
/// `None` when nothing waits for the hook.
pub fn steer_log_params(rec: &crate::hostrun::RunRecord, now_ms: u64) -> Option<LogParams> {
    let oldest = rec
        .messages
        .iter()
        .filter(|m| m.waiting() && m.channel == crate::hostrun::mailbox::CHANNEL_TOOL_CALL)
        .filter_map(|m| crate::hostrun::ms_of(&m.queued_at))
        .min()?;
    let secs = (now_ms.saturating_sub(oldest)).div_ceil(1000) as i64;
    Some(LogParams {
        container: Some("agent".into()),
        since_seconds: Some(secs + STEER_LOG_SKEW_SECS),
        limit_bytes: Some(STEER_LOG_BYTES),
        ..LogParams::default()
    })
}

impl DevJobs {
    /// Merge `data` into the run's mailbox ConfigMap, creating it when it is missing.
    pub async fn put_steer(
        &self,
        run_id: &str,
        data: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let name = crate::hostrun::mailbox::config_map_name(run_id);
        let patch = json!({ "data": data });
        for _ in 0..2 {
            match self
                .config_maps()
                .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
                .await
            {
                Ok(_) => return Ok(()),
                Err(kube::Error::Api(e)) if e.code == 404 => {
                    let cm = serde_json::from_value(steer_config_map(
                        &self.settings.namespace,
                        run_id,
                        data,
                    ))
                    .map_err(|e| e.to_string())?;
                    match self
                        .config_maps()
                        .create(&kube::api::PostParams::default(), &cm)
                        .await
                    {
                        Ok(_) => return Ok(()),
                        // Made meanwhile: patch it on the next turn of the loop.
                        Err(kube::Error::Api(e)) if e.code == 409 => continue,
                        Err(e) => return Err(format!("could not create {name}: {e}")),
                    }
                }
                Err(e) => return Err(format!("could not patch {name}: {e}")),
            }
        }
        Err(format!("{name} could not be written"))
    }

    /// Take `keys` out of the run's mailbox ConfigMap. A missing ConfigMap has none of them.
    pub async fn drop_steer_keys(&self, run_id: &str, keys: &[String]) -> Result<(), String> {
        if keys.is_empty() {
            return Ok(());
        }
        let name = crate::hostrun::mailbox::config_map_name(run_id);
        let data: serde_json::Map<String, Value> =
            keys.iter().map(|k| (k.clone(), Value::Null)).collect();
        let patch = json!({ "data": data });
        match self
            .config_maps()
            .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
        {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(e)) if e.code == 404 => Ok(()),
            Err(e) => Err(format!("could not trim {name}: {e}")),
        }
    }

    /// Delete the run's mailbox ConfigMap: its last Job has ended.
    pub async fn delete_steer(&self, run_id: &str) {
        let name = crate::hostrun::mailbox::config_map_name(run_id);
        match self
            .config_maps()
            .delete(&name, &DeleteParams::default())
            .await
        {
            Ok(_) => eprintln!("jesse-bridge: devjob cloud {run_id}: deleted {name}"),
            Err(kube::Error::Api(e)) if e.code == 404 => {}
            Err(e) => {
                eprintln!("jesse-bridge: devjob cloud {run_id}: could not delete {name}: {e}")
            }
        }
    }

    /// The mailbox of a run whose Job is about to be made: created, with every message still
    /// waiting for the hook. A failure is logged; the mount is optional, so the Job still
    /// starts, and the messages reach it at its next round.
    pub(super) async fn prepare_steer(&self, run_id: &str) {
        let Some(studio) = self.studio.get().cloned() else {
            return;
        };
        let id = run_id.to_string();
        let Some(rec) = tokio::task::spawn_blocking(move || studio.get(&id))
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        if let Err(e) = self.put_steer(&rec.run_id, &waiting_steer(&rec)).await {
            eprintln!("jesse-bridge: devjob cloud {run_id}: its mailbox: {e}");
        }
    }

    /// The deliveries of the Job `job`, into its run's record, and the messages that no longer
    /// wait taken out of the run's ConfigMap. The log is read from `pod` since the oldest waiting
    /// message ([`steer_log_params`]); `fallback` is used when there is no pod to read. Answers
    /// the record after it (`None` when no run owns the Job). An `Err` means the ConfigMap still
    /// holds a delivered message: the caller must not make a Job that mounts it.
    pub(super) async fn settle_delivered(
        &self,
        job: &str,
        pod: Option<&str>,
        fallback: Option<&str>,
    ) -> Result<Option<crate::hostrun::RunRecord>, String> {
        let Some(studio) = self.studio.get().cloned() else {
            return Ok(None);
        };
        let (s, j) = (studio.clone(), job.to_string());
        let Some(rec) = tokio::task::spawn_blocking(move || s.record_of_job(&j))
            .await
            .ok()
            .flatten()
        else {
            return Ok(None);
        };
        let read = match (pod, steer_log_params(&rec, crate::hostrun::now_ms())) {
            (Some(p), Some(lp)) => self.pods().logs(p, &lp).await.ok(),
            _ => None,
        };
        let log = read.as_deref().or(fallback).unwrap_or_default();
        let delivered = crate::hostrun::mailbox::parse_delivered_lines(log);
        let j = job.to_string();
        let rec =
            tokio::task::spawn_blocking(move || studio.cloud_messages_delivered(&j, &delivered))
                .await
                .ok()
                .flatten()
                .unwrap_or(rec);
        self.drop_steer_keys(&rec.run_id, &settled_steer(&rec))
            .await?;
        Ok(Some(rec))
    }

    /// Make the Job of a run record a cloud slot took: its mailbox first, with any message sent
    /// while it was being taken, then the Job. A Job that could not be made leaves no mailbox
    /// behind: the run may next run on the Studio, or never again, and the next try makes it
    /// afresh from the record.
    pub(super) async fn make_record_job(
        &self,
        run: &CloudRun,
        run_id: &str,
        origin: &str,
    ) -> Result<Started, String> {
        self.prepare_steer(run_id).await;
        let made = self.create_cloud_run(run, origin).await;
        if made.is_err() {
            self.delete_steer(run_id).await;
        }
        made
    }
}

/// A run record's cancel reaches its Job through the cloud host's server.
impl crate::hostrun::CloudJobs for DevJobs {
    fn steer<'a>(
        &'a self,
        run_id: &'a str,
        key: &'a str,
        body: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            self.runtime()?;
            let data = BTreeMap::from([(key.to_string(), body.to_string())]);
            self.put_steer(run_id, &data).await
        })
    }

    fn default_agent(&self) -> super::Agent {
        self.default_agent
    }

    fn cancel<'a>(
        &'a self,
        job: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<crate::hostrun::CloudCancel, String>> + Send + 'a>>
    {
        Box::pin(async move {
            match self.stop_job(job).await {
                Ok((_, how)) => Ok(how),
                // Finished already: the watcher reports it and ends the run.
                Err(e) if e.ends_with("has already finished") => {
                    Ok(crate::hostrun::CloudCancel::Stopping)
                }
                Err(e) => Err(e),
            }
        })
    }
}

#[cfg(test)]
mod tests;
