//! **The house model**: the optional `[house]` table, the token it names, and how the `house`
//! MCP server reaches a child.
//!
//! `house-mcp` runs on the home k3s cluster and holds a spatial model of the owner's house and
//! grounds (sites, floors, rooms, objects, files, notes) in PostGIS. It speaks MCP over
//! Streamable HTTP behind a certificate from the internal CA `pozza-ca`, with a bearer token
//! that can read and write.
//!
//! ---- WHY THIS SERVER IS DIFFERENT FROM EVERY OTHER ONE ----------------------
//!
//! Every other server in [`MAIN_CHILD_MCP_CONFIG`] is fixed at compile time and authenticates
//! from a variable in the LaunchAgent plist. This one is CONFIGURED: its URL and the file
//! holding its token come from the bridge config, and a deployment without a `[house]` table
//! does not register it at all. The containment posture stays fixed anyway, because the shipped
//! const always DECLARES `house` (with the URL as [`HOUSE_URL_PLACEHOLDER`]) and the row label,
//! the grant and the record all describe that declaration. What varies per deployment is only
//! the rendered `--mcp-config`:
//!
//!   * a ready `[house]` table: the placeholder becomes the configured URL, and the child gets
//!     the token as [`HOUSE_TOKEN_ENV`];
//!   * no table, or one that was refused at startup: the `house` entry is removed, so the child
//!     runs the recorded posture MINUS one server, which is narrower and never wider.
//!
//! ---- THE TOKEN -------------------------------------------------------------
//!
//! Read once, at config load, from `token_file`, which is refused unless it is a regular file
//! owned by the bridge's own user with mode `0600` exactly. It is held in a type whose `Debug`
//! prints nothing, and it leaves this process only as the value of [`HOUSE_TOKEN_ENV`] on a
//! child whose rendered set loads `house`: never on an argv, never in a file the bridge writes,
//! never in a log line. Claude Code expands `${JESSE_HOUSE_TOKEN}` in the server's header;
//! Codex reads the variable by name through `bearer_token_env_var`.

use std::borrow::Cow;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::*;

/// The placeholder the shipped `house` declaration carries where its URL goes. Spelled like an
/// environment expansion so that if it ever escaped [`render_house_mcp`] Claude Code would fail
/// the server loudly on an unset variable rather than dial something; a test asserts it never
/// reaches a rendered config on any harness.
pub const HOUSE_URL_PLACEHOLDER: &str = "${JESSE_HOUSE_URL}";

/// The variable a child reads the house token from. Named in the shipped declaration
/// (`Bearer ${JESSE_HOUSE_TOKEN}`) and in Codex's bearer table, and set by
/// [`apply_house_env`] on a child that loads the server.
pub const HOUSE_TOKEN_ENV: &str = "JESSE_HOUSE_TOKEN";

/// The `[house]` table as written. Every field optional so a partial table reaches
/// [`HouseConfig::from_toml`], which refuses it by name, rather than failing the parse of the
/// whole overlay file and silently taking the persona and the model registry down with it.
#[derive(Deserialize, Debug, Default, Clone)]
pub struct HouseToml {
    /// The server's MCP endpoint, `https://…/mcp`.
    pub url: Option<String>,
    /// A file holding the read and write token, mode `0600`, owned by the bridge's user.
    pub token_file: Option<String>,
    /// The internal CA's certificate (PEM), for the bridge's OWN HTTP client, whose bundled
    /// roots cannot see the macOS keychain. The harness children verify through the keychain.
    pub ca_file: Option<String>,
}

/// The house token. `Debug` prints a fixed marker, never the value, so no `{:?}` of a config
/// that holds one can put it in a log.
#[derive(Clone, PartialEq, Eq)]
pub struct HouseToken(String);

impl HouseToken {
    /// The value, for the one place it is meant to go: a child's environment.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for HouseToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HouseToken(<redacted>)")
    }
}

