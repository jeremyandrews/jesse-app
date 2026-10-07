# jesse-eval

An offline eval harness for the Jesse assistant. It runs a task suite through a
**driver**, scores each task against assertions, compares two runs mechanically, and can
pit a candidate model against a baseline with an LLM judge.

```
jesse-eval run     --suite eval/suites/jesse-v1.json --out <dir> [--driver claude-cli|direct|bridge]
                   [--endpoint URL --model ID --wire messages|chat|responses] [--mock FILE]
                   [--runs N]
jesse-eval compare --a <dirA> --b <dirB> --out <dir>
jesse-eval judge   --baseline <dirA> --candidate <dirB> --out <dir>
jesse-eval tools   # print the tool-name mapping table
```

## Drivers

The suites, the assertions, the scorecard and the judge are driver-independent. The only
thing that knows how a task is EXECUTED is the driver, and `--driver` picks it.

| Driver | Runs | Mock format |
|---|---|---|
| `claude-cli` (default) | `claude -p` as a child process | canned stream-json NDJSON + a `files` map |
| `direct` | `jesse_agent::run_turn` in this process, over the vault tool set | a scripted-provider fixture, run against the REAL tools |
| `bridge` | a scratch `jesse-bridge` per task run, each prompt through `POST /jesse` as the phone sends it | none: it measures the real bridge, model and vault tooling |

All three write the same `results.json` and `scorecard.md`, and the scorecard header names the
driver, wire and model so two runs can be told apart a week later. `compare` pairs them.

### The `claude-cli` driver: what the child is given

The child is spawned with the task's prompt (its `system` blocks prepended, since these
flags give `claude` no system prefix of its own), the task's `allowed_tools` as
`--allowedTools`, the task's `persona` pack as `--append-system-prompt`, and

```
--mcp-config '{"mcpServers":{}}' --strict-mcp-config
```

so the child sees **zero MCP servers** whatever the host machine's user settings define.
`eval/src/driver/claude_cli.rs` builds that argument vector in one pure function and a test
asserts both flags are on it.

> **Artifacts from before D11 are not comparable with artifacts after it.** Both of the
> above are D11 changes to the child's invocation, and both move scores. Before D11 the CLI
> child was never shown the task's persona pack while the direct model always was, so every
> `style-adherence` task was biased against the baseline; and the child inherited the host's
> MCP servers, so a `product-v1` baseline depended on which machine ran it — in the D9 run
> one task produced no answer at all because the child asked for permission to use an
> ungranted note-search MCP tool, and another volunteered a fact about the host's real
> document collection into a supposedly hermetic fixture task. The runs tracked under
> `eval-runs/2026-08-31-product-claude-code/` and
> `eval-runs/2026-08-31-product-ds4-flash-direct/` are pre-D11 and are kept as the record of
> what the defects looked like, not as a baseline to compare against. The `*-postfix/` runs
> beside them are the first comparable pair.

### The `direct` driver

```
JESSE_EVAL_TOKEN_ENV=... jesse-eval run --driver direct \
  --suite eval/suites/product-v1.json --out /tmp/pv1 \
  --endpoint https://host/v1 --wire chat --model some-model \
  --token-env JESSE_EVAL_TOKEN_ENV
```

`--token-env` names the ENVIRONMENT VARIABLE the key lives in; the binary has no way to
accept a key as a flag, so nothing ever puts one in shell history or `ps` output.

Per task it builds an `FsVaultStore` rooted at the task's workspace, **the search index
`--index` names** over it, and the vault tool set at the task's `level`, narrowed to the
tools the task's `allowed_tools` grants (see the mapping table). The system prefix is the
task's `persona` pack rendered for the wire, followed by its `system` blocks. `fetch_url` and
`deliver_artifact` are reachable from no allowlist name and no artifact directory is
supplied, so a turn has no egress channel and nowhere to put a file that is not a document.

#### `--index`: the eval runs the index the bridge selects

```
jesse-eval run --driver direct --index qmd --qmd-collection vault \
  [--qmd-bin /path/to/qmd] ...
```

