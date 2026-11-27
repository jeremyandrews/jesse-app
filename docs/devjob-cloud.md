# Dev jobs in the cloud: `host: "cloud"`

Two cloud slots on kc1 beside the Studio's slot, so a queue of Studio runs on one repository
(44 `jeremyandrews/trovato` runs behind one on 2026-11-23) can drain. The k3s side is
`cloud/infrastructure/jesse-dev/` and `images/dev-agent/` in `jeremyandrews/k3s`; the bridge side
is `bridge/src/devjob/cloud.rs`.

## Host routing

`devjob_start` takes `host`: absent or `cluster` is the home cluster, unchanged byte for byte;
`studio` is a Studio run, unchanged; `cloud` is a Job in `jesse-dev` on kc1. A cloud start takes
the Studio's three shapes (`strand` + `item_id`, `draft`, or `prompt` + `branch_slug` with
`repo`) plus the cluster's `agent` and `test_command`, `merge`, and (since 0.226.0) the
Studio's `priority`. `timeout_minutes` is capped
at `[devjob.cloud] max_timeout_minutes` (default 555), not the cluster's 240 — or less, live,
when kc-burst's own `max_ttl` leaves less room (see "How long a run may take"); a reduction is
named in the reply's `timeout_note`.

A draft is **cloud capable** only when its front matter says so, because Studio prompts use
Studio only things (LAN addresses, the keychain, `kc-burst`, `local/<name>` repositories):

| `devjob-host:` | Studio start | Cloud start | Queued on the Studio |
| --- | --- | --- | --- |
| absent | yes | refused | waits for a Studio slot, as today |
| `studio` | yes | refused | waits for a Studio slot |
| `cloud` | yes | yes | waits for a Studio slot |
| `any` | yes | yes | starts on whichever slot frees first |

A free prompt with `host: "cloud"` is opted in by the asking. `local/<name>` is always refused in
the cloud.

## Slots

