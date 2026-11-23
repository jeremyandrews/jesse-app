//! Cloud dev jobs on the app's Runs page (0.216.0).
//!
//! `GET /jesse/runs` lists the Studio's runs. The cloud's runs (`host: "cloud"`, Jobs in the
//! cloud namespace on kc1, `[devjob.cloud]`) are a separate [`DevJobs`] and were visible only
//! through `devjob_list`. This module puts them in the same list, read only:
//!
//! * **The list.** The cloud's queued, running and last [`crate::devjob::LIST_FINISHED`]
//!   finished Jobs (`devjob_list`'s window, [`DevJobs::listed`]) join `runs` in the run JSON
//!   shape with `host: "cloud"` ([`cloud_run_json`]), sorted with the Studio's rows by the same
//!   rule. `counts` gains `cloud`, its own tallies, so no Studio count changes meaning.
//!   `cloud_available` says whether the cloud was listed; when it is false,
//!   `cloud_unavailable` says why. The cloud lookup is bounded by [`CLOUD_LIST_TIMEOUT`] and
//!   runs beside the Studio's scan, so it never fails or slows the Studio list.
//! * **One run.** `GET /jesse/runs/:id` and `/log` answer for a cloud run: with `?host=cloud`
//!   only the cloud is asked; without it the Studio is asked first, as before, and the cloud
//!   only for an id the Studio does not know.
//! * **Every other action** (`reply`, `review`, `cancel`, `close`, `archive`, `unarchive`) on a
//!   cloud run is refused with [`managed_through_jesse`], never carried out on a Studio run of
//!   the same id (a queued Studio run the cloud took keeps its id; see
//!   [`super::MOVED_TO_CLOUD`]).
//!
//! The status a cloud run is shown with is [`cloud_status`]; `docs/devjob-cloud.md` lists the
//! mapping.

use std::future::Future;
use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{json, Value};

use super::{rfc, work, ArchivedMode, HostRuns, RunStatus, StrandNotes};
use crate::auth::ApiError;
use crate::devjob::{DevJobRun, DevJobs, JobView, RunOutcome};

/// How long `GET /jesse/runs` waits for the cloud's list before it answers without it.
pub const CLOUD_LIST_TIMEOUT: Duration = Duration::from_secs(3);
/// How long one cloud run's detail or log may take (a Job, its pod and its log tail).
pub const CLOUD_DETAIL_TIMEOUT: Duration = Duration::from_secs(6);
/// `cloud_unavailable` when this bridge has no cloud host.
pub const CLOUD_NOT_CONFIGURED: &str =
    "the cloud host ([devjob.cloud]) is not enabled on this bridge";

/// What the cloud said when asked for its runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudRuns {
    /// No cloud host on this bridge.
    Off,
    /// Its queued, running and recently finished Jobs.
    Listed(Vec<JobView>),
    /// It could not be listed in time, or at all: why.
    Unavailable(String),
}

/// The status a cloud run shows, in the Studio's words:
///
/// | the Job | status |
/// |---|---|
/// | suspended, waiting for a slot | `queued` |
/// | released, not finished (scheduling, pulling, running, or being cancelled) | `running` |
/// | finished, cancelled by a person | `cancelled` |
/// | finished, outcome `pull request opened` or `no changes` | `reported` |
/// | finished, outcome `failed` | `failed` |
/// | finished, not yet reported, the Job failed | `failed` |
/// | finished, not yet reported, the Job completed | `running` (its report is being written) |
/// | finished, an outcome this build does not know | `reported` if the Job completed, else `failed` |
pub fn cloud_status(v: &JobView) -> RunStatus {
    let Some(completed) = v.finished else {
        return if v.suspended {
            RunStatus::Queued
        } else {
            RunStatus::Running
        };
    };
    if v.cancelled {
        return RunStatus::Cancelled;
    }
    let reported = [RunOutcome::PullRequestOpened, RunOutcome::NoChanges].map(RunOutcome::label);
    match v.outcome.as_deref() {
        Some(o) if reported.contains(&o) => RunStatus::Reported,
        Some(o) if o == RunOutcome::Failed.label() => RunStatus::Failed,
        Some(_) if completed => RunStatus::Reported,
        None if completed => RunStatus::Running,
        _ => RunStatus::Failed,
    }
}

/// The run id a cloud Job is listed under: its `jesse.dev/run-id`, else its Job name.
pub fn cloud_run_id(v: &JobView) -> &str {
    v.run_id.as_deref().unwrap_or(&v.name)
}

