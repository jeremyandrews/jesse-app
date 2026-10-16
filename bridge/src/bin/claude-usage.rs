//! `jesse-claude-usage`: one Mac's current Claude Code login, who it is and how much of its
//! 5 hour and 7 day windows it has used, written into the vault as one Markdown file per host.
//!
//! Each Mac runs it hourly under launchd (`bridge/launchd/com.jesse.claude-usage.plist`,
//! installed by `scripts/install-claude-usage.sh`). The owner switches accounts with `/login`
//! separately on each Mac, so the Studio and the laptop can be on different accounts at the
//! same time; a file per host, keyed by account AND organization, is what lets one scheduled
//! agent turn see every account on every machine. Two accounts may share an email (a personal
//! plan and an organization seat), which is why the organization is part of the key.
//!
//!   jesse-claude-usage snap --vault <path> [--host <name>] [--dry-run]
//!
//! `--host` defaults to the short hostname, lowercased (`scutil --get LocalHostName` on
//! macOS, else `hostname -s`). `--dry-run` prints the file it would write and writes nothing.
//!
//! The login is read through the bridge's own reader ([`read_claude_login`]), read only: the
//! token is spent on one profile and one usage request and dropped. It is never written,
//! printed, logged, refreshed or copied. An expired login is recorded as `login expired` and
//! makes no request. Failures are recorded as a class, never a body. No model is called.
//!
//! Exit status: 0 when the file was written (a row that records a failure is still a
//! result), 1 when it could not be written, 2 on a usage error.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::{DateTime, Local, SecondsFormat, TimeZone};
use serde_json::Value;

use jesse_bridge::{
    claude_cli_version_of, claude_oauth_get, parse_claude_usage, read_claude_login, write_atomic,
    ClaudeCredential, QuotaError, QuotaSnapshot, CLAUDE_USAGE_URL,
};

const USAGE: &str = "usage: jesse-claude-usage snap --vault <path> [--host <name>] [--dry-run]

Reads this Mac's Claude Code login, asks Anthropic for its identity and its 5 hour and
7 day usage, and rewrites <vault>/Knowledge/Reference/Claude-Usage/<host>.md.";

/// The identity endpoint. Same headers, budget and failure classes as the usage endpoint.
const CLAUDE_PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";

/// Where the per host files live, under the vault.
const USAGE_DIR: &str = "Knowledge/Reference/Claude-Usage";

/// Three days of hourly readings.
const HISTORY_CAP: usize = 72;

const NONE: &str = "none";

/// The table columns, in order, for both tables. `week by model` holds the model scoped
/// 7 day windows (`Opus 12, Sonnet 3`) the usage parser maps.
const COLUMNS: [&str; 11] = [
    "account",
    "org",
    "org_id",
    "plan",
    "5h %",
    "5h resets",
    "week %",
    "week resets",
    "as of",
    "error",
    "week by model",
];

const LATEST_HEADING: &str = "## Latest per account";
const HISTORY_HEADING: &str = "## History";

// ---- Identity ----------------------------------------------------------------------

/// Who a reading belongs to, as it is shown in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    /// The account email, or `account <uuid prefix>` when only the uuid is known.
    account: String,
    /// The organization name, or `personal` when there is no organization.
    org: String,
    /// The first 8 characters of the organization uuid, or `none`.
    org_id: String,
    plan: Option<String>,
}