`[devjob.cloud] max_concurrent` (default 2, the namespace quota's 2 pods). A cloud run is a Job
created suspended when the slots are full or something is already waiting, as on the home
cluster. A suspended Job is resumed only when a **ready dev job worker with no dev job pod**
exists, not merely when a slot is free, because Kubernetes starts `activeDeadlineSeconds` on
resume: a Job resumed onto no worker would spend its clock waiting for a droplet. It is further
resumed only onto a worker whose **remaining life covers its own deadline**
(`worker_life_minutes` minus the worker's age, from kc-burst's `age_seconds`; with no age
reported, the worker's current `expires` stands in), and the resume pins the Job's pod to that
worker by a required node affinity on its name, so the scheduler cannot pick another idle worker
too old for it ([`cloud_resumable`], [`pin_to_worker`]). An idle worker too old for every queued
Job that found none is spent: it does not count as the queue's capacity, so `plan_pass` starts
the Job a fresh worker in the same pass, and the spent one is stopped once idle `idle_minutes`.

**One queue (0.226.0).** A cloud run is a run record like a Studio run's
(`<root>/runs/<run_id>/run.json`, `host: "cloud"`), created by the Studio's own start
(`HostRuns::create_on`) with the Studio's checks plus the cloud's two (no `local/<name>`
repository; a draft must say `devjob-host: cloud` or `any`), its id from the Studio's
`allocate_run_id`, and its priority from `priority` or the draft's `devjob-priority:`. It waits
in the one queue with `waiting_for: cloud`. Each cloud pass, while a cloud slot is free (active
plus queued cloud Jobs under `max_concurrent`) and a worker can be had, the cloud takes the
first queued run in `queue_order` (resumed rounds, then priority, then oldest first; see
"Priority" in `bridge/README.md`) whose host allows the cloud: a run asked for the cloud, or a
run whose draft says `devjob-host: any` that waits for a Studio slot and never started
(`HostRuns::take_for_cloud`). The take is atomic under the queue's gate: the record becomes
`running`, on the cloud (`cloud.taken_at`), then its Job is created from the record
(`DevJobs::record_run`: its `section.md`, wrapped by `cloud_paragraph`) and named on it
(`cloud.job_name`). The run keeps its id, its record, its priority and its place on the Runs
page; nothing is closed. If the Job cannot be created the record goes back to `queued`; if the
cloud can never run it (its prompt is gone) a cloud run fails with the reason and an `any` run
goes back to the queue as a Studio run. A run taken whose Job was never named (the bridge
stopped between the two) is adopted at the next cloud pass by the Job carrying its run id made
since the take, else queued again. A new run never takes the run id of a Job kc1 lists. A
cloud start whose draft names a `devjob-reviewer:` is refused until cloud runs are reviewed
(say `devjob-reviewer: none` to run it unreviewed). The Studio's pump only starts runs whose host allows
the Studio, and never counts a run on the cloud against its slots. `devjob_priority` changes a
queued cloud run's place exactly as a Studio run's.

Two runs on one repository may now run at once (two cloud runs, or a cloud run beside a Studio
run). Each cuts its branch from a freshly fetched `origin/main`; the Job rebases onto
`origin/main` again (and re-runs its test command) before its first push and before merging.
The text the cloud agent receives says so (`cloud_paragraph`).

## Burst workers

Each cloud slot is one `g-4vcpu-16gb` worker (4 vCPU, 16 GB), started by the bridge with `kc-burst up 1 --ttl <burst_ttl_minutes>m --size <burst_size>
--label kc.pozza/pool=devjob --json`. kc1's admission policy keeps `jesse-dev` pods on workers
with that label, so a dev job never lands on kc1 or a load test's worker.

The Job's resources come from config, sized to `burst_size`: `job_memory_request_gi` /
`job_memory_gi` (default 12 / 14 Gi) and `job_cpu_request` / `job_cpu` (default 3 / 4). Absent
keys follow `burst_size`: on an 8 GB worker they are the former 6 / 7 Gi and 3 / 4 CPU. A request
the named worker cannot hold (its GB and vCPUs read from the size's name) refuses to boot. A
required pod anti-affinity on the hostname keeps two cloud Jobs off one worker whatever the
requests. `CARGO_BUILD_JOBS` is set to the agent's share of `job_cpu` (see "Postgres and Redis
beside the agent"), so cargo never runs more parallel rustc processes than the agent has CPUs: the parallel rustcs, not the CPU count, set a build's
peak memory. A bigger `burst_size` needs these raised with it. Until 0.219.0 the pod was fixed at
3 CPU / 6Gi request, 7Gi limit for the 8 GB worker, and trovato's builds were `OOMKilled` there.

A cloud run's agent time is capped at `max_timeout_minutes` (default 555, sized so the whole
Job fits a worker's 12 hour life; see "How long a run may take"), not the cluster's 240. Bridge
0.219.0 called this key `max_agent_minutes`, and a config that still says so keeps working.

Each cloud pass ([`plan_burst`]):

- **Start** `min(active + queued, max_concurrent) - (workers + starts in flight)` workers, one
  `up` each, never two in flight for one slot.
- **Stop** a worker (`kc-burst down --run <run>`) that has had no dev job pod for
  `idle_minutes` (default 10) while there are more workers than wanted.
- **Extend** (`kc-burst extend --run <run> --ttl 60m`) a busy worker within 30 minutes of its
  expiry, so a running Job is not reaped. The burst system's own cap still holds — kc-burst's
  `max_ttl` and the reaper's `--max-age`, 8 hours as of 2026-11-26, 12 hours once a companion
  change in `jeremyandrews/k3s` lands and the Studio's live config is raised to match.

**How long a run may take.** `[devjob.cloud] max_timeout_minutes` (default 555) caps
`timeout_minutes` on this host — higher than the cluster's 240, because a cloud Job's own
deadline (`activeDeadlineSeconds`) is sized to fit a single worker's whole life, not doubled the
way the cluster's is. The deadline is the agent timeout, plus a 30 minute finish allowance (the
entrypoint's own rebase, test command, push and pull request after the agent exits), plus the
merge path's full wait (`MERGE_WAIT_SECONDS x MERGE_ATTEMPTS`, 120 minutes)
([`cloud_deadline_secs`]) — and this same number is also passed to the agent container as
`JOB_DEADLINE_SECONDS`, so the entrypoint knows its own wall-clock budget rather than guessing
one from `AGENT_TIMEOUT_SECONDS` alone. `worker_life_minutes` (default 720, the ceiling
`MAX_WORKER_LIFE_MINUTES`) is the burst system's own hard limit on a worker's age — kc-burst's
`max_ttl` and the reaper's `--max-age`; `CloudSettings::problems` refuses a config where
`max_timeout_minutes` plus the finish allowance plus the merge waits plus a 15 minute boot
allowance (the time from `kc-burst up` to a worker being ready, before a Job can resume onto it)
would exceed it, and refuses `worker_life_minutes` above 720 outright (the burst system's own
limits must be raised first). At the defaults these four add up to exactly 720:
`555 + 30 + 120 + 15`.

That boot-time check is on the *configured* `worker_life_minutes` — it never stops the bridge
booting. The *live* ceiling follows whatever kc-burst's own config actually says: `[devjob.cloud]
kc_burst_config` (default `~/.config/kc-burst/config.toml` under the bridge's home directory) is
read once at boot for its top level `max_ttl` (parsed with the `humantime` crate kc-burst itself
uses — `"8h"`, `"12h"`, `"11h 30m"`), giving `CloudSettings::effective_worker_life_minutes`
(`worker_life_minutes` capped at that `max_ttl` when it was read) and
`CloudSettings::effective_max_timeout_minutes` (`max_timeout_minutes` capped at what the
effective worker life leaves once the boot, finish and merge allowances are taken out). Every
live decision — the granted `timeout_minutes`, which worker a queued Job may resume onto, and the
`devjob_start` tool description — reads the effective values, not the configured ones, so a
worker that still only lives 8 hours never gets promised a 555 minute agent run. The file is
never logged; only its path, the parsed `max_ttl` in minutes, or why it could not be read. When a
request is reduced, either host's reply names the requested and the granted value in
`timeout_note`.

**Daily cost cap.** `daily_cost_cap_eur` (default 5) is hard. Each `up` commits its whole TTL
(`hourly_cost_eur` x TTL) and each `extend` one more hour against today's total in a ledger file
(`ledger`, default `~/.jesse-bridge/devjob-cloud-ledger.json`), before the command runs. A start
or extension that would pass the cap is not made: queued runs wait (`waiting_reason: daily cost
cap`) until the next day, and a running Job whose worker cannot be extended is reaped at its TTL
and reported failed. The ledger counts the worst case, so the real spend (`kc-burst status`) is
at most the cap.

## Postgres and Redis beside the agent

Since bridge 0.223.0 every cloud pod also runs Postgres 16 and Redis 7, so a run's database and
cache integration tests run on the worker, not only in GitHub CI. They are native sidecars
(`initContainers` with `restartPolicy: Always`, Kubernetes 1.29 or later; kc1 is 1.36): each
passes its startup probe before the agent starts, and both stop when the agent exits.

- `postgres`: `postgres:16`, user / password / database `trovato` / `trovato` / `trovato_test`,
  `PGDATA` on its own 4Gi `emptyDir`, started once `pg_isready -h 127.0.0.1 -U trovato -d
  trovato_test` answers (over TCP: the image's first-boot server answers on the socket alone,
  then restarts).
- `redis`: `redis:7 --save "" --appendonly no`, no persistence, started once `redis-cli ping`
  answers.

Both run as their image's own non-root user (uid and gid 999), with no privilege escalation, no
capabilities and the runtime's default seccomp profile, and both listen on the pod's loopback
only: no Service, no port. The agent's environment gains exactly what trovato's CI sets:

```
DATABASE_URL=postgres://trovato:trovato@localhost:5432/trovato_test
REDIS_URL=redis://127.0.0.1:6379
```

**The pod's total is unchanged.** The sidecars have fixed sizes, taken out of the agent's
requests and limits, so the agent plus the sidecars is exactly `job_memory_request_gi` /
`job_memory_gi` and `job_cpu_request` / `job_cpu`, and the `jesse-dev` ResourceQuota needs no
edit:

| container | memory request / limit | CPU request / limit |
|-----------|------------------------|---------------------|
| postgres  | 512Mi / 1Gi            | 500m / 500m         |
| redis     | 128Mi / 256Mi          | 250m / 250m         |
| agent     | configured − 640Mi / configured − 1.25Gi | configured − 750m |

On the live 28Gi / 8 CPU pod (26Gi / 7 CPU requests) the agent keeps 25984Mi / 27392Mi and
6250m / 7250m. `CARGO_BUILD_JOBS` is the agent's CPU limit rounded down (7 there; 3 on the 4 CPU
default). The bridge refuses to boot when the configured sizes leave the agent under 2Gi or
1 CPU, request or limit, or when `job_memory_request_gi` is not below `job_memory_gi` (the
sidecars' memory requests sit under their limits, so equal totals would give the agent a request
above its limit).

`[devjob.cloud] services = false` builds the single-container pod of 0.222.0, byte for byte, with
the agent holding the whole pod.

The k3s Job template (`cloud/infrastructure/jesse-dev/job-template.yaml`) should mirror this:
the two sidecars, the `pgdata` volume, the two URLs and the agent's reduced share.

## Merging

A cloud run merges when it is asked to (`merge: true`, or `devjob-merge: true` in its draft) and
its repository matches nothing on the deny list (`[devjob.cloud] merge_deny`, always including
`tag1consulting/scolta*` and `tag1consulting/goose`). The bridge then sets `AUTO_MERGE=true` and
passes the deny list as `MERGE_DENY`, which the entrypoint checks again.

The Job, not the agent, merges. The agent runs with no `GH_TOKEN` and no git credential store
(`AGENT_GITHUB=none`) and is told to commit only. After it exits the entrypoint rebases, runs the
test command, pushes `jesse/<slug>`, opens the pull request (`[<run id>] <title>`), then waits for
every workflow run whose `headSha` is the exact head to finish: merge only when all concluded
success or skipped; pending, red or no run at all is not merged. If `origin/main` moved it
rebases, tests, pushes with a lease and waits again (three attempts). The merge is `gh pr merge
--squash --delete-branch --admin --match-head-commit <head>`, read back with `gh pr view`. It never
pushes to `main`. The rule is `merge-guard.sh` in k3s, tested there; the bridge's half (when to
ask, the deny list) is tested in `devjob/tests.rs`.

The credential is `GH_TOKEN` in the Secret `jesse-dev-agent-keys` in `jesse-dev`. The agent and
the entrypoint are the same user, so hiding the token stops an agent following a Studio style
"merge it" sentence, not one that goes looking; the token's own reach is the wall. The bridge's
kubeconfig is the ServiceAccount `jesse:devjob`, which can create Jobs in `jesse-dev` (and so,
through a pod, read its Secret) and nothing else; chat turns use `jesse:jesse`, which cannot.

## Lifecycle

The Studio's, on the run's record: strand and item launch (the start turn, the Job carrying the
strand labels and the report the strand keys), `queued → running → reported / failed /
cancelled`, `devjob_status` by run id (or Job name), `devjob_list` (the record under `studio`,
with `host: "cloud"` and `job_name`, once: its Jobs are not listed again under `cloud`, 0.227.0),
`devjob_priority`, `devjob_cancel`
and `devjob_close`, and the Studio's own settling and archiving. The report is
`Inbox/<date>-run-<run_id>.md` with `host: cloud` and `run: <run_id>` in its front matter, the
agent's own report (it writes `$JESSE_RUN_REPORT`) as the report's final message, and a push.
`DevJobs::finalize` writes it from the pod's log, then ends the record's round with its path and
outcome (`HostRuns::cloud_ended`). A continuation (see "Continuing a capped run") is the same
record's next round: the record stays `running`, its `cloud.job_name` moves to the new Job and
`cloud.jobs` lists every link.

Cancel (`devjob_cancel`, or the Runs page) goes through the record (`HostRuns::cancel`): a
queued run is cancelled; a run on the cloud asks its Job to stop (`CloudJobs::cancel`, the
cloud host's `DevJobs`): a Job not yet released onto a worker is deleted and the run is
cancelled at once; a running one is stopped through `activeDeadlineSeconds` and reported, and
its report ends the run as cancelled. A cancel that lands while the Job is being made stops the
Job as soon as it is named.

## Review, and a further round (0.227.0)

A cloud run is reviewed as a Studio run is, by the same code (`docs/design/one-run-pipeline.md`).

- **Who reviews.** `reviewer`, `reviewers` and `review_focus` on `devjob_start` with `host:
  "cloud"`, else the draft's `devjob-reviewer:`, else `[hostrun] default_reviewers`, checked and
  refused by the same words (`HostRuns::resolve_reviewers`). `none` is no review.
- **The implementing Job is held.** Its Job carries `REVIEW=hold` (and the annotation
  `jesse.dev/review: hold`): the entrypoint pushes `jesse/<slug>` and opens the pull request as a
  draft, then stops. No CI wait, never ready, never merged, whatever the run's merge decision (its
  `AUTO_MERGE` is empty too; the merge annotation keeps the decision for the answer); the agent
  is told the run does not merge. Its checkpoint stays, taken again on the pushed head, so
  the progress file survives. Nothing is pushed to the phone for it.
- **The review.** When that Job reports, the record goes `reviewing` (`RunRecord::starts_review`,
  the Studio's rule) and the watcher runs the Studio's review: the pushed branch is fetched read
  only into the bridge's bare clone, the range and diff are read from a scratch checkout
  (`<run dir>/review-range`, removed at once), and every reviewer reads its own detached checkout
  of the head with the same prompt (`perform_review`). The note and the hand off are the Studio's.
- **The answer.** The hand off queues the run's next round with `handoff_reply`'s text, as on the
  Studio, but for the cloud: no Studio slot takes a run already on the cloud. A cloud slot takes
  it in queue order and makes the next link of the run's chain, `<run_id>-c<n>` (DJ29's naming,
  `RUN_ID` still the run's own, so the pull request keeps its title), with `REVIEW=answer`. Its
  prompt is the run's prompt, then the round's text under `### Next round`, then the cloud
  paragraph. The entrypoint checks out the run's branch as pushed, with its open pull request,
  restores the kept checkpoint and progress file, pushes to the same branch with a lease, and
  waits, marks ready and merges under the run's merge decision (`merge` or `devjob-merge`, the
  deny list, CI green on the exact head). Its report gets the Studio's marks: `UNREVIEWED:` or
  `PARTLY REVIEWED:` when the review could not run, and `REVIEW LEAK:` from the leak check of its
  new commits (the pull request's head, fetched read only into a scratch checkout,
  `<run dir>/review-answer`, removed at once) and of the run's pull requests. The reviews are
  marked answered, their notes archived and linked.
- **Unreviewed.** With no reviewer (`none`, or a default that cannot review) the Job has no
  `REVIEW` and merges as before 0.227.0.
- **Reply and `devjob_review`.** `devjob_reply` on a finished cloud run queues the same kind of
  round, answered by the next link on the same branch and open pull request; a run whose branch
  is gone (merged and deleted) has nothing to continue from and is refused, as on the Studio.
  `devjob_review` reviews a finished cloud run's pushed branch the same way.

A link is a link: an answer counts against `max_continuations` like a capped Job's continuation.

**Jobs from before 0.226.0.** A Job an earlier bridge created has no record that owns it (its
run either had none, or was closed `moved to the cloud`). It finishes under the old path: it is
released, reported and continued as before, listed read only on the Runs page, and
`devjob_cancel` and `devjob_status` still reach it by run id or Job name. It counts against the
cloud's slots.

## On the app's Runs page

`GET /jesse/runs` lists a cloud run as the run it is: its record's row, with `host: "cloud"`
and `job_name`, counted under `counts.cloud`, not the Studio's tallies. `GET /jesse/runs/:id`
(with or without `?host=cloud`) serves the record, with the Job's current `waiting_for`,
`last_line` and `elapsed_secs` while it runs; `/log` is the Job's log. `cancel`, `close`,
`archive` and `unarchive` act on the record as for a Studio run; `reply` and `review` are
refused by it until Part B. The app still hides the action buttons on a cloud row (it decides
by `host`); that is an app change of its own.

A Job no record owns (from before 0.226.0) is still merged in read only (0.216.0's behavior):
`cloud_available` (false, with `cloud_unavailable`, when there is no cloud host or kc1 does not
answer within 3 s), its detail and log by `?host=cloud` (503 when kc1 fails or does not answer
in 6 s, 404 only when it answers without the run), and every action refused with "cloud runs are
managed through Jesse". `src/hostrun/cloud.rs` has the details. Its status:

| The Job | Status |
| --- | --- |
| Suspended, waiting for a slot | `queued` |
| Released, not finished (scheduling, running, being cancelled) | `running` |
| Finished, cancelled | `cancelled` |
| Finished, outcome `pull request opened` or `no changes` | `reported` |
| Finished, outcome `failed` | `failed` |
| Finished, not yet reported | `failed` if the Job failed; `running` if it completed |
| Finished, an outcome this build does not know | `reported` if the Job completed, else `failed` |

## Continuing a capped run

When the agent hits its `AGENT_TIMEOUT_SECONDS` wall, the entrypoint pushes its last checkpoint
to `jesse/<slug>-wip` (a commit whose subject is `checkpoint of jesse/<slug> at <UTC time> [skip
ci]`, exactly one parent) and fails the Job. `DevJobs::finalize`, in its `None` branch (before the
finished run's own report is written), reads that ref back from GitHub and decides whether to
continue it ([`cloud::continuation_decision`]), first match wins:

1. The run was cancelled: stop.
2. The pod was killed for memory: stop.
3. The report already names a pull request (the work is already on GitHub): stop.
4. The run did not fail by hitting its time limit (a missing report, a test failure, an
   unexplained `BackoffLimitExceeded`): stop — only a capped run ever continues.
5. The agent's final message began `NEEDS YOU`: stop.
6. The checkpoint could not be read, none was found, or the one found does not look like the
   entrypoint's own: stop, naming why.
7. This link is already at `max_continuations` (the first run is link 0): stop.
8. The checkpoint's tree is the same as the one this run itself resumed from (no progress): stop.
9. Otherwise, continue as the next link.

A continuation is created (or, on a retried watcher pass, reused —
[`cloud::continuation_create_or_reuse`]) through the same `create_cloud_run` path a fresh start
uses, so it is created suspended and the slots, the worker life check and the daily cost cap all
apply unchanged, on a new worker once one is idle. Its run id is `<chain root>-c<link+1>` (`B248`
continuing once is `B248-c2`), but its pod's `RUN_ID` env — and so the pull request's title —
stays the chain's **root** run id, whichever link eventually opens it; `CONTINUATION` names the
new link. Its prompt is the finished Job's own `prompt.md`, with any earlier `### Continuation`
section stripped and a new one appended naming the link, the checkpoint and where to find
`JESSE_PROGRESS_FILE`. Chain bookkeeping (the root, this link's number, the previous link's run
id, and every earlier link's run id, report path, outcome, wall time and checkpoint sha) rides as
annotations on the continuation Job alone; a Job with none of them is link 0 of a chain rooted at
its own run id.

The capped link's own report starts with one line naming the checkpoint and the run it continues
as, or why it did not continue; each continuation's report has a `## Chain` section listing every
link in order. A capped run that continues pushes "Cloud run continuing" instead of "Failed".
Any other failed cloud run whose report does not already name a checkpoint still gets a line
pointing at `jesse/<slug>-wip`, when one is found, so a run's saved work is never invisible.

`max_continuations` (default 3, [`cloud::DEFAULT_MAX_CONTINUATIONS`]) caps a chain's length; `0`
turns continuations off outright. `CloudSettings::problems` refuses a value over 10. Raising the
cap does not raise `max_timeout_minutes` or `worker_life_minutes`: each link is its own fresh
worker, with its own deadline, from its own `kc-burst up`.

The Job's `JOB_DEADLINE_SECONDS` env now always equals its own `activeDeadlineSeconds`, so the
entrypoint's own wall-clock checks never disagree with the Job's real deadline (before this, the
entrypoint fell back to twice the agent timeout plus the merge waits, far more generous than the
cloud Job's actual deadline). `terminationGracePeriodSeconds` is 120s
([`cloud::CHECKPOINT_GRACE_SECS`]), not Kubernetes' default 30s, so the entrypoint's SIGTERM
checkpoint push and its report have time to complete between the deadline's SIGTERM and the
SIGKILL.

This half (the bridge's) is in `bridge/src/devjob/cloud.rs` and `bridge/src/devjob.rs`
(`continuation_decision`, `ChainInfo`, `build_cloud_job`, `DevJobs::finalize`,
`DevJobs::plan_continuation`, `DevJobs::create_continuation`). The entrypoint's half — restoring
a checkpoint onto a fresh clone, the SIGTERM checkpoint push, `JESSE_PROGRESS_FILE`, the
`stopped_at_limit` and `checkpoint:` report keys — lives in `jeremyandrews/k3s`, out of this
repository's reach.

## Failure modes

| What | What happens |
| --- | --- |
| `kc-burst up` refused (caps, fra1 out of the size, Tailscale) | The Jobs stay queued; `waiting_reason` names the error; the next `up` is tried after 5 minutes |
| Daily cost cap reached | No `up` or `extend`; queued Jobs wait, with that reason |
| Quota (2 pods) | Cannot bite: a Job is resumed only onto an idle ready worker, at most `max_concurrent` |
| Secret missing | The pod stops at `CreateContainerConfigError` (`secret "jesse-dev-agent-keys" not found`), which `devjob_status` shows as its waiting reason; the Job fails at its deadline and is reported, or is cancelled sooner |
| kubeconfig missing or unreadable | The bridge refuses to boot with `[devjob.cloud] enabled` |
| Worker reaped mid run | The Job fails; the report says the pod was lost, with the log tail the bridge kept |
| Out of memory | The pod is `OOMKilled`; the report's failure line says `killed at the N Gi memory limit (OOMKilled)` before the Job's `BackoffLimitExceeded` |
| Rebase conflict, red CI, CI timeout | Pull request left open, not merged; the report's `merge:` line says why |

## Config

```toml
[devjob.cloud]
enabled = true
kubeconfig = "~/.kube/kc1-devjob.yaml"
image = "ghcr.io/jeremyandrews/jesse-dev-agent@sha256:<amd64 digest>"
# namespace = "jesse-dev"
# max_concurrent = 2
# kc_burst = "/usr/local/bin/kc-burst"
# burst_size = "g-4vcpu-16gb"
# burst_ttl_minutes = 240
# idle_minutes = 10
# daily_cost_cap_eur = 5.0
# hourly_cost_eur = 0.0767
# job_memory_gi = 14          # the pod's memory limit; size it to burst_size
# job_memory_request_gi = 12  # at most job_memory_gi
# job_cpu = 4                 # the CPU limit; CARGO_BUILD_JOBS is the agent's share of it
# job_cpu_request = 3         # at most job_cpu
# services = true             # Postgres 16 + Redis 7 sidecars, their share out of the agent's;
                              # false is the single-container pod
# merge_deny = []            # added to the seed list, never replacing it
# ledger = "~/.jesse-bridge/devjob-cloud-ledger.json"
# max_timeout_minutes = 555  # this host's timeout_minutes cap; see "How long a run may take"
# worker_life_minutes = 720  # the burst system's own ceiling; raise that first to go higher, at most 720
# kc_burst_config = "~/.config/kc-burst/config.toml"  # read once at boot for its live max_ttl
# max_continuations = 3      # a capped run's chain length (<run_id>-c2, -c3, …); 0 turns it off
```

`[devjob.cloud]` needs `[devjob] enabled` (the server it is part of). A strand schedule entry (`kind = "devjob"`) still takes `devjob-host` `studio` or the cluster only: scheduling cloud runs is not part of this change.
