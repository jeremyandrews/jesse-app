//! **The capability map** — `bridge/capability-map.toml`, one row per grant in
//! [`DEFAULT_ALLOWED_TOOLS`], keyed by the exact grant string.
//!
//! The allowlist says WHAT a turn may call. Nothing said why each grant exists, which harness
//! it reaches, what credential or macOS feature it leans on, or where it can run once the
//! bridge is not on a Mac. The map records that, and the tests here keep it from rotting:
//!
//!   * every grant has exactly one row and every row names a grant that exists;
//!   * a row's `kind` and `server` agree with its grant string;
//!   * a row's `harnesses` agrees with what the CODE hands each harness today.
//!
//! The map changes nothing at runtime. It is read by the `tool-usage` audit (see
//! [`crate::toolusage`]) and by people; the allowlist stays the only boundary.

use crate::*;
use std::collections::{BTreeMap, BTreeSet};

/// The committed map, embedded so the audit and the tests read exactly what is in the tree.
pub const CAPABILITY_MAP_TOML: &str = include_str!("../capability-map.toml");

/// What a grant is, by the shape of its string.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "lowercase")]
pub enum GrantKind {
    /// `Read(…)`, `Edit(…)`, `Grep(…)`, `Glob(…)`: a path-scoped Claude Code file tool.
    File,
    /// `Bash(<verb>:*)`: one scoped shell verb.
    Bash,
    /// `Skill(<name>)`: one vault skill.
    Skill,
    /// `WebSearch`, `WebFetch`.
    Web,
    /// `mcp__<server>__<tool>`.
    Mcp,
}

impl GrantKind {
    pub fn label(self) -> &'static str {
        match self {
            GrantKind::File => "file",
            GrantKind::Bash => "bash",
            GrantKind::Skill => "skill",
            GrantKind::Web => "web",
            GrantKind::Mcp => "mcp",
        }
    }
}

/// Where a capability lives after the move off the Mac.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum GrantPlacement {
    /// Inside the agent's own pod, holding no credential.
    TurnPod,
    /// The bridge core does it with its own credentials.
    Core,
    /// An MCP server in its own pod, holding only its own secret.
    UpstreamPod,
    /// Needs macOS; served by the small service on the Mac.
    MacEdge,
    /// Removed.
    Drop,
}

impl GrantPlacement {
    pub fn label(self) -> &'static str {
        match self {
            GrantPlacement::TurnPod => "turn-pod",
            GrantPlacement::Core => "core",
            GrantPlacement::UpstreamPod => "upstream-pod",
            GrantPlacement::MacEdge => "mac-edge",
            GrantPlacement::Drop => "drop",
        }
    }
}

/// One grant's row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRow {
    pub kind: GrantKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    pub harnesses: Vec<String>,
    pub placement: GrantPlacement,
    pub dependency: String,
    pub workflows: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityMapFile {
    grant: BTreeMap<String, CapabilityRow>,
}

/// The parsed map: grant string → row.
///
/// A `BTreeMap` cannot hold a key twice, and neither can a TOML table: a grant written twice
/// in the file is a PARSE error, which is what makes "exactly one row per grant" a property
/// of reading the file rather than of a separate check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityMap {
    pub rows: BTreeMap<String, CapabilityRow>,
}

impl CapabilityMap {
    pub fn get(&self, grant: &str) -> Option<&CapabilityRow> {
        self.rows.get(grant)
    }
}

/// Parse a capability map from its TOML text.
pub fn parse_capability_map(text: &str) -> Result<CapabilityMap, String> {
    let file: CapabilityMapFile = toml::from_str(text).map_err(|e| e.to_string())?;
    Ok(CapabilityMap { rows: file.grant })
}

/// The committed map. Panics only if the committed file does not parse, which the tests in
/// this module make a build failure long before any binary reads it.
pub fn shipped_capability_map() -> CapabilityMap {
    parse_capability_map(CAPABILITY_MAP_TOML).expect("bridge/capability-map.toml parses")
}

/// The grants in an allowlist string, in order, trimmed, blanks dropped.
pub fn allowlist_grants(allowed: &str) -> Vec<&str> {
    allowed
        .split(',')
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .collect()
}

/// The kind a grant string has by its shape, or `None` for a shape this map does not know.
pub fn grant_kind(grant: &str) -> Option<GrantKind> {
    if grant.starts_with("mcp__") {
        return Some(GrantKind::Mcp);
    }
    if grant.starts_with("Bash(") && grant.ends_with(')') {
        return Some(GrantKind::Bash);
    }
    if grant.starts_with("Skill(") && grant.ends_with(')') {
        return Some(GrantKind::Skill);
    }
    if matches!(grant, "WebSearch" | "WebFetch") {
        return Some(GrantKind::Web);
    }
    for tool in FILE_GRANT_TOOLS {
        if grant.starts_with(&format!("{tool}(")) && grant.ends_with(')') {
            return Some(GrantKind::File);
        }
    }
    None
}