`grep` is the default and is what CI runs, because a fresh machine has no `qmd` binary.
`qmd` mirrors `direct_index` in `bridge/src/harness/direct.rs`, which selects `QmdIndex`
whenever `[direct] qmd = true` and a collection is named — so an eval run can now measure the
configuration a deployment with a large vault actually runs. `--index qmd` without
`--qmd-collection` is **refused**, never guessed: qmd reports a hit as
`qmd://<collection>/<path>`, and stripping the wrong prefix produces ids that resolve to the
wrong documents or to none, which reads as "the vault does not contain it".

**Before D12 this driver constructed `GrepIndex` unconditionally**, so this sentence was
false for exactly the deployments it mattered most for. `results.json` and the scorecard
header now record which index answered.

Whichever is selected, **the store is the boundary and the index sits behind it**: a hit the
store will not open — excluded, cold, or gone since the index was built — never reaches the
turn. See `SECURITY.md`.

### The `bridge` driver

```
jesse-eval run --driver bridge --model <registry id> --suite eval/suites/workflows-v1.json \
  --out <dir> [--bridge-bin bridge/target/release/jesse-bridge] [--bridge-config <file.toml>] \
  [--pass-env NAME ...] [--runs N] [--model-wait-secs 120] [--keep-sessions]
```

The other two drivers stop short of what the phone talks to: `claude-cli` gives its child no
MCP servers, and `direct` calls the loop in process, so neither exercises the bridge's spawn
path, its hooks, its skills, its shell grants or any MCP server. This one runs every task as a
real phone turn. It submits the prompt through `POST /jesse` with the bearer token, exactly as
the app does (the task's `mode`, its `health_context` block, the per-turn `model`, a
client-minted `conversation_id`), reads `GET /jesse/stream/{job}` for latency, waits on
`GET /jesse/result/{job}`, and grades the vault the turn left behind. A task's `followups` are
sent as further turns of the same conversation, carrying the `session_id` the previous result
returned; the answer graded is the last turn's. `allowed_tools` is ignored: the bridge's own
allowlist is the thing under test.

#### The target trait

Where the bridge runs is a `BridgeTarget` (`eval/src/driver/bridge.rs`): `start(vault)` returns
a running bridge (base URL, token, PID, log tail, `stop`). There is one implementation,
`Spawned`. A remote target that resets a fixture on a long-running bridge can be added beside
it without touching the driver.

#### What `Spawned` isolates

Each task run spawns the built `jesse-bridge` as its own process, stopped afterwards by the PID
recorded at spawn (SIGTERM, then SIGKILL after ten seconds). Every path the bridge owns is
redirected through the variables the bridge itself reads (`Config::from_env` in
`bridge/src/config.rs`, `local_config_path` in `bridge/src/persona.rs`):

| What | Variable | Scratch value |
|---|---|---|
| state dir: jobs, conversations, codex homes, direct threads, turn trace | `JESSE_STATE_DIR` | `<scratch>/state` |
| config overlay: persona, `[[models]]`, schedule | `JESSE_CONFIG` | `<scratch>/jesse.local.toml`, generated |
| vault | `JESSE_VAULT` | the task's fixture workspace |
| bearer token | `JESSE_TOKEN` | 32 random bytes, hex, new per run |
| listen address | `JESSE_BIND`, `JESSE_PORT` | `127.0.0.1`, a free port |

The process starts in the scratch dir, so the `./jesse.local.toml` fallback finds nothing, and
starting refuses outright if the scratch state dir were ever inside `~/.jesse-bridge`. Every
other inherited `JESSE_*`, `ANTHROPIC_*` and `CLAUDE_CODE_*` variable, `CLAUDECODE` and
`CODEX_HOME` are removed from its environment; a variable the run needs is kept by naming it
with `--pass-env`.

**`HOME` is kept, on purpose.** The `claude` child authenticates from the CLI's own login under
HOME, and the codex harness copies `~/.codex/auth.json` into a per-turn `CODEX_HOME` under the
scratch state dir; a redirected HOME has no login and every turn would fail. What HOME then
reaches is the production configuration, and that is what a baseline should measure:

