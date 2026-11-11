//! **READING THE ACTIVITY LEDGER** — a time range of its lines, and the overnight digest.
//!
//! Three readers share one function each:
//!
//!   * `GET /jesse/activity?since=<rfc3339>&until=<rfc3339>` — the raw lines of the range, as
//!     JSON, at most [`ACTIVITY_QUERY_CAP`] of them ([`jesse_activity`]).
//!   * `GET /jesse/activity/digest?since=<rfc3339>` — the same range rendered as short
//!     Markdown for a person deciding what to act on ([`jesse_activity_digest`]).
//!   * `jesse-bridge activity-digest --since <rfc3339> [--until <rfc3339>] [--state-dir <dir>]`
//!     — the same Markdown on stdout, read straight off the files; it never starts the server
//!     ([`run_digest_cli`]).
//!
//! The ledger is content-free by construction ([`crate::activity`]), so everything here is
//! too: the digest can only say what the lines hold — identifiers, enumerations, timestamps,
//! counts, hosts and vault-relative paths.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDate, TimeZone};
use serde_json::Value;

use crate::*;

/// The most lines one query returns. A range holding more is cut at the cap (oldest first) and
/// reported `truncated: true`; ask again with a later `since` for the rest.
pub const ACTIVITY_QUERY_CAP: usize = 10_000;

/// The digest's default window when `since` is absent.
pub const DIGEST_DEFAULT_WINDOW_MS: u64 = 24 * 3600 * 1000;

/// The instant a line describes: a turn's `start_ms`, every other kind's `at_ms`. `None` for a
/// line that is not a ledger object.
pub fn line_at_ms(v: &Value) -> Option<u64> {
    let kind = v.get("kind")?.as_str()?;
    let field = if kind == "turn" { "start_ms" } else { "at_ms" };
    v.get(field)?.as_u64()
}

/// Every ledger line in `dir` whose instant ([`line_at_ms`]) is in `[since_ms, until_ms)`,
/// oldest first, at most `cap` of them; `true` when the range held more.
///
/// Day files are the bridge's LOCAL calendar, so the files read are the local days of the two
/// ends widened by one day each way (a zone change or a turn that crossed midnight cannot hide
/// a line), and each line is then filtered on its own timestamp. Malformed lines are skipped.
/// A range wider than the retention window reads only the retained days.
///
/// Reading stops once a whole day has pushed the count past `cap`: later files hold later
/// lines (a turn line is written when the turn ENDS, so it can sit one file after its
/// `start_ms` — the cut is at the cap either way, and `truncated` says so).
pub fn read_range(dir: &Path, since_ms: u64, until_ms: u64, cap: usize) -> (Vec<Value>, bool) {
    if until_ms <= since_ms {
        return (Vec::new(), false);
    }
    let Some(first) = local_day(since_ms) else {
        return (Vec::new(), false);
    };
    let Some(last) = local_day(until_ms) else {
        return (Vec::new(), false);
    };
    let last = last + chrono::Duration::days(1);
    let floor = last - chrono::Duration::days(crate::activity::ACTIVITY_RETENTION_DAYS + 3);
    let mut day = (first - chrono::Duration::days(1)).max(floor);
    let mut out: Vec<(u64, Value)> = Vec::new();
    while day <= last {
        let path = dir.join(format!("{}.jsonl", day.format("%Y-%m-%d")));
        if let Ok(text) = std::fs::read_to_string(&path) {
            for raw in text.lines() {
                let Ok(v) = serde_json::from_str::<Value>(raw) else {
                    continue;
                };
                let Some(at) = line_at_ms(&v) else {
                    continue;
                };
                if at >= since_ms && at < until_ms {
                    out.push((at, v));
                }
            }
        }
        if out.len() > cap {
            break;
        }
        day += chrono::Duration::days(1);
    }
    // Stable: lines with one instant keep their file order.
    out.sort_by_key(|(at, _)| *at);
    let truncated = out.len() > cap;
    out.truncate(cap);
    (out.into_iter().map(|(_, v)| v).collect(), truncated)
}

fn local_day(ms: u64) -> Option<NaiveDate> {
    Local
        .timestamp_millis_opt(i64::try_from(ms).ok()?)
        .single()
        .map(|t| t.date_naive())
}

/// An RFC 3339 instant as Unix milliseconds. A `+` offset that arrived through a query string
/// unencoded has become a space; it is read back as the `+` it was.
pub fn parse_rfc3339_ms(s: &str) -> Option<u64> {
    let s = s.trim().replace(' ', "+");
    let t = chrono::DateTime::parse_from_rfc3339(&s).ok()?;
    u64::try_from(t.timestamp_millis()).ok()
}

/// Unix milliseconds as RFC 3339 UTC.
pub fn ms_to_rfc3339(ms: u64) -> String {
    chrono::Utc
        .timestamp_millis_opt(ms as i64)
        .single()
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

// ---- THE DIGEST ---------------------------------------------------------------------

/// What the digest knows about one configured scheduled job beyond what its turns recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestJob {
    pub id: String,
    /// The job's own model, when it names one (otherwise it runs the active model).
    pub model: Option<String>,
    /// The harness that model runs on, when the registry knows it.
    pub harness: Option<String>,
}