/// One cloud run in the run JSON shape (`run_json`'s keys where the cloud has them), with
/// `host: "cloud"`. A key the cloud has no value for is null, never made up: no rounds, no
/// reviewer, no `last_line` in a list.
pub fn cloud_run_json(v: &JobView, title: Option<String>) -> Value {
    let status = cloud_status(v);
    json!({
        "run_id": cloud_run_id(v),
        "host": "cloud",
        "job_name": v.name,
        "strand": v.strand,
        "item_id": v.strand_id,
        "repo": v.spec.repo,
        "branch": (!v.slug.is_empty()).then(|| format!("jesse/{}", v.slug)),
        "slug": v.slug,
        "origin": v.origin,
        "agent": v.spec.agent,
        "model": v.spec.model,
        "status": status,
        "created_at": rfc(v.created_ms),
        "queued_at": rfc(v.created_ms),
        "started_at": v.started_ms.map(rfc),
        "finished_at": v.finished.and(v.finished_ms).map(rfc),
        "outcome": v.outcome,
        "pull_request": v.pull_request,
        "report_paths": v.report_path.iter().collect::<Vec<_>>(),
        "error": v.failure_reason,
        "title": title,
    })
}

/// `counts.cloud`: the cloud's own tallies, by the Studio's rules for the app's filters.
pub fn cloud_counts(views: &[JobView]) -> Value {
    let statuses: Vec<RunStatus> = views.iter().map(cloud_status).collect();
    let n = |f: &dyn Fn(RunStatus) -> bool| statuses.iter().filter(|s| f(**s)).count();
    let needs_me = n(&|s| s.needs_me());
    let moving = n(&|s| s.is_moving());
    json!({
        "running": n(&|s| s == RunStatus::Running),
        "queued": n(&|s| s == RunStatus::Queued),
        "needs_me": needs_me,
        "moving": moving,
        "finished": statuses.len() - needs_me - moving,
    })
}

/// Put the cloud's runs into a Studio list ([`HostRuns::list_json_mode`]'s value): rows in
/// `runs` (none under `?archived=only`: a cloud run is never archived), `counts.cloud`,
/// `cloud_available` and, when it is false, `cloud_unavailable`. Nothing the Studio served
/// changes.
pub fn merge_cloud(
    list: &mut Value,
    cloud: &CloudRuns,
    mode: ArchivedMode,
    mut title: impl FnMut(&JobView) -> Option<String>,
) {
    let Some(obj) = list.as_object_mut() else {
        return;
    };
    let views = match cloud {
        CloudRuns::Listed(v) => v,
        CloudRuns::Off => {
            obj.insert("cloud_available".into(), json!(false));
            obj.insert("cloud_unavailable".into(), json!(CLOUD_NOT_CONFIGURED));
            return;
        }
        CloudRuns::Unavailable(why) => {
            obj.insert("cloud_available".into(), json!(false));
            obj.insert("cloud_unavailable".into(), json!(why));
            return;
        }
    };
    obj.insert("cloud_available".into(), json!(true));
    if let Some(counts) = obj.get_mut("counts").and_then(Value::as_object_mut) {
        counts.insert("cloud".into(), cloud_counts(views));
    }
    if mode == ArchivedMode::Only {
        return;
    }
    let Some(runs) = obj.get_mut("runs").and_then(Value::as_array_mut) else {
        return;
    };
    runs.extend(views.iter().map(|v| cloud_run_json(v, title(v))));
    // The Studio's own rule (newest created first, then by id), so its rows keep their order.
    let key = |r: &Value| {
        (
            r["created_at"].as_str().unwrap_or_default().to_string(),
            r["run_id"].as_str().unwrap_or_default().to_string(),
        )
    };
    runs.sort_by_key(|r| std::cmp::Reverse(key(r)));
}

/// `fut`, bounded by `limit`: what the cloud listed, or why it could not.
pub async fn bounded<F>(fut: F, limit: Duration) -> CloudRuns
where
    F: Future<Output = Result<Vec<JobView>, String>>,
{
    match tokio::time::timeout(limit, fut).await {
        Ok(Ok(views)) => CloudRuns::Listed(views),
        Ok(Err(e)) => CloudRuns::Unavailable(e),
        Err(_) => {
            CloudRuns::Unavailable(format!("kc1 did not answer within {} s", limit.as_secs()))
        }
    }
}

/// The cloud host's server, when this bridge has one.
pub fn cloud_server(devjob: Option<std::sync::Arc<DevJobs>>) -> Option<std::sync::Arc<DevJobs>> {
    devjob.and_then(|d| d.cloud().cloned())
}

