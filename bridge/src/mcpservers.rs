//! **THE MCP SERVER TABLE** — every MCP server a turn can be given, in ONE place.
//!
//! ---- WHAT THIS REPLACED ------------------------------------------------------
//!
//! Until 0.177.0 each server was a string fragment macro in `harness/claude_code.rs`
//! (`mcp_slack!()` and twenty-four siblings), concatenated into a dozen `*_MCP_CONFIG`
//! constants, while the environment each server needs lived in three separate tables in
//! `harness/codex.rs` that covered only the servers Codex happened to need help with. A
//! server's command, its transport, its credential and the variables it reads were four facts
//! in two files, and nothing made them agree.
//!
//! Now a server is a [`McpServerRow`]. Everything else is a VIEW:
//!
//!   * the Claude Code `--mcp-config` JSON for a set ([`mcp_config_json`]), byte for byte the
//!     string the old macros produced, so every containment record and argv fixture still
//!     matches ([`tests::the_table_reproduces_the_shipped_main_config_byte_for_byte`]);
//!   * the Codex `env_vars`, `env` and `bearer_token_env_var` tables
//!     ([`codex_env_passthrough`], [`codex_env_fixed`], [`codex_bearer_env`]);
//!   * the gateway's upstream spawn and call ([`crate::gateway`]), which is the reason the
//!     table exists: with the gateway on, the bridge is the only thing that starts or calls
//!     a server, and it gives each one exactly the variables its row names.
//!
//! ---- THE ENVIRONMENT COLUMN ---------------------------------------------------
//!
//! [`McpServerRow::env`] is the complete list of variable NAMES a `bridge_stdio` server is
//! started with, beside the five every process needs ([`BASE_ENV`]). Never a value. It was
//! seeded from the Codex tables (which Codex's own environment scrub had already forced to be
//! complete for the servers they named) and extended to every other server by running each
//! with a stripped environment and reading what it failed on (2026-10-13, launchd's PATH):
//!
//!   * `proxmox`, `plex`, `kubernetes`, `whatsapp`, `google-perseido`, `browser`, `qmd` list
//!     their tools AND answer a read call with no variable beyond [`BASE_ENV`]: each reads its
//!     own launcher or env file (`mcp-proxmox`, `plex-mcp` and its `~/.config/jesse-plex`,
//!     `jesse-k8s-mcp` and its kubeconfig, `workspace-mcp-perseido` and its own directory).
//!     `kubernetes-cloud` (0.188.0) does the same through `jesse-k8s-cloud-mcp` and its own
//!     kubeconfig (2026-10-28, launchd's PATH and `HOME` only).
//!   * `google` with no variable lists its tools and then fails every call ("OAuth client
//!     credentials not found"); `slack` with no variable does not start. Their rows carry the
//!     Codex set, each name of which was needed there.
//!   * `imcp` reaches the iMCP app over Bonjour, not through any variable.
//!
//! An HTTP row's `env` is empty: its credential goes on the wire from the bridge, named in
//! [`HttpAuthSpec`], and no process is started.
//!
//! ---- WHAT A ROW DOES NOT CARRY ------------------------------------------------
//!
//! The GRANTED TOOLS. They are derived from the allowlist (`DEFAULT_ALLOWED_TOOLS` and its
//! per-capability siblings) through [`crate::harness::codex::granted_mcp_tools`], exactly as
//! Codex's `enabled_tools` always has been, so there is one grant and it lives in one string.
//! A row carries only how each granted tool is CLASSED ([`McpServerRow::class_of`]).

use crate::{granted_mcp_tools, CODEX_ID};

/// The five variables every `bridge_stdio` server is started with, beside its own row's
/// names. Nothing else crosses: the spawn is `env_clear()` first.
pub const BASE_ENV: &[&str] = &["PATH", "HOME", "TMPDIR", "LANG", "TZ"];

/// How the bridge reaches a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// A stdio child. Started by the agent child's CLI with the gateway off, by the bridge's
    /// gateway with it on.
    BridgeStdio,
    /// Streamable HTTP at [`McpServerRow::url`].
    Http,
    /// Rust tools inside the bridge, served by the gateway with no process and no socket of
    /// their own. No shipped row uses it yet; the cluster's Mac-less servers will.
    InProcess,
}

impl Transport {
    pub fn label(self) -> &'static str {
        match self {
            Transport::BridgeStdio => "bridge_stdio",
            Transport::Http => "http",
            Transport::InProcess => "in_process",
        }
    }
}

/// How an HTTP server authenticates. Variable NAMES only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpAuthSpec {
    /// No credential (`roon`, `tag1`).
    None,
    /// `Authorization: Bearer <value of VAR>`.
    Bearer(&'static str),
    /// Each `(header, VAR)`: the header carries the variable's value. Clockify's `x-api-key`
    /// was this shape; a bearer there passes `tools/list` and fails every call.
    Headers(&'static [(&'static str, &'static str)]),
    /// Mutual TLS with the named client certificate (`[gateway.client_certs.<name>]`). Gateway
    /// only: a CLI child cannot present one.
    ClientCert(&'static str),
}

/// Where a server's calls are made from once the gateway is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// The bridge brokers every call; the child is handed a credential-free stub.
    Gateway,
    /// The child starts the server itself, as before the gateway. Only for a server with no
    /// credential (`qmd`).
    Child,
}

impl Placement {
    pub fn label(self) -> &'static str {
        match self {
            Placement::Gateway => "gateway",
            Placement::Child => "child",
        }
    }
}

/// How one tool is classed: the vocabulary the `direct` harness's levels are defined over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    /// Reads, and sends nothing caller-authored off the host beyond the server's own job.
    Read,
    /// Changes something: a device, a VM, a playlist, a page's form.
    Write,
    /// A read that sends caller-authored bytes to a third party (a search query to a maps
    /// provider, a URL to a browser).
    Egress,
}

impl ToolClass {
    pub fn label(self) -> &'static str {
        match self {
            ToolClass::Read => "read",
            ToolClass::Write => "write",
            ToolClass::Egress => "egress",
        }
    }

    /// The `direct` harness's class for this tool.
    ///
    /// **`Write` MAPS TO `VaultWrite`, NOT `ExternalWrite`**, and that is the house grant's
    /// precedent (0.170.0) applied to every server: `ExternalWrite` is permitted at NO level,
    /// by design, so mapping to it would withhold from `direct` every write tool the Claude
    /// Code allowlist grants at the write level — the gap this table exists to close. What
    /// matters for the boundary is the level a tool is exposed at, and `VaultWrite` is
    /// exposed at exactly the level the allowlist grants writes at: `write`, never `read`.
    pub fn agent_class(self) -> jesse_agent::tools::ActionClass {
        use jesse_agent::tools::ActionClass;
        match self {
            ToolClass::Read => ActionClass::Read,
            ToolClass::Write => ActionClass::VaultWrite,
            ToolClass::Egress => ActionClass::Egress,
        }
    }
}

/// The result cap a row takes unless it names its own: 8 MiB, which holds the largest honest
/// text reply any server here sends (a Kubernetes pod log, a batch of mail threads).
pub const DEFAULT_RESULT_CAP: usize = 8 * 1024 * 1024;

/// For servers that return images: a full-page screenshot, a rendered floor plan.
const IMAGE_RESULT_CAP: usize = 32 * 1024 * 1024;