/// A `[house]` table that passed every check at startup.
#[derive(Clone, Debug)]
pub struct HouseSettings {
    /// The MCP endpoint, already checked to be `https://` with nothing a JSON string or a URL
    /// would need escaped.
    pub url: String,
    /// The token read from `token_file`.
    pub token: HouseToken,
    /// Where the token came from, for messages.
    pub token_file: PathBuf,
    /// The CA certificate for the bridge's own client, when configured.
    pub ca_file: Option<PathBuf>,
}

/// What the deployment's `[house]` table amounts to.
#[derive(Clone, Debug, Default)]
pub enum HouseConfig {
    /// No `[house]` table: the server is not registered on any child. The shipped default.
    #[default]
    Absent,
    /// A table that passed every check: the server is registered on every child whose set
    /// declares it.
    Ready(HouseSettings),
    /// A table that is present and wrong. The startup gate refuses to boot on it
    /// ([`validate_house`]), because a credential file with the wrong mode is exactly the
    /// thing that must not be quietly worked around.
    Refused(String),
}

impl HouseConfig {
    /// Resolve the table: absent stays absent, and a present table is either ready or refused
    /// with every reason it was refused for.
    pub fn from_toml(table: Option<HouseToml>) -> HouseConfig {
        let Some(t) = table else {
            return HouseConfig::Absent;
        };
        let mut problems = Vec::new();
        let url = match t.url.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None => {
                problems.push("`url` is missing".to_string());
                None
            }
            Some(u) => match check_url(u) {
                Ok(()) => Some(u.to_string()),
                Err(e) => {
                    problems.push(e);
                    None
                }
            },
        };
        let token_file = match t
            .token_file
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            None => {
                problems.push("`token_file` is missing".to_string());
                None
            }
            Some(p) => Some(PathBuf::from(p)),
        };
        let token = token_file
            .as_deref()
            .and_then(|p| match read_token_file(p, current_uid()) {
                Ok(t) => Some(t),
                Err(e) => {
                    problems.push(e);
                    None
                }
            });
        let ca_file = t
            .ca_file
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        if let Some(ca) = &ca_file {
            if let Err(e) = load_ca(ca) {
                problems.push(e);
            }
        }
        match (url, token_file, token) {
            (Some(url), Some(token_file), Some(token)) if problems.is_empty() => {
                HouseConfig::Ready(HouseSettings {
                    url,
                    token,
                    token_file,
                    ca_file,
                })
            }
            _ => HouseConfig::Refused(problems.join("; ")),
        }
    }

    /// The settings, when the table is ready.
    pub fn ready(&self) -> Option<&HouseSettings> {
        match self {
            HouseConfig::Ready(s) => Some(s),
            HouseConfig::Absent | HouseConfig::Refused(_) => None,
        }
    }
}

/// The URL is substituted into a JSON string and handed to two CLIs as an endpoint, so it is
/// held to the narrow shape that needs no escaping anywhere: `https://`, then visible ASCII with
/// no quote, backslash or whitespace. Plain `http` is refused: this token can write.
fn check_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") || url.len() <= "https://".len() {
        return Err(format!("`url` must be an https:// URL, got {url:?}"));
    }
    if !url
        .chars()
        .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\')
    {
        return Err(format!(
            "`url` may hold only visible ASCII with no quote or backslash, got {url:?}"
        ));
    }
    Ok(())
}

/// The bridge's own effective uid.
fn current_uid() -> u32 {
    // SAFETY: `geteuid` takes no arguments, cannot fail, and touches no memory of ours.
    unsafe { libc::geteuid() }
}

