//! **The `tool-usage` audit**: which grants real turns used, over the window the timing log
//! holds, joined against the capability map.
//!
//! It reads `turn-timings.jsonl` (content free: tool names, counts, durations, and from
//! schema 2 the harness, the model and a per-call outcome) and prints three sections:
//!
//!   1. grants used, with call and distinct-turn counts;
//!   2. grants never used;
//!   3. trace tool names that match no grant, each with its count, the harness when the
//!      record names one, and why the call was possible at all.
//!
//! **WHAT IT CAN AND CANNOT MATCH.** MCP and web grants match a trace name exactly. A file
//! grant (`Read(//${WORKSPACE}/**)`) matches its bare tool name (`Read`), which is all the
//! trace records. `Bash` and `Skill` are bare CLASS names in the trace, so the 29 `Bash(…)`
//! and 6 `Skill(…)` grants cannot be told apart: they are reported as two class rows with
//! their totals and are never listed as "never used".
//!
//! Grouping by harness and model is only possible for records that carry them (schema 2,
//! bridge 0.168.0). Older records go in an `unknown` group.
//!
//! Read-only: it never writes the log, the map or anything else.

use crate::*;
use std::collections::{BTreeMap, BTreeSet};

/// The group name for a record that predates the `harness` and `model` fields.
pub const UNKNOWN_GROUP: &str = "unknown";

/// The direct harness's own typed tools. Its boundary is its manifest, not this allowlist.
pub const DIRECT_NATIVE_TOOLS: &[&str] = &[
    "vault_list",
    "vault_read",
    "vault_search",
    "vault_write",
    "vault_edit",
    "vault_move",
    "fetch_url",
    "deliver_artifact",
];

/// Claude Code built-ins that are not in the containment record's observed write-level root
/// but are the same tools under a current name (`Agent` is `Task`'s) or are part of the
/// CLI's permission-free set.
const EXTRA_UNGATED_BUILTINS: &[&str] = &["Agent", "TodoWrite", "BashOutput", "KillShell"];

/// File-editing tools Claude Code checks against `Edit(...)` rules.
const EDIT_RULE_TOOLS: &[&str] = &["Write", "MultiEdit"];

/// Why a trace name that matches no grant was callable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnmatchedReason {
    /// A Claude Code built-in the allowlist does not gate.
    BuiltinUngated,
    /// Admitted by a default permission rule rather than a grant of its own name.
    DefaultPermission,
    /// An MCP tool of a configured server that no grant names.
    OutsideAllowlist,
    /// A harness's own tool, behind its own boundary.
    HarnessNative,
    /// None of the above: a grant the map missed, or a built-in nobody recorded.
    Unknown,
}

impl UnmatchedReason {
    pub fn label(self) -> &'static str {
        match self {
            UnmatchedReason::BuiltinUngated => "builtin-ungated",
            UnmatchedReason::DefaultPermission => "default-permission",
            UnmatchedReason::OutsideAllowlist => "outside-allowlist",
            UnmatchedReason::HarnessNative => "harness-native",
            UnmatchedReason::Unknown => "unknown",
        }
    }

    /// Whether a call of this kind is a CONTAINMENT FINDING: a call to a tool the allowlist
    /// does not grant, by any rule. A file edit admitted by the `Edit(...)` rule and direct's
    /// own manifest tools are not.
    pub fn is_containment_finding(self) -> bool {
        matches!(
            self,
            UnmatchedReason::BuiltinUngated
                | UnmatchedReason::OutsideAllowlist
                | UnmatchedReason::Unknown
        )
    }

    pub fn explain(self) -> &'static str {
        match self {
            UnmatchedReason::BuiltinUngated => {
                "a Claude Code built-in the allowlist does not gate: a write-level turn has no \
                 --tools root restriction, and the CLI runs this tool without a permission check"
            }
            UnmatchedReason::DefaultPermission => {
                "a default permission: Claude Code checks file-editing tools against Edit(...) \
                 rules, so the Edit(//${WORKSPACE}/**) grant admits it under another name"
            }
            UnmatchedReason::OutsideAllowlist => {
                "an MCP tool the server registers and no grant names: Claude Code loads the whole \
                 server at the root, so the call is attempted and the permission layer must \
                 refuse it (a headless turn cannot answer the prompt); Codex never offers it"
            }
            UnmatchedReason::HarnessNative => {
                "the direct harness's own typed tool, bounded by its manifest rather than this \
                 allowlist"
            }
            UnmatchedReason::Unknown => {
                "not a grant, not a built-in the containment record observed, not a tool of a \
                 configured MCP server: a grant the map missed or a new built-in; investigate"
            }
        }
    }
}