/// The Claude Code file tools a path-scoped grant can name.
pub const FILE_GRANT_TOOLS: &[&str] = &["Read", "Edit", "Grep", "Glob"];

/// `(server, tool)` of an `mcp__<server>__<tool>` grant or trace name.
pub fn mcp_parts(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    (!server.is_empty() && !tool.is_empty()).then_some((server, tool))
}

/// Every harness id the map may name.
pub const MAP_HARNESSES: &[&str] = &[CLAUDE_CODE_ID, CODEX_ID, DIRECT_ID];

/// Everything wrong with `map` as a description of `allowed`. Empty means the map covers the
/// allowlist exactly: one row per grant, no row without a grant, and every row's own fields
/// consistent with its grant string.
pub fn capability_map_mismatches(map: &CapabilityMap, allowed: &str) -> Vec<String> {
    let mut out = Vec::new();
    let grants = allowlist_grants(allowed);
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for g in &grants {
        if !seen.insert(g) {
            out.push(format!("grant `{g}` is listed twice in the allowlist"));
        }
        if !map.rows.contains_key(*g) {
            out.push(format!("grant `{g}` has no row in capability-map.toml"));
        }
    }
    for (grant, row) in &map.rows {
        if !seen.contains(grant.as_str()) {
            out.push(format!(
                "capability-map.toml row `{grant}` names no grant in the allowlist"
            ));
        }
        match grant_kind(grant) {
            Some(k) if k == row.kind => {}
            Some(k) => out.push(format!(
                "row `{grant}` says kind `{}` but the grant is `{}`",
                row.kind.label(),
                k.label()
            )),
            None => out.push(format!(
                "row `{grant}` has a grant shape this map does not know"
            )),
        }
        let server = mcp_parts(grant).map(|(s, _)| s);
        if row.server.as_deref() != server {
            out.push(format!(
                "row `{grant}` says server {:?} but the grant names {:?}",
                row.server, server
            ));
        }
        for h in &row.harnesses {
            if !MAP_HARNESSES.contains(&h.as_str()) {
                out.push(format!("row `{grant}` names an unknown harness `{h}`"));
            }
        }
        if row.reason.trim().is_empty() {
            out.push(format!("row `{grant}` has no reason"));
        }
    }
    out
}

/// MCP rows that do not reach every harness: `(grant, missing harness ids)`.
///
/// The rule is that every harness gets the same servers and tools, so each of these is a
/// PARITY DEFECT. The map records them as they are; this names them.
pub fn parity_defects(map: &CapabilityMap) -> Vec<(String, Vec<&'static str>)> {
    map.rows
        .iter()
        .filter(|(_, r)| r.kind == GrantKind::Mcp)
        .filter_map(|(g, r)| {
            let missing: Vec<&'static str> = MAP_HARNESSES
                .iter()
                .copied()
                .filter(|h| !r.harnesses.iter().any(|x| x == h))
                .collect();
            (!missing.is_empty()).then(|| (g.clone(), missing))
        })
        .collect()
}

/// Which harnesses the CODE hands `grant` to, under `cfg`, at the write level of a main turn.
///
///   * claude-code: every grant in `cfg.allowed_tools`, which is its `--allowedTools`.
///   * codex: an `mcp__` grant whose server is in the main MCP set, because
///     [`granted_mcp_tools`] reads Codex's `enabled_tools` out of the same allowlist. Codex
///     consumes no other grant: its shell and file edits are an OS sandbox, not a list.
///   * direct: an `mcp__` name that the deployment's `[[direct.mcp]]` grants at `Write`.
pub fn harnesses_reaching(cfg: &Config, grant: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    if allowlist_grants(&cfg.allowed_tools).contains(&grant) {
        out.push(CLAUDE_CODE_ID);
    }
    if let Some((server, tool)) = mcp_parts(grant) {
        let codex_servers = mcp_config_servers(MAIN_CHILD_MCP_CONFIG);
        if codex_servers.iter().any(|s| s == server)
            && granted_mcp_tools(&cfg.allowed_tools, server)
                .iter()
                .any(|t| t == tool)
        {
            out.push(CODEX_ID);
        }
        let direct: Vec<String> = cfg
            .direct
            .mcp
            .iter()
            .flat_map(|g| g.granted_names_at(jesse_agent::tools::Level::from(Capability::Write)))
            .collect();
        if direct.iter().any(|n| n == grant) {
            out.push(DIRECT_ID);
        }
    }
    out
}