fn prefix8(s: &str) -> String {
    s.chars().take(8).collect()
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// `claude_max` with tier `default_claude_max_20x` is `max 20x`; `claude_pro` is `pro`.
fn plan_from(org_type: Option<&str>, tier: Option<&str>) -> Option<String> {
    let base = org_type?;
    let base = base
        .strip_prefix("claude_")
        .unwrap_or(base)
        .replace('_', " ");
    let multiplier = tier
        .and_then(|t| t.rsplit('_').next())
        .filter(|m| m.ends_with('x') && m.starts_with(|c: char| c.is_ascii_digit()));
    Some(match multiplier {
        Some(m) => format!("{base} {m}"),
        None => base,
    })
}

fn identity_from(
    email: Option<&str>,
    account_uuid: Option<&str>,
    org_uuid: Option<&str>,
    org_name: Option<&str>,
    plan: Option<String>,
) -> Option<Identity> {
    let account = match (email, account_uuid) {
        (Some(e), _) => e.to_string(),
        (None, Some(u)) => format!("account {}", prefix8(u)),
        (None, None) => return None,
    };
    let (org, org_id) = match org_uuid {
        Some(u) => (
            org_name.unwrap_or("unnamed organization").to_string(),
            prefix8(u),
        ),
        None => ("personal".to_string(), NONE.to_string()),
    };
    Some(Identity {
        account,
        org,
        org_id,
        plan,
    })
}

/// `GET /api/oauth/profile`.
///
/// Observed live on 2026-10-16 (HTTP 200), key names and nesting only:
///
/// ```text
/// account:       uuid, full_name, display_name, email, has_claude_max, has_claude_pro,
///                created_at
/// organization:  uuid, name, organization_type ("claude_max"), billing_type,
///                rate_limit_tier ("default_claude_max_20x"), seat_tier, has_extra_usage_enabled,
///                subscription_status, subscription_created_at, cc_onboarding_flags {},
///                claude_code_trial_ends_at, claude_code_trial_duration_days,
///                payment_auth_hosted_invoice_url, claude_ai_completion_feedback_enabled,
///                claude_ai_safety_feedback_enabled
/// application:   uuid, name, slug
/// enabled_plugins: []
/// ```
///
/// There is no plan field; the plan is `organization_type` without its `claude_` prefix, plus
/// the multiplier the rate limit tier ends in. A null or missing `organization` is `personal`.
fn parse_profile(v: &Value) -> Result<Identity, QuotaError> {
    let account = v.get("account").ok_or(QuotaError::Protocol)?;
    let org = v.get("organization").filter(|o| o.is_object());
    let plan =
        org.and_then(|o| plan_from(str_at(o, "organization_type"), str_at(o, "rate_limit_tier")));
    identity_from(
        str_at(account, "email"),
        str_at(account, "uuid"),
        org.and_then(|o| str_at(o, "uuid")),
        org.and_then(|o| str_at(o, "name")),
        plan,
    )
    .ok_or(QuotaError::Protocol)
}

/// The fallback: `~/.claude.json`'s `oauthAccount`, which Claude Code rewrites on `/login`.
///
/// Observed 2026-10-16, key names only: `accountCreatedAt`, `accountUuid`, `billingType`,
/// `ccOnboardingFlags`, `claudeCodeTrialDurationDays`, `claudeCodeTrialEndsAt`, `displayName`,
/// `emailAddress`, `fullName`, `hasExtraUsageEnabled`, `organizationName`,
/// `organizationRateLimitTier`, `organizationRole`, `organizationType`, `organizationUuid`,
/// `profileFetchedAt`, `seatTier`, `subscriptionCreatedAt`, `userRateLimitTier`,
/// `workspaceRole`.
fn parse_oauth_account(doc: &Value) -> Option<Identity> {
    let a = doc.get("oauthAccount").filter(|a| a.is_object())?;
    identity_from(
        str_at(a, "emailAddress"),
        str_at(a, "accountUuid"),
        str_at(a, "organizationUuid"),
        str_at(a, "organizationName"),
        plan_from(
            str_at(a, "organizationType"),
            str_at(a, "organizationRateLimitTier"),
        ),
    )
}

fn read_oauth_account(home: &Path) -> Option<Identity> {
    let bytes = std::fs::read(home.join(".claude.json")).ok()?;
    parse_oauth_account(&serde_json::from_slice(&bytes).ok()?)
}

// ---- The row -----------------------------------------------------------------------

/// One reading, every cell already in its written form. Empty cells read `none`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    account: String,
    org: String,
    org_id: String,
    plan: String,
    five_pct: String,
    five_resets: String,
    week_pct: String,
    week_resets: String,
    as_of: String,
    error: String,
    week_by_model: String,
}

impl Row {
    fn cells(&self) -> [&str; 11] {
        [
            &self.account,
            &self.org,
            &self.org_id,
            &self.plan,
            &self.five_pct,
            &self.five_resets,
            &self.week_pct,
            &self.week_resets,
            &self.as_of,
            &self.error,
            &self.week_by_model,
        ]
    }

    fn from_cells(c: Vec<String>) -> Option<Row> {
        let [account, org, org_id, plan, five_pct, five_resets, week_pct, week_resets, as_of, error, week_by_model]: [String; 11] =
            c.try_into().ok()?;
        Some(Row {
            account,
            org,
            org_id,
            plan,
            five_pct,
            five_resets,
            week_pct,
            week_resets,
            as_of,
            error,
            week_by_model,
        })
    }

    fn key(&self) -> (&str, &str) {
        (&self.account, &self.org_id)
    }
}

