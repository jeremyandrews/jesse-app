# Platform dependencies and conversation state

The baseline for moving the bridge off the Mac Studio. Two inventories:

1. every place the bridge or the agent crate depends on macOS, with what replaces it;
2. where each harness keeps conversation state, how the path is derived, what a turn reads
   and writes, and how big it is on the Studio today.

Nothing here changes runtime behaviour. It records what later steps are measured against.

## Platform dependencies

`bridge/tests/platform_inventory.rs` scans every `.rs` file under `bridge/src` and
`agent/src` (whole-line `//` comments skipped) and `bridge/Cargo.toml` and
`agent/Cargo.toml` (whole-line `#` comments skipped) for the spellings below, and fails
when a file contains a spelling that no row of this table lists for that file. It also
fails when a row lists a spelling its file no longer contains, so the table cannot rot in
either direction.

The spellings: `target_os = "macos"`; `Command::new("sips")`, `Command::new("afconvert")`,
`Command::new("security")`, `Command::new("launchctl")`, `Command::new("sandbox-exec")`;
`core_graphics`, `core_foundation`, `kind = "framework"`; `Library/`; the absolute macOS
tool paths `/usr/bin/`, `/bin/launchctl`, `/System/`, `/Applications/`; `resolve_bin(`;
and the tool names `"sips"`, `"afconvert"`, `"AFCONVERT"`, `"security"`, `"launchctl"`,
`"codesign"`, `"plutil"`, `"sandbox-exec"`, `"textutil"`, `"osascript"`, `"xcodebuild"` as
string literals; plus `"metal"` in a Cargo manifest. The code calls its tools by absolute
path, which is why `/usr/bin/` is the spelling that catches `sips`, `afconvert`, `plutil`
and `sandbox-exec`; `kind = "framework"` is what catches the Core Graphics FFI, which uses
no crate. The test also asserts that each dependency listed in the K01 spec is caught by at
least one spelling.

Matching scheme: a row covers the (file, spelling) pairs formed by its **File** cell (one
backticked repo-relative path) and each backticked entry of its **Spellings** cell. Several
rows may name one file. **Replacement** starts with `portable` (a Linux implementation),
`mac-edge` (delegate to the service on the Mac) or `drop`.