/// One MCP server.
#[derive(Debug, Clone, Copy)]
pub struct McpServerRow {
    /// The server's name: the middle of every `mcp__<server>__<tool>` grant.
    pub name: &'static str,
    pub transport: Transport,
    /// `bridge_stdio`: the program. A bare name is resolved on the spawning process's `PATH`.
    pub command: &'static str,
    /// `bridge_stdio`: its arguments.
    pub args: &'static [&'static str],
    /// `http`: the endpoint. `${JESSE_HOUSE_URL}` is a placeholder the deployment fills.
    pub url: &'static str,
    /// `http`: how it authenticates.
    pub auth: HttpAuthSpec,
    /// `bridge_stdio`: the variable NAMES it is started with, beside [`BASE_ENV`].
    pub env: &'static [&'static str],
    /// A variable that receives the TURN'S HARNESS ID, for the one server whose output depends
    /// on which CLI will read it (`inbound` prepares a staged file for that harness's `Read`).
    /// The value is the caller's to supply: no gateway code reads the harness.
    pub harness_env: Option<&'static str>,
    pub placement: Placement,
    /// The most one `tools/call` result may carry through the gateway, in bytes.
    pub result_cap: usize,
    /// The class every tool takes unless listed in `write_tools` or `egress_tools`.
    pub default_class: ToolClass,
    pub write_tools: &'static [&'static str],
    pub egress_tools: &'static [&'static str],
    /// Reads, on a row whose default is not `read` (Home Assistant, where nearly every tool
    /// actuates something and the three reads are the exceptions).
    pub read_tools: &'static [&'static str],
    /// No shipped set a live turn uses carries it; kept so retired set labels still resolve.
    pub retired: bool,
}

impl McpServerRow {
    /// How this server's `tool` is classed.
    pub fn class_of(&self, tool: &str) -> ToolClass {
        if self.write_tools.contains(&tool) {
            ToolClass::Write
        } else if self.egress_tools.contains(&tool) {
            ToolClass::Egress
        } else if self.read_tools.contains(&tool) {
            ToolClass::Read
        } else {
            self.default_class
        }
    }

    /// The tools `allowed_tools` grants on this server, by their name on the server.
    pub fn granted_tools(&self, allowed_tools: &str) -> Vec<String> {
        granted_mcp_tools(allowed_tools, self.name)
    }
}

const fn stdio(
    name: &'static str,
    command: &'static str,
    args: &'static [&'static str],
    env: &'static [&'static str],
) -> McpServerRow {
    McpServerRow {
        name,
        transport: Transport::BridgeStdio,
        command,
        args,
        url: "",
        auth: HttpAuthSpec::None,
        env,
        harness_env: None,
        placement: Placement::Gateway,
        result_cap: DEFAULT_RESULT_CAP,
        default_class: ToolClass::Read,
        write_tools: &[],
        egress_tools: &[],
        read_tools: &[],
        retired: false,
    }
}

const fn http(name: &'static str, url: &'static str, auth: HttpAuthSpec) -> McpServerRow {
    McpServerRow {
        name,
        transport: Transport::Http,
        command: "",
        args: &[],
        url,
        auth,
        env: &[],
        harness_env: None,
        placement: Placement::Gateway,
        result_cap: DEFAULT_RESULT_CAP,
        default_class: ToolClass::Read,
        write_tools: &[],
        egress_tools: &[],
        read_tools: &[],
        retired: false,
    }
}

const fn writes(mut r: McpServerRow, tools: &'static [&'static str]) -> McpServerRow {
    r.write_tools = tools;
    r
}

const fn egress(mut r: McpServerRow, tools: &'static [&'static str]) -> McpServerRow {
    r.egress_tools = tools;
    r
}

const fn reads(mut r: McpServerRow, tools: &'static [&'static str]) -> McpServerRow {
    r.read_tools = tools;
    r
}

const fn class(mut r: McpServerRow, c: ToolClass) -> McpServerRow {
    r.default_class = c;
    r
}

const fn cap(mut r: McpServerRow, bytes: usize) -> McpServerRow {
    r.result_cap = bytes;
    r
}

/// The Home Assistant endpoint: the one deployment address that must live in tracked source.
/// See `scripts/ci-guards.sh` (5b).
pub const HOME_ASSISTANT_MCP_URL: &str = "http://100.105.110.60:8123/api/mcp"; // ci-guards:deployment-address

use crate::house::HOUSE_URL_PLACEHOLDER;

// ---- PROVENANCE OF THE ENVIRONMENT COLUMN (moved from `harness/codex.rs`, 0.177.0) ----
//
// The rationale the three Codex tables carried, kept with the rows they became.
//
// The environment variables each MCP server needs forwarded into it, BY NAME.
//
// Codex-only: Claude Code's MCP subprocesses inherit the bridge's environment, so this
// table has no Claude Code equivalent and must not grow one. Empty for every server that
// needs no secret — qmd and browser take nothing, and adding a name here that a server does
// not need widens what a subprocess can read for no gain.
//
// A NAME, NOT A VALUE. The bridge never reads the token; it tells Codex which variable to
// pass through from its own environment, where launchd put it. Nothing here is a secret and
// nothing here belongs in the plist beyond the value itself.
// **THE NAMES HERE ARE THE SERVERS' OWN, NOT THE PLIST'S.** The LaunchAgent stores
// `JESSE_*`-prefixed variables (`JESSE_UNIFI_USERNAME`, `JESSE_GITHUB_PAT`, …) so the
// bridge's own environment stays legible, but each server reads the name its vendor chose
// (`UNIFI_USERNAME`, `GITHUB_PERSONAL_ACCESS_TOKEN`, …). Codex's `env_vars` forwards a
// variable BY NAME and cannot rename one in flight, so [`crate::export_mcp_server_env`]
// republishes the plist values under the vendor names in the bridge's process environment
// before any child spawns — which is also what makes them reachable on Claude Code, whose
// MCP subprocesses simply inherit. Name a variable here that `export_mcp_server_env` does
// not publish and the server silently registers zero tools.
//
// `proxmox` is deliberately ABSENT and that is not an oversight: it loads its credentials
// itself from `__dirname/../.env`, relative to its own file rather than the cwd, so it needs
// nothing forwarded and forwarding anything would widen the subprocess for no gain.
//
// **`whatsapp`, `imcp` and `google-perseido` are ABSENT for the same class of reason,
// and each was confirmed rather than assumed** — an unforwarded secret means a server that
// starts, registers zero tools, and reports itself "not installed", so absence here is a
// claim that has to be checked:
//   * `whatsapp` reads a SQLite store it locates from its own `__file__`, and reaches the Go
//     bridge at a hard-coded `http://localhost:8080/api`. Neither is configurable by
//     environment, so there is nothing to name.
//   * `imcp` reads NOTHING ITSELF and holds no secret. It finds the iMCP app over Bonjour
//     (`_mcp._tcp` on `local.`) and proxies MCP to it; the app does the reading, under its
//     own identity, through a security-scoped grant on `~/Library/Messages`. There is no
//     token to forward and no path to configure — the endpoint is DISCOVERED, not addressed
//     — so `env_vars` for this server is correctly empty rather than merely unfilled.
//     CONFIRMED against the running 1.4.1 server on 2026-08-11 rather than assumed: it was
//     driven to a successful `messages_fetch` from a launchd-spawned job with an environment
//     carrying nothing of its own. Note what this replaces: `mac-messages-mcp` needed a Full
//     Disk Access grant, which was equally unforwardable (TCC is a property of a binary, not
//     a variable) but ALSO never worked, because TCC blames the responsible process.
//   * `google-perseido` needs the SAME four variables the `google` entry forwards, and must
//     NOT be given them: they carry the tag1 client and the tag1 credentials directory, and
//     `workspace-mcp` resolves the client from the environment BEFORE its client-secret
//     file, so forwarding them is precisely how this instance would silently authenticate as
//     the wrong account. The `workspace-mcp-perseido` launcher unsets the two `GOOGLE_OAUTH_*`
//     names for exactly that reason and then points the client and credentials directory at
//     the Perseido ones itself. Codex's env scrub happens to do the same job here, but the
//     launcher does not rely on it — Claude Code's MCP children inherit the bridge's full
//     environment, tag1 client included, and the same launcher has to hold there too.
// NO `NODE_OPTIONS` HERE, and that is a measured decision rather than an omission. The
// JMAP fork needed `--experimental-require-module` on older node; it does NOT on the
// 22.20 first on the bridge's PATH, where `require(esm)` is unflagged (verified
// 2026-08-09: 3 tools registered with the variable unset). Forwarding it anyway would
// have meant publishing a global node flag into the bridge's own environment, which
// every other node MCP child — slack, browser, qmd — would then inherit for no reason.
// THE BRIDGE'S OWN THREE SERVERS read the bridge's own `JESSE_*` names, and every name
// each one reads is listed, not only the ones this deployment's plist sets today: Claude
// Code's children inherit the whole environment, so a variable an operator adds later
// reaches them, and naming it here is what makes it reach Codex's too. An unset name is
// simply not forwarded.
//
// `build` constructs the bridge's `Config` but reads only the vault and the home from it;
// `HOME` survives Codex's scrub, `JESSE_VAULT` does not.
// `places` without `JESSE_PLACES_GOOGLE_API_KEY` still registers its two tools and answers
// from OpenStreetMap alone, so a missing key here would be a silent downgrade rather than
// a missing server. `JESSE_STATE_DIR` locates the Google spend ledger.
// `inbound` fetches attachments with the SAME mail credentials `google` and `fastmail`
// read, and stages them under the vault. Without `JESSE_VAULT` it would stage into its own
// cwd; without the mail names it would list attachments it could not fetch.
// `healthdb` (0.173.0) reads two PATHS and a ledger path, never a credential: the bridge
// sets them on the Codex child that loads the server ([`crate::apply_healthdb_env`]) and
// they are not in the plist. Without the first two it exits at once and registers nothing.
// `clockify-rs` (0.175.0) is a stdio server, so its key travels by NAME here and never in
// the bearer table. `JESSE_CLOCKIFY_API_KEY` is set in the plist; the `clockify-rs-mcp`
// launcher renames it to the `CLOCKIFY_API_KEY` the server reads and `exec`s the binary.
// Without it the launcher exits at once and registers nothing.
// `runn-rs` (0.180.0) is a stdio server, so its token travels by NAME here and never in the
// bearer table. `JESSE_RUNN_API_TOKEN` is set in the plist; the `runn-rs-mcp` launcher renames
// it to the `RUNN_API_TOKEN` the server reads, and sets the Runn host and the caller email
// itself, so neither travels from here. Without the token the launcher exits at once and
// registers nothing.
// Fixed, NON-SECRET environment values a stdio server is started with on Codex, set with
// Codex's `env` key rather than forwarded from the bridge's environment.
//
// One entry, and it exists because the bridge's process environment is shared by every
// harness and so cannot say which one a child serves. `jesse-inbound-mcp` prepares a staged
// attachment for the harness that will open it — a PDF that Claude Code's `Read` takes
// directly has to become page images for Codex's `view_image` — and it learns which from
// `JESSE_INBOUND_HARNESS`, defaulting to Claude Code when unset. This table is what makes
// that default correct on both harnesses.
//
// **NEVER A CREDENTIAL.** A value here is on the child's argv and in `ps`. Anything secret is
// forwarded BY NAME through [`CODEX_MCP_ENV_PASSTHROUGH`] or [`CODEX_MCP_BEARER_ENV`].
// The environment variable holding each HTTP MCP server's BEARER TOKEN, BY NAME.
//
// The HTTP counterpart of [`CODEX_MCP_ENV_PASSTHROUGH`], and separate from it because the
// mechanisms are genuinely different rather than merely differently spelled: `env_vars`
// populates a subprocess's environment, which an HTTP server does not have, while
// `bearer_token_env_var` tells Codex to read the variable and send an `Authorization`
// header itself. Putting an HTTP server in the stdio table would forward a token to a
// process that does not exist and leave the request unauthenticated.
//
// A NAME, NOT A VALUE — the same rule and the same reason as the stdio table. `roon` is
// deliberately ABSENT: the Roon bridge serves plain HTTP on the LAN with no auth at all, so
// there is no token to name (see SECURITY.md; this adds no credential surface).
//
// `HA_MCP_TOKEN` is a Home Assistant long-lived access token, set in the LaunchAgent plist.
// Claude Code reaches the SAME variable through `${HA_MCP_TOKEN}` expansion in the
// `Authorization` header of [`MAIN_CHILD_MCP_CONFIG`] — one variable, two spellings, and
// they must not drift apart.
//
// `RYBBIT_API_KEY` is a Rybbit personal API key for tag1.com's analytics, scoped to read,
// also set in the plist and reached by Claude Code as `${RYBBIT_API_KEY}` in the same way.
//
// `JESSE_HOUSE_TOKEN` (0.169.0) is the house model's read and write token. It is NOT in the
// plist: the bridge reads it from the `[house]` table's `token_file` at start and sets it on
// each child whose rendered set loads `house` (see [`crate::apply_house_env`]), and Claude
// Code reaches it as `${JESSE_HOUSE_TOKEN}` in that server's header.