/// A table cell: no pipe, no line break, never empty.
fn cell(s: &str) -> String {
    let s = s.replace('|', "/").replace(['\n', '\r'], " ");
    let s = s.trim();
    if s.is_empty() {
        NONE.to_string()
    } else {
        s.to_string()
    }
}

/// A prose cell: a [`cell`] with every dash character turned into a space, because the file
/// carries no dashes outside its dates and its Markdown syntax.
fn prose(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| {
            if matches!(c, '-' | '\u{2010}'..='\u{2015}' | '\u{2212}') {
                ' '
            } else {
                c
            }
        })
        .collect();
    cell(&s.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn local_minute<Tz: TimeZone>(t: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    t.format("%Y-%m-%d %H:%M").to_string()
}

fn local_from_ms(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| local_minute(&t))
        .unwrap_or_else(|| NONE.to_string())
}

fn pct(p: f64) -> String {
    format!("{}", p.round() as i64)
}

/// An error class in words: `login-expired` is `login expired`, `http-503` is `http 503`.
fn class_words(e: &QuotaError) -> String {
    e.class().replace('-', " ")
}

/// Fold the identity, the usage reading and their failures into one row.
fn make_row(
    identity: Option<&Identity>,
    usage: Option<&QuotaSnapshot>,
    errors: &[String],
    as_of: String,
) -> Row {
    let window = |id: &str| usage.and_then(|s| s.windows.iter().find(|w| w.id == id));
    let (five_pct, five_resets) = match window("five_hour") {
        Some(w) => (pct(w.used_percent), w.resets_at_ms.map(local_from_ms)),
        None => (NONE.to_string(), None),
    };
    let (week_pct, week_resets) = match window("seven_day") {
        Some(w) => (pct(w.used_percent), w.resets_at_ms.map(local_from_ms)),
        None => (NONE.to_string(), None),
    };
    let by_model: Vec<String> = usage
        .map(|s| s.windows.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|w| w.id.starts_with("seven_day_") && w.id != "seven_day_oauth_apps")
        .map(|w| format!("{} {}", w.label, pct(w.used_percent)))
        .collect();
    let plan = identity
        .and_then(|i| i.plan.clone())
        .or_else(|| usage.and_then(|s| s.plan.clone()))
        .unwrap_or_default();
    Row {
        account: cell(identity.map_or("unknown", |i| i.account.as_str())),
        org: prose(identity.map_or("unknown", |i| i.org.as_str())),
        org_id: cell(identity.map_or(NONE, |i| i.org_id.as_str())),
        plan: prose(&plan),
        five_pct,
        five_resets: five_resets.unwrap_or_else(|| NONE.to_string()),
        week_pct,
        week_resets: week_resets.unwrap_or_else(|| NONE.to_string()),
        as_of,
        error: prose(&errors.join(", ")),
        week_by_model: prose(&by_model.join(", ")),
    }
}

// ---- Taking the reading ------------------------------------------------------------

/// The network seam: one authenticated `GET`, so a test can count the calls.
trait Remote {
    async fn get(&self, url: &str, cred: &ClaudeCredential) -> Result<Value, QuotaError>;
}

struct LiveRemote {
    client: reqwest::Client,
    version: String,
}

impl Remote for LiveRemote {
    async fn get(&self, url: &str, cred: &ClaudeCredential) -> Result<Value, QuotaError> {
        claude_oauth_get(&self.client, url, cred, &self.version).await
    }
}

/// Take one reading. An unreadable, expired or narrowed login makes no request; otherwise
/// profile and usage are asked once each, concurrently, and never retried. Identity falls
/// back to `fallback` (the `~/.claude.json` reader) when the profile is not available.
async fn take_reading<R: Remote>(
    cred: Result<ClaudeCredential, QuotaError>,
    remote: &R,
    fallback: impl FnOnce() -> Option<Identity>,
    now: DateTime<Local>,
) -> Row {
    let as_of = local_minute(&now);
    let now_ms = now.timestamp_millis();
    let cred = match cred.and_then(|c| c.check(now_ms).map(|()| c)) {
        Ok(c) => c,
        Err(e) => {
            let identity = fallback();
            return make_row(identity.as_ref(), None, &[class_words(&e)], as_of);
        }
    };
    let (profile, usage) = tokio::join!(
        remote.get(CLAUDE_PROFILE_URL, &cred),
        remote.get(CLAUDE_USAGE_URL, &cred),
    );
    let mut errors = Vec::new();
    let usage = match usage {
        Ok(v) => Some(parse_claude_usage(
            &v,
            cred.plan().map(str::to_string),
            now_ms,
        )),
        Err(e) => {
            errors.push(class_words(&e));
            None
        }
    };
    let identity = match profile.and_then(|v| parse_profile(&v)) {
        Ok(i) => Some(i),
        Err(e) => {
            let fb = fallback();
            if fb.is_none() {
                errors.push(format!("identity {}", class_words(&e)));
            }
            fb
        }
    };
    make_row(identity.as_ref(), usage.as_ref(), &errors, as_of)
}

