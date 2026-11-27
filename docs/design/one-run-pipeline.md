# One run pipeline: a cloud run is a Studio run that executes elsewhere

DJ30. Bridge 0.226.0 (Part A), then Part B.

## Why

A run had two lifecycles. A Studio run is a `HostRuns` record (`<root>/runs/<run_id>/run.json`)
with a status, a queue place (`queue_order`, `Priority`), reviewers, a reply, cancel, close,
archive and a report. A cloud run was a Kubernetes Job on kc1 and nothing else: its queue was
the order Jobs were created in, it had no priority, no review, and the Runs page could only
look at it. Every rule written for one host had to be written again, or was simply missing,
for the other. The two drifted: `devjob_priority` never reached the cloud, `[hostrun]
default_reviewers` never reached the cloud, and a queued `devjob-host: any` run lost its
record, priority and reviewers when a cloud slot took it (`MOVED_TO_CLOUD`).

## The shape

One record, one queue, one set of rules. The record says where the run executes:

| `run.json` | Meaning |
| --- | --- |
| `host` absent | a Studio run (every record before 0.226.0) |
| `host: "cloud"` | asked for the cloud: only a cloud slot takes it |
| `host: "any"` | its draft says `devjob-host: any`: a Studio or a cloud slot, whichever frees first |
| `cloud` | the cloud half: what the start asked (`merge`, `agent`, `test_command`, `timeout_minutes`), and once a cloud slot took it, `taken_at`, the current Job (`job_name`) and every Job it ran as (`jobs`) |

A run is **on the cloud** once `cloud.taken_at` is set. From then on the Studio's own passes
(the runner scan, the stall and interrupt checks, the Studio slot count) leave it alone, and
the cloud's watcher drives it: the record's status moves `queued → running → reported / failed
/ cancelled` as the Job does.

### What moves where

| Rule | Before | After |
| --- | --- | --- |
| Start (strand item, draft, free prompt) | `DevJobs::resolve_cloud` built a Job | `HostRuns::create_on`, the Studio's own start, with the cloud's two extra checks (no `local/` repository, the draft says `devjob-host: cloud` or `any`) |
| Run id | `cloud::unique_run_id` over Jobs and Studio ids | `allocate_run_id`, the Studio's |
| Queue order | Job creation order | `queue_order` (priority, then oldest), one queue for both hosts |
| A free cloud slot | took only queued `any` drafts, closing their record | `HostRuns::take_for_cloud`: the first queued run in `queue_order` whose host allows the cloud; the record is kept and gains its Job name |
| `priority`, `devjob_priority` | refused / Studio only | the same code for both hosts |
| Cancel | `devjob_cancel` only, through the Job | `HostRuns::cancel` for both; on the cloud it asks the attached cloud ([`CloudJobs`]) to delete a Job that never started or end a running one through its deadline |
| Close, archive, unarchive | refused for cloud runs | the Studio's, on the record |
| Runs page list | Jobs merged in read only | the record is a row (`host: "cloud"`, `job_name`); only Jobs no record owns (from before 0.226.0) are still merged in read only |
| Report and push | `DevJobs::finalize` | unchanged (it reads the pod's log); it then ends the record's round with the report's path and outcome ([`HostRuns::cloud_ended`]) |
| DJ29 continuation | a new Job `<root>-c<n>` the record never heard of | the same record's next round, its `job_name` moved to the new Job |

### What is deleted

- `MOVED_TO_CLOUD` closing, `HostRuns::note_moved` and the `drainable`-only take of `any`
  drafts (the take now covers both hosts; `drainable` stays as the `any` half of it).
- `DevJobs::start_cloud`, `resolve_cloud`, `finish_resolve`'s Job-from-draft path and
  `moved_run`: a cloud run is built from its record (`DevJobs::record_run`).
- `plan_start`'s refusal of `priority` with host `cloud`.
- The Runs page's `managed_through_jesse` refusal for any run that has a record: it now
  applies only to a Job no record owns.

### The differences that stay, and why

Access forces them; nothing else does.

- **Where the agent runs.** A pod on a kc1 burst worker, not a worktree on the Studio. The Job
  builder, the worker life and timeout caps, burst worker start and stop, the daily cost cap,
  the sidecars and the continuation chain stay in `devjob/cloud.rs`.
- **Who pushes.** The cloud agent has no GitHub credential and no LAN, so the Job's entrypoint
  rebases, pushes, opens and (when asked) merges. The Studio's agent does its own.
- **The report's body.** It is built from the pod's log (`render_report_with`), not from a
  session's stream. Its path and front matter are the Studio's (`Inbox/<date>-run-<run_id>.md`,
  `run: <run_id>`).
- **Reply.** A Studio round resumes the agent's session. A pod has no session to resume; a
  cloud run's next round is a continuation Job (Part B uses this for review findings). Until
  Part B, `reply` and `review` on a cloud run are refused with the record's own refusal, not a
  "managed through Jesse" one.

### Restarts

The take and the Job's name are two writes. A run left `running` on the cloud with no Job name
(the bridge stopped between them) is adopted at the next cloud pass by the Job carrying its run
id created since the take, else queued again; the pass is the only taker, so nothing is being
taken at that moment. A capped link's continuation is read from kc1 on every pass, including a
retried one, so a lookup that fails never ends the run early.

## Migration (0.226.0)

Jobs created by an earlier bridge have no record, or a Studio record closed as moved to the
cloud. They are **left to finish under the old path**: the cloud's watcher still releases them
onto workers, reports them, continues a capped one, and the Runs page still lists them read
only (`managed_through_jesse`), because nothing owns them. `devjob_cancel` and `devjob_status`
still reach them by run id or Job name. They count against the cloud's slots, so a new run
waits for them. No record is rewritten.

## Part B: review the same way

Host cloud takes `reviewer`, `reviewers`, `review_focus` and `devjob-reviewer:`, with the
Studio's validation and default. A reviewed cloud run's Job opens its pull request as a draft
and does not merge; the bridge reviews the pushed branch's diff with the same `perform_review`
from a scratch checkout it owns; the findings go to the run's agent as the next link of its
continuation chain with the same `handoff_reply` text; the merge happens only when that link
ends green, under the same merge decision.