/// Read the token, refusing a file anyone but its owner could read or change, or one owned by
/// another user. The SAME bar the bridge sets for every credential file it writes itself
/// (`0600`, its own user), applied to the one it reads.
///
/// The token must be one line of visible ASCII: it goes into an HTTP header, and a stray
/// newline or space inside it would be a malformed header rather than a refused credential.
pub fn read_token_file(path: &Path, uid: u32) -> Result<HouseToken, String> {
    let shown = path.display();
    let meta =
        std::fs::metadata(path).map_err(|e| format!("`token_file` {shown} cannot be read: {e}"))?;
    if !meta.is_file() {
        return Err(format!("`token_file` {shown} is not a regular file"));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o600 {
        return Err(format!(
            "`token_file` {shown} has mode {mode:04o}; it must be 0600 (chmod 600 {shown})"
        ));
    }
    if meta.uid() != uid {
        return Err(format!(
            "`token_file` {shown} is owned by uid {}, not by the bridge's uid {uid}",
            meta.uid()
        ));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("`token_file` {shown} cannot be read: {e}"))?;
    let token = text.trim();
    if token.is_empty() {
        return Err(format!("`token_file` {shown} is empty"));
    }
    if !token.chars().all(|c| c.is_ascii_graphic()) {
        return Err(format!(
            "`token_file` {shown} must hold one token of visible ASCII with no spaces or line breaks"
        ));
    }
    Ok(HouseToken(token.to_string()))
}

/// Load the CA certificate as the bridge's HTTP client takes it.
fn load_ca(path: &Path) -> Result<reqwest::Certificate, String> {
    let shown = path.display();
    let pem = std::fs::read(path).map_err(|e| format!("`ca_file` {shown} cannot be read: {e}"))?;
    reqwest::Certificate::from_pem(&pem)
        .map_err(|e| format!("`ca_file` {shown} is not a PEM certificate: {e}"))
}

/// The startup gate's view of the `[house]` table: a refused table stops the bridge, by name.
/// An absent table is the shipped default and is not an error.
pub fn validate_house(cfg: &Config) -> Vec<ConfigError> {
    match &cfg.house {
        HouseConfig::Refused(why) => vec![ConfigError {
            model: None,
            message: format!(
                "the [house] table is present but unusable: {why}. Fix it in \
                 jesse.local.toml, or remove the table to run without the house model."
            ),
        }],
        HouseConfig::Absent | HouseConfig::Ready(_) => Vec::new(),
    }
}

/// The `--mcp-config` a child is actually given, from the shipped one it was spawned with.
///
/// A config that does not carry the shipped `house` entry ([`HOUSE_MCP_ENTRY`]) is returned
/// unchanged, byte for byte: every set but the main one, and an operator's
/// `JESSE_MAIN_MCP_CONFIG`. One that does gets the configured URL in place of
/// [`HOUSE_URL_PLACEHOLDER`] on a deployment with a ready `[house]` table, and loses the entry
/// on any other.
///
/// String surgery on the one known entry rather than a JSON round trip, so no other server's
/// declaration moves by a byte.
pub fn render_house_mcp<'a>(cfg: &Config, config: &'a str) -> Cow<'a, str> {
    if !config.contains(HOUSE_MCP_ENTRY) {
        return Cow::Borrowed(config);
    }
    let entry = match cfg.house.ready() {
        // `check_url` admitted nothing JSON needs escaped, so the URL goes in as written.
        Some(h) => HOUSE_MCP_ENTRY.replacen(HOUSE_URL_PLACEHOLDER, &h.url, 1),
        None => String::new(),
    };
    Cow::Owned(config.replacen(HOUSE_MCP_ENTRY, &entry, 1))
}

/// Put the house token on a child that loads the server, and on no other.
///
/// It only ever ADDS: the bridge never puts the token in its own environment (it reads the
/// file into [`HouseSettings`] and nowhere else), so a child that does not load `house` has
/// nothing to inherit, and its environment is byte for byte what it was before this server.
pub fn apply_house_env(cmd: &mut Command, cfg: &Config, config: &str) {
    if let Some(h) = cfg.house.ready() {
        if config.contains(HOUSE_MCP_ENTRY) {
            cmd.env(HOUSE_TOKEN_ENV, h.token.expose());
        }
    }
}

/// Whether a main turn on `harness` registers the `house` server on this deployment: a ready
/// table, a spawned harness, and a main set that declares it. The `direct` harness never does
/// (its MCP client is stdio only), and an operator's `JESSE_MAIN_MCP_CONFIG` decides for
/// itself.
pub fn main_turn_loads_house(cfg: &Config, harness: &dyn Harness) -> bool {
    if cfg.house.ready().is_none() {
        return false;
    }
    match harness.runner() {
        Runner::Spawned(h) => main_mcp_config(cfg, h).contains(HOUSE_MCP_ENTRY),
        _ => false,
    }
}

