# Dev jobs in the cloud: `host: "cloud"`

Two cloud slots on kc1 beside the Studio's slot, so a queue of Studio runs on one repository
(44 `jeremyandrews/trovato` runs behind one on 2026-11-23) can drain. The k3s side is
`cloud/infrastructure/jesse-dev/` and `images/dev-agent/` in `jeremyandrews/k3s`; the bridge side
is `bridge/src/devjob/cloud.rs`.

## Host routing

`devjob_start` takes `host`: absent or `cluster` is the home cluster, unchanged byte for byte;
`studio` is a Studio run, unchanged; `cloud` is a Job in `jesse-dev` on kc1. A cloud start takes
the Studio's three shapes (`strand` + `item_id`, `draft`, or `prompt` + `branch_slug` with
`repo`) plus the cluster's `agent` and `test_command`, and `merge`. `timeout_minutes` is capped
at `[devjob.cloud] max_timeout_minutes` (default 315), not the cluster's 240; a reduction is
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
reported, the worker's current `expires` stands in): a worker too old for the Job stays idle
(and is stopped by the idle rule below, in time) while `plan_burst` starts the Job a fresh one
([`cloud_resumable`]).

**Draining (`any`).** Each cloud pass, while a cloud slot is free (active plus queued cloud Jobs
under `max_concurrent`) and the cost cap allows, the cloud takes the first queued Studio run, in
the Studio queue's own order (resumed rounds, then priority, then oldest first; see "Priority"
in `bridge/README.md`), that is waiting (`waiting_for` set: slot, repo, disk or limit), has
never started a round, and whose draft says `devjob-host: any`. The take is atomic under
the Studio queue's gate: the Studio record becomes `closed` with `closed_reason: moved to the
cloud`, then the Job is created with the same run id, so the report keeps its name. If the Job
cannot be created the record goes back to `queued`. The Studio's pump starts the same run if a
Studio slot frees first; both decide under the same gate, so a run starts once.

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
requests. `CARGO_BUILD_JOBS` is set to `job_cpu`, so cargo never runs more parallel
rustc processes than the pod has CPUs: the parallel rustcs, not the CPU count, set a build's
peak memory. A bigger `burst_size` needs these raised with it. Until 0.219.0 the pod was fixed at
3 CPU / 6Gi request, 7Gi limit for the 8 GB worker, and trovato's builds were `OOMKilled` there.

A cloud run's agent time is capped at `max_timeout_minutes` (default 315, sized so the whole
Job fits a worker's 8 hour life; see "How long a run may take"), not the cluster's 240. Bridge
0.219.0 called this key `max_agent_minutes`, and a config that still says so keeps working.

Each cloud pass ([`plan_burst`]):

- **Start** `min(active + queued, max_concurrent) - (workers + starts in flight)` workers, one
  `up` each, never two in flight for one slot.
- **Stop** a worker (`kc-burst down --run <run>`) that has had no dev job pod for
  `idle_minutes` (default 10) while there are more workers than wanted.
- **Extend** (`kc-burst extend --run <run> --ttl 60m`) a busy worker within 30 minutes of its
  expiry, so a running Job is not reaped. kc-burst's own 8 hour cap still holds.

**How long a run may take.** `[devjob.cloud] max_timeout_minutes` (default 315) caps
`timeout_minutes` on this host — higher than the cluster's 240, because a cloud Job's own
deadline (`activeDeadlineSeconds`) is sized to fit a single worker's whole life, not doubled the
way the cluster's is. The deadline is the agent timeout, plus a 30 minute finish allowance (the
entrypoint's own rebase, test command, push and pull request after the agent exits), plus the
merge path's full wait (`MERGE_WAIT_SECONDS x MERGE_ATTEMPTS`, 120 minutes)
([`cloud_deadline_secs`]). `worker_life_minutes` (default 480) is kc-burst's own `max_ttl`;
`CloudSettings::problems` refuses a config where `max_timeout_minutes` plus the finish allowance
plus the merge waits plus a 15 minute boot allowance (the time from `kc-burst up` to a worker
being ready, before a Job can resume onto it) would exceed it, and refuses `worker_life_minutes`
above 480 outright (kc-burst's `max_ttl` must be raised first). At the defaults these four add up
to exactly 480: `315 + 30 + 120 + 15`. When a request is reduced, either host's reply names the
requested and the granted value in `timeout_note`.

**Daily cost cap.** `daily_cost_cap_eur` (default 5) is hard. Each `up` commits its whole TTL
(`hourly_cost_eur` x TTL) and each `extend` one more hour against today's total in a ledger file
(`ledger`, default `~/.jesse-bridge/devjob-cloud-ledger.json`), before the command runs. A start
or extension that would pass the cap is not made: queued runs wait (`waiting_reason: daily cost
cap`) until the next day, and a running Job whose worker cannot be extended is reaped at its TTL
and reported failed. The ledger counts the worst case, so the real spend (`kc-burst status`) is
at most the cap.

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

The same as a Studio run where it applies: strand and item launch (the Job carries the strand
labels and the report the strand keys), the report in `Inbox/<date>-run-<run_id>.md` with
`host: cloud` and `run: <run_id>` in its front matter, the agent's own report (it writes
`$JESSE_RUN_REPORT`) as the report's final message, a push when it finishes, `devjob_status` by
run id or Job name, `devjob_list` (under `cloud`), and `devjob_cancel` (a queued Job is deleted;
a running one is stopped through `activeDeadlineSeconds` and reported). There is no reply: a
cloud run has no session to continue, so a report that needs Jeremy is answered by a new run.

## On the app's Runs page

`GET /jesse/runs` (0.216.0) lists the cloud's queued, running and last 20 finished runs beside
the Studio's, with `host: "cloud"`, `counts.cloud`, and `cloud_available` (false, with
`cloud_unavailable`, when there is no cloud host or kc1 does not answer within 3 s). A cloud run
is read only there: its detail and log answer (`?host=cloud`; 503 when kc1 fails or does not answer
in 6 s, 404 only when it answers without the run), and every action is refused with "cloud runs
are managed through Jesse". `src/hostrun/cloud.rs` has the details. Its status:

| The Job | Status |
| --- | --- |
| Suspended, waiting for a slot | `queued` |
| Released, not finished (scheduling, running, being cancelled) | `running` |
| Finished, cancelled | `cancelled` |
| Finished, outcome `pull request opened` or `no changes` | `reported` |
| Finished, outcome `failed` | `failed` |
| Finished, not yet reported | `failed` if the Job failed; `running` if it completed |
| Finished, an outcome this build does not know | `reported` if the Job completed, else `failed` |

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
# job_cpu = 4                 # the CPU limit, and CARGO_BUILD_JOBS
# job_cpu_request = 3         # at most job_cpu
# merge_deny = []            # added to the seed list, never replacing it
# ledger = "~/.jesse-bridge/devjob-cloud-ledger.json"
# max_timeout_minutes = 315  # this host's timeout_minutes cap; see "How long a run may take"
# worker_life_minutes = 480  # kc-burst's max_ttl; raise that first to go higher, at most 480
```

`[devjob.cloud]` needs `[devjob] enabled` (the server it is part of). A strand schedule entry (`kind = "devjob"`) still takes `devjob-host` `studio` or the cluster only: scheduling cloud runs is not part of this change.
