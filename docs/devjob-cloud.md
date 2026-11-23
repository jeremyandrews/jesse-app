# Dev jobs in the cloud: `host: "cloud"`

Two cloud slots on kc1 beside the Studio's slot, so a queue of Studio runs on one repository
(44 `jeremyandrews/trovato` runs behind one on 2026-11-23) can drain. The k3s side is
`cloud/infrastructure/jesse-dev/` and `images/dev-agent/` in `jeremyandrews/k3s`; the bridge side
is `bridge/src/devjob/cloud.rs`.

## Host routing

`devjob_start` takes `host`: absent or `cluster` is the home cluster, unchanged byte for byte;
`studio` is a Studio run, unchanged; `cloud` is a Job in `jesse-dev` on kc1. A cloud start takes
the Studio's three shapes (`strand` + `item_id`, `draft`, or `prompt` + `branch_slug` with
`repo`) plus the cluster's `agent`, `test_command` and `timeout_minutes`, and `merge`.

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
resume: a Job resumed onto no worker would spend its clock waiting for a droplet.

**Draining (`any`).** Each cloud pass, while a cloud slot is free (active plus queued cloud Jobs
under `max_concurrent`) and the cost cap allows, the cloud takes the oldest queued Studio run that
is waiting (`waiting_for` set: slot, repo, disk or limit), has never started a round, and whose
draft says `devjob-host: any`. The take is atomic under the Studio queue's gate: the Studio record
becomes `closed` with `closed_reason: moved to the cloud`, then the Job is created with the same
run id, so the report keeps its name. If the Job cannot be created the record goes back to
`queued`. The Studio's pump starts the same run if a Studio slot frees first; both decide under
the same gate, so a run starts once.

Two runs on one repository may now run at once (two cloud runs, or a cloud run beside a Studio
run). Each cuts its branch from a freshly fetched `origin/main`; the Job rebases onto
`origin/main` again (and re-runs its test command) before its first push and before merging.
The text the cloud agent receives says so (`cloud_paragraph`).

## Burst workers

Each cloud slot is one `s-4vcpu-8gb-intel` worker (the Job asks 3 CPU / 6Gi, so two never share
one), started by the bridge with `kc-burst up 1 --ttl <burst_ttl_minutes>m --size <burst_size>
--label kc.pozza/pool=devjob --json`. kc1's admission policy keeps `jesse-dev` pods on workers
with that label, so a dev job never lands on kc1 or a load test's worker.

Each cloud pass ([`plan_burst`]):

- **Start** `min(active + queued, max_concurrent) - (workers + starts in flight)` workers, one
  `up` each, never two in flight for one slot.
- **Stop** a worker (`kc-burst down --run <run>`) that has had no dev job pod for
  `idle_minutes` (default 10) while there are more workers than wanted.
- **Extend** (`kc-burst extend --run <run> --ttl 60m`) a busy worker within 30 minutes of its
  expiry, so a running Job is not reaped. kc-burst's own 8 hour cap still holds.

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

## Failure modes

| What | What happens |
| --- | --- |
| `kc-burst up` refused (caps, fra1 out of the size, Tailscale) | The Jobs stay queued; `waiting_reason` names the error; the next `up` is tried after 5 minutes |
| Daily cost cap reached | No `up` or `extend`; queued Jobs wait, with that reason |
| Quota (2 pods) | Cannot bite: a Job is resumed only onto an idle ready worker, at most `max_concurrent` |
| Secret missing | The pod stops at `CreateContainerConfigError` (`secret "jesse-dev-agent-keys" not found`), which `devjob_status` shows as its waiting reason; the Job fails at its deadline and is reported, or is cancelled sooner |
| kubeconfig missing or unreadable | The bridge refuses to boot with `[devjob.cloud] enabled` |
| Worker reaped mid run | The Job fails; the report says the pod was lost, with the log tail the bridge kept |
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
# burst_size = "s-4vcpu-8gb-intel"
# burst_ttl_minutes = 240
# idle_minutes = 10
# daily_cost_cap_eur = 5.0
# hourly_cost_eur = 0.0767
# merge_deny = []            # added to the seed list, never replacing it
# ledger = "~/.jesse-bridge/devjob-cloud-ledger.json"
```

`[devjob.cloud]` needs `[devjob] enabled` (the server it is part of). A strand schedule entry (`kind = "devjob"`) still takes `devjob-host` `studio` or the cluster only: scheduling cloud runs is not part of this change.