// ---- The file ----------------------------------------------------------------------

#[derive(Debug, Default, PartialEq, Eq)]
struct Tables {
    latest: Vec<Row>,
    history: Vec<Row>,
}

fn table_line(cells: &[&str]) -> String {
    format!("| {} |", cells.join(" | "))
}

fn render_table(out: &mut String, rows: &[Row]) {
    out.push_str(&table_line(&COLUMNS));
    out.push('\n');
    out.push_str(&table_line(&["---"; 11]));
    out.push('\n');
    for r in rows {
        out.push_str(&table_line(&r.cells()));
        out.push('\n');
    }
}

fn yaml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn render(host: &str, at_iso: &str, current: &Row, tables: &Tables) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("host: {}\n", yaml_quote(host)));
    out.push_str(&format!("updated: {at_iso}\n"));
    out.push_str(&format!(
        "current_account: {}\n",
        yaml_quote(&format!("{} / {}", current.account, current.org))
    ));
    out.push_str("---\n\n");
    out.push_str(&format!("# Claude usage: {host}\n\n"));
    out.push_str(
        "Written hourly by jesse-claude-usage. Do not edit; the next run overwrites it.\n\n",
    );
    out.push_str(LATEST_HEADING);
    out.push_str("\n\n");
    render_table(&mut out, &tables.latest);
    out.push('\n');
    out.push_str(HISTORY_HEADING);
    out.push_str("\n\n");
    render_table(&mut out, &tables.history);
    out
}

fn split_row(line: &str) -> Option<Vec<String>> {
    let inner = line.trim().strip_prefix('|')?.strip_suffix('|')?;
    Some(inner.split('|').map(|c| c.trim().to_string()).collect())
}

/// The table under `heading`: the exact header, the separator, then rows to the first line
/// that is not a table row.
fn parse_table(text: &str, heading: &str) -> Result<Vec<Row>, String> {
    let mut lines = text
        .lines()
        .skip_while(|l| l.trim() != heading)
        .skip(1)
        .skip_while(|l| l.trim().is_empty());
    let header = lines.next().ok_or(format!("no table under {heading}"))?;
    if split_row(header).as_deref() != Some(&COLUMNS.map(str::to_string)[..]) {
        return Err(format!("unexpected columns under {heading}"));
    }
    let sep = lines.next().and_then(split_row).unwrap_or_default();
    if sep.len() != COLUMNS.len()
        || !sep
            .iter()
            .all(|c| c.chars().all(|ch| ch == '-' || ch == ':') && !c.is_empty())
    {
        return Err(format!("no separator under {heading}"));
    }
    let mut rows = Vec::new();
    for line in lines {
        if !line.trim_start().starts_with('|') {
            break;
        }
        let cells = split_row(line).ok_or(format!("malformed row under {heading}"))?;
        rows.push(Row::from_cells(cells).ok_or(format!("wrong cell count under {heading}"))?);
    }
    Ok(rows)
}

fn parse_file(text: &str) -> Result<Tables, String> {
    Ok(Tables {
        latest: parse_table(text, LATEST_HEADING)?,
        history: parse_table(text, HISTORY_HEADING)?,
    })
}

/// This run's row folded into the previous tables. Latest keeps one row per (`account`,
/// `org_id`), the newer by `as of` (this run's row wins a tie), newest first. History gains
/// this row at the top and keeps at most [`HISTORY_CAP`].
fn merge(prev: Tables, row: Row) -> Tables {
    let mut latest: Vec<Row> = vec![row.clone()];
    for r in prev.latest {
        match latest.iter_mut().find(|l| l.key() == r.key()) {
            Some(l) if r.as_of > l.as_of => *l = r,
            Some(_) => {}
            None => latest.push(r),
        }
    }
    latest.sort_by(|a, b| b.as_of.cmp(&a.as_of));
    let mut history = vec![row];
    history.extend(prev.history);
    history.truncate(HISTORY_CAP);
    Tables { latest, history }
}

