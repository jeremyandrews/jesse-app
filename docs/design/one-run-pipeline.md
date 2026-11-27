# One run pipeline: a cloud run is a Studio run that executes elsewhere

DJ30. Bridge 0.226.0 (Part A), 0.227.0 (Part B).

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
- **A further round.** A Studio round resumes the agent's session in its worktree. A pod has no
  session to resume and no worktree here: a cloud run's further round (a review's hand off, a
  reply) is the next Job of its chain, on the same branch and pull request, whose prompt is the
  run's prompt with the round's text after it.
- **What the review reads.** A Studio run's work is its worktree; a cloud run's is its pushed
  branch, fetched read only into a scratch checkout the bridge removes at once.

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

## Part B: review the same way (0.227.0)

| Rule | One implementation | What the cloud adds |
| --- | --- | --- |
| Who reviews | `HostRuns::resolve_reviewers` (start, draft `devjob-reviewer:`, `[hostrun] default_reviewers`), `merge_spellings`, `choose`, `checked_focus` | nothing; `plan_cloud` and `plan_start` share `start_reviewer` |
| A reviewed run's implementing round goes `reviewing` | `RunRecord::starts_review`, `RunRecord::open_start_review` | asked in `HostRuns::cloud_ended` when the held Job reports |
| The range and diff | `HostRuns::cycle_range` (`review_range`, `diff`) | the pushed branch fetched read only (`git::fetch_branch_head`) into a scratch checkout, `review-range`, removed at once |
| The review | `perform_review`, `reviewer_prompt`, the note | nothing |
| The hand off | `HostRuns::handoff_text`, `hand_off`, `queue_round_with` | the queued round goes back to a cloud slot (`take_for_cloud`), never a Studio one (`pump` skips a run on the cloud) |
| The answer's report | `HostRuns::review_marks` (`UNREVIEWED:`, `PARTLY REVIEWED:`, `review_leaks`), `archive_answered_notes`, `RunRecord::mark_answered` | the answer's commits read from the pull request's head in a scratch checkout, `review-answer` (`HostRuns::cloud_report_marks`) |
| The merge decision | `merge_decision` (`merge`, `devjob-merge`, the deny list) and the entrypoint's `merge-guard.sh` (CI green on the exact head) | the held Job never merges (`REVIEW=hold`); the answer merges by the same decision (`REVIEW=answer`) |

What the Job does is the k3s half: `REVIEW=hold` pushes and opens the draft and stops, keeping
its checkpoint; `REVIEW=answer` continues the run's branch and open pull request from that
checkpoint and progress file and merges as any Job does (`images/dev-agent/run.sh`, DJ30's k3s
pull request).

### What is deleted

- `HostRuns::cloud_reviewers` and its two refusals (a start's `reviewer` or `review_focus`; a
  draft's `devjob-reviewer:`), and `plan_cloud`'s own reviewer refusal.
- The `reply` and `request_review` refusals of a cloud run.
- The "Nobody can reply to this run" sentence of the cloud paragraph.

### Tests that hold it to one implementation

`hostrun/tests/cloud_review.rs`: the same starts resolve the same reviewers on both hosts, or
are refused with the same words; a Studio run and a cloud run with the same task and change are
reviewed with the same prompt (but for the head's id) and handed the same text; a reviewed cloud
run is not merged before its review is answered, then is; an unreviewed one merges as before; a
leak in a cloud answer leads its report as on the Studio.

### Migration (0.227.0)

A cloud run started before 0.227.0 has no reviewer (the start refused one), so it runs and
merges as it did. A Job in flight carries no `REVIEW` and behaves as before; nothing is
rewritten. A reply to a cloud run finished before 0.227.0 makes its next link like any other,
if its branch and pull request are still open. The answer needs the k3s entrypoint with
`REVIEW`, so `[devjob.cloud] image` names that image before the bridge is deployed. On an older
image a held Job still never merges (its `AUTO_MERGE` is empty), but it marks its pull request
ready at green and does not keep its checkpoint, and the answer fails at its start, the branch
being on origin already.