/// The server names in an `{"mcpServers":{…}}` config.
pub fn mcp_config_servers(config: &str) -> Vec<String> {
    serde_json::from_str::<Value>(config)
        .ok()
        .and_then(|v| {
            v.get("mcpServers")
                .and_then(|s| s.as_object())
                .map(|o| o.keys().cloned().collect())
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::test_config;

    #[test]
    fn the_committed_map_parses() {
        let map = shipped_capability_map();
        assert!(!map.rows.is_empty());
    }

    /// EVERY GRANT HAS EXACTLY ONE ROW AND EVERY ROW NAMES A GRANT THAT EXISTS. The second
    /// half of "exactly one" is the TOML parser's: see [`a_grant_written_twice_does_not_parse`].
    #[test]
    fn every_grant_has_exactly_one_row_and_every_row_names_a_grant() {
        let map = shipped_capability_map();
        let errors = capability_map_mismatches(&map, DEFAULT_ALLOWED_TOOLS);
        assert!(
            errors.is_empty(),
            "capability-map.toml drifted:\n{}",
            errors.join("\n")
        );
        assert_eq!(
            map.rows.len(),
            allowlist_grants(DEFAULT_ALLOWED_TOOLS).len(),
            "one row per grant"
        );
    }

    /// THE NEGATIVE: remove a grant in a test COPY of the allowlist and the assertion fires,
    /// naming the row that is now orphaned. Without this the check above could be vacuous.
    #[test]
    fn removing_a_grant_from_a_copy_makes_the_assertion_fire() {
        let map = shipped_capability_map();
        let victim = "mcp__plex__media_search";
        let copy: Vec<&str> = allowlist_grants(DEFAULT_ALLOWED_TOOLS)
            .into_iter()
            .filter(|g| *g != victim)
            .collect();
        assert_eq!(
            copy.len() + 1,
            allowlist_grants(DEFAULT_ALLOWED_TOOLS).len(),
            "the victim must be a real grant, or this test proves nothing"
        );
        let errors = capability_map_mismatches(&map, &copy.join(","));
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains(victim) && errors[0].contains("names no grant"));
    }

    /// The other direction: a grant added to the allowlist with no row fires too.
    #[test]
    fn adding_a_grant_without_a_row_makes_the_assertion_fire() {
        let map = shipped_capability_map();
        let widened = format!("{DEFAULT_ALLOWED_TOOLS},mcp__plex__media_delete");
        let errors = capability_map_mismatches(&map, &widened);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("mcp__plex__media_delete") && errors[0].contains("no row"));
    }

    #[test]
    fn a_grant_written_twice_does_not_parse() {
        let row = "kind = \"web\"\nharnesses = []\nplacement = \"turn-pod\"\n\
                   dependency = \"x\"\nworkflows = \"x\"\nreason = \"x\"\n";
        let twice = format!("[grant.\"WebSearch\"]\n{row}\n[grant.\"WebSearch\"]\n{row}");
        assert!(parse_capability_map(&twice).is_err());
    }

    #[test]
    fn a_row_whose_kind_or_server_disagrees_with_its_grant_is_named() {
        let mut map = shipped_capability_map();
        let row = map.rows.get_mut("mcp__qmd__query").unwrap();
        row.kind = GrantKind::Web;
        row.server = Some("slack".into());
        let errors = capability_map_mismatches(&map, DEFAULT_ALLOWED_TOOLS);
        assert_eq!(errors.len(), 2, "{errors:?}");
    }

    /// THE `harnesses` FIELD IS DERIVED FROM THE CODE, NOT TYPED BY HAND. Each row must say
    /// exactly which harnesses [`harnesses_reaching`] finds under the shipped config. When a
    /// later change gives direct its MCP servers, or takes one from Codex, this fails until
    /// the map says so.
    #[test]
    fn every_rows_harnesses_match_what_the_code_hands_each_harness() {
        let cfg = test_config();
        assert!(
            cfg.direct.mcp.is_empty(),
            "the shipped posture grants direct no MCP"
        );
        let map = shipped_capability_map();
        let mut wrong = Vec::new();
        for (grant, row) in &map.rows {
            let mut want: Vec<&str> = harnesses_reaching(&cfg, grant);
            want.sort_unstable();
            let mut have: Vec<&str> = row.harnesses.iter().map(String::as_str).collect();
            have.sort_unstable();
            if want != have {
                wrong.push(format!("{grant}: map says {have:?}, code says {want:?}"));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// The parity defects as they stand: every MCP grant misses `direct`, because the shipped
    /// direct posture has no `[[direct.mcp]]`. Pinned so that closing them is a visible change.
    #[test]
    fn the_parity_defects_are_exactly_the_mcp_grants_direct_does_not_reach() {
        let map = shipped_capability_map();
        let defects = parity_defects(&map);
        let mcp = map
            .rows
            .values()
            .filter(|r| r.kind == GrantKind::Mcp)
            .count();
        assert_eq!(defects.len(), mcp);
        assert!(defects
            .iter()
            .all(|(_, missing)| missing == &vec![DIRECT_ID]));
    }

    /// The placement seeds the spec fixed, held so a casual edit cannot move one.
    #[test]
    fn the_seeded_placements_hold() {
        let map = shipped_capability_map();
        let server_placement = |server: &str| -> BTreeSet<GrantPlacement> {
            map.rows
                .values()
                .filter(|r| r.server.as_deref() == Some(server))
                .map(|r| r.placement)
                .collect()
        };
        for s in [
            "homeassistant",
            "roon",
            "unifi",
            "routeros",
            "proxmox",
            "slack",
            "google",
            "google-perseido",
            "github",
            "fastmail",
            "whatsapp",
            "browser",
            "places",
            "inbound",
            "kubernetes",
            "plex",
            "build",
        ] {
            assert_eq!(
                server_placement(s),
                BTreeSet::from([GrantPlacement::UpstreamPod]),
                "{s}"
            );
        }
        for s in ["qmd", "tag1", "rybbit", "clockify"] {
            assert_eq!(
                server_placement(s),
                BTreeSet::from([GrantPlacement::Core]),
                "{s}"
            );
        }
        assert_eq!(
            server_placement("imcp"),
            BTreeSet::from([GrantPlacement::MacEdge])
        );
        for (g, r) in &map.rows {
            match r.kind {
                GrantKind::File => assert_eq!(r.placement, GrantPlacement::TurnPod, "{g}"),
                GrantKind::Skill if g == "Skill(health-export-import)" => {
                    assert_eq!(r.placement, GrantPlacement::Drop);
                    assert_eq!(r.reason, "skill deleted 2026-08-15");
                }
                GrantKind::Skill => assert_eq!(r.placement, GrantPlacement::TurnPod, "{g}"),
                GrantKind::Bash if g.starts_with("Bash(gh ") || g == "Bash(git:*)" => {
                    assert_eq!(r.placement, GrantPlacement::Core, "{g}")
                }
                _ => {}
            }
        }
        assert!(map
            .rows
            .values()
            .any(|r| r.server.as_deref() == Some("build")
                && r.note
                    .as_deref()
                    .is_some_and(|n| n.contains("Linux path does not exist yet"))));
    }

    /// The counts by kind, from the code: file, bash, skill, web and mcp, and the server count.
    #[test]
    fn the_grant_counts_by_kind() {
        let map = shipped_capability_map();
        let count = |k: GrantKind| map.rows.values().filter(|r| r.kind == k).count();
        let servers: BTreeSet<&str> = map
            .rows
            .values()
            .filter_map(|r| r.server.as_deref())
            .collect();
        assert_eq!(
            (
                count(GrantKind::File),
                count(GrantKind::Bash),
                count(GrantKind::Skill)
            ),
            (4, 29, 6)
        );
        assert_eq!((count(GrantKind::Web), count(GrantKind::Mcp)), (2, 324));
        assert_eq!(servers.len(), 22);
    }

    /// **`capability_args` STAYS BYTE-IDENTICAL.** A committed golden of every harness's
    /// `capability_args` for every row it ships, under the shipped config. This change (the
    /// capability map, the trace fields, the audit) must not move a byte of what a child is
    /// handed, and any later change that does fails here before it fails the startup gate.
    ///
    /// The golden is also cross-checked against the containment records' own `toolset_args`
    /// for every row that has one, so it cannot be "fixed" by re-blessing it alone.
    #[test]
    fn capability_args_match_the_committed_golden_byte_for_byte() {
        let cfg = test_config();
        let golden: BTreeMap<String, BTreeMap<String, Vec<String>>> =
            serde_json::from_str(include_str!("../golden/capability-args.json"))
                .expect("golden parses");
        let harnesses: [(&str, Box<dyn Harness>); 3] = [
            (CLAUDE_CODE_ID, Box::new(ClaudeCode)),
            (CODEX_ID, Box::new(Codex)),
            (DIRECT_ID, Box::new(Direct)),
        ];
        let mut running: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
        for (id, h) in &harnesses {
            for row in h.shipped_rows() {
                running.entry(id.to_string()).or_default().insert(
                    row.label(),
                    h.capability_args(&cfg, row.capability, row.mcp),
                );
            }
        }
        assert_eq!(
            running,
            golden,
            "capability_args moved; this change must not move them. Running:\n{}",
            serde_json::to_string_pretty(&running).unwrap()
        );
        for (id, text) in CONTAINMENT_RECORDS {
            let record = parse_results(text).expect("record parses");
            for r in &record.rows {
                let label = format!("{}/{}", r.capability, r.mcp_set);
                let g = &golden[*id][&label];
                assert_eq!(
                    g, &r.toolset_args,
                    "{id} {label}: golden and record disagree"
                );
            }
        }
    }
}