/// The configured jobs of `schedule`, with each model's harness from `registry`.
pub fn digest_jobs(schedule: &Schedule, registry: &ModelRegistry) -> Vec<DigestJob> {
    schedule
        .jobs
        .iter()
        .map(|j| DigestJob {
            id: j.id.clone(),
            model: j.model.clone(),
            harness: j
                .model
                .as_deref()
                .and_then(|m| registry.get(m))
                .map(|m| m.harness.clone()),
        })
        .collect()
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn n(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

/// Everything the ledger holds about one job id.
#[derive(Default)]
struct JobLines<'a> {
    tools: Vec<&'a Value>,
    models: Vec<&'a Value>,
    egress: Vec<&'a Value>,
    writes: Vec<&'a Value>,
    commits: Vec<&'a Value>,
}

impl JobLines<'_> {
    fn is_empty(&self) -> bool {
        self.tools.is_empty()
            && self.models.is_empty()
            && self.egress.is_empty()
            && self.writes.is_empty()
            && self.commits.is_empty()
    }

    fn cost_micros(&self) -> u64 {
        self.models.iter().map(|m| n(m, "cost_micros")).sum()
    }

    fn hosts(&self) -> BTreeSet<&str> {
        self.egress.iter().map(|e| s(e, "host")).collect()
    }

    /// The problems in these lines, one short phrase each.
    fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut refused_tools: BTreeMap<&str, u64> = BTreeMap::new();
        let mut failed_tools: BTreeMap<&str, u64> = BTreeMap::new();
        for t in &self.tools {
            match s(t, "outcome") {
                "refused" => *refused_tools.entry(s(t, "server")).or_default() += 1,
                "failed" | "truncated" => *failed_tools.entry(s(t, "server")).or_default() += 1,
                _ => {}
            }
        }
        for (server, c) in refused_tools {
            out.push(format!("tool calls refused on `{server}` ×{c}"));
        }
        for (server, c) in failed_tools {
            out.push(format!("tool calls failed on `{server}` ×{c}"));
        }
        let mut refused_hosts: BTreeMap<(&str, &str), u64> = BTreeMap::new();
        for e in &self.egress {
            if s(e, "verdict") == "refused" {
                *refused_hosts
                    .entry((s(e, "host"), s(e, "reason")))
                    .or_default() += 1;
            }
        }
        for ((host, reason), c) in refused_hosts {
            out.push(format!("egress to `{host}` refused ({reason}) ×{c}"));
        }
        let mut bad_status: BTreeMap<u64, u64> = BTreeMap::new();
        for m in &self.models {
            let st = n(m, "status");
            if !(200..300).contains(&st) {
                *bad_status.entry(st).or_default() += 1;
            }
        }
        for (st, c) in bad_status {
            out.push(if st == 0 {
                format!("model calls unreachable ×{c}")
            } else {
                format!("model calls HTTP {st} ×{c}")
            });
        }
        let invalid = self
            .writes
            .iter()
            .filter(|w| s(w, "path") == crate::activity::Token::INVALID)
            .count();
        if invalid > 0 {
            out.push(format!("writes outside the vault or unnamed ×{invalid}"));
        }
        out
    }
}

fn dollars(micros: u64) -> String {
    if micros > 0 && micros < 10_000 {
        format!("${:.4}", micros as f64 / 1e6)
    } else {
        format!("${:.2}", micros as f64 / 1e6)
    }
}

fn count(x: u64) -> String {
    match x {
        0..=999 => x.to_string(),
        1_000..=999_999 => format!("{:.1}k", x as f64 / 1e3),
        _ => format!("{:.1}M", x as f64 / 1e6),
    }
}