/// The previous file's tables. A missing file is a fresh start; one that does not parse is
/// moved aside to `<name>.bad-<epoch>` (unless `dry_run`) and is also a fresh start.
fn load_previous(path: &Path, dry_run: bool, epoch: i64) -> Tables {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Tables::default(),
        Err(e) => {
            eprintln!(
                "jesse-claude-usage: could not read {}: {e}; starting a fresh file",
                path.display()
            );
            return Tables::default();
        }
    };
    match parse_file(&text) {
        Ok(t) => t,
        Err(why) => {
            let aside = bad_path(path, epoch);
            if dry_run {
                eprintln!(
                    "jesse-claude-usage: {} does not parse ({why}); a real run would move it to {}",
                    path.display(),
                    aside.display()
                );
            } else {
                match std::fs::rename(path, &aside) {
                    Ok(()) => eprintln!(
                        "jesse-claude-usage: {} did not parse ({why}); moved it to {} and started a fresh file",
                        path.display(),
                        aside.display()
                    ),
                    Err(e) => eprintln!(
                        "jesse-claude-usage: {} did not parse ({why}) and could not be moved aside: {e}; overwriting it",
                        path.display()
                    ),
                }
            }
            Tables::default()
        }
    }
}

fn bad_path(path: &Path, epoch: i64) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{name}.bad-{epoch}"))
}

// ---- Command line ------------------------------------------------------------------

struct Args {
    vault: PathBuf,
    host: Option<String>,
    dry_run: bool,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut it = argv.iter();
    match it.next().map(String::as_str) {
        Some("snap") => {}
        Some(other) => return Err(format!("unknown subcommand {other}")),
        None => return Err("missing subcommand".into()),
    }
    let mut vault = None;
    let mut host = None;
    let mut dry_run = false;
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--vault" => vault = Some(PathBuf::from(it.next().ok_or("--vault needs a path")?)),
            "--host" => {
                let h = it.next().ok_or("--host needs a name")?.trim().to_string();
                if h.is_empty() || h.contains(['/', '\\']) || h.starts_with('.') {
                    return Err(format!("not a usable host name: {h:?}"));
                }
                host = Some(h);
            }
            "--dry-run" => dry_run = true,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(Args {
        vault: vault.ok_or("--vault is required")?,
        host,
        dry_run,
    })
}