/// EVERY SERVER. Order is declaration order and carries no meaning; a SET's order (below) is
/// what fixes the order of a rendered config.
pub const MCP_SERVERS: &[McpServerRow] = &[
    McpServerRow {
        placement: Placement::Child,
        ..stdio("qmd", "qmd", &["mcp"], &[])
    },
    stdio(
        "slack",
        "npx",
        &["-y", "slack-mcp-server@latest", "--transport", "stdio"],
        &["SLACK_MCP_XOXP_TOKEN"],
    ),
    cap(
        egress(
            writes(
                stdio(
                    "browser",
                    "npx",
                    &[
                        "-y",
                        "@playwright/mcp@latest",
                        "--headless",
                        "--isolated",
                        "--output-dir",
                        "/tmp/jesse-browser",
                        "--output-max-size",
                        "104857600",
                    ],
                    &[],
                ),
                &[
                    "browser_click",
                    "browser_type",
                    "browser_fill_form",
                    "browser_press_key",
                    "browser_select_option",
                    "browser_drag",
                    "browser_handle_dialog",
                ],
            ),
            &["browser_navigate", "browser_navigate_back", "browser_tabs"],
        ),
        IMAGE_RESULT_CAP,
    ),
    reads(
        class(
            http(
                "homeassistant",
                HOME_ASSISTANT_MCP_URL,
                HttpAuthSpec::Bearer("HA_MCP_TOKEN"),
            ),
            ToolClass::Write,
        ),
        &["GetLiveContext", "GetDateTime", "todo_get_items"],
    ),
    writes(
        http("roon", "http://10.40.0.2:8088/mcp", HttpAuthSpec::None),
        &["hifi_control", "hifi_play"],
    ),
    stdio(
        "google",
        "workspace-mcp",
        &[
            "--single-user",
            "--read-only",
            "--tools",
            "calendar",
            "gmail",
            "drive",
        ],
        &[
            "GOOGLE_OAUTH_CLIENT_ID",
            "GOOGLE_OAUTH_CLIENT_SECRET",
            "WORKSPACE_MCP_CREDENTIALS_DIR",
            "WORKSPACE_ATTACHMENT_DIR",
            "USER_GOOGLE_EMAIL",
        ],
    ),
    stdio(
        "github",
        "github-mcp-server",
        &[
            "stdio",
            "--read-only",
            "--toolsets",
            "repos,actions,issues,pull_requests",
        ],
        &["GITHUB_PERSONAL_ACCESS_TOKEN"],
    ),
    stdio(
        "fastmail",
        "npx",
        &["-y", "github:jeremyandrews/jmap-mcp-server"],
        &["JMAP_TOKEN", "JMAP_SESSION_URL"],
    ),
    writes(
        stdio(
            "unifi",
            "unifi-network-mcp",
            &[],
            &[
                "UNIFI_USERNAME",
                "UNIFI_PASSWORD",
                "UNIFI_HOST",
                "UNIFI_PORT",
                "UNIFI_CONTROLLER_TYPE",
                "UNIFI_VERIFY_SSL",
                "UNIFI_SITE",
            ],
        ),
        &["unifi_execute", "unifi_batch"],
    ),
    egress(
        stdio(
            "routeros",
            "routeros-mcp",
            &[],
            &["ROUTEROS_DEVICES_CONFIG", "ROUTEROS_SSH_PORT"],
        ),
        &["ping"],
    ),
    writes(
        stdio("proxmox", "mcp-proxmox", &[], &[]),
        &[
            "proxmox_execute_vm_command",
            "proxmox_create_lxc",
            "proxmox_create_vm",
            "proxmox_start_lxc",
            "proxmox_start_vm",
            "proxmox_stop_lxc",
            "proxmox_stop_vm",
            "proxmox_delete_lxc",
            "proxmox_delete_vm",
            "proxmox_reboot_lxc",
            "proxmox_reboot_vm",
            "proxmox_shutdown_lxc",
            "proxmox_shutdown_vm",
            "proxmox_pause_vm",
            "proxmox_resume_vm",
            "proxmox_clone_lxc",
            "proxmox_clone_vm",
            "proxmox_resize_lxc",
            "proxmox_resize_vm",
            "proxmox_create_snapshot_lxc",
            "proxmox_create_snapshot_vm",
            "proxmox_rollback_snapshot_lxc",
            "proxmox_rollback_snapshot_vm",
            "proxmox_delete_snapshot_lxc",
            "proxmox_delete_snapshot_vm",
            "proxmox_create_backup_lxc",
            "proxmox_create_backup_vm",
            "proxmox_restore_backup_lxc",
            "proxmox_restore_backup_vm",
            "proxmox_delete_backup",
            "proxmox_add_disk_vm",
            "proxmox_add_mountpoint_lxc",
            "proxmox_resize_disk_vm",
            "proxmox_resize_disk_lxc",
            "proxmox_remove_disk_vm",
            "proxmox_remove_mountpoint_lxc",
            "proxmox_move_disk_vm",
            "proxmox_move_disk_lxc",
            "proxmox_add_network_vm",
            "proxmox_add_network_lxc",
            "proxmox_update_network_vm",
            "proxmox_update_network_lxc",
            "proxmox_remove_network_vm",
            "proxmox_remove_network_lxc",
            "proxmox_migrate_vm",
            "proxmox_convert_to_template",
            "proxmox_set_cloudinit",
        ],
    ),
    stdio("whatsapp", "whatsapp-mcp", &[], &[]),
    egress(
        stdio(
            "imcp",
            "/Applications/iMCP.app/Contents/MacOS/imcp-server",
            &[],
            &[],
        ),
        &["maps_search"],
    ),
    stdio(
        "google-perseido",
        "workspace-mcp-perseido",
        &[
            "--single-user",
            "--read-only",
            "--tools",
            "calendar",
            "gmail",
            "drive",
        ],
        &[],
    ),
    class(
        stdio("build", "jesse-build-mcp", &[], &["JESSE_VAULT"]),
        ToolClass::Write,
    ),
    class(
        stdio(
            "places",
            "jesse-places-mcp",
            &[],
            &[
                "JESSE_PLACES_OVERPASS_URL",
                "JESSE_PLACES_NOMINATIM_URL",
                "JESSE_PLACES_USER_AGENT",
                "JESSE_PLACES_MIN_INTERVAL_MS",
                "JESSE_PLACES_CACHE_TTL_SECS",
                "JESSE_PLACES_HTTP_TIMEOUT_SECS",
                "JESSE_PLACES_TIMEZONE",
                "JESSE_PLACES_PROVIDER",
                "JESSE_PLACES_GOOGLE_API_KEY",
                "JESSE_PLACES_GOOGLE_BASE_URL",
                "JESSE_PLACES_GOOGLE_MAX_CALLS",
                "JESSE_PLACES_GOOGLE_MAX_RICH_CALLS",
                "JESSE_PLACES_GOOGLE_WINDOW_SECS",
                "JESSE_PLACES_GOOGLE_LEDGER",
                "JESSE_STATE_DIR",
            ],
        ),
        ToolClass::Egress,
    ),
    McpServerRow {
        harness_env: Some("JESSE_INBOUND_HARNESS"),
        ..stdio(
            "inbound",
            "jesse-inbound-mcp",
            &[],
            &[
                "JESSE_VAULT",
                "JESSE_INBOUND_TTL_SECS",
                "JESSE_INBOUND_MAX_DOCUMENT_BYTES",
                "JESSE_INBOUND_HTTP_TIMEOUT_SECS",
                "JESSE_INBOUND_WHATSAPP_API",
                "JESSE_INBOUND_WHATSAPP_MEDIA_ROOT",
                "JESSE_INBOUND_GMAIL_PERSEIDO_CREDS",
                "WORKSPACE_MCP_CREDENTIALS_DIR",
                "JMAP_SESSION_URL",
                "JMAP_TOKEN",
                "JESSE_VISION_PDF_PAGE_CAP",
                "JESSE_VISION_PDF_DPI",
                "JESSE_VISION_MAX_TOKENS",
                "JESSE_VISION_TIMEOUT_SECS",
            ],
        )
    },
    writes(
        stdio(
            "kubernetes",
            "jesse-k8s-mcp",
            &["--toolsets", "core,config"],
            &[],
        ),
        &[
            "pods_run",
            "pods_exec",
            "pods_delete",
            "resources_create_or_update",
            "resources_scale",
            "resources_delete",
        ],
    ),
    class(
        http(
            "rybbit",
            "https://app.rybbit.io/api/mcp",
            HttpAuthSpec::Bearer("RYBBIT_API_KEY"),
        ),
        ToolClass::Egress,
    ),
    class(
        http("tag1", "https://www.tag1.com/mcp", HttpAuthSpec::None),
        ToolClass::Egress,
    ),
    writes(
        stdio("plex", "plex-mcp", &[], &[]),
        &[
            "playlist_create",
            "playlist_edit",
            "playlist_add_to",
            "playlist_remove_from",
            "playlist_delete",
            "collection_create",
            "collection_add_to",
            "collection_remove_from",
            "collection_edit",
            "collection_delete",
            "media_edit_metadata",
        ],
    ),
    McpServerRow {
        retired: true,
        ..http(
            "clockify",
            "https://api.clockify.me/mcp-server/mcp",
            HttpAuthSpec::Headers(&[("x-api-key", "JESSE_CLOCKIFY_API_KEY")]),
        )
    },
    // `house`'s classes are `house_grant_shape`'s (0.170.0): every read is egress (the server
    // is off the host), every write is a write.
    cap(
        writes(
            class(
                http(
                    "house",
                    HOUSE_URL_PLACEHOLDER,
                    HttpAuthSpec::Bearer("JESSE_HOUSE_TOKEN"),
                ),
                ToolClass::Egress,
            ),
            &crate::house::HOUSE_WRITE_TOOLS,
        ),
        IMAGE_RESULT_CAP,
    ),
    class(
        stdio(
            "healthdb",
            "jesse-healthdb-mcp",
            &[],
            &[
                "JESSE_HEALTHDB_RO_URL_FILE",
                "JESSE_HEALTHDB_CA_FILE",
                "JESSE_HEALTHDB_MCP_LEDGER",
            ],
        ),
        ToolClass::Egress,
    ),
    stdio(
        "clockify-rs",
        "clockify-rs-mcp",
        &[],
        &["JESSE_CLOCKIFY_API_KEY"],
    ),
    stdio("runn-rs", "runn-rs-mcp", &[], &["JESSE_RUNN_API_TOKEN"]),
    // The work account's SHEETS, write capable, as a third `workspace-mcp` instance (0.182.0).
    // A separate row because `--read-only` is per server: dropping it from `google` would
    // register Gmail send and draft, Calendar writes and Drive mutation there too. This one
    // loads `sheets` alone, so its token carries `spreadsheets`, `spreadsheets.readonly` and
    // `drive.readonly` (with the three identity scopes) and nothing that sends or shares.
    //
    // `--tool-tier complete` is the narrowest switch that still registers conditional
    // formatting (`extended` stops at `format_sheet_range`), and it drops `start_google_auth`
    // at the root. `manage_spreadsheet_comment` registers and is DELIBERATELY UNGRANTED: a
    // comment can @mention people, which mails them, and that is a send path.
    //
    // The command is the `workspace-mcp-sheets` launcher (`bridge/launchers/`), which points
    // the server at its OWN credentials directory, `~/.config/jesse-google-sheets/creds`. The
    // bridge's environment carries `google`'s directory, so `WORKSPACE_MCP_CREDENTIALS_DIR` is
    // deliberately NOT forwarded here; the launcher sets it. The client pair and the account
    // are `google`'s own.
    writes(
        stdio(
            "google-sheets",
            "workspace-mcp-sheets",
            &[
                "--single-user",
                "--tools",
                "sheets",
                "--tool-tier",
                "complete",
            ],
            &[
                "GOOGLE_OAUTH_CLIENT_ID",
                "GOOGLE_OAUTH_CLIENT_SECRET",
                "USER_GOOGLE_EMAIL",
            ],
        ),
        &GOOGLE_SHEETS_WRITE_TOOLS,
    ),
    // THE CLOUD CLUSTER (0.188.0): the same pinned binary and toolsets as `kubernetes`, through
    // the `jesse-k8s-cloud-mcp` launcher, which points it at kc1 as the ServiceAccount
    // `jesse:jesse`. The six writers are classed `write` as on `kubernetes`. What each tool can
    // actually do there is decided by kc1's RBAC and admission policy, not by this row.
    writes(
        stdio(
            "kubernetes-cloud",
            "jesse-k8s-cloud-mcp",
            &["--toolsets", "core,config"],
            &[],
        ),
        &[
            "pods_run",
            "pods_exec",
            "pods_delete",
            "resources_create_or_update",
            "resources_scale",
            "resources_delete",
        ],
    ),
    // CODING AGENTS ON THE CLUSTER (0.186.0), served inside the bridge by
    // [`crate::devjob::DevJobs`]: no process, no socket, and the only row whose credential (the
    // kubeconfig) never leaves the bridge process. `in_process` rows render only with the gateway
    // on ([`render_in_process_entry`]), so a gateway-off child never sees it, and only when
    // `[devjob] enabled` registered the server. `devjob_start` creates a Job that can push a
    // branch and open a pull request, so it is a write; the other two only read.
    McpServerRow {
        transport: Transport::InProcess,
        ..writes(
            http(crate::devjob::DEVJOB_SERVER, "", HttpAuthSpec::None),
            &[crate::devjob::TOOL_START],
        )
    },
];