/// The cloud's runs for `GET /jesse/runs`, within [`CLOUD_LIST_TIMEOUT`].
pub async fn list_cloud(cloud: Option<std::sync::Arc<DevJobs>>) -> CloudRuns {
    match cloud {
        None => CloudRuns::Off,
        Some(c) => bounded(async move { c.listed().await }, CLOUD_LIST_TIMEOUT).await,
    }
}

/// One cloud run, by run id or Job name, with its pod's state and log tail, within
/// [`CLOUD_DETAIL_TIMEOUT`]. `None` when the cloud has no such run, or did not answer.
pub async fn cloud_detail(
    cloud: Option<std::sync::Arc<DevJobs>>,
    key: &str,
) -> Option<(JobView, DevJobRun)> {
    let c = cloud?;
    let look = async {
        let v = c.find_cloud_job(key).await.ok()??;
        let run = c.run(&v.name).await.ok()??;
        Some((v, run))
    };
    tokio::time::timeout(CLOUD_DETAIL_TIMEOUT, look)
        .await
        .ok()
        .flatten()
}

/// Whether the cloud has a run named `key`, within [`CLOUD_LIST_TIMEOUT`]. No answer is no.
pub async fn cloud_has(cloud: Option<std::sync::Arc<DevJobs>>, key: &str) -> bool {
    let Some(c) = cloud else {
        return false;
    };
    matches!(
        tokio::time::timeout(CLOUD_LIST_TIMEOUT, c.find_cloud_job(key)).await,
        Ok(Ok(Some(_)))
    )
}

/// The log lines a cloud run's detail shows: what the watcher keeps of a running Job's log,
/// else the tail `devjob_status` read; at most `tail`.
pub fn cloud_log_lines(kept: Option<Vec<String>>, run: &DevJobRun, tail: usize) -> Vec<String> {
    let lines = kept
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| run.log_tail.clone());
    let skip = lines.len().saturating_sub(tail.max(1));
    lines.into_iter().skip(skip).collect()
}

/// One cloud run as `GET /jesse/runs/:id` serves it: [`cloud_run_json`] plus why a queued or
/// pending one waits, and its log's last line.
pub fn cloud_detail_json(v: &JobView, run: &DevJobRun, title: Option<String>) -> Value {
    let mut out = cloud_run_json(v, title);
    out["waiting_for"] = json!(run.waiting_reason);
    out["last_line"] = json!(work::last_line_of(&run.log_tail));
    out["elapsed_secs"] = json!(run.elapsed_secs);
    out
}

/// What a per run action answers for a cloud run.
pub fn managed_through_jesse(run_id: &str) -> ApiError {
    (
        StatusCode::CONFLICT,
        format!(
            "{run_id} is a cloud run: cloud runs are managed through Jesse, not from the Runs page"
        ),
    )
}

/// Which host a per run route was asked about: `?host=` absent or `studio`, or `cloud`.
pub fn asked_cloud(host: Option<&str>) -> Result<bool, ApiError> {
    match host.map(str::trim) {
        None | Some("") | Some("studio") => Ok(false),
        Some("cloud") => Ok(true),
        Some(other) => Err((
            StatusCode::BAD_REQUEST,
            format!("host {other:?} is not studio or cloud"),
        )),
    }
}

/// Whether a per run action (`reply`, `review`, `cancel`, `close`, `archive`, `unarchive`) may
/// go on to the Studio. Refused for `?host=cloud`, whatever the Studio holds under that id, and
/// for an id only the cloud knows. `cloud_has` is asked only when the Studio does not know the
/// id, so a Studio run's action costs nothing more than it did.
pub async fn refuse_cloud<F>(
    run_id: &str,
    host: Option<&str>,
    studio_has: bool,
    cloud_has: F,
) -> Result<(), ApiError>
where
    F: Future<Output = bool>,
{
    if asked_cloud(host)? {
        return Err(managed_through_jesse(run_id));
    }
    if studio_has || !cloud_has.await {
        return Ok(());
    }
    Err(managed_through_jesse(run_id))
}

impl HostRuns {
    /// A cloud run's title: its strand line's, when a strand entry started it.
    pub fn cloud_title(&self, notes: &mut StrandNotes, v: &JobView) -> Option<String> {
        match (&v.strand, &v.strand_id) {
            (Some(s), Some(id)) => notes.line_title(&self.notes_root, s, id),
            _ => None,
        }
    }

    /// [`merge_cloud`] into a [`HostRuns::list_json_mode`] value, each row titled by its strand
    /// line.
    pub fn add_cloud(&self, list: &mut Value, mode: ArchivedMode, cloud: &CloudRuns) {
        let mut notes = StrandNotes::default();
        merge_cloud(list, cloud, mode, |job| self.cloud_title(&mut notes, job));
    }
}

#[cfg(test)]
pub(crate) mod tests;