fn command_line(prog: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(prog)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn default_host() -> Option<String> {
    let raw = if cfg!(target_os = "macos") {
        command_line("scutil", &["--get", "LocalHostName"])
    } else {
        None
    }
    .or_else(|| command_line("hostname", &["-s"]))?;
    Some(raw.to_lowercase())
}

/// The `claude` binary for the `User-Agent`: `JESSE_CLAUDE_BIN`, else `claude` on `PATH`,
/// else where Claude Code installs itself. launchd's `PATH` holds none of those.
fn claude_bin(home: &Path) -> String {
    if let Some(b) = std::env::var("JESSE_CLAUDE_BIN")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        return b;
    }
    let on_path = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("claude"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    on_path
        .into_iter()
        .chain([
            home.join(".local/bin/claude"),
            home.join(".claude/local/claude"),
            PathBuf::from("/opt/homebrew/bin/claude"),
            PathBuf::from("/usr/local/bin/claude"),
        ])
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "claude".to_string())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("jesse-claude-usage: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Some(host) = args.host.or_else(default_host) else {
        eprintln!("jesse-claude-usage: could not read the host name; pass --host\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        eprintln!("jesse-claude-usage: HOME is not set");
        return ExitCode::from(2);
    };

    let now = Local::now();
    let remote = LiveRemote {
        client: reqwest::Client::builder().build().unwrap_or_default(),
        version: claude_cli_version_of(&claude_bin(&home)).await,
    };
    let cred = read_claude_login(&home).await;
    let row = take_reading(cred, &remote, || read_oauth_account(&home), now).await;

    let path = args.vault.join(USAGE_DIR).join(format!("{host}.md"));
    let prev = load_previous(&path, args.dry_run, now.timestamp());
    let tables = merge(prev, row.clone());
    let at_iso = now.to_rfc3339_opts(SecondsFormat::Secs, false);
    let text = render(&host, &at_iso, &row, &tables);

    if args.dry_run {
        print!("{text}");
        return ExitCode::SUCCESS;
    }
    match write_atomic(&path, text.as_bytes()) {
        Ok(()) => {
            println!(
                "{at_iso} wrote {}: {} / {}, 5h {}, week {}, error {}",
                path.display(),
                row.account,
                row.org,
                row.five_pct,
                row.week_pct,
                row.error
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!(
                "jesse-claude-usage: could not write {}: {e}",
                path.display()
            );
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The live profile body's shape (2026-10-16) with every identifier replaced.
    const PROFILE_FIXTURE: &str = r#"{
      "account": {"uuid": "aaaaaaaa-0000-0000-0000-000000000000", "full_name": "Placeholder Person",
                  "display_name": "Placeholder", "email": "person@example.com",
                  "has_claude_max": true, "has_claude_pro": false,
                  "created_at": "2024-01-01T00:00:00.000000Z"},
      "organization": {"uuid": "bbbbbbbb-1111-1111-1111-111111111111", "name": "Example Org",
                       "organization_type": "claude_max", "billing_type": "stripe_subscription",
                       "rate_limit_tier": "default_claude_max_20x", "seat_tier": null,
                       "has_extra_usage_enabled": false, "subscription_status": "active",
                       "subscription_created_at": "2024-01-01T00:00:00.000000Z",
                       "cc_onboarding_flags": {}, "claude_code_trial_ends_at": null,
                       "claude_code_trial_duration_days": null,
                       "payment_auth_hosted_invoice_url": null,
                       "claude_ai_completion_feedback_enabled": false,
                       "claude_ai_safety_feedback_enabled": false},
      "application": {"uuid": "cccccccc-2222-2222-2222-222222222222", "name": "Claude Code",
                      "slug": "claude-code"},
      "enabled_plugins": []
    }"#;

    /// `~/.claude.json`'s `oauthAccount` shape (2026-10-16) with every identifier replaced.
    const CLAUDE_JSON_FIXTURE: &str = r#"{
      "numStartups": 1,
      "oauthAccount": {"accountUuid": "aaaaaaaa-0000-0000-0000-000000000000",
                       "emailAddress": "person@example.com",
                       "organizationUuid": "dddddddd-3333-3333-3333-333333333333",
                       "hasExtraUsageEnabled": false, "billingType": "stripe_subscription",
                       "accountCreatedAt": "2024-01-01T00:00:00.000000Z",
                       "subscriptionCreatedAt": "2024-01-01T00:00:00.000000Z",
                       "ccOnboardingFlags": {}, "claudeCodeTrialEndsAt": null,
                       "claudeCodeTrialDurationDays": null, "seatTier": null,
                       "displayName": "Placeholder", "fullName": "Placeholder Person",
                       "profileFetchedAt": 1700000000000, "organizationRole": "admin",
                       "workspaceRole": null, "organizationName": "Other Org",
                       "organizationType": "claude_pro",
                       "organizationRateLimitTier": "default_claude_pro",
                       "userRateLimitTier": null}
    }"#;

    fn row(account: &str, org_id: &str, as_of: &str, five: &str) -> Row {
        Row {
            account: account.into(),
            org: "Example Org".into(),
            org_id: org_id.into(),
            plan: "max 20x".into(),
            five_pct: five.into(),
            five_resets: "2026-10-16 18:00".into(),
            week_pct: "27".into(),
            week_resets: "2026-10-20 09:00".into(),
            as_of: as_of.into(),
            error: NONE.into(),
            week_by_model: "Opus 12, Sonnet 3".into(),
        }
    }

    #[test]
    fn the_profile_names_the_account_its_organization_and_plan() {
        let v: Value = serde_json::from_str(PROFILE_FIXTURE).unwrap();
        assert_eq!(
            parse_profile(&v).unwrap(),
            Identity {
                account: "person@example.com".into(),
                org: "Example Org".into(),
                org_id: "bbbbbbbb".into(),
                plan: Some("max 20x".into()),
            }
        );
        let personal = serde_json::json!({"account": {"email": "p@example.com"},
                                          "organization": null});
        let i = parse_profile(&personal).unwrap();
        assert_eq!((i.org.as_str(), i.org_id.as_str()), ("personal", NONE));
        assert_eq!(
            parse_profile(&serde_json::json!({"error": "x"})).err(),
            Some(QuotaError::Protocol)
        );
    }

    #[test]
    fn the_claude_json_fallback_reads_oauth_account() {
        let v: Value = serde_json::from_str(CLAUDE_JSON_FIXTURE).unwrap();
        assert_eq!(
            parse_oauth_account(&v).unwrap(),
            Identity {
                account: "person@example.com".into(),
                org: "Other Org".into(),
                org_id: "dddddddd".into(),
                plan: Some("pro".into()),
            }
        );
        assert!(parse_oauth_account(&serde_json::json!({"numStartups": 1})).is_none());
    }

    #[test]
    fn the_table_writer_and_parser_round_trip() {
        let tables = Tables {
            latest: vec![row("a@example.com", "bbbbbbbb", "2026-10-16 14:00", "8")],
            history: vec![
                row("a@example.com", "bbbbbbbb", "2026-10-16 14:00", "8"),
                row("a@example.com", "bbbbbbbb", "2026-10-16 13:00", "5"),
            ],
        };
        let text = render(
            "studio",
            "2026-10-16T14:00:00+02:00",
            &tables.latest[0],
            &tables,
        );
        assert!(text.starts_with("---\nhost: \"studio\"\nupdated: 2026-10-16T14:00:00+02:00\n"));
        assert!(text.contains("current_account: \"a@example.com / Example Org\"\n"));
        assert!(text.contains("# Claude usage: studio\n"));
        assert_eq!(parse_file(&text).unwrap(), tables);
        // No dash in the prose: only the frontmatter fences, the table separators, the dates and
        // the binary's own name.
        for line in text.lines() {
            let l = line.trim();
            if l == "---" || l.starts_with("| ---") || l.starts_with("updated:") {
                continue;
            }
            let without_dates: String = l
                .split(' ')
                .filter(|w| !w.starts_with("2026-") && *w != "jesse-claude-usage.")
                .collect::<Vec<_>>()
                .join(" ");
            assert!(!without_dates.contains('-'), "{line}");
        }
    }

    #[test]
    fn merge_keeps_one_row_per_account_and_org_and_takes_the_newer() {
        let prev = Tables {
            latest: vec![
                row("a@example.com", "bbbbbbbb", "2026-10-16 13:00", "5"),
                row("a@example.com", "dddddddd", "2026-10-15 09:00", "40"),
            ],
            history: vec![],
        };
        let t = merge(
            prev,
            row("a@example.com", "bbbbbbbb", "2026-10-16 14:00", "8"),
        );
        assert_eq!(t.latest.len(), 2, "same email, two organizations: two rows");
        assert_eq!(t.latest[0].as_of, "2026-10-16 14:00");
        assert_eq!(t.latest[0].five_pct, "8");
        assert_eq!(t.latest[1].org_id, "dddddddd");
        assert_eq!(t.history.len(), 1);

        // A previous row newer than this run's (a clock step back) is kept.
        let prev = Tables {
            latest: vec![row("a@example.com", "bbbbbbbb", "2026-10-16 15:00", "9")],
            history: vec![],
        };
        let t = merge(
            prev,
            row("a@example.com", "bbbbbbbb", "2026-10-16 14:00", "8"),
        );
        assert_eq!(t.latest.len(), 1);
        assert_eq!(t.latest[0].five_pct, "9");
    }

    #[test]
    fn history_is_newest_first_and_caps_at_72() {
        let mut t = Tables::default();
        for h in 0..80 {
            let at = format!("2026-10-{:02} {:02}:00", 10 + h / 24, h % 24);
            t = merge(t, row("a@example.com", "bbbbbbbb", &at, &h.to_string()));
        }
        assert_eq!(t.history.len(), HISTORY_CAP);
        assert_eq!(t.history[0].five_pct, "79");
        assert_eq!(t.history[71].five_pct, "8");
        assert_eq!(t.latest.len(), 1);
    }

    #[derive(Default)]
    struct CountingRemote {
        calls: AtomicUsize,
    }

    impl Remote for CountingRemote {
        async fn get(&self, url: &str, _cred: &ClaudeCredential) -> Result<Value, QuotaError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if url == CLAUDE_PROFILE_URL {
                Ok(serde_json::from_str(PROFILE_FIXTURE).unwrap())
            } else {
                Ok(serde_json::json!({
                    "five_hour": {"utilization": 8.0, "resets_at": "2026-10-16T16:00:00+00:00"},
                    "seven_day": {"utilization": 27.4, "resets_at": "2026-10-20T07:00:00+00:00"},
                    "seven_day_opus": {"utilization": 12.0, "resets_at": null},
                    "seven_day_oauth_apps": null
                }))
            }
        }
    }

    fn credential(expires_at_ms: i64) -> ClaudeCredential {
        ClaudeCredential::from_document(&serde_json::json!({"claudeAiOauth": {
            "accessToken": "placeholder", "expiresAt": expires_at_ms,
            "scopes": ["user:inference", "user:profile"], "subscriptionType": "max"}}))
        .unwrap()
    }

    #[tokio::test]
    async fn an_expired_login_makes_no_request() {
        let remote = CountingRemote::default();
        let now = Local::now();
        let r = take_reading(
            Ok(credential(now.timestamp_millis() - 1)),
            &remote,
            || None,
            now,
        )
        .await;
        assert_eq!(remote.calls.load(Ordering::SeqCst), 0, "no request made");
        assert_eq!(r.error, "login expired");
        assert_eq!((r.account.as_str(), r.org_id.as_str()), ("unknown", NONE));
        assert_eq!((r.five_pct.as_str(), r.week_pct.as_str()), (NONE, NONE));
    }

    #[tokio::test]
    async fn a_live_login_asks_profile_and_usage_once_each() {
        let remote = CountingRemote::default();
        let now = Local::now();
        let r = take_reading(
            Ok(credential(now.timestamp_millis() + 3_600_000)),
            &remote,
            || panic!("the profile answered; no fallback"),
            now,
        )
        .await;
        assert_eq!(remote.calls.load(Ordering::SeqCst), 2);
        assert_eq!(r.account, "person@example.com");
        assert_eq!(
            (r.org.as_str(), r.org_id.as_str()),
            ("Example Org", "bbbbbbbb")
        );
        assert_eq!((r.five_pct.as_str(), r.week_pct.as_str()), ("8", "27"));
        assert_eq!(r.week_by_model, "Opus 12");
        assert_eq!(r.error, NONE);
    }

    struct FailingRemote;

    impl Remote for FailingRemote {
        async fn get(&self, _url: &str, _cred: &ClaudeCredential) -> Result<Value, QuotaError> {
            Err(QuotaError::RateLimited {
                retry_after_secs: 0,
            })
        }
    }

    #[tokio::test]
    async fn a_429_is_rate_limited_and_identity_falls_back_to_claude_json() {
        let now = Local::now();
        let v: Value = serde_json::from_str(CLAUDE_JSON_FIXTURE).unwrap();
        let r = take_reading(
            Ok(credential(now.timestamp_millis() + 3_600_000)),
            &FailingRemote,
            || parse_oauth_account(&v),
            now,
        )
        .await;
        assert_eq!(r.error, "rate limited");
        assert_eq!(r.org, "Other Org");
        let r = take_reading(
            Ok(credential(now.timestamp_millis() + 3_600_000)),
            &FailingRemote,
            || None,
            now,
        )
        .await;
        assert_eq!(r.account, "unknown");
        assert_eq!(r.error, "rate limited, identity rate limited");
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "jesse-claude-usage-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_malformed_previous_file_is_moved_aside() {
        let dir = scratch("bad");
        let path = dir.join("studio.md");
        std::fs::write(&path, "# something else entirely\n").unwrap();
        assert_eq!(load_previous(&path, true, 42), Tables::default());
        assert!(path.exists(), "a dry run moves nothing");
        assert_eq!(load_previous(&path, false, 42), Tables::default());
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("studio.md.bad-42")).unwrap(),
            "# something else entirely\n"
        );
        assert_eq!(
            load_previous(&path, false, 43),
            Tables::default(),
            "missing is fresh"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn arguments_and_classes() {
        let a = |s: &[&str]| parse_args(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        let ok = a(&["snap", "--vault", "/v", "--host", "studio", "--dry-run"]).unwrap();
        assert_eq!(
            (ok.vault, ok.host, ok.dry_run),
            ("/v".into(), Some("studio".into()), true)
        );
        assert!(a(&["snap"]).is_err());
        assert!(a(&["snap", "--vault", "/v", "--host", "../x"]).is_err());
        assert!(a(&["take", "--vault", "/v"]).is_err());
        assert_eq!(class_words(&QuotaError::LoginExpired), "login expired");
        assert_eq!(class_words(&QuotaError::Http(503)), "http 503");
        assert_eq!(
            plan_from(Some("claude_max"), Some("default_claude_max_5x")).as_deref(),
            Some("max 5x")
        );
        assert_eq!(
            prose("Tag1 \u{2014} Consulting-Team"),
            "Tag1 Consulting Team"
        );
        assert_eq!(cell(""), NONE);
    }
}