/// The `google-sheets` tools that change a spreadsheet: granted on write turns only, and
/// classed `write` so the `direct` harness exposes them at the same level.
pub const GOOGLE_SHEETS_WRITE_TOOLS: [&str; 8] = [
    "create_spreadsheet",
    "create_sheet",
    "modify_sheet_values",
    "append_table_rows",
    "format_sheet_range",
    "manage_conditional_formatting",
    "resize_sheet_dimensions",
    "move_sheet_rows",
];

/// The `google-sheets` tools that only read, granted beside the writes.
pub const GOOGLE_SHEETS_READ_TOOLS: [&str; 5] = [
    "list_spreadsheets",
    "get_spreadsheet_info",
    "read_sheet_values",
    "list_sheet_tables",
    "list_spreadsheet_comments",
];

/// The row for `name`, or `None`.
pub fn row(name: &str) -> Option<&'static McpServerRow> {
    #[cfg(feature = "containment-probe")]
    let mut rows = MCP_SERVERS.iter().chain(PROBE_ROWS.iter());
    #[cfg(not(feature = "containment-probe"))]
    let mut rows = MCP_SERVERS.iter();
    rows.find(|r| r.name == name)
}

/// THE GATEWAY'S PROBE UPSTREAMS, one per transport, for its tests and its containment probes.
/// Present only in a `containment-probe` build, so no deployed bridge can resolve them; no
/// shipped set names them and no shipped allowlist grants them.
///
/// `gwprobe` is `jesse-gateway-probe-server` started by the bridge, with the two variables
/// that tell it where to log calls and dump its environment as its whole environment column.
/// `gwprobe-http` is the same server behind `jesse-mcp-http-adapter`, its URL supplied per run
/// by an `[mcp.gwprobe-http]` override. `gwprobe-inproc` is served inside the bridge by
/// whatever the test registers.
#[cfg(feature = "containment-probe")]
pub const PROBE_ROWS: &[McpServerRow] = &[
    writes(
        stdio(
            "gwprobe",
            "jesse-gateway-probe-server",
            &[],
            &["JESSE_GWPROBE_LOG", "JESSE_GWPROBE_ENV_DUMP"],
        ),
        &["probe_hidden"],
    ),
    writes(
        http(
            "gwprobe-http",
            "http://127.0.0.1:9/mcp",
            HttpAuthSpec::Bearer("JESSE_GWPROBE_HTTP_TOKEN"),
        ),
        &["probe_hidden"],
    ),
    McpServerRow {
        transport: Transport::InProcess,
        ..writes(
            http("gwprobe-inproc", "", HttpAuthSpec::None),
            &["probe_hidden"],
        )
    },
];