| Dependency | File | Symbol | Spellings | What it does | Breaks on Linux | Replacement |
|---|---|---|---|---|---|---|
| Core Graphics PDF rendering | `bridge/src/cgpdf.rs` | `render_pdf_pages`, `mod mac` (CoreGraphics FFI, `#[link(name = "CoreGraphics")]`) | `target_os = "macos"`, `kind = "framework"` | Rasterizes PDF pages for harnesses that cannot read a PDF (Codex `view_image`, the vision path, `inbound` staging) | Returns "PDF rendering needs macOS's Core Graphics"; a PDF attachment reaches a Codex or vision turn as an error | portable: a Linux PDF rasterizer (pdfium or poppler) behind the same function |
| `sips` HEIC to JPEG, attachments | `bridge/src/attachments.rs` | `convert_heic_to_jpeg` (`/usr/bin/sips`), and one macOS-gated PDF test | `/usr/bin/`, `target_os = "macos"` | Transcodes an iPhone HEIC photo to JPEG before the agent is told to read it | Spawn fails; a HEIC attachment is refused | portable: libheif (or an image crate with HEIF support) |
| `sips` HEIC to JPEG, vision | `bridge/src/vision.rs` | `transcode_heic_in` (`/usr/bin/sips`), and the macOS-gated fixture tests | `/usr/bin/`, `target_os = "macos"` | The same transcode for the vision preprocessing path | Spawn fails; HEIC images are not described | portable: the same replacement as attachments |
| `afconvert` audio decode | `bridge/src/speech/decode.rs` | `AFCONVERT` (`/usr/bin/afconvert`) and the decoder that spawns it; macOS-gated tests | `/usr/bin/`, `target_os = "macos"` | Decodes a recording (m4a, caf, …) to 16 kHz mono PCM for transcription | Spawn fails; every voice note is unreadable | portable: a Rust decoder (symphonia) or ffmpeg in the pod |
| whisper.cpp with Metal | `bridge/Cargo.toml` | `[target.'cfg(target_os = "macos")'.dependencies] whisper-rs` with `features = ["metal"]` | `target_os = "macos"`, `"metal"` | Builds local transcription against the Studio's GPU, shader library embedded | Builds without Metal: CPU only, far slower on the cluster's arm64 nodes | mac-edge: local Metal models run on the Mac service; the hosted engines in `speech/hosted.rs` are portable |
| whisper GPU switch | `bridge/src/speech/engine.rs` | `WhisperContextParameters { use_gpu: cfg!(target_os = "macos") }` | `target_os = "macos"` | Turns the GPU on only where Metal is compiled in | Nothing breaks; transcription runs on the CPU | mac-edge: follows the whisper row |
| Keychain read | `bridge/src/quota.rs` | `read_claude_credential` (`security find-generic-password`) | `Command::new("security")`, `"security"` | Reads Claude Code's OAuth credential from the login Keychain to poll subscription quota | No `security`, no Keychain: the quota poll fails and the usage line goes stale | portable: read the credential file Claude Code keeps on Linux, or have the core own the credential |
| launchd service control | `bridge/src/sentinel/mod.rs` | `resolve_bin("launchctl", &["/bin/launchctl"])`, the `~/Library/LaunchAgents/<label>.plist` lookup | `"launchctl"`, `/bin/launchctl`, `Library/` | The sentinel restarts, kickstarts and inspects the bridge's launchd jobs | No launchd: deploy and restart verbs cannot act | drop: Kubernetes rollouts replace it; the Mac service keeps its own launchd job |
| `codesign` deploy signing | `bridge/src/sentinel/mod.rs` | `resolve_bin("codesign", &["/usr/bin/codesign"])` (bridge 0.158.0) | `"codesign"` | Signs deployed binaries so their TCC identity survives a deploy | No codesign: the deploy verb has no signer | drop: no TCC on Linux, images are the unit of deploy |
| Sentinel binary fallbacks | `bridge/src/sentinel/mod.rs` | `resolve_bin(name, fallbacks)` and its macOS fallback paths for tailscale (`/Applications/Tailscale.app`), git, df, pgrep, qmd, node, cargo | `resolve_bin(`, `/usr/bin/`, `/Applications/` | Finds each probe's binary on `PATH`, else at a macOS path | The `PATH` lookup works; only the macOS fallbacks are dead | portable |
| macOS output parsers | `bridge/src/sentinel/probes.rs` | `parse_launchctl_print`, `parse_df_k`; fixtures shaped like `launchctl print` and macOS `df` | `/System/`, `Library/` | Reads `launchctl print` and macOS `df -k` output for health probes | `launchctl print` does not exist; `df` output differs in shape | drop: cluster health comes from Kubernetes |
| `plutil` plist environment | `bridge/src/bin/jesse-transcribe.rs` | `load_plist_env` (`/usr/bin/plutil`) | `/usr/bin/` | `--env-plist` loads the bridge's launchd environment into a manual transcription run | No plutil and no plist | drop: a pod's environment comes from its spec |
| `sandbox-exec` build sandbox | `bridge/src/buildsvc.rs` | `build_sandbox_profile`, the `/usr/bin/sandbox-exec` spawn, `confstr_dir`, `darwin_scratch_dirs` | `/usr/bin/`, `target_os = "macos"` | Runs `mcp__build__*` cargo builds and tests inside a macOS sandbox profile | Spawn fails and the build reports NOT RUN | portable: a Linux sandbox (bubblewrap or the build server's own pod) |
| `~/Library` default path | `bridge/src/config.rs` | the shadow log default `~/Library/Logs/jesse-shadow/shadow.jsonl` | `Library/` | Default location of the shadow comparison log | Creates `~/Library/Logs` on Linux, which works but is not where logs go | portable: an XDG state path or the state dir |
| `~/Library` audit output | `bridge/src/bin/shadow-audit.rs` | `Library/Logs/jesse-shadow-audit` output directory | `Library/` | Where the shadow audit writes its report | Same as above | portable |
| `~/Library` audit output | `bridge/src/bin/vaultqa-audit.rs` | `Library/Logs/jesse-vaultqa-audit` output directory | `Library/` | Where the vault-QA audit writes its report | Same as above | portable |
| iMCP server | `bridge/src/harness/claude_code.rs` | `mcp_imcp!` (`/Applications/iMCP.app/Contents/MacOS/imcp-server`), and the golden MCP config in its tests | `/Applications/` | iMessage history and Apple Maps search through the iMCP app, which needs a logged-in GUI session, Bonjour discovery and a security-scoped grant on `~/Library/Messages` | The app does not exist | mac-edge |
| Core Graphics tests in `inbound` | `bridge/src/inbound.rs` | `read_fixture` and the PDF staging tests gated to macOS | `target_os = "macos"` | Tests that a staged PDF is rasterized for Codex, which needs `cgpdf` | The tests are compiled out; the runtime path inherits the `cgpdf` row | portable: follows `cgpdf` |
| Test fixture path | `bridge/src/startup.rs` | a fixture naming `/Applications/X.app/...` for the unresolved MCP command check | `/Applications/` | Test data only | Nothing | portable: test fixture, no runtime dependency |

Not present in code today, and so not rows: `textutil` (document text extraction) and
`osascript` (iWork export through AppleScript) are queued work. Their spellings are already in
the scan, so the change that adds either one fails until it adds its row here.

## Conversation state

Measured on the Studio on **2026-10-07** with `du -sh` and `du -sk`, read only. The vault is
`JESSE_VAULT=$HOME/jesse`; the bridge's state directory is `JESSE_STATE_DIR`, unset on the
Studio, so `~/.jesse-bridge` (`codex_home_base` and the direct runtime both default to it).

### claude-code

**Where.** `~/.claude/projects/<cwd key>/<session_id>.jsonl`, resumed with `--resume
<session_id>` from `claude_code.rs`. The cwd key is the turn's working directory with every
character that is not ASCII alphanumeric replaced by `-` (`sessions::escape_project_path`,
used by `vault_sessions_dir(home, vault)`), which is the same key the CLI computes. For the
vault at `$HOME/jesse` that is `-Users-<user>-jesse`, and that directory exists on the Studio.
A turn whose cwd is not the vault (a scratch-dir one-shot, a code checkout) gets that
directory's own key and its own transcript directory.

**Read at the start of a resumed turn.** The session's `<session_id>.jsonl`; the whole
conversation is replayed from it.

**Written during the turn.** Appends to `<session_id>.jsonl` (a fresh one for a new
conversation or a retry), writes `<session_id>/subagents/…` for any subagent, and file
snapshots under `~/.claude/file-history/` for each edited file. The bridge itself writes one
per-turn settings file, `<state>/claude-settings/<job>.json` (the write-lock hooks, passed
with `--settings`), removed when the turn ends.

**Also read at start**, every turn:

- user settings `~/.claude/settings.json`; project settings and hooks in the vault's
  `.claude/settings.json` (`--setting-sources user,project`, never `local`);
- the vault's `CLAUDE.md` in the cwd (and `~/.claude/CLAUDE.md`, absent on the Studio);
- the auto-memory directory `~/.claude/projects/<cwd key>/memory/` (`MEMORY.md` and its
  notes);
- skills: user skills in `~/.claude/skills/`, project skills in the vault's
  `.claude/skills/`;
- `~/.claude.json` (CLI global state; MCP servers in it are ignored under
  `--strict-mcp-config`), and plugins under `~/.claude/plugins/`;
- credentials: the macOS login Keychain item Claude Code writes on `/login`; there is no
  `~/.claude/.credentials.json` on the Studio. A model on its own provider key takes
  `ANTHROPIC_*` from the environment instead.

| Path | What | du -sh | du -sk |
|---|---|---|---|
| `~/.claude/projects/-Users-<user>-jesse/` | the vault's transcripts: 7586 session files, 518 session directories, `memory/` | 3.9G | 4109112 |
| of which `*.jsonl` | session transcripts | | 2424292 |
| of which session directories | subagent transcripts | | 1684140 |
| `~/.claude/projects/-Users-<user>-jesse/memory/` | auto-memory | 540K | 540 |
| `~/.claude/projects/` | every project key, interactive sessions included | 4.6G | 4832800 |
| `~/.claude/file-history/` | edit snapshots | 112M | 114892 |
| `~/.claude/settings.json` | user settings | 4.0K | 4 |
| `~/.claude/skills/` | user skills | 8.5M | 8752 |
| `~/.claude/plugins/` | plugins | 17M | 17512 |
| `~/.claude.json` | CLI global state | 116K | 116 |
| `~/.claude/` | everything | 4.7G | 4977648 |
| vault `.claude/` | project settings, hooks, skills | 328K | 328 |
| vault `CLAUDE.md` | instructions | 40K | 40 |
| `~/.jesse-bridge/claude-settings/` | per-turn hook settings | 56K | 56 |

### codex

**Where.** One `CODEX_HOME` per conversation under `<state>/codex-homes/<uuid>/`
(`codex_home_base`). A conversation's first turn mints a fresh home (`codex_turn_home`) with a
copy of the canonical `auth.json`; a resumed turn finds the home its thread lives in
(`codex_home_for_turn`) through `<state>/codex-homes/index.json` (thread id to home), verified
by the rollout file actually being there, else by a bounded scan of the homes. Inside a home,
Codex files the thread at `sessions/<yyyy>/<mm>/<dd>/rollout-<ts>-<thread id>.jsonl`.

**Read at the start of a resumed turn.** The rollout file, Codex's own SQLite state in the
home (`state_5.sqlite`, `thread_history_1.sqlite`, `memories_1.sqlite`, `goals_1.sqlite`,
`queue_1.sqlite`, `logs_2.sqlite`), `config.toml` that Codex itself writes there (the bridge
never writes one; the posture is on the command line), and `models_cache.json`. Before the
turn the bridge re-copies `~/.codex/auth.json` into the home for a subscription turn, removes
it for a turn on its own provider key, and removes `hooks.json` (rewritten for a write turn).

**Written during the turn.** Appends to the rollout, writes the SQLite databases and their
WAL files, `shell_snapshots/`, `cache/`, `plugins/`, and any refreshed token into the home's
`auth.json` copy, which is thrown away (the canonical is never written back).

**Also read at start.** Only the canonical `~/.codex/auth.json`, copied. The CLI loads no
configuration, skills or memories from `~/.codex`: its home is the per-conversation directory.
(A read-level sandbox is read-only, not read-scoped, so the child could still `cat` them.)

| Path | What | du -sh | du -sk |
|---|---|---|---|
| `~/.jesse-bridge/codex-homes/` | 151 homes (median 6952 KB, largest 96652 KB) | 1.9G | 1961024 |
| of which `*/sessions/` | 161 rollout files | | 251148 |
| of which `*/*.sqlite*` | Codex state databases | | 838176 |
| of which `*/plugins`, `*/cache` | | | 72056 |
| `~/.jesse-bridge/codex-homes/index.json` | thread id to home | 4.0K | 4 |
| `~/.codex/auth.json` | the canonical credential, copied per turn | 8.0K | 8 |
| `~/.codex/` | the operator's interactive Codex, not read by turns beyond `auth.json` | 1.2G | 1221648 |

### direct

**Where.** `<state>/direct-threads/` (`DIRECT_THREADS_DIR`), one `FileThreadStore` thread per
conversation: `direct-<uuid>.jsonl` (the messages, in the agent crate's own format) and
`direct-<uuid>.meta.json`. The session id the bridge stores is the thread id.

**Read at the start of a resumed turn.** The thread's `.jsonl` and `.meta.json`, and the
vault's `CLAUDE.md` (framed as the vault's manual by the direct system prefix).

**Written during the turn.** Appends to the thread `.jsonl` and updates its `.meta.json`;
appends one line per provider call to `<state>/usage.jsonl`.

**Also read at start.** Nothing under `~/.claude` or `~/.codex`. Provider tokens come from the
environment (`JESSE_MODEL_<ID>_AUTH_TOKEN`).

| Path | What | du -sh | du -sk |
|---|---|---|---|
| `~/.jesse-bridge/direct-threads/` | 10 threads, 20 files | 500K | 500 |
| `~/.jesse-bridge/usage.jsonl` | per-call usage ledger | 32K | 32 |

### The bridge's own index, every harness

| Path | What | du -sh | du -sk |
|---|---|---|---|
| `~/.jesse-bridge/conversations.json` | conversation to harness session ids | 864K | 864 |
| `~/.jesse-bridge/context.json` | the context ledger fed into catch-up | 600K | 600 |
| `~/.jesse-bridge/titles.json` | conversation titles | 176K | 176 |
| `~/.jesse-bridge/` | the whole state directory | 6.4G | 6714472 |