/// Calls and distinct turns, with a per-group and per-outcome breakdown.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct UseCount {
    pub calls: usize,
    pub turns: usize,
    /// `harness/model` (or `unknown`) → calls.
    pub by_group: BTreeMap<String, usize>,
    /// `ok` / `error` / `refused` → calls, from records that carry an outcome.
    pub outcomes: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GrantUse {
    pub grant: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    pub placement: String,
    #[serde(flatten)]
    pub count: UseCount,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ClassUse {
    /// `Bash` or `Skill`.
    pub class: String,
    /// The grants of that class, none of which the trace can tell apart.
    pub grants: Vec<String>,
    #[serde(flatten)]
    pub count: UseCount,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NeverUsed {
    pub grant: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    pub placement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UnmatchedUse {
    pub tool: String,
    /// Harness id → calls, for records that name one; `unknown` for the rest.
    pub harnesses: BTreeMap<String, usize>,
    pub reason: UnmatchedReason,
    pub explanation: String,
    pub containment_finding: bool,
    #[serde(flatten)]
    pub count: UseCount,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GroupTotal {
    pub group: String,
    pub turns: usize,
    pub calls: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ToolUsageReport {
    pub since_days: u64,
    /// RFC3339 UTC: records whose `ended_at` is at or after this are in the window.
    pub window_start: String,
    pub records_in_file: usize,
    pub unparsed_lines: usize,
    pub records_in_window: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_in_window: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_in_window: Option<String>,
    pub records_with_identity: usize,
    pub groups: Vec<GroupTotal>,
    pub used: Vec<GrantUse>,
    pub classes: Vec<ClassUse>,
    pub never_used: Vec<NeverUsed>,
    pub unmatched: Vec<UnmatchedUse>,
}

/// Parse a timing log body: the records, and how many non-blank lines did not parse.
pub fn parse_timing_log(body: &str) -> (Vec<TurnTiming>, usize) {
    let mut out = Vec::new();
    let mut bad = 0;
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<TurnTiming>(line) {
            Ok(r) => out.push(r),
            Err(_) => bad += 1,
        }
    }
    (out, bad)
}

fn group_of(r: &TurnTiming) -> String {
    match (&r.harness, &r.model) {
        (Some(h), Some(m)) => format!("{h}/{m}"),
        (Some(h), None) => format!("{h}/{UNKNOWN_GROUP}"),
        _ => UNKNOWN_GROUP.to_string(),
    }
}

/// The Claude Code built-ins at the write-level root, as the committed containment record
/// observed them in the child's own init event, plus [`EXTRA_UNGATED_BUILTINS`].
pub fn claude_code_root_builtins() -> BTreeSet<String> {
    let mut out: BTreeSet<String> = EXTRA_UNGATED_BUILTINS
        .iter()
        .map(|s| s.to_string())
        .collect();
    if let Some(text) = containment_record(CLAUDE_CODE_ID) {
        if let Ok(record) = parse_results(text) {
            for row in record.rows.iter().filter(|r| r.capability == "write") {
                out.extend(
                    row.root_tools
                        .iter()
                        .filter(|t| !t.starts_with("mcp__"))
                        .cloned(),
                );
            }
        }
    }
    out
}

/// What a trace name is, against the map.
enum Match<'a> {
    Grant(&'a str),
    Class(&'static str),
    Unmatched(UnmatchedReason),
}

fn classify<'a>(
    map: &'a CapabilityMap,
    builtins: &BTreeSet<String>,
    servers: &BTreeSet<&str>,
    tool: &str,
) -> Match<'a> {
    if tool == "Bash" {
        return Match::Class("Bash");
    }
    if tool == "Skill" {
        return Match::Class("Skill");
    }
    if let Some((g, _)) = map.rows.get_key_value(tool) {
        return Match::Grant(g.as_str());
    }
    if FILE_GRANT_TOOLS.contains(&tool) {
        let prefix = format!("{tool}(");
        if let Some(g) = map.rows.keys().find(|g| g.starts_with(&prefix)) {
            return Match::Grant(g.as_str());
        }
    }
    if EDIT_RULE_TOOLS.contains(&tool) {
        return Match::Unmatched(UnmatchedReason::DefaultPermission);
    }
    if DIRECT_NATIVE_TOOLS.contains(&tool) {
        return Match::Unmatched(UnmatchedReason::HarnessNative);
    }
    if let Some((server, _)) = mcp_parts(tool) {
        if servers.contains(server) {
            return Match::Unmatched(UnmatchedReason::OutsideAllowlist);
        }
        return Match::Unmatched(UnmatchedReason::Unknown);
    }
    if builtins.contains(tool) {
        return Match::Unmatched(UnmatchedReason::BuiltinUngated);
    }
    Match::Unmatched(UnmatchedReason::Unknown)
}

fn bump(count: &mut UseCount, group: &str, outcome: Option<ToolCallOutcome>) {
    count.calls += 1;
    *count.by_group.entry(group.to_string()).or_default() += 1;
    if let Some(o) = outcome {
        *count.outcomes.entry(o.label().to_string()).or_default() += 1;
    }
}

/// Build the report over the records whose `ended_at` is at or after `window_start`.
pub fn tool_usage_report(
    map: &CapabilityMap,
    records: &[TurnTiming],
    unparsed_lines: usize,
    since_days: u64,
    window_start: &str,
) -> ToolUsageReport {
    let builtins = claude_code_root_builtins();
    let servers: BTreeSet<&str> = map
        .rows
        .values()
        .filter_map(|r| r.server.as_deref())
        .collect();
    let in_window: Vec<&TurnTiming> = records
        .iter()
        .filter(|r| r.ended_at.as_str() >= window_start)
        .collect();

    let mut grant_counts: BTreeMap<&str, UseCount> = BTreeMap::new();
    let mut class_counts: BTreeMap<&'static str, UseCount> = BTreeMap::new();
    let mut unmatched: BTreeMap<String, (UnmatchedReason, UseCount, BTreeMap<String, usize>)> =
        BTreeMap::new();
    let mut groups: BTreeMap<String, (usize, usize)> = BTreeMap::new();

    for r in &in_window {
        let group = group_of(r);
        let g = groups.entry(group.clone()).or_default();
        g.0 += 1;
        g.1 += r.tools.len();
        // Distinct turns: each key counted once per record.
        let mut seen_grant: BTreeSet<&str> = BTreeSet::new();
        let mut seen_class: BTreeSet<&str> = BTreeSet::new();
        let mut seen_tool: BTreeSet<&str> = BTreeSet::new();
        for t in &r.tools {
            match classify(map, &builtins, &servers, &t.tool) {
                Match::Grant(grant) => {
                    let c = grant_counts.entry(grant).or_default();
                    bump(c, &group, t.outcome);
                    if seen_grant.insert(grant) {
                        c.turns += 1;
                    }
                }
                Match::Class(class) => {
                    let c = class_counts.entry(class).or_default();
                    bump(c, &group, t.outcome);
                    if seen_class.insert(class) {
                        c.turns += 1;
                    }
                }
                Match::Unmatched(reason) => {
                    let e = unmatched
                        .entry(t.tool.clone())
                        .or_insert_with(|| (reason, UseCount::default(), BTreeMap::new()));
                    bump(&mut e.1, &group, t.outcome);
                    let h = r
                        .harness
                        .clone()
                        .unwrap_or_else(|| UNKNOWN_GROUP.to_string());
                    *e.2.entry(h).or_default() += 1;
                    if seen_tool.insert(t.tool.as_str()) {
                        e.1.turns += 1;
                    }
                }
            }
        }
    }

    let row_of = |g: &str| map.get(g).expect("a matched grant has a row");
    let mut used: Vec<GrantUse> = grant_counts
        .into_iter()
        .map(|(g, count)| {
            let row = row_of(g);
            GrantUse {
                grant: g.to_string(),
                kind: row.kind.label().to_string(),
                server: row.server.clone(),
                placement: row.placement.label().to_string(),
                count,
            }
        })
        .collect();
    used.sort_by(|a, b| {
        b.count
            .calls
            .cmp(&a.count.calls)
            .then(a.grant.cmp(&b.grant))
    });

    let class_grants = |kind: GrantKind| -> Vec<String> {
        map.rows
            .iter()
            .filter(|(_, r)| r.kind == kind)
            .map(|(g, _)| g.clone())
            .collect()
    };
    let classes = vec![
        ClassUse {
            class: "Bash".to_string(),
            grants: class_grants(GrantKind::Bash),
            count: class_counts.remove("Bash").unwrap_or_default(),
        },
        ClassUse {
            class: "Skill".to_string(),
            grants: class_grants(GrantKind::Skill),
            count: class_counts.remove("Skill").unwrap_or_default(),
        },
    ];

    let used_set: BTreeSet<&str> = used.iter().map(|u| u.grant.as_str()).collect();
    let never_used: Vec<NeverUsed> = map
        .rows
        .iter()
        .filter(|(_, r)| !matches!(r.kind, GrantKind::Bash | GrantKind::Skill))
        .filter(|(g, _)| !used_set.contains(g.as_str()))
        .map(|(g, r)| NeverUsed {
            grant: g.clone(),
            kind: r.kind.label().to_string(),
            server: r.server.clone(),
            placement: r.placement.label().to_string(),
        })
        .collect();

    let mut unmatched: Vec<UnmatchedUse> = unmatched
        .into_iter()
        .map(|(tool, (reason, count, harnesses))| UnmatchedUse {
            tool,
            harnesses,
            reason,
            explanation: reason.explain().to_string(),
            containment_finding: reason.is_containment_finding(),
            count,
        })
        .collect();
    unmatched.sort_by(|a, b| b.count.calls.cmp(&a.count.calls).then(a.tool.cmp(&b.tool)));

    ToolUsageReport {
        since_days,
        window_start: window_start.to_string(),
        records_in_file: records.len(),
        unparsed_lines,
        records_in_window: in_window.len(),
        first_in_window: in_window.iter().map(|r| r.started_at.clone()).min(),
        last_in_window: in_window.iter().map(|r| r.ended_at.clone()).max(),
        records_with_identity: in_window.iter().filter(|r| r.harness.is_some()).count(),
        groups: groups
            .into_iter()
            .map(|(group, (turns, calls))| GroupTotal {
                group,
                turns,
                calls,
            })
            .collect(),
        used,
        classes,
        never_used,
        unmatched,
    }
}

fn outcomes_note(c: &UseCount) -> String {
    if c.outcomes.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = c.outcomes.iter().map(|(k, v)| format!("{k} {v}")).collect();
    format!("  [{}]", parts.join(", "))
}

fn groups_note(m: &BTreeMap<String, usize>) -> String {
    let parts: Vec<String> = m.iter().map(|(k, v)| format!("{k} {v}")).collect();
    parts.join(", ")
}

/// The human-readable report.
pub fn render_tool_usage(rep: &ToolUsageReport) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "tool-usage: turn records against bridge/capability-map.toml"
    );
    let _ = writeln!(
        s,
        "Window: the last {} days (ended at or after {}). {} records in the file, {} in the \
         window{}.",
        rep.since_days,
        rep.window_start,
        rep.records_in_file,
        rep.records_in_window,
        match (&rep.first_in_window, &rep.last_in_window) {
            (Some(a), Some(b)) => format!(", {a} to {b}"),
            _ => String::new(),
        }
    );
    if rep.unparsed_lines > 0 {
        let _ = writeln!(
            s,
            "{} lines did not parse and were skipped.",
            rep.unparsed_lines
        );
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "Groups (harness/model, from records that carry them):");
    for g in &rep.groups {
        let _ = writeln!(
            s,
            "  {:<32} {:>6} turns {:>7} calls",
            g.group, g.turns, g.calls
        );
    }
    if rep.records_with_identity < rep.records_in_window {
        let _ = writeln!(
            s,
            "  ({} records predate the harness and model fields (bridge 0.168.0) and are \
             grouped as `unknown`.)",
            rep.records_in_window - rep.records_with_identity
        );
    }

    let findings: Vec<&UnmatchedUse> = rep
        .unmatched
        .iter()
        .filter(|u| u.containment_finding)
        .collect();
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "CONTAINMENT FINDINGS: {} tool names were called that no grant names",
        findings.len()
    );
    for u in &findings {
        let _ = writeln!(
            s,
            "  {:<40} {:>6} calls {:>5} turns  [{}]{}",
            u.tool,
            u.count.calls,
            u.count.turns,
            u.reason.label(),
            outcomes_note(&u.count)
        );
    }
    if !findings.is_empty() {
        let _ = writeln!(s, "  Details and reasons are in section 3.");
    }

    let matchable = rep.used.len() + rep.never_used.len();
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "1. GRANTS USED: {} of {} individually matchable grants",
        rep.used.len(),
        matchable
    );
    let _ = writeln!(s, "  {:>7} {:>6}  grant  [placement]", "calls", "turns");
    for u in &rep.used {
        let _ = writeln!(
            s,
            "  {:>7} {:>6}  {}  [{}]{}",
            u.count.calls,
            u.count.turns,
            u.grant,
            u.placement,
            outcomes_note(&u.count)
        );
    }
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "  Class rows. The trace records `Bash` and `Skill` as bare class names, never the \
         scoped grant a call matched, so the {} Bash(...) grants and the {} Skill(...) grants \
         cannot be told apart. Each class is one row with its total, and none of its grants is \
         ever reported as never used.",
        rep.classes
            .iter()
            .find(|c| c.class == "Bash")
            .map_or(0, |c| c.grants.len()),
        rep.classes
            .iter()
            .find(|c| c.class == "Skill")
            .map_or(0, |c| c.grants.len())
    );
    for c in &rep.classes {
        let _ = writeln!(
            s,
            "  {:>7} {:>6}  {} (class, {} grants; by group: {}){}",
            c.count.calls,
            c.count.turns,
            c.class,
            c.grants.len(),
            if c.count.by_group.is_empty() {
                "none".to_string()
            } else {
                groups_note(&c.count.by_group)
            },
            outcomes_note(&c.count)
        );
    }

    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "2. GRANTS NEVER USED: {} of {} individually matchable grants",
        rep.never_used.len(),
        matchable
    );
    let mut by_server: BTreeMap<String, Vec<&NeverUsed>> = BTreeMap::new();
    for n in &rep.never_used {
        by_server
            .entry(n.server.clone().unwrap_or_else(|| format!("({})", n.kind)))
            .or_default()
            .push(n);
    }
    let used_by_server: BTreeMap<String, usize> =
        rep.used.iter().fold(BTreeMap::new(), |mut m, u| {
            *m.entry(u.server.clone().unwrap_or_else(|| format!("({})", u.kind)))
                .or_default() += 1;
            m
        });
    for (server, list) in &by_server {
        let used_here = used_by_server.get(server).copied().unwrap_or(0);
        let names: Vec<&str> = list
            .iter()
            .map(|n| {
                n.server
                    .as_ref()
                    .and_then(|sv| n.grant.strip_prefix(&format!("mcp__{sv}__")))
                    .unwrap_or(n.grant.as_str())
            })
            .collect();
        let _ = writeln!(
            s,
            "  {} [{}]: {} of {} never used: {}",
            server,
            list[0].placement,
            list.len(),
            list.len() + used_here,
            names.join(", ")
        );
    }

    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "3. TRACE NAMES THAT MATCH NO GRANT: {}",
        rep.unmatched.len()
    );
    for u in &rep.unmatched {
        let _ = writeln!(
            s,
            "  {}: {} calls in {} turns; harness: {}{}",
            u.tool,
            u.count.calls,
            u.count.turns,
            groups_note(&u.harnesses),
            outcomes_note(&u.count)
        );
        let _ = writeln!(
            s,
            "    {}{}: {}",
            u.reason.label(),
            if u.containment_finding {
                ", containment finding"
            } else {
                ""
            },
            u.explanation
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(
        id: &str,
        ended: &str,
        tools: &[(&str, Option<ToolCallOutcome>)],
        who: Option<(&str, &str)>,
    ) -> TurnTiming {
        TurnTiming {
            v: if who.is_some() { 2 } else { 1 },
            job_id: id.to_string(),
            started_at: ended.to_string(),
            ended_at: ended.to_string(),
            elapsed_ms: 1,
            status: "done".to_string(),
            tool_calls: tools.len(),
            tools: tools
                .iter()
                .map(|(t, o)| ToolCallTiming {
                    tool: t.to_string(),
                    ms: 1,
                    outcome: *o,
                })
                .collect(),
            usage: None,
            cost_usd: None,
            harness: who.map(|(h, _)| h.to_string()),
            model: who.map(|(_, m)| m.to_string()),
        }
    }

    fn report(records: &[TurnTiming]) -> ToolUsageReport {
        tool_usage_report(
            &shipped_capability_map(),
            records,
            0,
            7,
            "2026-10-01T00:00:00Z",
        )
    }

    #[test]
    fn exact_mcp_and_web_names_and_bare_file_names_match_their_grants() {
        let r = report(&[rec(
            "a",
            "2026-10-02T00:00:00Z",
            &[
                ("mcp__qmd__query", None),
                ("mcp__qmd__query", None),
                ("WebFetch", None),
                ("Read", None),
            ],
            None,
        )]);
        let used: BTreeMap<&str, (usize, usize)> = r
            .used
            .iter()
            .map(|u| (u.grant.as_str(), (u.count.calls, u.count.turns)))
            .collect();
        assert_eq!(
            used["mcp__qmd__query"],
            (2, 1),
            "two calls, one distinct turn"
        );
        assert_eq!(used["WebFetch"], (1, 1));
        assert_eq!(used["Read(//${WORKSPACE}/**)"], (1, 1));
        assert!(r.unmatched.is_empty());
    }

    #[test]
    fn bash_and_skill_are_class_rows_and_never_never_used() {
        let r = report(&[rec(
            "a",
            "2026-10-02T00:00:00Z",
            &[("Bash", None), ("Bash", None)],
            None,
        )]);
        let bash = r.classes.iter().find(|c| c.class == "Bash").unwrap();
        assert_eq!((bash.count.calls, bash.count.turns), (2, 1));
        assert_eq!(bash.grants.len(), 29);
        let skill = r.classes.iter().find(|c| c.class == "Skill").unwrap();
        assert_eq!((skill.count.calls, skill.grants.len()), (0, 6));
        assert!(r
            .never_used
            .iter()
            .all(|n| !n.grant.starts_with("Bash(") && !n.grant.starts_with("Skill(")));
        let text = render_tool_usage(&r);
        assert!(text.contains("cannot be told apart"), "{text}");
    }

    #[test]
    fn the_never_used_section_is_every_matchable_grant_not_used() {
        let r = report(&[]);
        let map = shipped_capability_map();
        let matchable = map
            .rows
            .values()
            .filter(|row| !matches!(row.kind, GrantKind::Bash | GrantKind::Skill))
            .count();
        assert_eq!(r.never_used.len(), matchable);
        assert!(r.used.is_empty());
    }

    #[test]
    fn unmatched_names_carry_a_count_a_harness_and_a_reason() {
        let r = report(&[
            rec(
                "a",
                "2026-10-02T00:00:00Z",
                &[
                    ("Agent", None),
                    ("Write", None),
                    ("mcp__browser__browser_evaluate", None),
                    ("Mystery", None),
                ],
                None,
            ),
            rec(
                "b",
                "2026-10-03T00:00:00Z",
                &[
                    ("Agent", Some(ToolCallOutcome::Ok)),
                    ("vault_read", Some(ToolCallOutcome::Ok)),
                ],
                Some(("claude-code", "opus")),
            ),
        ]);
        let by: BTreeMap<&str, &UnmatchedUse> =
            r.unmatched.iter().map(|u| (u.tool.as_str(), u)).collect();
        assert_eq!(by["Agent"].reason, UnmatchedReason::BuiltinUngated);
        assert_eq!((by["Agent"].count.calls, by["Agent"].count.turns), (2, 2));
        assert_eq!(by["Agent"].harnesses["unknown"], 1);
        assert_eq!(by["Agent"].harnesses["claude-code"], 1);
        assert_eq!(by["Agent"].count.outcomes["ok"], 1);
        assert_eq!(by["Write"].reason, UnmatchedReason::DefaultPermission);
        assert!(!by["Write"].containment_finding);
        assert_eq!(
            by["mcp__browser__browser_evaluate"].reason,
            UnmatchedReason::OutsideAllowlist
        );
        assert!(by["mcp__browser__browser_evaluate"].containment_finding);
        assert_eq!(by["Mystery"].reason, UnmatchedReason::Unknown);
        assert_eq!(by["vault_read"].reason, UnmatchedReason::HarnessNative);
        let text = render_tool_usage(&r);
        assert!(text.contains("CONTAINMENT FINDINGS: 3"), "{text}");
    }

    #[test]
    fn records_without_identity_group_as_unknown_and_the_window_is_honoured() {
        let r = report(&[
            rec("old", "2026-09-01T00:00:00Z", &[("Read", None)], None),
            rec("v1", "2026-10-02T00:00:00Z", &[("Read", None)], None),
            rec(
                "v2",
                "2026-10-02T00:00:00Z",
                &[("Read", Some(ToolCallOutcome::Error))],
                Some(("codex", "codex-write")),
            ),
        ]);
        assert_eq!(
            r.records_in_window, 2,
            "the September record is outside the window"
        );
        let groups: Vec<&str> = r.groups.iter().map(|g| g.group.as_str()).collect();
        assert_eq!(groups, vec!["codex/codex-write", "unknown"]);
        let read = r
            .used
            .iter()
            .find(|u| u.grant.starts_with("Read("))
            .unwrap();
        assert_eq!(read.count.by_group["unknown"], 1);
        assert_eq!(read.count.by_group["codex/codex-write"], 1);
        assert_eq!(read.count.outcomes["error"], 1);
    }

    #[test]
    fn the_built_in_list_comes_from_the_containment_record() {
        let b = claude_code_root_builtins();
        for t in ["ToolSearch", "SendMessage", "ListAgents", "Agent"] {
            assert!(b.contains(t), "{t}");
        }
        assert!(b.iter().all(|t| !t.starts_with("mcp__")));
    }

    #[test]
    fn the_json_report_is_the_same_data() {
        let r = report(&[rec("a", "2026-10-02T00:00:00Z", &[("Bash", None)], None)]);
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["classes"][0]["class"], "Bash");
        assert_eq!(v["classes"][0]["calls"], 1);
        assert!(v["never_used"].as_array().unwrap().len() > 300);
    }
}