// ---- THE SHIPPED SETS -----------------------------------------------------------------
//
// A set is an ORDERED list of row names. Its order is the order of the rendered config, which
// the containment record and the argv fixtures compare byte for byte, so a set is never
// re-sorted. Each `*_MCP_CONFIG` in `harness/claude_code.rs` is one of these, rendered.

/// qmd, slack, browser, Home Assistant, Roon: the main set from 0.67.0.
pub const HOUSE_SET: &[&str] = &["qmd", "slack", "browser", "homeassistant", "roon"];

/// [`HOUSE_SET`] plus the morning-routine servers.
pub const MORNING_SET: &[&str] = &[
    "qmd",
    "slack",
    "browser",
    "homeassistant",
    "roon",
    "google",
    "github",
    "fastmail",
    "unifi",
    "routeros",
    "proxmox",
];

/// [`MORNING_SET`] plus the message servers.
pub const MESSAGES_SET: &[&str] = &[
    "qmd",
    "slack",
    "browser",
    "homeassistant",
    "roon",
    "google",
    "github",
    "fastmail",
    "unifi",
    "routeros",
    "proxmox",
    "whatsapp",
    "imcp",
    "google-perseido",
];

/// The live main set (`MAIN_CHILD_MCP_CONFIG`): every main turn on both spawned harnesses.
pub const MAIN_SET: &[&str] = &[
    "qmd",
    "slack",
    "browser",
    "homeassistant",
    "roon",
    "google",
    "github",
    "fastmail",
    "unifi",
    "routeros",
    "proxmox",
    "whatsapp",
    "imcp",
    "google-perseido",
    "build",
    "places",
    "inbound",
    "kubernetes",
    "rybbit",
    "tag1",
    "plex",
    "house",
    "healthdb",
    "clockify-rs",
    "runn-rs",
    "google-sheets",
    "kubernetes-cloud",
    // `in_process`: absent from the rendered string with the gateway off, so
    // `MAIN_CHILD_MCP_CONFIG` is byte for byte the twenty-seven-server golden. With the gateway on
    // and `[devjob] enabled`, [`crate::render_child_mcp`] adds its entry and the gateway serves it.
    "devjob",
];

/// The today-brief child's opt-in set.
pub const REPLIES_SET: &[&str] = &[
    "google",
    "google-perseido",
    "fastmail",
    "slack",
    "whatsapp",
    "imcp",
];