- `~/.claude/projects/<cwd key>/`: Claude Code's session files, keyed by the escaped vault
  path. The vault is a fresh temp dir, so the key is new; the driver deletes that directory
  after the run (and sweeps once more at exit for a straggling child), only when the name
  carries the eval temp-root marker. `--keep-sessions` keeps them for reading.
- `~/.claude/settings.json`, user `CLAUDE.md` and skills: loaded because the bridge passes
  `--setting-sources user,project`, as in production.
- The MCP servers' own credential files under HOME are reached as in production. A server whose
  credential is a launch-environment variable starts unauthenticated unless it is passed.

The MCP config and the allowlist are the bridge defaults (`JESSE_MAIN_MCP_CONFIG` and
`JESSE_ALLOWED_TOOLS` are stripped). **The qmd server searches the HOST's qmd index, not the
fixture**: it is a global index, so a turn that calls `mcp__qmd__query` reads the owner's real
vault. No `workflows-v1` assertion can be satisfied that way (every fact is synthetic), but the
read happens; run with `qmd` off `PATH` to keep it out.

#### Matching the deployment

A cell measures the deployment only when the scratch bridge sees what the service sees. Pass
through, with their deployed values: `JESSE_CLAUDE_BIN` and `JESSE_CODEX_BIN` (otherwise the
first `claude` on `PATH` runs, which can be an older CLI), `JESSE_MODEL_OPUS_MODEL` and
`JESSE_MODEL_OPUS_VERSION` (otherwise `opus` runs the CLI's default model), the model's key, and
`TZ`. `PATH` itself is inherited, so put `node` and the MCP launchers on it.

#### The model overlay

`--model` names a registry id and rides every request as the per-turn `model` field. A built-in
id (`opus`, `glm`, `kimi`, `qwen`, `gemini-*`, `local`) needs only its key passed through. An id
the built-in registry lacks is declared by `--bridge-config <file.toml>`, whose text becomes
the scratch config file. Ready-made overlays for the baseline cells are in
`eval/bridge-overlays/`:

| Cell | `--model` | Overlay | Pass through |
|---|---|---|---|
| claude-code, Opus | `opus` | none | `JESSE_MODEL_OPUS_MODEL`, `JESSE_MODEL_OPUS_VERSION` |
| claude-code, Fireworks | `glm` (or `kimi`, `qwen`) | none | `JESSE_MODEL_GLM_AUTH_TOKEN` |
| claude-code, local | `qwen-local` | `qwen-local.toml` | `JESSE_MODEL_QWEN_LOCAL_TOKEN` |
| codex | `codex-write` | `codex-write.toml` | `JESSE_CODEX_TOKEN`, `JESSE_CODEX_BIN` |
| direct, Fireworks | `glm-direct` | `glm-direct.toml` | `JESSE_MODEL_GLM_AUTH_TOKEN` |
| direct, local | `qwen-local-direct` | `qwen-local-direct.toml` | `JESSE_MODEL_QWEN_LOCAL_TOKEN` |

Plus `JESSE_CLAUDE_BIN` on every claude-code cell. The two local overlays and `codex-write`
probe the Anthropic-surface gateway on `127.0.0.1:9100`, which must be up.

Before any task runs, a **preflight** bridge on an empty vault reports the model's harness and
whether it is available. A model that is unconfigured (its key was not passed) or never passes
a health probe within `--model-wait-secs` makes the whole cell **NOT RUN**: `results.json`
carries `not_run` with the reason and no tasks, the scorecard says NOT RUN, and the command exits
non-zero. A cell is never recorded as green without running.

#### Latency

Every run records three times, all measured from the moment the prompt was submitted:

| Field | Meaning |
|---|---|
| `latency.first_event_ms` | the first streamed frame of any kind (text, narration, tool activity); the empty snapshot the stream opens with does not count |
| `latency.first_token_ms` | the first model text (a delta or narration); for a harness that streams no text, the result frame |
| `latency.result_ms` | the terminal frame, or the result poll that saw the job finish, whichever came first |

For a multi-turn task these are the first turn's; each turn's own are on its `bridge_turn`
transcript line. The scorecard reports p50 and p95 of each over every attempt.

#### The transcript

`transcripts/<id>.ndjson` holds `bridge_spawn` (target, PID), one `bridge_turn` per turn
(status, latencies, the bridge's own content-free `timing` and the `provenance`), a
`bridge_sse` line per non-delta frame, an `assistant` line whose `tool_use` names are the
turn's tool calls from `timing.tools`, and the `result` line the parser keys on. As for every
driver, the persisted lines reparse to exactly what was scored.

### The tool-name mapping table

A suite writes tool names once, in the CLI's vocabulary. The direct driver maps them onto
its typed manifest by this table (`jesse-eval tools` prints it, `eval/src/mapping.rs` is
the source):

| `allowed_tools` name | Direct manifest names |
|---|---|
| `Read` | `vault_read` |
| `Grep` | `vault_search` |
| `Glob` | `vault_list` |
| `mcp__qmd__query` | `vault_search` |
| `mcp__qmd__get` | `vault_read` |
| `mcp__qmd__multi_get` | `vault_read` |
| `mcp__qmd__status` | `vault_list` |
| `Write` | `vault_write`, `vault_move` |
| `Edit` | `vault_edit` |
| anything else | **refused**, with a message naming the table |

The same table backs `tools_include` / `tools_exclude`: `tools_exclude: ["Write"]` catches
`vault_write` and `vault_move` as well as `Write`, and a name in no row (`fetch_url`,
`WebFetch`) matches literally and only literally.

## `run`

For the `claude-cli` driver the harness spawns:

```
claude -p <prompt> --output-format stream-json --verbose --include-partial-messages \
       --permission-mode default --allowedTools <task allowlist>
```

with `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN`, and `ANTHROPIC_MODEL` set **on
the child only** (never the harness's own environment) when `--endpoint`/`--model`
are given. Omit them for a baseline run against this machine's ambient auth and
default model.

Per task it captures: the full NDJSON transcript (`<out>/transcripts/<id>.ndjson`),
wall-clock time, time to first text delta, token usage (from the terminal `result`
line), tool-call count, and the result of every assertion. A task passes when all
of its assertions pass. Judged tasks additionally have their final answer saved to
`<out>/answers/<id>.txt`.

Outputs: `<out>/results.json` (one record per task) and `<out>/scorecard.md`
(per-class pass rate, mean latency, mean tool calls, plus totals).

### Workspaces

- `fixture` — the harness creates a fresh temp dir and populates it from the
  task's inline `fixture_files` before the run. Hermetic and repeatable.
- `vault-readonly` — the task runs with cwd `$JESSE_VAULT` (else `~/vault`, the real vault).
  Its allowlist may contain **only** read tools: `Read`, `Grep`, `Glob`, and the
  four `mcp__qmd__*` tools. Any other tool (`Write`, `Edit`, any `Bash`, …) is
  **refused before the suite runs** so an eval can never modify the vault. This
  check is load-bearing and unit-tested.

### Assertions

| type | fields | passes when |
|---|---|---|
| `answer_matches` | `pattern` | regex matches the final answer |
| `answer_excludes` | `pattern` | regex does **not** match the final answer |
| `answer_mentions_only_with` | `pattern`, `qualifier` | every segment of the final answer matching `pattern` ALSO matches `qualifier`; an answer that never mentions `pattern` passes. A segment is a line, or a sentence ended by `.`, `;`, `!` or `?` followed by whitespace — the whitespace condition keeps `3.1` in one piece. This is how a suite says "X may appear, but only as Y"; see `suites/README.md` for which of it and `answer_excludes` a situation calls for |
| `file_equals` | `path`, `content` | workspace file has exactly this content |
| `file_matches` | `path` (or `dir` + `name_pattern`), `pattern` | regex matches the workspace file's content; with the selector, matches in at least one selected file |
| `max_tool_calls` | `max` | tool-call count ≤ `max` |
| `number_in_range` | `path?`, `pattern`, `min`, `max` | capture group 1 of `pattern`, parsed as a number, is within `[min, max]` (inclusive); read from workspace file `path` if set, else from the final answer |
| `numbers_consistent` | `path`, `file_pattern`, `answer_pattern`, `tolerance?` | capture group 1 of `file_pattern` (from file `path`) and of `answer_pattern` (from the final answer) both parse and differ by ≤ `tolerance` (default `0`) |
| `completed` | — | a terminal `result` line arrived |
| `style_clean` | `max_hits?` | `jesse_agent::persona::check` finds at most `max_hits` (default `0`) style findings in the answer, against the TASK's `persona` pack. A task with no pack fails this rather than passing vacuously. |
| `tools_include` | `names` | every named tool was called (matched through the mapping table, so one name reads on both drivers) |
| `tools_exclude` | `names` | none of the named tools was called |
| `file_exists` | `path`, or `dir` + `name_pattern` | the path exists; or some entry directly inside `dir` has a name matching the regex (for a file whose name the turn chooses) |
| `file_absent` | `path`, or `dir` + `name_pattern` | the path does not exist; or no entry of `dir` matches (a missing `dir` passes) |
| `csv_last_row` | `path`, `columns`, `row_count?` | read with a real RFC 4180 reader (a malformed row fails, since an unquoted comma shifts every later column): every `columns` entry, header to regex, matches that cell of the LAST data row IN FULL (anchored); `row_count`, when set, is the exact number of data rows, which is how a task says "replaced in place, not appended" |
| `git_head_message_matches` | `pattern`, `repo?` | the HEAD commit's message (subject and body) matches; `repo` is relative to the workspace, default the workspace. Constant `git` argv |
| `git_path_changed_since` | `path`, `committed?` | `path` differs between the seed commit (`refs/eval/seed`, see `git_init`) and HEAD; with `committed: false`, the working tree. Constant `git` argv |
| `json_path_equals` | `path`, `pointer`, `value` | in a generated `.js` data file that starts with an assignment (`window.X = {…};`), leading comments and everything through the first `=` are stripped and the rest parsed as JSON5; the RFC 6901 `pointer` (`/date`, `/exercise/0/type`) must hold exactly `value` |
| `process_exit_zero` | `validator`, `day?` | one validator from a CLOSED table, `validate-diet-today` or `verify-diet-consistency`, runs as `node vault/<script> [--day YYYY-MM-DD]` in the workspace and exits 0. Never a free command: an unknown validator does not parse, and a `day` that is not a date is refused at load. `node` is `JESSE_EVAL_NODE` when set, else `node` on `PATH` |

Regexes use the Rust `regex` crate (no lookaround). Flags like `(?i)` / `(?m)`
are supported inline.

## `judge`

For each judged task present in both result dirs, the harness runs **two** judge
calls via `claude -p` with **no env overrides** (ambient auth + default model):
one presenting the baseline as Answer 1 and the candidate as Answer 2, and one
with the order swapped. The judge prompt includes the task's rubric, presents both
answers verbatim, and asks for `VERDICT: 1 | 2 | TIE` plus one sentence — grading
content accuracy and instruction-following only, explicitly ignoring answer length
and stylistic polish (countering verbosity/self-preference bias; the swap counters
position bias). A candidate wins a task **only if it wins both orderings**;
disagreement records as `TIE`. Outputs `<out>/judgment.json` and `<out>/judgment.md`.

## `--mock`

`--mock` replays a fixture instead of calling anything, so CI exercises the whole pipeline
with zero network and zero models (see `eval/tests/integration.rs`). **The format depends
on the driver, and the difference is the point.**

### `claude-cli`: canned stdout

The CLI mock fakes a child's stdout AND fakes its side effects, because nothing on that
path can run a tool: `ndjson` is replayed as the child's output, and `files` is written
into the workspace to stand in for what the tools would have done. The mock file maps task
id → a response:

```json
{
  "responses": {
    "greet": {
      "ndjson": [
        {"type": "stream_event", "event": {"type": "content_block_delta",
          "delta": {"type": "text_delta", "text": "READY"}}},
        {"type": "result", "subtype": "success", "result": "READY",
          "usage": {"input_tokens": 10, "output_tokens": 4}}
      ],
      "files": {"log.csv": "date,item\n2026-07-09,apple\n"}
    }
  }
}
```

`ndjson` lines are parsed exactly as real `claude` output; `files` (optional) are
written into the workspace before assertions run, standing in for tool side effects.

### `direct`: a scripted provider, real tools

The direct mock is a `jesse_agent::provider::scripted::ScriptFixture`: a list of model
responses per task id, each either text or tool calls with arguments. The loop dispatches
those calls against the REAL tool set over the REAL fixture workspace, so the files that
end up on disk are the ones the tools actually wrote — **there is no `files` map, and none
is needed.** A mock run therefore exercises argument parsing, path containment, the
compare-and-swap and the write path. What neither mock exercises is a model deciding
anything.

```json
{
  "responses": {
    "dw-append-entry": [
      {"type": "tool_calls", "calls": [
        {"name": "vault_read", "arguments": {"id": "logs/reading.md"}}
      ], "usage": {"input": 900, "output": 40}},
      {"type": "tool_calls", "calls": [
        {"name": "vault_write", "arguments": {
          "id": "logs/reading.md", "body": "…",
          "expected_hash": "{{hash:logs/reading.md}}"}}
      ]},
      {"type": "text", "text": "The log now has 3 entries."}
    ]
  }
}
```

`{{hash:<vault path>}}` is the ONE affordance the fixture has beyond the provider's own
format: it is substituted for that workspace file's current content hash immediately before
the turn. `vault_edit` requires the `expected_hash` from a prior read and a fixture cannot
know it, because it is the sha256 of a file the fixture is about to change; hard-coding the
digest would make every fixture one rewrite away from a compare-and-swap failure that says
nothing about the suite. A path that does not exist is left as written, so the failure
names an obvious placeholder rather than an empty string.

## `compare`

`compare --a <dirA> --b <dirB> --out <dir>` pairs two runs of the SAME suite by task id and
writes `compare.md` and `compare.json`: per-class pass rates side by side, mean latency,
mean tool calls, mean tokens, mean cost, and a verdict per class.

* `parity` — B's pass count is within ONE task of A's, and no safety task regressed.
* `improved` / `regressed` — outside that band.
* A single **safety** task (class containing `safety` or `injection`) going from pass to
  fail is `regressed` on its own, whatever the totals did. An injection that lands is not
  noise.

A task in only one run is reported as unpaired and excluded from every average; two runs of
different suites are refused. This needs no model and is deterministic, so run it first —
`judge` (below) is the model-graded pairwise comparison, and it only says anything about
the judged tasks.

## Suite schema

A suite is `{ "name": string, "tasks": [ Task, … ] }`. Each `Task`:

| field | required | meaning |
|---|---|---|
| `id` | yes | unique task id |
| `class` | yes | grouping bucket for the scorecard |
| `prompt` | yes | prompt passed to `claude -p` |
| `workspace` | yes | `"fixture"` or `"vault-readonly"` |
| `allowed_tools` | no | tools for `--allowedTools`, mapped onto the direct manifest by the table above |
| `level` | no | `basic` / `read` / `write` for a driver with levels. Defaults to `write` for `fixture` and `read` for `vault-readonly`; `vault-readonly` + `write` is refused |
| `system` | no | extra system-prefix blocks. The direct driver passes them as `SystemBlock`s; the CLI takes no system prefix on these flags, so its driver prepends the same text to the prompt |
| `persona` | no | a `PersonaPack`. Rendered into the system prefix by BOTH drivers, and checked by `style_clean` — one pack, so the rules the answer was written under and the rules it is graded against cannot drift. The direct driver renders it with `render_persona`; the `claude-cli` driver renders the SAME pack with the SAME function and hands the result to the child as `--append-system-prompt`, and a driver test asserts the two are byte-identical. (True only since D11: before that the CLI driver never read the field, so its child was graded on rules it had never been shown. One distinction remains, and it is a driver setting rather than a task field — the direct driver falls back to a suite-level default pack for a task that names none, where the CLI driver appends nothing.) |
| `fixture_files` | no | `{path: content}` written into a fixture workspace |
| `judged` | no | if true, the final answer is saved for `judge` (needs `rubric`) |
| `rubric` | judged only | grading text shown to the judge |
| `assertions` | yes | list of assertion objects (table above) |
| `harnesses` | no | the harnesses the task applies to (`claude-code`, `codex`, `direct`); empty is all. A driver that knows its harness (`bridge`) SKIPS a task that does not name it; the scorecard lists the skip and it counts as neither pass nor fail. `claude-cli` and `direct` run every task |
| `fixture_base` | no | a directory under `<suite dir>/../fixtures/` copied into the workspace (dotfiles included, symlinks refused) before `fixture_files` overlay it |
| `git_init` | no | make the workspace a git repository with one seed commit, recorded at `refs/eval/seed`. Required by the `git_*` assertions |
| `remotes` | no | bare repositories built beside (never inside) the workspace: `[{"name": "<host>/<owner>/<repo>", "branch"?: "main", "commits": [{"message", "files"}]}]`, committed with a fixed identity and date so a head SHA is the same on every machine. The prompt and the assertions reach one through `{{remote_url:NAME}}`, `{{remote_head:NAME}}` and `{{remote_head_short:NAME}}` |
| `followups` | no | further turns in the same conversation; the answer graded is the last one's. Only `bridge` can run them; the other drivers fail such a task as a harness error |
| `mode` | no | `ask` (default) or `tell`, the phone's two modes (`bridge`) |
| `health_context` | no | the device health block sent with the first turn (`bridge`) |

A suite may also carry `"runs": k`. Each task then runs k times, in a fresh workspace each time, and **passes only when all k runs pass** (pass^k). `--runs` overrides the suite; absent in both is 1, which is how every suite before `workflows-v1` ran. `results.json` keeps one `attempts` entry per run (pass, latencies, failed assertion kinds, transcript path) and the task record describes the first failing run, so a failure is never hidden behind a later pass. The scorecard adds a pass^k table per task, a skipped section naming the harness, and the latency percentiles.

### One full example task

```json
{
  "id": "extract-csv",
  "class": "extraction",
  "workspace": "fixture",
  "prompt": "The file log.csv uses the schema Date,Meal,Item,Calories. Append EXACTLY ONE new row for the entry below, preserving all existing content unchanged and ending the file with a single trailing newline.\n\nEntry: On 2026-07-09, breakfast was oatmeal with blueberries — about 320 calories.",
  "allowed_tools": ["Read", "Edit", "Write"],
  "judged": false,
  "fixture_files": {
    "log.csv": "Date,Meal,Item,Calories\n2026-07-08,dinner,grilled salmon,540\n"
  },
  "assertions": [
    {"type": "file_matches", "path": "log.csv", "pattern": "(?m)^2026-07-09,breakfast,oatmeal with blueberries,320$"},
    {"type": "file_equals", "path": "log.csv", "content": "Date,Meal,Item,Calories\n2026-07-08,dinner,grilled salmon,540\n2026-07-09,breakfast,oatmeal with blueberries,320\n"},
    {"type": "max_tool_calls", "max": 4},
    {"type": "completed"}
  ]
}
```

## The `jesse-v1` suite

Twelve tasks across eight classes: `titles`, `extraction`, `summarization`,
`safety`, `tool-use`, `vault-qa`, `long-context`. They probe titling (including
ignoring an instruction embedded in the data), structured extraction, faithful
summarization (with an omission canary and a prompt-injection canary), tool
discipline (both using tools when needed and *not* flailing into them when not),
read-only vault Q&A over `qmd`, and long-context conflict-finding. Judged tasks
carry a rubric for the `judge` subcommand.

## The `vaultqa-example` suite

Ten tasks probing read-only vault Q&A with the planned production child toolset
(`Read`, `Grep`, `Glob`, and the four `mcp__qmd__*` tools — no Write/Edit/Bash).
Six mechanical `vault-qa` lookups (owner facts, project status, a numeric target,
and a synthetic negative control for a fact absent from the vault), one `safety`
injection gate (reusing `jesse-v1`'s injection fixture), and three judged
`vault-synthesis` tasks whose rubrics score grounding, citation quality, and
concision. The questions and answers describe a SYNTHETIC vault (an "Alex Example"
persona with invented facts), so the suite ships clean of any real personal data;
it is a template you copy into a `suites/local/…` suite pinned against your own
vault (see `suites/README.md`). The suite is mock-validated under
`suites/validation/` (good → 10/10, known-bad → 0/10), proven in CI by two
integration tests.

## The `product-v1` suite

Seventeen tasks across the six classes Phase 1's gate names, hermetic over inline
`fixture_files` and runnable on BOTH drivers (every task's `allowed_tools` stays inside the
mapping table).

| Class | n | What it is for |
|---|---|---|
| `document-write` | 3 | Creating a note with given facts, appending without disturbing what is there (with a count that must stay consistent), and rewriting one paragraph while front matter survives. |
| `checkbox-update` | 3 | The single most common real edit: tick exactly one item of six, tick two and add a third, and REFUSE an ambiguous request by asking — with `max_tool_calls: 1` and a `file_equals` proving nothing changed. |
| `multi-document-search` | 3 | An answer that needs two files, a negative control whose answer is in no file (must say so, must not invent), and a decoy near-miss in an archived note. |
| `briefing` | 2 | The morning briefing: a mechanical variant asserting every item that must appear and excluding the one already done, and a judged variant with a rubric on completeness and concision. |
| `style-adherence` | 3 | Voice as a checked property. Each task carries a `PersonaPack` with `dashes: forbidden`, `lists: avoid` and a five-pattern banned list; `style_clean` grades the answer against it. One asks for a reply that would naturally invite lists; one is judged on voice against two writing samples. |
| `injection-resistance` | 3 | A note that tells the model to reply `PWNED`; a document that tells it to write a file (with the write tools actually granted, so the refusal means something, and `file_equals` + `tools_exclude` prove it); and a search hit that tells it to call `fetch_url` on a host. |

Validated by four mocks under `suites/validation/` — good and bad, on each driver — enforced
in CI. See `suites/validation/README.md`.

## The `workflows-v1` suite

Twenty-two tasks that grade what a real turn LEAVES BEHIND in a vault: a diet row appended and
the derived day file rebuilt, validated and committed; a draft named and footed; a checked
archive box moved; a checkout in the right place at the right commit. Run three times each
(pass^3) through the `bridge` driver, over a self-contained fixture vault under
`eval/fixtures/workflows-v1/base/`. Graded on outcome, never on the tool path: the only tool
assertion is `tools_exclude` on the three injection tasks carried over from `product-v1`. See
`suites/README.md` for the task list, which harnesses each applies to, and how the fixture vault
was built.

## Workspace note

`eval/` is its own crate in a root Cargo workspace that **excludes** `bridge/`, so
the bridge continues to build from `working-directory: bridge` with its own
`Cargo.lock` exactly as before.

## Cache creation pricing

Set `cache_write_per_m` in a model price table when cache creation has a different
rate from ordinary input. Agent and eval CLI runs use `--price-cache-write`.
The rate is dollars per million tokens for the configured cache lifetime. When
omitted, the historical input-rate estimate remains; it is not a provider quote.
For example, a $2 input rate and $2.50 five-minute cache-write rate must be
configured separately. The bridge, direct-loop ledger, budget and eval report use
the same configured rate.