fn duration(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

fn clock(ms: u64, with_date: bool) -> String {
    match Local.timestamp_millis_opt(ms as i64).single() {
        Some(t) if with_date => t.format("%m-%d %H:%M").to_string(),
        Some(t) => t.format("%H:%M").to_string(),
        None => "?".into(),
    }
}

fn short_hash(h: &str) -> &str {
    h.get(..7).unwrap_or(h)
}

/// Render the lines of `[since_ms, until_ms)` as short Markdown, for a person deciding what to
/// act on: a "needs attention" list of everything that failed or was refused first, then each
/// scheduled job (origin `schedule:<id>`) with its runs in detail, then one line per other
/// turn (phone, strand tick), then bridge activity that belongs to no turn in the window.
///
/// Content-free because its input is: every value printed is a field of a ledger line.
pub fn render_digest(
    lines: &[Value],
    schedule: &[DigestJob],
    since_ms: u64,
    until_ms: u64,
) -> String {
    render_digest_with(lines, schedule, since_ms, until_ms, false)
}

/// [`render_digest`], saying when the line list was cut at [`ACTIVITY_QUERY_CAP`].
pub fn render_digest_with(
    lines: &[Value],
    schedule: &[DigestJob],
    since_ms: u64,
    until_ms: u64,
    truncated: bool,
) -> String {
    let with_date = local_day(since_ms) != local_day(until_ms.saturating_sub(1));
    // Turns, in order, and every other line grouped by its job id.
    let mut turns: Vec<&Value> = Vec::new();
    let mut by_job: BTreeMap<&str, JobLines> = BTreeMap::new();
    for v in lines {
        let job = s(v, "job_id");
        match s(v, "kind") {
            "turn" => turns.push(v),
            "tool_call" => by_job.entry(job).or_default().tools.push(v),
            "model_call" => by_job.entry(job).or_default().models.push(v),
            "egress" => by_job.entry(job).or_default().egress.push(v),
            "vault_write" => by_job.entry(job).or_default().writes.push(v),
            "commit" => by_job.entry(job).or_default().commits.push(v),
            _ => {}
        }
    }
    let empty = JobLines::default();
    let turn_ids: BTreeSet<&str> = turns.iter().map(|t| s(t, "job_id")).collect();

    // Scheduled runs by job id; everything else is an "other" turn.
    let mut scheduled: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    let mut others: Vec<&Value> = Vec::new();
    for t in &turns {
        match s(t, "origin").strip_prefix("schedule:") {
            Some(id) => scheduled.entry(id).or_default().push(t),
            None => others.push(t),
        }
    }

    let total_cost: u64 = by_job.values().map(JobLines::cost_micros).sum();
    let mut out = String::new();
    out.push_str(&format!(
        "# Activity {} – {}\n\n",
        clock(since_ms, true),
        clock(until_ms, true)
    ));
    out.push_str(&format!(
        "{} scheduled run(s), {} other turn(s), {} model cost.\n",
        scheduled.values().map(Vec::len).sum::<usize>(),
        others.len(),
        dollars(total_cost)
    ));
    if truncated {
        out.push_str(&format!(
            "\n**Truncated:** the window holds more than {ACTIVITY_QUERY_CAP} ledger lines; \
             only the oldest {ACTIVITY_QUERY_CAP} are summarised.\n"
        ));
    }

    // ---- Needs attention: everything that failed or was refused, first.
    // Turns that did not end `ok` lead; refusals and failed calls follow.
    let mut attention: Vec<String> = Vec::new();
    let mut refusals: Vec<String> = Vec::new();
    for t in &turns {
        let outcome = s(t, "outcome");
        let label = turn_label(t);
        if outcome != "ok" {
            attention.push(format!(
                "{label} **{outcome}** at {} (job `{}`)",
                clock(n(t, "start_ms"), with_date),
                s(t, "job_id")
            ));
        }
        for p in by_job.get(s(t, "job_id")).unwrap_or(&empty).problems() {
            refusals.push(format!("{label}: {p}"));
        }
    }
    for (job, jl) in &by_job {
        if !turn_ids.contains(job) {
            for p in jl.problems() {
                refusals.push(format!("`{job}` (no turn in window): {p}"));
            }
        }
    }
    attention.extend(refusals);
    out.push_str("\n## Needs attention\n\n");
    if attention.is_empty() {
        out.push_str("Nothing failed or was refused.\n");
    } else {
        for a in attention {
            out.push_str(&format!("- {a}\n"));
        }
    }

    // ---- Scheduled jobs: those with a problem first, then by id.
    if !scheduled.is_empty() {
        out.push_str("\n## Scheduled jobs\n");
        let mut ids: Vec<&str> = scheduled.keys().copied().collect();
        let troubled = |id: &str| {
            scheduled[id].iter().any(|t| {
                s(t, "outcome") != "ok"
                    || !by_job
                        .get(s(t, "job_id"))
                        .unwrap_or(&empty)
                        .problems()
                        .is_empty()
            })
        };
        ids.sort_by_key(|id| (!troubled(id), *id));
        for id in ids {
            let conf = schedule.iter().find(|j| j.id == id);
            out.push_str(&format!("\n### {id}\n\n"));
            for t in &scheduled[id] {
                let jl = by_job.get(s(t, "job_id")).unwrap_or(&empty);
                out.push_str(&scheduled_run(t, jl, conf, with_date));
            }
        }
    }
    let idle: Vec<&str> = schedule
        .iter()
        .map(|j| j.id.as_str())
        .filter(|id| !scheduled.contains_key(id))
        .collect();
    if !idle.is_empty() {
        out.push_str(&format!(
            "\nConfigured, no run in this window: {}\n",
            idle.iter()
                .map(|i| format!("`{i}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // ---- Other turns, one line each, in order.
    if !others.is_empty() {
        out.push_str("\n## Other turns\n\n");
        for t in &others {
            let jl = by_job.get(s(t, "job_id")).unwrap_or(&empty);
            out.push_str(&format!(
                "- {} {} · {} / {} · {} · {} · {} · {} tool(s) · {} host(s)\n",
                clock(n(t, "start_ms"), with_date),
                s(t, "origin"),
                s(t, "harness"),
                s(t, "model"),
                s(t, "outcome"),
                duration(n(t, "end_ms").saturating_sub(n(t, "start_ms"))),
                dollars(jl.cost_micros()),
                jl.tools.len(),
                jl.hosts().len()
            ));
        }
    }

    // ---- Lines of no turn in the window: the bridge's own commits and writes, and turns
    // that started before `since`.
    let loose: Vec<(&&str, &JobLines)> = by_job
        .iter()
        .filter(|(job, jl)| !turn_ids.contains(**job) && !jl.is_empty())
        .collect();
    if !loose.is_empty() {
        out.push_str("\n## Outside any turn in this window\n\n");
        for (job, jl) in loose {
            let mut parts = Vec::new();
            for c in &jl.commits {
                parts.push(format!(
                    "commit {} ({} file(s))",
                    short_hash(s(c, "hash")),
                    n(c, "files")
                ));
            }
            if !jl.writes.is_empty() {
                parts.push(format!("wrote {}", paths(&jl.writes)));
            }
            if !jl.tools.is_empty() {
                parts.push(format!("{} tool call(s)", jl.tools.len()));
            }
            if !jl.models.is_empty() {
                parts.push(format!(
                    "{} model call(s) {}",
                    jl.models.len(),
                    dollars(jl.cost_micros())
                ));
            }
            if !jl.egress.is_empty() {
                parts.push(format!("{} host(s)", jl.hosts().len()));
            }
            out.push_str(&format!("- `{job}`: {}\n", parts.join(" · ")));
        }
    }
    out
}

fn turn_label(t: &Value) -> String {
    let origin = s(t, "origin");
    match origin.strip_prefix("schedule:") {
        Some(id) => format!("Scheduled `{id}`"),
        None => format!("`{origin}` turn"),
    }
}

fn paths(writes: &[&Value]) -> String {
    let mut seen: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for w in writes {
        seen.entry(s(w, "path")).or_default().insert(s(w, "op"));
    }
    seen.into_iter()
        .map(|(p, ops)| format!("`{p}` ({})", ops.into_iter().collect::<Vec<_>>().join("/")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn scheduled_run(t: &Value, jl: &JobLines, conf: Option<&DigestJob>, with_date: bool) -> String {
    let tier = t
        .get("tier")
        .and_then(Value::as_str)
        .or_else(|| jl.egress.first().map(|e| s(e, "tier")))
        .unwrap_or("not recorded");
    let mut model = format!("{} / {}", s(t, "harness"), s(t, "model"));
    if let Some(c) = conf {
        if let Some(m) = c.model.as_deref().filter(|m| *m != s(t, "model")) {
            model.push_str(&format!(
                " (configured {}{m})",
                c.harness
                    .as_deref()
                    .map(|h| format!("{h} / "))
                    .unwrap_or_default()
            ));
        }
    }
    let mut out = format!(
        "- {} · **{}** · {} · tier {} · {}\n",
        clock(n(t, "start_ms"), with_date),
        s(t, "outcome"),
        duration(n(t, "end_ms").saturating_sub(n(t, "start_ms"))),
        tier,
        model
    );
    // Tools per server, refused counted apart.
    if !jl.tools.is_empty() {
        let mut per: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
        for c in &jl.tools {
            let e = per.entry(s(c, "server")).or_default();
            e.0 += 1;
            if s(c, "outcome") == "refused" {
                e.1 += 1;
            }
        }
        let list = per
            .into_iter()
            .map(|(server, (all, refused))| {
                if refused > 0 {
                    format!("{server} {all} ({refused} refused)")
                } else {
                    format!("{server} {all}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("  - tools: {list}\n"));
    }
    // Hosts, refused ones marked with their reason.
    if !jl.egress.is_empty() {
        let mut hosts: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for e in &jl.egress {
            let reasons = hosts.entry(s(e, "host")).or_default();
            if s(e, "verdict") == "refused" {
                reasons.insert(s(e, "reason"));
            }
        }
        let list = hosts
            .into_iter()
            .map(|(h, reasons)| {
                if reasons.is_empty() {
                    h.to_string()
                } else {
                    format!(
                        "{h} (REFUSED: {})",
                        reasons.into_iter().collect::<Vec<_>>().join(", ")
                    )
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("  - egress: {list}\n"));
    }
    if !jl.models.is_empty() {
        let sum = |k: &str| jl.models.iter().map(|m| n(m, k)).sum::<u64>();
        out.push_str(&format!(
            "  - model: {} · {} call(s) · {} in / {} out / {} cache read / {} cache write\n",
            dollars(jl.cost_micros()),
            jl.models.len(),
            count(sum("input_tokens")),
            count(sum("output_tokens")),
            count(sum("cache_read_tokens")),
            count(sum("cache_write_tokens"))
        ));
    }
    if !jl.writes.is_empty() {
        out.push_str(&format!("  - wrote: {}\n", paths(&jl.writes)));
    }
    if !jl.commits.is_empty() {
        let list = jl
            .commits
            .iter()
            .map(|c| format!("{} ({} file(s))", short_hash(s(c, "hash")), n(c, "files")))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("  - commits: {list}\n"));
    }
    out
}

// ---- THE ROUTES -----------------------------------------------------------------------

fn ledger_dir(cfg: &Config) -> Option<PathBuf> {
    cfg.state_dir
        .as_deref()
        .map(|d| Path::new(d).join("activity"))
}

fn bad_request(msg: &str) -> ApiError {
    (StatusCode::BAD_REQUEST, msg.to_string())
}

fn ledger_off() -> ApiError {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the activity ledger is off: this bridge has no state directory".to_string(),
    )
}

/// `GET /jesse/activity?since=<rfc3339>&until=<rfc3339>`: the ledger lines whose instant is in
/// `[since, until)`, oldest first. `since` is required (400 when absent or not RFC 3339);
/// `until` defaults to now. At most [`ACTIVITY_QUERY_CAP`] (10 000) lines: a range holding more
/// is cut there and answered `"truncated": true`. Same bearer auth and rate limiter as every
/// `/jesse` route; 503 when the bridge has no state directory.
///
/// `{ "since", "until", "cap": 10000, "truncated", "lines": [...] }`.
pub async fn jesse_activity(
    State(st): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &st.cfg.token)?;
    if !st.limiter.allow() {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded".to_string(),
        ));
    }
    let since = match q.get("since") {
        None => return Err(bad_request("`since` (RFC 3339) is required")),
        Some(s) => parse_rfc3339_ms(s).ok_or_else(|| bad_request("`since` is not RFC 3339"))?,
    };
    let until = match q.get("until") {
        None => crate::activity::now_ms(),
        Some(s) => parse_rfc3339_ms(s).ok_or_else(|| bad_request("`until` is not RFC 3339"))?,
    };
    let dir = ledger_dir(&st.cfg).ok_or_else(ledger_off)?;
    let (lines, truncated) =
        tokio::task::spawn_blocking(move || read_range(&dir, since, until, ACTIVITY_QUERY_CAP))
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({
        "since": ms_to_rfc3339(since),
        "until": ms_to_rfc3339(until),
        "cap": ACTIVITY_QUERY_CAP,
        "truncated": truncated,
        "lines": lines,
    })))
}

/// `GET /jesse/activity/digest?since=<rfc3339>`: [`render_digest`] over `[since, now)` as
/// `text/markdown`. `since` defaults to 24 hours ago; a bad one is 400. Reads at most
/// [`ACTIVITY_QUERY_CAP`] lines and says so in the Markdown when the window held more. Same
/// bearer auth and rate limiter as every `/jesse` route.
pub async fn jesse_activity_digest(
    State(st): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    check_auth(&headers, &st.cfg.token)?;
    if !st.limiter.allow() {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded".to_string(),
        ));
    }
    let until = crate::activity::now_ms();
    let since = match q.get("since") {
        None => until.saturating_sub(DIGEST_DEFAULT_WINDOW_MS),
        Some(s) => parse_rfc3339_ms(s).ok_or_else(|| bad_request("`since` is not RFC 3339"))?,
    };
    let dir = ledger_dir(&st.cfg).ok_or_else(ledger_off)?;
    let jobs = digest_jobs(&st.scheduler.schedule(), &st.cfg.model_registry);
    let md = tokio::task::spawn_blocking(move || {
        let (lines, truncated) = read_range(&dir, since, until, ACTIVITY_QUERY_CAP);
        render_digest_with(&lines, &jobs, since, until, truncated)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/markdown; charset=utf-8",
        )],
        md,
    )
        .into_response())
}

// ---- THE CLI ----------------------------------------------------------------------------

/// `jesse-bridge activity-digest`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestArgs {
    pub since_ms: u64,
    pub until_ms: Option<u64>,
    pub state_dir: Option<PathBuf>,
}

/// The CLI's usage line.
pub const DIGEST_USAGE: &str = "usage: jesse-bridge activity-digest --since <rfc3339> \
                                [--until <rfc3339>] [--state-dir <dir>]";

/// Parse the arguments AFTER `activity-digest`. `--flag value` and `--flag=value` both work.
pub fn parse_digest_args(args: &[String]) -> Result<DigestArgs, String> {
    let mut since = None;
    let mut until = None;
    let mut state_dir = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--since" => {
                let v = value()?;
                since = Some(parse_rfc3339_ms(&v).ok_or(format!("--since {v:?} is not RFC 3339"))?);
            }
            "--until" => {
                let v = value()?;
                until = Some(parse_rfc3339_ms(&v).ok_or(format!("--until {v:?} is not RFC 3339"))?);
            }
            "--state-dir" => state_dir = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(DigestArgs {
        since_ms: since.ok_or("--since is required")?,
        until_ms: until,
        state_dir,
    })
}

/// The bridge's state directory exactly as [`Config::from_env`] resolves it: `JESSE_STATE_DIR`
/// when set and non-empty, else `$HOME/.jesse-bridge`, else none.
pub fn state_dir_from_env() -> Option<PathBuf> {
    if let Some(d) = env_string("JESSE_STATE_DIR") {
        return Some(PathBuf::from(d));
    }
    let home = std::env::var("HOME").unwrap_or_default();
    (!home.is_empty()).then(|| PathBuf::from(format!("{home}/.jesse-bridge")))
}

/// `jesse-bridge activity-digest …`: the digest Markdown for the given arguments, read off the
/// ledger files. Starts nothing, binds nothing, reads no config file: the scheduled-job list is
/// what the ledger's turns name (a job with no run in the window is not listed).
pub fn run_digest_cli(args: &[String]) -> Result<String, String> {
    let a = parse_digest_args(args)?;
    let state = a
        .state_dir
        .or_else(state_dir_from_env)
        .ok_or("no state directory: pass --state-dir or set JESSE_STATE_DIR")?;
    let until = a.until_ms.unwrap_or_else(crate::activity::now_ms);
    let (lines, truncated) = read_range(
        &state.join("activity"),
        a.since_ms,
        until,
        ACTIVITY_QUERY_CAP,
    );
    Ok(render_digest_with(
        &lines,
        &[],
        a.since_ms,
        until,
        truncated,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "jesse-actq-{tag}-{}-{}",
            std::process::id(),
            crate::random_hex()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn day_of(ms: u64) -> String {
        local_day(ms).unwrap().format("%Y-%m-%d").to_string()
    }

    fn write_lines(dir: &Path, day: &str, lines: &[Value]) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{day}.jsonl")))
            .unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn tool(job: &str, at: u64) -> Value {
        json!({"kind":"tool_call","v":1,"job_id":job,"server":"s","tool":"t","at_ms":at,
               "duration_ms":1,"outcome":"ok","request_bytes":0,"response_bytes":0})
    }

    /// Noon local, `d` days from 2026-11-10 — a fixed, DST-free anchor in any zone.
    fn noon(d: i64) -> u64 {
        let day = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap() + chrono::Duration::days(d);
        Local
            .from_local_datetime(&day.and_hms_opt(12, 0, 0).unwrap())
            .single()
            .unwrap()
            .timestamp_millis() as u64
    }

    #[test]
    fn a_range_is_filtered_by_each_lines_own_instant_across_days() {
        let dir = tmp("range");
        let (d0, d1, d2) = (noon(0), noon(1), noon(2));
        write_lines(&dir, &day_of(d0), &[tool("a", d0), tool("b", d0 + 10)]);
        write_lines(
            &dir,
            &day_of(d1),
            &[
                tool("c", d1),
                json!({"kind":"turn","v":1,"job_id":"t","harness":"h","model":"m",
                       "capability":"read","origin":"phone","start_ms":d1 - 5,"end_ms":d1 + 5,
                       "outcome":"ok"}),
            ],
        );
        // Malformed lines are skipped, not fatal.
        std::fs::write(
            dir.join(format!("{}.jsonl", day_of(d2))),
            format!("not json\n{{\"kind\":\"turn\"}}\n{}\n", tool("d", d2)),
        )
        .unwrap();

        let (all, truncated) = read_range(&dir, d0, d2 + 1, 100);
        assert!(!truncated);
        let ids: Vec<&str> = all.iter().map(|v| s(v, "job_id")).collect();
        assert_eq!(
            ids,
            ["a", "b", "t", "c", "d"],
            "oldest first, turn by start_ms"
        );

        let (mid, _) = read_range(&dir, d0 + 5, d1, 100);
        let ids: Vec<&str> = mid.iter().map(|v| s(v, "job_id")).collect();
        assert_eq!(ids, ["b", "t"], "since inclusive, until exclusive");

        assert!(read_range(&dir, d2, d0, 100).0.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_range_is_cut_at_the_cap_and_says_so() {
        let dir = tmp("cap");
        let d0 = noon(0);
        let lines: Vec<Value> = (0..5).map(|i| tool(&format!("j{i}"), d0 + i)).collect();
        write_lines(&dir, &day_of(d0), &lines);
        let (got, truncated) = read_range(&dir, d0, d0 + 100, 3);
        assert!(truncated);
        let ids: Vec<&str> = got.iter().map(|v| s(v, "job_id")).collect();
        assert_eq!(ids, ["j0", "j1", "j2"]);
        let (got, truncated) = read_range(&dir, d0, d0 + 100, 5);
        assert!(!truncated);
        assert_eq!(got.len(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rfc3339_is_read_with_an_offset_mangled_by_a_query_string() {
        let z = parse_rfc3339_ms("2026-11-10T22:00:00Z").unwrap();
        assert_eq!(parse_rfc3339_ms("2026-11-10T23:00:00+01:00"), Some(z));
        assert_eq!(parse_rfc3339_ms("2026-11-10T23:00:00 01:00"), Some(z));
        assert_eq!(parse_rfc3339_ms("yesterday"), None);
        assert_eq!(ms_to_rfc3339(z), "2026-11-10T22:00:00.000Z");
    }

    /// A night: two scheduled jobs (one clean, one with a refused host and a refused tool) and
    /// two phone turns, one failed.
    fn fixture(base: u64) -> Vec<Value> {
        vec![
            json!({"kind":"turn","v":1,"job_id":"jA","harness":"claude-code","model":"opus",
                   "capability":"write","origin":"schedule:vault-lint","start_ms":base,
                   "end_ms":base + 252_000,"outcome":"ok","tier":"allowlist"}),
            json!({"kind":"tool_call","v":1,"job_id":"jA","server":"obsidian","tool":"read",
                   "at_ms":base + 1,"duration_ms":3,"outcome":"ok","request_bytes":1,
                   "response_bytes":2}),
            json!({"kind":"model_call","v":1,"job_id":"jA","route":"anthropic-api",
                   "model":"opus","at_ms":base + 2,"duration_ms":900,"status":200,
                   "input_tokens":12_000,"output_tokens":800,"cache_write_tokens":0,
                   "cache_read_tokens":5_000,"cost_micros":420_000}),
            json!({"kind":"egress","v":1,"job_id":"jA","tier":"allowlist",
                   "host":"api.github.com","port":443,"method_class":"read","at_ms":base + 3,
                   "bytes_out":10,"bytes_in":20,"verdict":"allowed","reason":"none"}),
            json!({"kind":"vault_write","v":1,"job_id":"jA",
                   "path":"Inbox/2026-11-11-vault-lint.md","op":"write","at_ms":base + 4}),
            json!({"kind":"commit","v":1,"job_id":"jA",
                   "hash":"abcdef0123456789abcdef0123456789abcdef01","files":3,
                   "at_ms":base + 5}),
            json!({"kind":"turn","v":1,"job_id":"jB","harness":"codex","model":"gpt-5",
                   "capability":"read","origin":"schedule:morning-brief",
                   "start_ms":base + 10,"end_ms":base + 70_010,"outcome":"ok"}),
            json!({"kind":"egress","v":1,"job_id":"jB","tier":"open-logged",
                   "host":"paste.example.com","port":443,"method_class":"send",
                   "at_ms":base + 11,"bytes_out":900,"bytes_in":0,"verdict":"refused",
                   "reason":"content_match"}),
            json!({"kind":"tool_call","v":1,"job_id":"jB","server":"google-perseido",
                   "tool":"send_gmail_message","at_ms":base + 12,"duration_ms":0,
                   "outcome":"refused","request_bytes":0,"response_bytes":0}),
            json!({"kind":"turn","v":1,"job_id":"jP1","harness":"claude-code","model":"opus",
                   "capability":"write","origin":"phone","start_ms":base + 20,
                   "end_ms":base + 32_020,"outcome":"ok"}),
            json!({"kind":"model_call","v":1,"job_id":"jP1","route":"anthropic-subscription",
                   "model":"opus","at_ms":base + 21,"duration_ms":900,"status":200,
                   "input_tokens":100,"output_tokens":10,"cache_write_tokens":0,
                   "cache_read_tokens":0,"cost_micros":50_000}),
            json!({"kind":"turn","v":1,"job_id":"jP2","harness":"claude-code","model":"opus",
                   "capability":"write","origin":"phone","start_ms":base + 30,
                   "end_ms":base + 5_030,"outcome":"failed"}),
            json!({"kind":"commit","v":1,"job_id":"diet-pipeline",
                   "hash":"1234567890abcdef1234567890abcdef12345678","files":2,
                   "at_ms":base + 40}),
        ]
    }

    #[test]
    fn the_digest_leads_with_what_failed_and_details_each_job() {
        let base = noon(0);
        let jobs = vec![
            DigestJob {
                id: "vault-lint".into(),
                model: Some("opus".into()),
                harness: Some("claude-code".into()),
            },
            DigestJob {
                id: "morning-brief".into(),
                model: None,
                harness: None,
            },
            DigestJob {
                id: "weekly-review".into(),
                model: None,
                harness: None,
            },
        ];
        let md = render_digest(&fixture(base), &jobs, base, base + 3_600_000);
        let at = |needle: &str| {
            md.find(needle)
                .unwrap_or_else(|| panic!("{needle:?} missing from:\n{md}"))
        };
        // Needs attention, before anything else.
        let attention = at("## Needs attention");
        assert!(at("`phone` turn **failed**") > attention);
        assert!(at("`phone` turn **failed**") < at("egress to `paste.example.com`"));
        assert!(at("egress to `paste.example.com` refused (content_match)") > attention);
        assert!(at("tool calls refused on `google-perseido` ×1") > attention);
        assert!(at("## Scheduled jobs") > at("tool calls refused on `google-perseido`"));
        // The troubled job before the clean one.
        assert!(at("### morning-brief") < at("### vault-lint"));
        // Per job: tier, model, outcome, duration, tools, hosts, cost, paths, commits.
        assert!(md.contains("**ok** · 4m12s · tier allowlist · claude-code / opus"));
        assert!(
            md.contains("tier open-logged · codex / gpt-5"),
            "tier from egress: {md}"
        );
        assert!(md.contains("tools: obsidian 1"));
        assert!(md.contains("tools: google-perseido 1 (1 refused)"));
        assert!(md.contains("egress: api.github.com\n"));
        assert!(md.contains("egress: paste.example.com (REFUSED: content_match)"));
        assert!(md.contains("model: $0.42 · 1 call(s) · 12.0k in / 800 out / 5.0k cache read"));
        assert!(md.contains("wrote: `Inbox/2026-11-11-vault-lint.md` (write)"));
        assert!(md.contains("commits: abcdef0 (3 file(s))"));
        assert!(md.contains("Configured, no run in this window: `weekly-review`"));
        // Phone turns, one line each.
        assert!(
            md.contains("phone · claude-code / opus · ok · 32s · $0.05 · 0 tool(s) · 0 host(s)")
        );
        assert!(md.contains("phone · claude-code / opus · failed · 5s"));
        // The bridge's own commit, outside any turn.
        assert!(md.contains("`diet-pipeline`: commit 1234567 (2 file(s))"));
        assert!(md.contains("2 scheduled run(s), 2 other turn(s), $0.47 model cost."));
        assert!(!md.contains("Truncated"));
    }

    #[test]
    fn a_quiet_window_says_nothing_failed() {
        let md = render_digest(&[], &[], noon(0), noon(0) + 1000);
        assert!(md.contains("Nothing failed or was refused."));
        assert!(md.contains("0 scheduled run(s), 0 other turn(s), $0.00 model cost."));
    }

    #[test]
    fn digest_args_parse() {
        let a =
            |v: &[&str]| parse_digest_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let got = a(&["--since", "2026-11-10T22:00:00Z", "--state-dir", "/tmp/x"]).unwrap();
        assert_eq!(
            got.since_ms,
            parse_rfc3339_ms("2026-11-10T22:00:00Z").unwrap()
        );
        assert_eq!(got.until_ms, None);
        assert_eq!(got.state_dir, Some(PathBuf::from("/tmp/x")));
        let got = a(&[
            "--since=2026-11-10T22:00:00Z",
            "--until=2026-11-11T06:00:00+01:00",
        ])
        .unwrap();
        assert_eq!(got.until_ms, parse_rfc3339_ms("2026-11-11T05:00:00Z"));
        assert!(a(&[]).unwrap_err().contains("--since is required"));
        assert!(a(&["--since"]).unwrap_err().contains("needs a value"));
        assert!(a(&["--since", "soon"])
            .unwrap_err()
            .contains("not RFC 3339"));
        assert!(a(&["--since", "2026-11-10T22:00:00Z", "--bogus"])
            .unwrap_err()
            .contains("unknown argument"));
    }

    #[test]
    fn the_cli_reads_the_state_dirs_ledger() {
        let state = tmp("cli");
        let dir = state.join("activity");
        std::fs::create_dir_all(&dir).unwrap();
        let base = noon(0);
        write_lines(&dir, &day_of(base), &fixture(base));
        let args: Vec<String> = [
            "--since",
            &ms_to_rfc3339(base),
            "--until",
            &ms_to_rfc3339(base + 3_600_000),
            "--state-dir",
            state.to_str().unwrap(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let md = run_digest_cli(&args).unwrap();
        assert!(md.contains("### vault-lint"));
        assert!(md.contains("paste.example.com (REFUSED: content_match)"));
        let _ = std::fs::remove_dir_all(&state);
    }

    async fn get(app: axum::Router, uri: &str, auth: bool) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut req = axum::http::Request::builder().uri(uri);
        if auth {
            req = req.header("authorization", format!("Bearer {}", test_token()));
        }
        let resp = app
            .oneshot(req.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, headers, bytes.to_vec())
    }

    fn test_token() -> String {
        crate::testutil::test_config().token
    }

    #[tokio::test]
    async fn the_routes_are_bearer_protected_and_answer_the_range() {
        let state = tmp("route");
        let dir = state.join("activity");
        std::fs::create_dir_all(&dir).unwrap();
        let now = crate::activity::now_ms();
        let base = now - 60_000;
        write_lines(&dir, &day_of(base), &fixture(base));
        let mut cfg = crate::testutil::test_config();
        cfg.state_dir = Some(state.to_string_lossy().into_owned());
        let app = crate::app(AppState::new(cfg));

        let since = ms_to_rfc3339(base - 1000);
        for uri in [
            format!("/jesse/activity?since={since}"),
            "/jesse/activity/digest".to_string(),
        ] {
            let (status, _, _) = get(app.clone(), &uri, false).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        }

        let (status, _, body) = get(app.clone(), "/jesse/activity", true).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(String::from_utf8_lossy(&body).contains("since"));
        let (status, _, _) = get(app.clone(), "/jesse/activity?since=yesterday", true).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _, body) =
            get(app.clone(), &format!("/jesse/activity?since={since}"), true).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["cap"], ACTIVITY_QUERY_CAP);
        assert_eq!(v["truncated"], false);
        assert_eq!(v["since"], since);
        assert_eq!(v["lines"].as_array().unwrap().len(), fixture(base).len());
        assert_eq!(v["lines"][0]["job_id"], "jA");

        let until = ms_to_rfc3339(base + 15);
        let (_, _, body) = get(
            app.clone(),
            &format!("/jesse/activity?since={since}&until={until}"),
            true,
        )
        .await;
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["lines"].as_array().unwrap().len(),
            9,
            "until is exclusive"
        );

        let (status, headers, body) = get(app.clone(), "/jesse/activity/digest", true).await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/markdown"));
        let md = String::from_utf8(body).unwrap();
        assert!(md.contains("### vault-lint"), "{md}");
        let (status, _, _) = get(app.clone(), "/jesse/activity/digest?since=nope", true).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(&state);
    }
}