/// [`MESSAGES_SET`] followed by `extra`, for the retired `MESSAGES_*` sets.
pub fn messages_plus(extra: &[&'static str]) -> Vec<&'static str> {
    MESSAGES_SET.iter().chain(extra.iter()).copied().collect()
}

/// One server's entry in Claude Code's `--mcp-config` JSON: `"name":{…}`, with no leading
/// comma. THE EXACT SPELLING the fragment macros produced, key order and all: the rendered
/// config is compared byte for byte by the containment record, the argv fixtures, and the
/// `house` and `healthdb` render-time surgery, which finds its entry by substring.
///
/// An HTTP credential is rendered as Claude Code's own `${VAR}` expansion of a NAME; a client
/// certificate is not expressible here and renders no header. An `in_process` row has no
/// entry a CLI could start, so it renders nothing HERE: this is the gateway-off spelling, and a
/// gateway-off child must never see it. With the gateway on, [`gateway_config_json`] renders it
/// as [`render_in_process_entry`], which the gateway's child config then swaps for the stub.
pub fn render_entry(row: &McpServerRow) -> String {
    let q = |s: &str| serde_json::to_string(s).expect("a string serialises");
    match row.transport {
        Transport::BridgeStdio => {
            let args: Vec<String> = row.args.iter().map(|a| q(a)).collect();
            format!(
                r#"{}:{{"type":"stdio","command":{},"args":[{}]}}"#,
                q(row.name),
                q(row.command),
                args.join(",")
            )
        }
        Transport::Http => {
            // The URL goes in as written: a placeholder must survive for the house surgery,
            // and `serde_json` would not escape anything a valid URL carries anyway.
            let mut out = format!(r#"{}:{{"type":"http","url":"{}""#, q(row.name), row.url);
            match row.auth {
                HttpAuthSpec::None | HttpAuthSpec::ClientCert(_) => {}
                HttpAuthSpec::Bearer(var) => {
                    out.push_str(&format!(
                        r#","headers":{{"Authorization":"Bearer ${{{var}}}"}}"#
                    ));
                }
                HttpAuthSpec::Headers(pairs) => {
                    let h: Vec<String> = pairs
                        .iter()
                        .map(|(k, v)| format!(r#""{k}":"${{{v}}}""#))
                        .collect();
                    out.push_str(&format!(r#","headers":{{{}}}"#, h.join(",")));
                }
            }
            out.push('}');
            out
        }
        Transport::InProcess => String::new(),
    }
}

/// The whole `{"mcpServers":{…}}` for an ordered set of row names.
///
/// # Panics
/// On a name with no row. Every set is a constant of this module or a literal list built from
/// them, and [`tests::every_set_names_only_rows`] proves each resolves.
pub fn mcp_config_json(names: &[&str]) -> String {
    let rendered = render_set(names, |_| false);
    // A set that names an `in_process` row remembers its names under its gateway-off string, so
    // the gateway can render the row back in ([`gateway_config_json`]) from the only thing a spawn
    // site carries: the string.
    if names
        .iter()
        .any(|n| row(n).is_some_and(|r| r.transport == Transport::InProcess))
    {
        IN_PROCESS_SETS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                rendered.clone(),
                names.iter().map(|n| n.to_string()).collect(),
            );
    }
    rendered
}

/// Render `names`, giving each `in_process` row for which `include` is true its gateway entry.
fn render_set(names: &[&str], include: impl Fn(&str) -> bool) -> String {
    let entries: Vec<String> = names
        .iter()
        .map(|n| {
            let r = row(n).unwrap_or_else(|| panic!("no MCP_SERVERS row '{n}'"));
            if r.transport == Transport::InProcess && include(n) {
                render_in_process_entry(r)
            } else {
                render_entry(r)
            }
        })
        .filter(|e| !e.is_empty())
        .collect();
    format!(r#"{{"mcpServers":{{{}}}}}"#, entries.join(","))
}

/// Every rendered set that names an `in_process` row, keyed by its gateway-off string, with the
/// ordered names it was rendered from. Filled by [`mcp_config_json`].
static IN_PROCESS_SETS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>,
> = std::sync::LazyLock::new(Default::default);

/// The `type` an `in_process` entry carries in a gateway-on config. No CLI could start it, and
/// none is ever handed it: [`crate::gateway::child_mcp_config`] replaces every gateway-placed
/// entry with the stub before a child sees the config.
pub const IN_PROCESS_ENTRY_TYPE: &str = "in_process";

/// An `in_process` row's entry in a GATEWAY-ON config: `"name":{"type":"in_process"}`. It is a
/// marker for [`crate::gateway::admitted_servers`] and [`crate::gateway::child_mcp_config`], which
/// admit it and swap it for the stub like any other gateway-placed server.
pub fn render_in_process_entry(row: &McpServerRow) -> String {
    let q = |s: &str| serde_json::to_string(s).expect("a string serialises");
    format!(r#"{}:{{"type":{}}}"#, q(row.name), q(IN_PROCESS_ENTRY_TYPE))
}

/// `config` (a string [`mcp_config_json`] rendered) with each `in_process` row that `include`
/// accepts rendered back in at its place in the set. `None` when `config` names no `in_process`
/// row, or none is included: the caller keeps `config` unchanged, byte for byte.
pub fn gateway_config_json(config: &str, include: impl Fn(&str) -> bool) -> Option<String> {
    let names = IN_PROCESS_SETS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(config)
        .cloned()?;
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    if !names
        .iter()
        .any(|n| row(n).is_some_and(|r| r.transport == Transport::InProcess) && include(n))
    {
        return None;
    }
    Some(render_set(&names, include))
}

// ---- THE CODEX VIEWS -------------------------------------------------------------------
//
// What `harness/codex.rs` used to keep as three hand-written tables. Each is now derived from
// the rows, in the rows' order, keeping only the rows the old table had a reason to name, so
// the Codex argv is unchanged byte for byte.

/// `bridge_stdio` rows that need any variable: `mcp_servers.<name>.env_vars`.
pub fn codex_env_passthrough(name: &str) -> Option<&'static [&'static str]> {
    row(name)
        .filter(|r| r.transport == Transport::BridgeStdio && !r.env.is_empty())
        .map(|r| r.env)
}

/// Fixed, non-secret values a Codex-started server needs: the harness id for `inbound`.
pub fn codex_env_fixed(name: &str) -> Option<(&'static str, &'static str)> {
    row(name)
        .and_then(|r| r.harness_env)
        .map(|var| (var, CODEX_ID))
}

/// HTTP rows with a bearer: `mcp_servers.<name>.bearer_token_env_var`.
pub fn codex_bearer_env(name: &str) -> Option<&'static str> {
    match row(name)?.auth {
        HttpAuthSpec::Bearer(var) => Some(var),
        _ => None,
    }
}

/// HTTP rows with named headers: `mcp_servers.<name>.env_http_headers`.
pub fn codex_header_env(name: &str) -> Option<&'static [(&'static str, &'static str)]> {
    match row(name)?.auth {
        HttpAuthSpec::Headers(pairs) => Some(pairs),
        _ => None,
    }
}

/// THE SHIPPED MAIN CONFIG, SPELLED OUT: the string `MAIN_CHILD_MCP_CONFIG` was a literal
/// concatenation of before the table, kept as the one golden the table is held to. A row
/// edited by accident changes the rendered string and fails
/// [`tests::the_table_reproduces_the_shipped_main_config_byte_for_byte`], and the Claude Code
/// argv goldens that read it, before any child is spawned with a server nobody probed.
///
/// The Home Assistant HOST is the one value not spelled here: it is exempted from the
/// personal-infrastructure scan by line, once, at [`HOME_ASSISTANT_MCP_URL`].
#[cfg(test)]
pub(crate) fn golden_main_config() -> String {
    [
        r#"{"mcpServers":{"qmd":{"type":"stdio","command":"qmd","args":["mcp"]},"slack":{"type":"stdio","command":"npx","args":["-y","slack-mcp-server@latest","--transport","stdio"]},"browser":{"type":"stdio","command":"npx","args":["-y","@playwright/mcp@latest","--headless","--isolated","--output-dir","/tmp/jesse-browser","--output-max-size","104857600"]},"homeassistant":{"type":"http","url":""#,
        HOME_ASSISTANT_MCP_URL,
        r#"","headers":{"Authorization":"Bearer ${HA_MCP_TOKEN}"}},"roon":{"type":"http","url":"http://10.40.0.2:8088/mcp"},"google":{"type":"stdio","command":"workspace-mcp","args":["--single-user","--read-only","--tools","calendar","gmail","drive"]},"github":{"type":"stdio","command":"github-mcp-server","args":["stdio","--read-only","--toolsets","repos,actions,issues,pull_requests"]},"fastmail":{"type":"stdio","command":"npx","args":["-y","github:jeremyandrews/jmap-mcp-server"]},"unifi":{"type":"stdio","command":"unifi-network-mcp","args":[]},"routeros":{"type":"stdio","command":"routeros-mcp","args":[]},"proxmox":{"type":"stdio","command":"mcp-proxmox","args":[]},"whatsapp":{"type":"stdio","command":"whatsapp-mcp","args":[]},"imcp":{"type":"stdio","command":"/Applications/iMCP.app/Contents/MacOS/imcp-server","args":[]},"google-perseido":{"type":"stdio","command":"workspace-mcp-perseido","args":["--single-user","--read-only","--tools","calendar","gmail","drive"]},"build":{"type":"stdio","command":"jesse-build-mcp","args":[]},"places":{"type":"stdio","command":"jesse-places-mcp","args":[]},"inbound":{"type":"stdio","command":"jesse-inbound-mcp","args":[]},"kubernetes":{"type":"stdio","command":"jesse-k8s-mcp","args":["--toolsets","core,config"]},"rybbit":{"type":"http","url":"https://app.rybbit.io/api/mcp","headers":{"Authorization":"Bearer ${RYBBIT_API_KEY}"}},"tag1":{"type":"http","url":"https://www.tag1.com/mcp"},"plex":{"type":"stdio","command":"plex-mcp","args":[]},"house":{"type":"http","url":"${JESSE_HOUSE_URL}","headers":{"Authorization":"Bearer ${JESSE_HOUSE_TOKEN}"}},"healthdb":{"type":"stdio","command":"jesse-healthdb-mcp","args":[]},"clockify-rs":{"type":"stdio","command":"clockify-rs-mcp","args":[]},"runn-rs":{"type":"stdio","command":"runn-rs-mcp","args":[]},"google-sheets":{"type":"stdio","command":"workspace-mcp-sheets","args":["--single-user","--tools","sheets","--tool-tier","complete"]},"kubernetes-cloud":{"type":"stdio","command":"jesse-k8s-cloud-mcp","args":["--toolsets","core,config"]}}}"#,
    ]
    .concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        McpSet, DEFAULT_ALLOWED_TOOLS, MAIN_CHILD_MCP_CONFIG, READ_ALLOWED_TOOLS,
        REPLIES_ALLOWED_TOOLS,
    };

    fn servers_of(config: &str) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(config).expect("JSON");
        v["mcpServers"]
            .as_object()
            .expect("mcpServers")
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn the_table_reproduces_the_shipped_main_config_byte_for_byte() {
        assert_eq!(mcp_config_json(MAIN_SET), golden_main_config());
        assert_eq!(MAIN_CHILD_MCP_CONFIG.as_str(), golden_main_config());
    }

    #[test]
    fn row_names_are_unique_and_usable_in_a_tool_name() {
        let mut seen = std::collections::BTreeSet::new();
        for r in MCP_SERVERS {
            assert!(seen.insert(r.name), "duplicate row {}", r.name);
            assert!(
                r.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "{}",
                r.name
            );
        }
    }

    #[test]
    fn every_set_names_only_rows() {
        for set in [HOUSE_SET, MORNING_SET, MESSAGES_SET, MAIN_SET, REPLIES_SET] {
            for n in set {
                assert!(row(n).is_some(), "set names '{n}', which has no row");
            }
        }
        for set in McpSet::ALL {
            for n in servers_of(set.config()) {
                assert!(row(&n).is_some(), "{} names '{n}'", set.label());
            }
        }
    }

    #[test]
    fn every_granted_mcp_tool_names_a_row() {
        for list in [
            DEFAULT_ALLOWED_TOOLS,
            READ_ALLOWED_TOOLS,
            REPLIES_ALLOWED_TOOLS,
        ] {
            for g in list.split(',').map(str::trim) {
                if let Some(rest) = g.strip_prefix("mcp__") {
                    let server = rest.split("__").next().unwrap();
                    assert!(row(server).is_some(), "{g} names no row");
                }
            }
        }
    }

    /// A class names a tool the allowlist grants, so a renamed or withdrawn tool cannot leave a
    /// stale class behind that would quietly apply to a future tool of the same name.
    #[test]
    fn every_classed_tool_is_granted() {
        for r in MCP_SERVERS {
            let granted = r.granted_tools(DEFAULT_ALLOWED_TOOLS);
            for t in r
                .write_tools
                .iter()
                .chain(r.egress_tools)
                .chain(r.read_tools)
            {
                assert!(
                    granted.iter().any(|g| g == t),
                    "{}: classes '{t}', which DEFAULT_ALLOWED_TOOLS does not grant",
                    r.name
                );
            }
        }
    }

    #[test]
    fn the_seeded_egress_tools_are_egress() {
        for (server, tool) in [
            ("places", "places_search"),
            ("places", "place_details"),
            ("imcp", "maps_search"),
            ("browser", "browser_navigate"),
        ] {
            assert_eq!(row(server).unwrap().class_of(tool), ToolClass::Egress);
        }
    }

    /// NO TOOL IS EXPOSED ABOVE THE LEVEL THE CLAUDE CODE ALLOWLIST GRANTS IT. A tool the
    /// read-level allowlist grants must not be classed `write` (which `direct` maps to a class
    /// permitted only at `write`), or the two harnesses would disagree in the safe direction
    /// for one and the unsafe direction for the other.
    #[test]
    fn nothing_granted_at_read_is_classed_write() {
        for r in MCP_SERVERS {
            for t in r.granted_tools(READ_ALLOWED_TOOLS) {
                assert_ne!(r.class_of(&t), ToolClass::Write, "{}::{t}", r.name);
            }
        }
    }

    #[test]
    fn only_qmd_is_placed_in_the_child_and_it_holds_no_credential() {
        for r in MCP_SERVERS {
            if r.placement == Placement::Child {
                assert_eq!(r.name, "qmd");
                assert!(r.env.is_empty());
                assert_eq!(r.auth, HttpAuthSpec::None);
            }
        }
    }

    #[test]
    fn the_environment_column_holds_names_and_never_a_base_variable() {
        for r in MCP_SERVERS {
            for n in r.env {
                assert!(
                    n.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                    "{}: '{n}' is not a variable name",
                    r.name
                );
                assert!(
                    !BASE_ENV.contains(n),
                    "{}: {n} is already in BASE_ENV",
                    r.name
                );
            }
            if r.transport == Transport::Http {
                assert!(
                    r.env.is_empty(),
                    "{}: an http row starts no process",
                    r.name
                );
            }
        }
    }

    /// THE CODEX VIEWS REPRODUCE THE TABLES THEY REPLACED, name for name. The old tables are
    /// written out here once, as the record of what the view must keep producing.
    #[test]
    fn the_codex_views_reproduce_the_tables_they_replaced() {
        let passthrough: &[(&str, &[&str])] = &[
            ("slack", &["SLACK_MCP_XOXP_TOKEN"]),
            (
                "google",
                &[
                    "GOOGLE_OAUTH_CLIENT_ID",
                    "GOOGLE_OAUTH_CLIENT_SECRET",
                    "WORKSPACE_MCP_CREDENTIALS_DIR",
                    "WORKSPACE_ATTACHMENT_DIR",
                    "USER_GOOGLE_EMAIL",
                ],
            ),
            ("github", &["GITHUB_PERSONAL_ACCESS_TOKEN"]),
            ("fastmail", &["JMAP_TOKEN", "JMAP_SESSION_URL"]),
            (
                "unifi",
                &[
                    "UNIFI_USERNAME",
                    "UNIFI_PASSWORD",
                    "UNIFI_HOST",
                    "UNIFI_PORT",
                    "UNIFI_CONTROLLER_TYPE",
                    "UNIFI_VERIFY_SSL",
                    "UNIFI_SITE",
                ],
            ),
            (
                "routeros",
                &["ROUTEROS_DEVICES_CONFIG", "ROUTEROS_SSH_PORT"],
            ),
            ("build", &["JESSE_VAULT"]),
            (
                "healthdb",
                &[
                    "JESSE_HEALTHDB_RO_URL_FILE",
                    "JESSE_HEALTHDB_CA_FILE",
                    "JESSE_HEALTHDB_MCP_LEDGER",
                ],
            ),
            ("clockify-rs", &["JESSE_CLOCKIFY_API_KEY"]),
            ("runn-rs", &["JESSE_RUNN_API_TOKEN"]),
            (
                "google-sheets",
                &[
                    "GOOGLE_OAUTH_CLIENT_ID",
                    "GOOGLE_OAUTH_CLIENT_SECRET",
                    "USER_GOOGLE_EMAIL",
                ],
            ),
        ];
        for (name, vars) in passthrough {
            assert_eq!(codex_env_passthrough(name), Some(*vars), "{name}");
        }
        assert_eq!(codex_env_passthrough("places").map(<[_]>::len), Some(15));
        assert_eq!(codex_env_passthrough("inbound").map(<[_]>::len), Some(14));
        for absent in [
            "qmd",
            "browser",
            "proxmox",
            "whatsapp",
            "imcp",
            "google-perseido",
            "kubernetes",
            "kubernetes-cloud",
            "plex",
            "homeassistant",
        ] {
            assert_eq!(codex_env_passthrough(absent), None, "{absent}");
        }
        assert_eq!(
            codex_env_fixed("inbound"),
            Some(("JESSE_INBOUND_HARNESS", CODEX_ID))
        );
        assert_eq!(codex_env_fixed("build"), None);
        assert_eq!(codex_bearer_env("homeassistant"), Some("HA_MCP_TOKEN"));
        assert_eq!(codex_bearer_env("rybbit"), Some("RYBBIT_API_KEY"));
        assert_eq!(codex_bearer_env("house"), Some("JESSE_HOUSE_TOKEN"));
        for absent in ["roon", "tag1", "clockify", "slack", "runn-rs"] {
            assert_eq!(codex_bearer_env(absent), None, "{absent}");
        }
        assert_eq!(
            codex_header_env("clockify"),
            Some(&[("x-api-key", "JESSE_CLOCKIFY_API_KEY")][..])
        );
    }

    /// The host launcher and the consent helper for `google-sheets`, as committed.
    const SHEETS_LAUNCHER: &str = include_str!("../launchers/workspace-mcp-sheets");
    const SHEETS_CONSENT: &str = include_str!("../launchers/consent-google-sheets");

    /// `google-sheets` LOADS THE SHEETS SERVICE AND NOTHING ELSE. `--tools` names `sheets`
    /// alone, so no Gmail, Calendar or Drive tool registers on the one Google instance that can
    /// write, and `--read-only` is absent because writing Sheets is the row's whole purpose.
    #[test]
    fn google_sheets_loads_only_the_sheets_service() {
        let r = row("google-sheets").expect("the google-sheets row");
        assert_eq!(r.transport, Transport::BridgeStdio);
        assert_eq!(r.command, "workspace-mcp-sheets");
        assert_eq!(
            r.args,
            [
                "--single-user",
                "--tools",
                "sheets",
                "--tool-tier",
                "complete"
            ]
        );
        let tools_at = r.args.iter().position(|a| *a == "--tools").unwrap();
        let services: Vec<&str> = r.args[tools_at + 1..]
            .iter()
            .take_while(|a| !a.starts_with("--"))
            .copied()
            .collect();
        assert_eq!(services, ["sheets"]);
        for other in [
            "gmail", "calendar", "drive", "docs", "slides", "forms", "chat",
        ] {
            assert!(!r.args.contains(&other), "google-sheets loads {other}");
        }
        assert!(!r.args.contains(&"--read-only"));
        assert!(!r.args.contains(&"--permissions"));
    }

    /// `google-sheets` NEVER SHARES A TOKEN CACHE. The bridge's environment carries the
    /// read-only `google` instance's `WORKSPACE_MCP_CREDENTIALS_DIR`
    /// (`~/.config/jesse-google/creds`, set by `export_mcp_server_env`); this row does not
    /// forward it, and its launcher overrides both names the server reads with its own
    /// directory, which is neither `google`'s nor `google-perseido`'s.
    #[test]
    fn google_sheets_uses_its_own_credentials_directory() {
        let r = row("google-sheets").unwrap();
        assert!(!r.env.contains(&"WORKSPACE_MCP_CREDENTIALS_DIR"));
        assert!(!r.env.contains(&"GOOGLE_MCP_CREDENTIALS_DIR"));
        assert!(!r.env.contains(&"WORKSPACE_ATTACHMENT_DIR"));
        // The client pair and the account are `google`'s own.
        let google = row("google").unwrap();
        for n in r.env {
            assert!(google.env.contains(n), "{n} is not one of google's names");
        }

        let assigned = |var: &str| -> String {
            SHEETS_LAUNCHER
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{var}=")))
                .unwrap_or_else(|| panic!("the launcher does not set {var}"))
                .trim_matches('"')
                .to_string()
        };
        assert_eq!(assigned("SHEETS_DIR"), "$HOME/.config/jesse-google-sheets");
        assert_eq!(
            assigned("WORKSPACE_MCP_CREDENTIALS_DIR"),
            "$SHEETS_DIR/creds"
        );
        assert_eq!(
            assigned("GOOGLE_MCP_CREDENTIALS_DIR"),
            "$WORKSPACE_MCP_CREDENTIALS_DIR"
        );
        assert!(SHEETS_LAUNCHER.contains(
            "export WORKSPACE_MCP_CREDENTIALS_DIR GOOGLE_MCP_CREDENTIALS_DIR WORKSPACE_ATTACHMENT_DIR"
        ));
        assert!(SHEETS_LAUNCHER.contains("\nexec workspace-mcp \"$@\"\n"));
        for other in [".config/jesse-google/", ".config/jesse-google-perseido"] {
            assert!(
                !SHEETS_LAUNCHER.contains(other),
                "the sheets launcher names {other}"
            );
        }
    }

    /// THE CONSENT HELPER CONSENTS FOR EXACTLY THE ROW'S ARGUMENTS, so the scopes Jeremy
    /// approves are the scopes the bridge runs with, and it starts the same launcher.
    #[test]
    fn google_sheets_consent_helper_matches_the_row() {
        let r = row("google-sheets").unwrap();
        let quoted: Vec<String> = r.args.iter().map(|a| format!("\"{a}\"")).collect();
        let line = format!("SERVER_ARGS = [{}]", quoted.join(", "));
        assert!(SHEETS_CONSENT.contains(&line), "expected `{line}`");
        assert!(SHEETS_CONSENT.contains(&format!("[\"{}\", *SERVER_ARGS]", r.command)));
        assert!(SHEETS_CONSENT.contains("~/.config/jesse-google-sheets/creds/"));
    }

    /// EVERY `google-sheets` TOOL IS DECIDED: the eight writes are classed `write`, the five
    /// reads are not, and the grant is exactly those thirteen.
    #[test]
    fn google_sheets_classes_every_granted_tool() {
        let r = row("google-sheets").unwrap();
        for t in GOOGLE_SHEETS_WRITE_TOOLS {
            assert_eq!(r.class_of(t), ToolClass::Write, "{t}");
        }
        for t in GOOGLE_SHEETS_READ_TOOLS {
            assert_eq!(r.class_of(t), ToolClass::Read, "{t}");
        }
        let mut granted = r.granted_tools(DEFAULT_ALLOWED_TOOLS);
        granted.sort_unstable();
        let mut expected: Vec<String> = GOOGLE_SHEETS_WRITE_TOOLS
            .iter()
            .chain(GOOGLE_SHEETS_READ_TOOLS.iter())
            .map(|t| t.to_string())
            .collect();
        expected.sort_unstable();
        assert_eq!(granted, expected);
    }

    /// `inbound` FETCHES GMAIL ATTACHMENTS WITH `google`'s TOKEN AND NO OTHER. It reads
    /// `WORKSPACE_MCP_CREDENTIALS_DIR` from the bridge's environment, whose default is the
    /// read-only `google` cache; `google-sheets` adds no variable it could read instead, and
    /// sets its own directory only inside its own launcher process.
    #[test]
    fn inbound_still_reads_only_the_google_token() {
        let inbound = row("inbound").unwrap();
        assert!(inbound.env.contains(&"WORKSPACE_MCP_CREDENTIALS_DIR"));
        assert!(
            !inbound.env.iter().any(|n| n.contains("SHEETS")),
            "{:?}",
            inbound.env
        );
        assert!(!inbound.env.contains(&"GOOGLE_MCP_CREDENTIALS_DIR"));
    }
}