/// ADVISORY, never fatal: can the bridge's OWN HTTP client reach the server and complete an MCP
/// `initialize` with the token? Returns the server's name and version.
///
/// The bridge's client bundles its roots (rustls with webpki roots) and cannot see the macOS
/// keychain, so it verifies the internal CA only through `ca_file`. Certificate verification
/// is never switched off. Neither harness child depends on this; it exists so a deployment
/// whose house server is down, or whose CA or token is wrong, says so at startup instead of on
/// the first turn that needs it.
pub async fn probe_house(h: &HouseSettings) -> Result<String, String> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(10));
    if let Some(ca) = &h.ca_file {
        builder = builder.add_root_certificate(load_ca(ca)?);
    }
    let client = builder.build().map_err(|e| e.to_string())?;
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "jesse-bridge", "version": env!("CARGO_PKG_VERSION")}
        }
    });
    let resp = client
        .post(&h.url)
        .bearer_auth(h.token.expose())
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| format!("{e:?}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let info = &v["result"]["serverInfo"];
    match (info["name"].as_str(), info["version"].as_str()) {
        (Some(name), Some(version)) => Ok(format!("{name} {version}")),
        _ => Err("the reply to initialize carried no serverInfo".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn tempdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("jesse-house-{}", random_hex()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn token_file(dir: &Path, mode: u32, body: &str) -> PathBuf {
        let p = dir.join("house-token");
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    fn ready() -> HouseConfig {
        HouseConfig::Ready(HouseSettings {
            url: "https://house.example/mcp".to_string(),
            token: HouseToken("t0ken".to_string()),
            token_file: PathBuf::from("/nonexistent"),
            ca_file: None,
        })
    }

    #[test]
    fn a_token_file_is_read_only_at_0600_and_only_from_its_owner() {
        let dir = tempdir();
        let uid = current_uid();
        let p = token_file(&dir, 0o600, "abc123\n");
        assert_eq!(read_token_file(&p, uid).unwrap().expose(), "abc123");
        for mode in [0o640, 0o644, 0o400, 0o660, 0o700] {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
            let e = read_token_file(&p, uid).unwrap_err();
            assert!(e.contains("must be 0600"), "{mode:o}: {e}");
        }
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        let e = read_token_file(&p, uid.wrapping_add(1)).unwrap_err();
        assert!(e.contains("owned by uid"), "{e}");
    }

    #[test]
    fn a_token_must_be_one_visible_line() {
        let dir = tempdir();
        let uid = current_uid();
        for bad in ["", "  \n", "two words", "a\nb"] {
            let p = token_file(&dir, 0o600, bad);
            assert!(read_token_file(&p, uid).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_token_never_prints() {
        let t = HouseToken("s3cret".to_string());
        assert!(!format!("{t:?}").contains("s3cret"));
        let HouseConfig::Ready(h) = ready() else {
            unreachable!()
        };
        assert!(!format!("{h:?}").contains("t0ken"));
    }

    #[test]
    fn a_partial_or_insecure_table_is_refused_by_name() {
        let dir = tempdir();
        let p = token_file(&dir, 0o644, "abc");
        let HouseConfig::Refused(why) = HouseConfig::from_toml(Some(HouseToml {
            url: Some("http://house.example/mcp".to_string()),
            token_file: Some(p.display().to_string()),
            ca_file: Some(dir.join("missing.crt").display().to_string()),
        })) else {
            panic!("refused");
        };
        assert!(why.contains("https://"), "{why}");
        assert!(why.contains("must be 0600"), "{why}");
        assert!(why.contains("ca_file"), "{why}");
        assert!(matches!(
            HouseConfig::from_toml(Some(HouseToml::default())),
            HouseConfig::Refused(_)
        ));
        assert!(matches!(HouseConfig::from_toml(None), HouseConfig::Absent));
    }

    #[test]
    fn a_good_table_is_ready_and_a_refused_one_stops_the_bridge() {
        let dir = tempdir();
        let p = token_file(&dir, 0o600, "abc");
        let ready = HouseConfig::from_toml(Some(HouseToml {
            url: Some("https://house.example/mcp".to_string()),
            token_file: Some(p.display().to_string()),
            ca_file: None,
        }));
        assert_eq!(ready.ready().unwrap().token.expose(), "abc");
        let mut cfg = test_config();
        cfg.house = ready;
        assert!(validate_house(&cfg).is_empty());
        cfg.house = HouseConfig::Refused("`url` is missing".to_string());
        let errors = validate_house(&cfg);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("[house]"), "{}", errors[0]);
    }

    #[test]
    fn the_main_set_renders_with_the_configured_url_or_without_the_server() {
        let mut cfg = test_config();
        cfg.house = ready();
        let with = render_house_mcp(&cfg, MAIN_CHILD_MCP_CONFIG);
        let v: serde_json::Value = serde_json::from_str(&with).expect("still JSON");
        assert_eq!(
            v["mcpServers"]["house"],
            serde_json::json!({
                "type": "http",
                "url": "https://house.example/mcp",
                "headers": {"Authorization": "Bearer ${JESSE_HOUSE_TOKEN}"}
            })
        );
        assert!(!with.contains(HOUSE_URL_PLACEHOLDER));

        for absent in [HouseConfig::Absent, HouseConfig::Refused("x".to_string())] {
            cfg.house = absent;
            let without = render_house_mcp(&cfg, MAIN_CHILD_MCP_CONFIG);
            let v: serde_json::Value = serde_json::from_str(&without).expect("still JSON");
            assert!(v["mcpServers"].get("house").is_none(), "{without}");
            assert!(!without.contains("JESSE_HOUSE"), "{without}");
            // Nothing else moved: the rendered config IS the retired twenty-two-server set.
            assert_eq!(
                without,
                MESSAGES_BUILD_PLACES_INBOUND_KUBERNETES_RYBBIT_TAG1_PLEX_CLOCKIFY_MCP_CONFIG
            );
        }
    }

    #[test]
    fn every_other_set_renders_byte_for_byte() {
        let mut cfg = test_config();
        for house in [ready(), HouseConfig::Absent] {
            cfg.house = house;
            for set in McpSet::ALL {
                if set.contains_house() {
                    continue;
                }
                assert!(matches!(
                    render_house_mcp(&cfg, set.config()),
                    Cow::Borrowed(_)
                ));
            }
        }
    }

    #[test]
    fn only_a_child_that_loads_house_gets_the_token() {
        let mut cfg = test_config();
        cfg.house = ready();
        let env_of = |cmd: &Command| {
            cmd.as_std()
                .get_envs()
                .find(|(k, _)| *k == HOUSE_TOKEN_ENV)
                .map(|(_, v)| v.map(|v| v.to_string_lossy().to_string()))
        };
        let mut cmd = Command::new("true");
        apply_house_env(&mut cmd, &cfg, MAIN_CHILD_MCP_CONFIG);
        assert_eq!(env_of(&cmd), Some(Some("t0ken".to_string())));

        // A set without the server gets nothing, and neither does any child when there is
        // no ready table: the environment is untouched.
        let mut cmd = Command::new("true");
        apply_house_env(&mut cmd, &cfg, McpSet::Replies.config());
        assert_eq!(env_of(&cmd), None);

        cfg.house = HouseConfig::Absent;
        let mut cmd = Command::new("true");
        apply_house_env(&mut cmd, &cfg, MAIN_CHILD_MCP_CONFIG);
        assert_eq!(env_of(&cmd), None);
    }

    /// CLAUDE CODE, THE WHOLE ARGV: with a ready table the main turn declares twenty-three
    /// servers and `house` carries the configured URL and the token BY NAME; without one it
    /// declares twenty-two. The placeholder and the token value are on no argv either way.
    #[test]
    fn a_claude_code_main_turn_carries_house_exactly_when_the_table_is_ready() {
        let mut cfg = test_config();
        for (house, count) in [(ready(), 23), (HouseConfig::Absent, 22)] {
            let loads = house.ready().is_some();
            cfg.house = house;
            let args = build_claude_args(
                &cfg,
                "hi",
                None,
                Capability::Write,
                main_mcp_config(&cfg, &ClaudeCode),
                None,
                None,
            );
            let at = args.iter().position(|a| a == "--mcp-config").unwrap();
            let v: serde_json::Value = serde_json::from_str(&args[at + 1]).unwrap();
            let servers = v["mcpServers"].as_object().unwrap();
            assert_eq!(servers.len(), count);
            assert_eq!(servers.contains_key("house"), loads);
            if loads {
                assert_eq!(servers["house"]["url"], "https://house.example/mcp");
                assert_eq!(
                    servers["house"]["headers"]["Authorization"],
                    "Bearer ${JESSE_HOUSE_TOKEN}"
                );
            }
            let flat = args.join("\n");
            assert!(!flat.contains(HOUSE_URL_PLACEHOLDER), "{flat}");
            assert!(!flat.contains("t0ken"), "the token value is never on argv");
            // The GRANT is the row's either way: the set resolves from the shipped string.
            assert!(flat.contains("mcp__house__what_is_at"));
        }
    }

    /// CODEX: the rendered set reaches Codex as the configured URL plus
    /// `bearer_token_env_var`, every house tool enabled and auto approved, and no placeholder.
    /// Without a ready table Codex is handed no `house` server at all.
    #[test]
    fn a_codex_main_turn_carries_house_exactly_when_the_table_is_ready() {
        let mut cfg = test_config();
        cfg.house = ready();
        let args = codex_mcp_args(
            CODEX_ID,
            &render_house_mcp(&cfg, MAIN_CHILD_MCP_CONFIG),
            DEFAULT_ALLOWED_TOOLS,
        )
        .expect("renders");
        let flat = args.join("\n");
        assert!(
            flat.contains(r#"mcp_servers.house.url="https://house.example/mcp""#),
            "{flat}"
        );
        assert!(
            flat.contains(r#"mcp_servers.house.bearer_token_env_var="JESSE_HOUSE_TOKEN""#),
            "{flat}"
        );
        assert!(
            flat.contains(r#"mcp_servers.house.default_tools_approval_mode="approve""#),
            "{flat}"
        );
        let enabled = args
            .iter()
            .find(|a| a.starts_with("mcp_servers.house.enabled_tools="))
            .expect("house carries an enabled_tools override");
        for tool in [
            "what_is_at",
            "render_plan",
            "create_site",
            "delete",
            "upsert_space",
        ] {
            assert!(
                enabled.contains(&format!(r#""{tool}""#)),
                "{tool}: {enabled}"
            );
        }
        assert!(!flat.contains(HOUSE_URL_PLACEHOLDER), "{flat}");
        assert!(!flat.contains("t0ken"), "{flat}");

        cfg.house = HouseConfig::Absent;
        let args = codex_mcp_args(
            CODEX_ID,
            &render_house_mcp(&cfg, MAIN_CHILD_MCP_CONFIG),
            DEFAULT_ALLOWED_TOOLS,
        )
        .expect("renders");
        assert!(
            !args.iter().any(|a| a.starts_with("mcp_servers.house.")),
            "{args:?}"
        );
    }

    /// THE PROMPT NOTE FOLLOWS THE SERVER: a spawned harness with a ready table, and nothing
    /// else. `direct` never registers it.
    #[test]
    fn only_a_turn_that_registers_house_is_told_about_it() {
        let mut cfg = test_config();
        assert!(!main_turn_loads_house(&cfg, &ClaudeCode));
        cfg.house = ready();
        assert!(main_turn_loads_house(&cfg, &ClaudeCode));
        assert!(main_turn_loads_house(&cfg, &Codex));
        assert!(!main_turn_loads_house(&cfg, &Direct));
    }

    #[test]
    fn the_shipped_entry_carries_the_placeholder_and_the_token_by_name() {
        assert!(HOUSE_MCP_ENTRY.contains(HOUSE_URL_PLACEHOLDER));
        assert!(HOUSE_MCP_ENTRY.contains(&format!("${{{HOUSE_TOKEN_ENV}}}")));
        assert!(MAIN_CHILD_MCP_CONFIG.ends_with(&format!("{HOUSE_MCP_ENTRY}}}}}")));
    }
}
