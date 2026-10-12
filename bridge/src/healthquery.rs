//! **The health archive, read by a turn**: the logic behind `jesse-healthdb-mcp`.
//!
//! Two tools. `healthdb_schema` describes what is there; `healthdb_query` runs ONE read-only
//! SQL statement. The binary in `src/bin/jesse-healthdb-mcp.rs` is a transport and nothing
//! else, for the reason `jesse-places-mcp` is.
//!
//! # The guards are HERE, not only in the role
//!
//! The server connects as `health_ro`, a role that can `SELECT` from `raw` and `analytics` and
//! nothing else. That is one layer. Every guard below holds whatever the role turns out to be
//! allowed, because a grant is a deployment fact that can drift and this file is not:
//!
//!   * **One statement.** [`single_statement`] scans the text (quotes, quoted identifiers,
//!     dollar quotes, both comment forms) and refuses a second statement, an unterminated
//!     quote or comment, and anything that does not begin `SELECT`, `WITH`, `TABLE` or
//!     `VALUES`. The statement then runs through the EXTENDED protocol, which Postgres itself
//!     refuses to run more than one statement through.
//!   * **Read only.** Every call is `BEGIN READ ONLY`, the statement, `ROLLBACK`. A rollback
//!     also reverts any `set_config` the statement made, and every call is a fresh connection,
//!     so nothing a statement does outlives it.
//!   * **Time and memory.** `SET LOCAL statement_timeout = '15s'` and a modest `work_mem`, with
//!     a client-side timeout a little longer than the statement's as the backstop.
//!   * **Size.** The statement is wrapped as a subquery with `LIMIT 501`, so Postgres stops
//!     early, and at most [`HEALTHQ_MAX_ROWS`] rows come back, each string value cut at
//!     [`HEALTHQ_MAX_TEXT`] characters, with a flag for either cut.
//!
//! # Nothing content-bearing is logged
//!
//! The one record a call leaves is a ledger line ([`HealthQueryLedger`]): the tool, the
//! outcome, the duration and the row count. Never the SQL, a value, an error message or the
//! URL. A Postgres error message goes back to the turn that asked, because the turn needs it to
//! fix its query, and nowhere else.

use std::collections::BTreeMap;
use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgConnection, PgDatabaseError, PgSslMode};
use sqlx::{Column, ConnectOptions, Connection, Executor, Row, TypeInfo};

use crate::*;

// ---- Limits -------------------------------------------------------------------------------

/// The most rows a query returns.
pub const HEALTHQ_MAX_ROWS: usize = 500;
/// The most characters any one string value keeps.
pub const HEALTHQ_MAX_TEXT: usize = 500;
/// `statement_timeout` for every statement a tool runs.
pub const HEALTHQ_STATEMENT_TIMEOUT: &str = "15s";
/// The client's own ceiling on one call, past the statement timeout, for a server that stops
/// answering rather than cancelling.
pub const HEALTHQ_CALL_TIMEOUT: Duration = Duration::from_secs(25);
/// `work_mem` for every call: enough for a sort over a route, not a hash join over the archive.
pub const HEALTHQ_WORK_MEM: &str = "16MB";
/// How long a call waits to connect.
pub const HEALTHQ_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A table whose row estimate is at or above this is reported by estimate, not counted.
pub const HEALTHQ_EXACT_COUNT_BELOW: i64 = 100_000;
/// The session time zone every call runs in: the owner's.
pub const HEALTHQ_TIME_ZONE: &str = "Europe/Rome";

/// The URL file of the read-only role, by path. The server reads and checks it itself.
pub const HEALTHQ_ENV_RO_URL_FILE: &str = "JESSE_HEALTHDB_RO_URL_FILE";
/// The CA the server's certificate is verified against.
pub const HEALTHQ_ENV_CA_FILE: &str = "JESSE_HEALTHDB_CA_FILE";
/// An explicit ledger path, overriding `<JESSE_STATE_DIR>/healthdb-mcp-calls.log`.
pub const HEALTHQ_ENV_LEDGER: &str = "JESSE_HEALTHDB_MCP_LEDGER";
/// The ledger's file name in the state directory.
pub const HEALTHQ_LEDGER_FILE: &str = "healthdb-mcp-calls.log";

/// The counter a `healthdb_query` call lands in, labelled by outcome.
pub const METRIC_MCP_QUERY: &str = "healthdb_mcp_query_total";
/// The counter a `healthdb_schema` call lands in, labelled by outcome.
pub const METRIC_MCP_SCHEMA: &str = "healthdb_mcp_schema_total";

/// The fixed guide `healthdb_schema` returns beside the live description.
pub const HEALTHQ_GUIDE: &[&str] = &[
    "Read only: one SELECT, WITH, TABLE or VALUES statement per healthdb_query call, at most 500 rows back, each text value cut at 500 characters, 15 second timeout. Aggregate in SQL rather than fetching rows to count them.",
    "Timestamps are timestamptz. Every call runs with TimeZone Europe/Rome, where Jeremy lives, so ::date and date_trunc give his local days.",
    "raw.quantity_sample: one row per HealthKit sample, `type` is the HealthKit identifier (for example HKQuantityTypeIdentifierHeartRate), `value` in `unit`. raw.category_sample: `value` is an integer code.",
    "Sleep (raw.category_sample, type HKCategoryTypeIdentifierSleepAnalysis) value: 0 in bed, 1 asleep (unspecified), 2 awake, 3 core, 4 deep, 5 REM.",
    "VO2 max: Jeremy changed watch on 2026-09-21, and readings before and after that date come from different devices; compare within one side of it, or say that the series spans the change.",
    "Swimming: the watch undercounts swim time by about 18 percent; say so when reporting swim duration or pace.",
    "raw.diet_row: one row per diet CSV row, `file` names the CSV, `data` is jsonb keyed by the CSV header and every value in it is TEXT; cast it, e.g. (data->>'calories')::numeric.",
    "Routes: raw.route (one per workout route, workout_uuid joins raw.workout.uuid) and raw.route_point (lat, lon, alt, ts, speed; idx orders the points). Use the analytics trail and terrain views first when they answer the question.",
    "A route holds up to 100000 points and the row cap cuts silently past 500, so never select all of a route's points: aggregate (min/max lat and lon, total climb, first and last point), or thin the sample (WHERE idx % 50 = 0).",
    "Distances are haversine in metres: 2 * 6371000 * asin(sqrt(sin(radians(lat2 - lat1) / 2)^2 + cos(radians(lat1)) * cos(radians(lat2)) * sin(radians(lon2 - lon1) / 2)^2)).",
    "Quote the numbers from the result when you answer; do not estimate what a query could have returned.",
];

// ---- The tools ----------------------------------------------------------------------------

/// The two tools, as a closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthDbTool {
    Schema,
    Query,
}

impl HealthDbTool {
    pub const ALL: [HealthDbTool; 2] = [HealthDbTool::Schema, HealthDbTool::Query];

    pub fn tool_name(self) -> &'static str {
        match self {
            HealthDbTool::Schema => "healthdb_schema",
            HealthDbTool::Query => "healthdb_query",
        }
    }

    pub fn parse(name: &str) -> Option<HealthDbTool> {
        HealthDbTool::ALL
            .into_iter()
            .find(|t| t.tool_name() == name)
    }

    pub fn description(self) -> &'static str {
        match self {
            HealthDbTool::Schema => {
                "Describe Jeremy's health archive (Apple Health samples, workouts, GPS routes \
                 and diet logs in Postgres): every table and view in the raw and analytics \
                 schemas with columns, types and row counts, the sample types present with \
                 units and date ranges, and a guide to reading them. Call this before writing \
                 a healthdb_query."
            }
            HealthDbTool::Query => {
                "Run ONE read-only SQL statement (SELECT, WITH, TABLE or VALUES) against \
                 Jeremy's health archive and get the rows back as JSON. At most 500 rows, each \
                 text value cut at 500 characters, 15 second timeout; flags say when anything \
                 was cut. Aggregate in SQL. Use it for health questions that reach past the \
                 health block attached to the turn, and quote the numbers it returns."
            }
        }
    }

    pub fn input_schema(self) -> Value {
        match self {
            HealthDbTool::Schema => json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
            HealthDbTool::Query => json!({
                "type": "object",
                "properties": {
                    "sql": {
                        "type": "string",
                        "description": "One SELECT, WITH, TABLE or VALUES statement.",
                    }
                },
                "required": ["sql"],
                "additionalProperties": false,
            }),
        }
    }
}

/// How a call ended, as the fixed word the ledger and the counters carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryOutcome {
    Ok,
    /// Refused before reaching Postgres: not one statement, not a read, no `sql`.
    Rejected,
    /// Cancelled by `statement_timeout`, or the client's own ceiling.
    Timeout,
    /// Postgres refused it: a syntax error, a missing relation, a write in a read-only
    /// transaction, a permission.
    Error,
    /// Postgres could not be reached.
    Unavailable,
}

impl QueryOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            QueryOutcome::Ok => "ok",
            QueryOutcome::Rejected => "rejected",
            QueryOutcome::Timeout => "timeout",
            QueryOutcome::Error => "error",
            QueryOutcome::Unavailable => "unavailable",
        }
    }
}

/// What a tool call produced: the body the turn sees, and the content-free shape the ledger
/// keeps.
#[derive(Debug, Clone)]
pub struct ToolReply {
    /// JSON on success; on failure, one line the turn can act on.
    pub text: String,
    pub is_error: bool,
    pub outcome: QueryOutcome,
    pub rows: usize,
}

impl ToolReply {
    fn ok(body: &Value, rows: usize) -> ToolReply {
        ToolReply {
            text: serde_json::to_string_pretty(body).unwrap_or_else(|_| body.to_string()),
            is_error: false,
            outcome: QueryOutcome::Ok,
            rows,
        }
    }

    fn fail(outcome: QueryOutcome, text: String) -> ToolReply {
        ToolReply {
            text,
            is_error: true,
            outcome,
            rows: 0,
        }
    }
}

// ---- One statement ------------------------------------------------------------------------

/// The leading keywords a statement may start with. Each is a read in a read-only transaction
/// and each is valid as a subquery, which the row cap relies on.
const READ_KEYWORDS: [&str; 4] = ["select", "with", "table", "values"];

/// Check that `sql` is exactly one statement that begins with a read keyword, and return it
/// without its trailing semicolons, whitespace and comments.
///
/// This is a lexer, not a parser: it knows where strings, quoted identifiers, dollar quotes
/// and comments begin and end, which is all it takes to find a `;` that ends a statement.
/// Anything it cannot account for (an unterminated quote or comment) is refused rather than
/// guessed at.
pub fn single_statement(sql: &str) -> Result<&str, String> {
    let b = sql.as_bytes();
    let n = b.len();
    let mut i = 0;
    // Where the last significant (non-space, non-comment) byte of the first statement ends.
    let mut end = 0;
    let mut first: Option<usize> = None;
    let mut terminated = false;
    let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80;

    while i < n {
        let c = b[i];
        // Whitespace and comments are never significant.
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            // Postgres block comments nest.
            let mut depth = 1;
            i += 2;
            while i < n && depth > 0 {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if depth > 0 {
                return Err("the SQL has an unterminated /* comment".to_string());
            }
            continue;
        }
        if c == b';' {
            terminated = true;
            i += 1;
            continue;
        }
        // Anything else is significant, and significant text after a `;` is a second statement.
        if terminated {
            return Err(
                "send exactly one SQL statement per call; this text holds more than one"
                    .to_string(),
            );
        }
        if first.is_none() {
            first = Some(i);
        }
        let prev_ident = i > 0 && is_ident(b[i - 1]);
        match c {
            b'\'' => {
                // E'...' strings take backslash escapes; every string takes '' as a quote.
                let escapes = i > 0
                    && (b[i - 1] == b'E' || b[i - 1] == b'e')
                    && !(i > 1 && is_ident(b[i - 2]));
                i = skip_quoted(b, i + 1, b'\'', escapes)
                    .ok_or_else(|| "the SQL has an unterminated string literal".to_string())?;
            }
            b'"' => {
                i = skip_quoted(b, i + 1, b'"', false)
                    .ok_or_else(|| "the SQL has an unterminated quoted identifier".to_string())?;
            }
            b'$' if !prev_ident => match dollar_tag(b, i) {
                Some(tag_len) => {
                    let tag = &b[i..i + tag_len];
                    let body = i + tag_len;
                    let close = b[body..]
                        .windows(tag_len)
                        .position(|w| w == tag)
                        .ok_or_else(|| "the SQL has an unterminated dollar quote".to_string())?;
                    i = body + close + tag_len;
                }
                // `$1`, a parameter; not a quote.
                None => i += 1,
            },
            _ => i += 1,
        }
        end = i;
    }

    let Some(start) = first else {
        return Err("the SQL is empty".to_string());
    };
    let stmt = &sql[start..end];
    let word: String = stmt
        .trim_start_matches(|ch: char| ch == '(' || ch.is_whitespace())
        .chars()
        .take_while(|ch| ch.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    if !READ_KEYWORDS.contains(&word.as_str()) {
        return Err(
            "only a read is allowed: the statement must begin with SELECT, WITH, TABLE or VALUES"
                .to_string(),
        );
    }
    Ok(stmt)
}

/// The index just past the closing `quote`, from `i` (just past the opening one), or `None`
/// when there is no closing quote. A doubled quote is an escaped quote.
fn skip_quoted(b: &[u8], mut i: usize, quote: u8, backslash: bool) -> Option<usize> {
    while i < b.len() {
        if backslash && b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == quote {
            if b.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// The length of a dollar-quote tag (`$$` or `$tag$`) starting at `i`, or `None` when the `$`
/// does not open one.
fn dollar_tag(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if b.get(j).is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    while let Some(&c) = b.get(j) {
        if c == b'$' {
            return Some(j - i + 1);
        }
        if !(c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80) {
            return None;
        }
        j += 1;
    }
    None
}

/// The statement as it is sent: a subquery, so the row cap stops Postgres early, rendered one
/// JSON text per row. The newlines keep a trailing `--` comment from swallowing the wrapper.
pub fn wrap_for_rows(stmt: &str) -> String {
    format!(
        "SELECT row_to_json(q)::text FROM (\n{stmt}\n) AS q LIMIT {}",
        HEALTHQ_MAX_ROWS + 1
    )
}

// ---- Shaping the rows ---------------------------------------------------------------------

/// Cut every string inside `v` to `max` characters, saying whether anything was cut.
fn truncate_strings(v: &mut Value, max: usize) -> bool {
    match v {
        Value::String(s) => {
            if s.chars().count() > max {
                *s = s.chars().take(max).collect();
                true
            } else {
                false
            }
        }
        Value::Array(a) => a
            .iter_mut()
            .fold(false, |cut, x| truncate_strings(x, max) | cut),
        Value::Object(o) => o
            .values_mut()
            .fold(false, |cut, x| truncate_strings(x, max) | cut),
        _ => false,
    }
}

/// Turn the column list and the per-row JSON texts into the reply body: rows as arrays in
/// column order, at most `max_rows` of them, strings cut at `max_text`, with a flag for each
/// kind of cut.
pub fn shape_rows(
    columns: &[(String, String)],
    raw: &[String],
    max_rows: usize,
    max_text: usize,
) -> Result<Value, String> {
    let rows_truncated = raw.len() > max_rows;
    let mut values_truncated = false;
    let mut rows = Vec::with_capacity(raw.len().min(max_rows));
    for text in raw.iter().take(max_rows) {
        let obj: serde_json::Map<String, Value> = serde_json::from_str(text)
            .map_err(|_| "a row could not be read back as JSON".to_string())?;
        let mut row: Vec<Value> = columns
            .iter()
            .map(|(name, _)| obj.get(name).cloned().unwrap_or(Value::Null))
            .collect();
        for v in &mut row {
            values_truncated |= truncate_strings(v, max_text);
        }
        rows.push(Value::Array(row));
    }
    let mut body = json!({
        "columns": columns
            .iter()
            .map(|(name, ty)| json!({"name": name, "type": ty}))
            .collect::<Vec<_>>(),
        "row_count": rows.len(),
        "rows": rows,
        "rows_truncated": rows_truncated,
        "values_truncated": values_truncated,
    });
    let mut notes = Vec::new();
    if rows_truncated {
        notes.push(format!(
            "more than {max_rows} rows matched and only the first {max_rows} are here; \
             aggregate, filter or thin the query rather than reading on"
        ));
    }
    if values_truncated {
        notes.push(format!(
            "at least one text value was longer than {max_text} characters and was cut"
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    if columns.iter().any(|(name, _)| !seen.insert(name)) {
        notes.push(
            "two columns share a name, so they share a value here; alias them apart".to_string(),
        );
    }
    if !notes.is_empty() {
        body["notes"] = json!(notes);
    }
    Ok(body)
}

// ---- Running ------------------------------------------------------------------------------

/// The server's connection settings: the read-only role's options with TLS forced to
/// `verify-full` against the CA, whatever the URL's own `sslmode` says.
#[derive(Clone, Debug)]
pub struct HealthQueryConfig {
    options: PgConnectOptions,
}

impl HealthQueryConfig {
    /// From [`HEALTHQ_ENV_RO_URL_FILE`] and [`HEALTHQ_ENV_CA_FILE`], with the same file checks
    /// the bridge makes at startup, run again here as the uid this process actually has.
    pub fn from_env() -> Result<HealthQueryConfig, String> {
        let var = |k: &str| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| format!("{k} is not set"))
        };
        let url_file = PathBuf::from(var(HEALTHQ_ENV_RO_URL_FILE)?);
        let ca_file = PathBuf::from(var(HEALTHQ_ENV_CA_FILE)?);
        // SAFETY: `geteuid` takes no arguments, cannot fail, and touches no memory of ours.
        let uid = unsafe { libc::geteuid() };
        let url = read_url_file_keyed(&url_file, uid, "ro_url_file")?;
        check_ca_file(&ca_file)?;
        Ok(HealthQueryConfig {
            options: url
                .connect_options()
                .ssl_mode(PgSslMode::VerifyFull)
                .ssl_root_cert(&ca_file),
        })
    }

    /// Over any options, as given. For tests against a server without TLS.
    pub fn from_options(options: PgConnectOptions) -> HealthQueryConfig {
        HealthQueryConfig { options }
    }

    pub fn options(&self) -> &PgConnectOptions {
        &self.options
    }

    async fn connect(&self) -> Result<PgConnection, ToolReply> {
        let options = self
            .options
            .clone()
            .application_name("jesse-healthdb-mcp")
            .disable_statement_logging();
        match tokio::time::timeout(HEALTHQ_CONNECT_TIMEOUT, options.connect()).await {
            Ok(Ok(c)) => Ok(c),
            Ok(Err(e)) => Err(failure(&e)),
            Err(_) => Err(ToolReply::fail(
                QueryOutcome::Unavailable,
                "the health archive did not answer within the connect timeout".to_string(),
            )),
        }
    }
}

/// A driver error as the turn should see it: Postgres's own message and SQLSTATE when it
/// answered, a fixed phrase when it could not be reached. Never logged.
fn failure(e: &sqlx::Error) -> ToolReply {
    if let sqlx::Error::Database(d) = e {
        let code = d.code().map(|c| c.into_owned()).unwrap_or_default();
        let outcome = if code == "57014" {
            QueryOutcome::Timeout
        } else {
            QueryOutcome::Error
        };
        let mut text = format!("Postgres error {code}: {}", d.message());
        if let Some(pg) = d.try_downcast_ref::<PgDatabaseError>() {
            if let Some(detail) = pg.detail() {
                text.push_str(&format!("\nDetail: {detail}"));
            }
            if let Some(hint) = pg.hint() {
                text.push_str(&format!("\nHint: {hint}"));
            }
        }
        if outcome == QueryOutcome::Timeout {
            text.push_str(&format!(
                "\nThe statement ran past {HEALTHQ_STATEMENT_TIMEOUT}; narrow the time range, \
                 filter on an indexed column (type, start_ts) or aggregate less."
            ));
        }
        return ToolReply::fail(outcome, text);
    }
    let why = match classify_db_error(e) {
        DbFailure::Unavailable(why) => why.to_string(),
        DbFailure::Database(code) => code,
    };
    ToolReply::fail(
        QueryOutcome::Unavailable,
        format!("the health archive could not be reached ({why})"),
    )
}

/// Open the guarded transaction every call runs in. Our own constant text, sent through the
/// simple protocol.
async fn begin_read_only(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    conn.execute(sqlx::raw_sql(&format!(
        "BEGIN READ ONLY; \
         SET LOCAL statement_timeout = '{HEALTHQ_STATEMENT_TIMEOUT}'; \
         SET LOCAL work_mem = '{HEALTHQ_WORK_MEM}'; \
         SET LOCAL TimeZone = '{HEALTHQ_TIME_ZONE}';"
    )))
    .await
    .map(|_| ())
}

/// Run `body` inside the guarded transaction on a fresh connection, roll back whatever
/// happened, and close. The client-side ceiling drops the connection outright, which Postgres
/// sees as a disconnect and answers by ending the backend.
async fn guarded<F, Fut>(cfg: &HealthQueryConfig, body: F) -> ToolReply
where
    F: FnOnce(PgConnection) -> Fut,
    Fut: std::future::Future<Output = (PgConnection, ToolReply)>,
{
    let mut conn = match cfg.connect().await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if let Err(e) = begin_read_only(&mut conn).await {
        return failure(&e);
    }
    match tokio::time::timeout(HEALTHQ_CALL_TIMEOUT, body(conn)).await {
        Ok((mut conn, reply)) => {
            let _ = conn.execute(sqlx::raw_sql("ROLLBACK")).await;
            let _ = conn.close().await;
            reply
        }
        Err(_) => ToolReply::fail(
            QueryOutcome::Timeout,
            format!(
                "the statement did not finish within {}s; narrow or aggregate it",
                HEALTHQ_CALL_TIMEOUT.as_secs()
            ),
        ),
    }
}

/// `healthdb_query`.
pub async fn run_health_query(cfg: &HealthQueryConfig, args: &Value) -> ToolReply {
    let Some(sql) = args.get("sql").and_then(Value::as_str) else {
        return ToolReply::fail(
            QueryOutcome::Rejected,
            "healthdb_query takes one argument, `sql`, a string".to_string(),
        );
    };
    let stmt = match single_statement(sql) {
        Ok(s) => s.to_string(),
        Err(why) => return ToolReply::fail(QueryOutcome::Rejected, why),
    };
    guarded(cfg, |mut conn| async move {
        let reply = query_rows(&mut conn, &stmt).await;
        (conn, reply)
    })
    .await
}

async fn query_rows(conn: &mut PgConnection, stmt: &str) -> ToolReply {
    // Describe prepares through the extended protocol: one statement or an error, and the
    // column names and types in order.
    let columns: Vec<(String, String)> = match conn.describe(stmt).await {
        Ok(d) => d
            .columns()
            .iter()
            .map(|c| (c.name().to_string(), c.type_info().name().to_string()))
            .collect(),
        Err(e) => return failure(&e),
    };
    let wrapped = wrap_for_rows(stmt);
    let rows = match sqlx::query(&wrapped)
        .persistent(false)
        .fetch_all(&mut *conn)
        .await
    {
        Ok(r) => r,
        Err(e) => return failure(&e),
    };
    let raw: Vec<String> = rows
        .iter()
        .map(|r| {
            r.try_get::<Option<String>, _>(0)
                .ok()
                .flatten()
                .unwrap_or_default()
        })
        .collect();
    match shape_rows(&columns, &raw, HEALTHQ_MAX_ROWS, HEALTHQ_MAX_TEXT) {
        Ok(body) => ToolReply::ok(&body, raw.len().min(HEALTHQ_MAX_ROWS)),
        Err(why) => ToolReply::fail(QueryOutcome::Error, why),
    }
}

/// Every table and view in `raw` and `analytics` this role can read, with its columns and a
/// row estimate. `_sqlx_*` bookkeeping is left out.
const SCHEMA_RELATIONS_SQL: &str = "\
SELECT coalesce(json_agg(r ORDER BY r.schema, r.name), '[]')::text FROM (
  SELECT n.nspname AS schema, c.relname AS name,
    CASE c.relkind WHEN 'r' THEN 'table' WHEN 'p' THEN 'table' WHEN 'v' THEN 'view'
      WHEN 'm' THEN 'materialized view' ELSE 'foreign table' END AS kind,
    (SELECT coalesce(json_agg(json_build_object('name', a.attname,
              'type', format_type(a.atttypid, a.atttypmod)) ORDER BY a.attnum), '[]')
       FROM pg_attribute a
      WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped) AS columns,
    CASE WHEN c.relkind IN ('r', 'p', 'm')
      THEN greatest(c.reltuples::bigint, coalesce(s.n_live_tup, 0)) END AS estimate
  FROM pg_class c
  JOIN pg_namespace n ON n.oid = c.relnamespace
  LEFT JOIN pg_stat_all_tables s ON s.relid = c.oid
  WHERE n.nspname IN ('raw', 'analytics')
    AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
    AND c.relname NOT LIKE '\\_sqlx%'
    AND has_table_privilege(c.oid, 'SELECT')
) r";

/// The distinct `type` values of a sample table with their first and last dates, by a skip
/// scan over the `(type, start_ts)` index rather than a full scan, and the unit of the first
/// and last sample where the table has one.
fn sample_types_sql(table: &str, with_unit: bool) -> String {
    let unit = if with_unit {
        format!(
            ", 'units', (SELECT coalesce(json_agg(DISTINCT u), '[]') FROM ( \
               (SELECT unit AS u FROM {table} x WHERE x.type = t.type ORDER BY x.start_ts LIMIT 1) \
               UNION ALL \
               (SELECT unit FROM {table} x WHERE x.type = t.type ORDER BY x.start_ts DESC LIMIT 1) \
             ) us)"
        )
    } else {
        String::new()
    };
    format!(
        "WITH RECURSIVE t(type) AS ( \
           (SELECT type FROM {table} ORDER BY type LIMIT 1) \
           UNION ALL \
           SELECT (SELECT x.type FROM {table} x WHERE x.type > t.type ORDER BY x.type LIMIT 1) \
             FROM t WHERE t.type IS NOT NULL) \
         SELECT coalesce(json_agg(json_build_object( \
           'type', t.type{unit}, \
           'first', (SELECT min(x.start_ts) FROM {table} x WHERE x.type = t.type)::date, \
           'last', (SELECT max(x.start_ts) FROM {table} x WHERE x.type = t.type)::date \
         ) ORDER BY t.type), '[]')::text FROM t WHERE t.type IS NOT NULL"
    )
}

/// A Postgres identifier, double quoted.
fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `healthdb_schema`.
pub async fn run_health_schema(cfg: &HealthQueryConfig) -> ToolReply {
    guarded(cfg, |mut conn| async move {
        let reply = match describe_schema(&mut conn).await {
            Ok((body, n)) => ToolReply::ok(&body, n),
            Err(e) => failure(&e),
        };
        (conn, reply)
    })
    .await
}

async fn json_text(conn: &mut PgConnection, sql: &str) -> Result<Value, sqlx::Error> {
    let text: String = sqlx::query_scalar(sql)
        .persistent(false)
        .fetch_one(&mut *conn)
        .await?;
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn describe_schema(conn: &mut PgConnection) -> Result<(Value, usize), sqlx::Error> {
    let mut relations = match json_text(conn, SCHEMA_RELATIONS_SQL).await? {
        Value::Array(a) => a,
        _ => Vec::new(),
    };
    let mut present = std::collections::BTreeSet::new();
    for r in &mut relations {
        let schema = r["schema"].as_str().unwrap_or_default().to_string();
        let name = r["name"].as_str().unwrap_or_default().to_string();
        present.insert(format!("{schema}.{name}"));
        let estimate = r.get("estimate").and_then(Value::as_i64);
        let (rows, estimated) = match estimate {
            // A view has no estimate and is not counted: counting it would run it.
            None => (Value::Null, false),
            Some(e) if e >= HEALTHQ_EXACT_COUNT_BELOW => (json!(e), true),
            Some(_) => {
                let n: i64 = sqlx::query_scalar(&format!(
                    "SELECT count(*) FROM {}.{}",
                    quote_ident(&schema),
                    quote_ident(&name)
                ))
                .persistent(false)
                .fetch_one(&mut *conn)
                .await?;
                (json!(n), false)
            }
        };
        if let Value::Object(o) = r {
            o.remove("estimate");
            o.insert("rows".to_string(), rows);
            o.insert("rows_estimated".to_string(), json!(estimated));
        }
    }
    let mut body = json!({ "relations": relations });
    if present.contains("raw.quantity_sample") {
        body["quantity_types"] =
            json_text(conn, &sample_types_sql("raw.quantity_sample", true)).await?;
    }
    if present.contains("raw.category_sample") {
        body["category_types"] =
            json_text(conn, &sample_types_sql("raw.category_sample", false)).await?;
    }
    body["guide"] = json!(HEALTHQ_GUIDE);
    let n = present.len();
    Ok((body, n))
}

/// Run one tool.
pub async fn run_health_tool(
    cfg: &HealthQueryConfig,
    tool: HealthDbTool,
    args: &Value,
) -> ToolReply {
    match tool {
        HealthDbTool::Schema => run_health_schema(cfg).await,
        HealthDbTool::Query => run_health_query(cfg, args).await,
    }
}

// ---- The ledger and the counters ----------------------------------------------------------

/// One line per call, appended by the server and tallied by the bridge for `GET /health`.
///
/// A file rather than an in-process counter for the reason `places-google-calls.log` is one:
/// the server is spawned per turn, so a counter in it would reset with every question and the
/// bridge would never see it. Each line is content-free: the time, the tool, the outcome, the
/// duration and the row count.
#[derive(Debug, Clone)]
pub struct HealthQueryLedger {
    path: Option<PathBuf>,
}

impl HealthQueryLedger {
    /// [`HEALTHQ_ENV_LEDGER`], else `<JESSE_STATE_DIR>/healthdb-mcp-calls.log`, else
    /// `$HOME/.jesse-bridge/healthdb-mcp-calls.log`, the two-step the places ledger uses.
    pub fn from_env() -> HealthQueryLedger {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let path = var(HEALTHQ_ENV_LEDGER).map(PathBuf::from).or_else(|| {
            var("JESSE_STATE_DIR")
                .or_else(|| var("HOME").map(|h| format!("{h}/.jesse-bridge")))
                .map(|d| PathBuf::from(d).join(HEALTHQ_LEDGER_FILE))
        });
        HealthQueryLedger { path }
    }

    pub fn at(path: PathBuf) -> HealthQueryLedger {
        HealthQueryLedger { path: Some(path) }
    }

    /// Append one line. A failure is reported on stderr, content-free, and never fails the call.
    pub fn record(&self, tool: HealthDbTool, reply: &ToolReply, elapsed: Duration) {
        let Some(path) = &self.path else {
            return;
        };
        let line = json!({
            "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "tool": tool.tool_name(),
            "outcome": reply.outcome.as_str(),
            "ms": elapsed.as_millis() as u64,
            "rows": reply.rows,
        });
        let res = (|| -> std::io::Result<()> {
            use std::os::unix::fs::OpenOptionsExt;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)?;
            writeln!(f, "{line}")
        })();
        if let Err(e) = res {
            eprintln!("jesse-healthdb-mcp: the call ledger could not be written: {e}");
        }
    }
}

/// The ledger tallied into counters: `{metric: {outcome: n}}`, one metric per tool. Lines that
/// do not parse are skipped. Lifetime of the ledger, not of the bridge process.
pub fn healthdb_mcp_counters(ledger: &Path) -> BTreeMap<&'static str, BTreeMap<String, u64>> {
    let mut out: BTreeMap<&'static str, BTreeMap<String, u64>> = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(ledger) else {
        return out;
    };
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let metric = match v["tool"].as_str().and_then(HealthDbTool::parse) {
            Some(HealthDbTool::Query) => METRIC_MCP_QUERY,
            Some(HealthDbTool::Schema) => METRIC_MCP_SCHEMA,
            None => continue,
        };
        let Some(outcome) = v["outcome"].as_str() else {
            continue;
        };
        *out.entry(metric)
            .or_default()
            .entry(outcome.to_string())
            .or_default() += 1;
    }
    out
}

/// The `healthdb.counters` object `GET /health` serves: the bridge's own ingest counters, plus
/// the server's call counters tallied from the ledger in `state_dir` when there is one.
pub fn healthdb_all_counters_json(state_dir: Option<&str>) -> Value {
    let mut body = healthdb_counters_json();
    if let (Some(dir), Value::Object(o)) = (state_dir, &mut body) {
        for (metric, labels) in healthdb_mcp_counters(&PathBuf::from(dir).join(HEALTHQ_LEDGER_FILE))
        {
            o.insert(metric.to_string(), json!(labels));
        }
    }
    body
}

/// Parse a `postgres://` URL into options, for tests and the smoke harness.
pub fn healthq_options_from_url(url: &str) -> Result<PgConnectOptions, String> {
    PgConnectOptions::from_str(url).map_err(|_| "not a postgres:// URL".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_statement_passes_with_its_trailing_semicolon_and_comment_removed() {
        assert_eq!(
            single_statement("  SELECT 1 ;  -- done\n").unwrap(),
            "SELECT 1"
        );
        assert_eq!(
            single_statement("with a as (select 1) select * from a").unwrap(),
            "with a as (select 1) select * from a"
        );
        assert_eq!(single_statement("(SELECT 1)").unwrap(), "(SELECT 1)");
        assert_eq!(
            single_statement("TABLE raw.route").unwrap(),
            "TABLE raw.route"
        );
        assert_eq!(single_statement("VALUES (1)").unwrap(), "VALUES (1)");
    }

    #[test]
    fn two_statements_are_rejected() {
        for sql in [
            "SELECT 1; SELECT 2",
            "SELECT 1;DELETE FROM raw.workout",
            "select 1; -- fine\n commit",
            "SELECT 1 ; ; SELECT 2",
            "SELECT 1; /* c */ SET ROLE health",
        ] {
            let e = single_statement(sql).unwrap_err();
            assert!(e.contains("exactly one"), "{sql}: {e}");
        }
    }

    #[test]
    fn a_semicolon_inside_a_quote_or_comment_is_not_a_second_statement() {
        for sql in [
            "SELECT ';'",
            "SELECT 'it''s; fine'",
            "SELECT E'a\\'; b'",
            "SELECT \"a;b\" FROM t",
            "SELECT $$ ; $$",
            "SELECT $tag$ ; $ $tag$",
            "SELECT 1 /* ; /* nested ; */ ; */",
            "SELECT 1 -- ; trailing\n",
            "SELECT a$b FROM t WHERE x = $1",
        ] {
            assert!(single_statement(sql).is_ok(), "{sql}");
        }
    }

    #[test]
    fn unterminated_text_and_non_reads_are_rejected() {
        for sql in [
            "SELECT 'open",
            "SELECT \"open",
            "SELECT $$ open",
            "SELECT 1 /* open",
        ] {
            assert!(
                single_statement(sql).unwrap_err().contains("unterminated"),
                "{sql}"
            );
        }
        for sql in [
            "INSERT INTO raw.workout DEFAULT VALUES",
            "UPDATE raw.workout SET effort = 1",
            "CREATE TABLE x (a int)",
            "SET ROLE health",
            "COMMIT",
            "DO $$ BEGIN END $$",
            "COPY raw.workout TO STDOUT",
            "EXPLAIN ANALYZE SELECT 1",
        ] {
            assert!(
                single_statement(sql).unwrap_err().contains("only a read"),
                "{sql}"
            );
        }
        assert!(single_statement("  -- nothing\n ; ")
            .unwrap_err()
            .contains("empty"));
    }

    #[test]
    fn the_wrapper_survives_a_trailing_line_comment() {
        let w = wrap_for_rows("SELECT 1 -- note");
        assert!(w.contains("-- note\n) AS q LIMIT 501"), "{w}");
    }

    fn cols(names: &[&str]) -> Vec<(String, String)> {
        names
            .iter()
            .map(|n| (n.to_string(), "TEXT".to_string()))
            .collect()
    }

    #[test]
    fn the_row_cap_and_value_truncation_are_flagged() {
        let raw: Vec<String> = (0..501).map(|i| format!("{{\"n\":{i}}}")).collect();
        let body = shape_rows(&cols(&["n"]), &raw, 500, 500).unwrap();
        assert_eq!(body["row_count"], json!(500));
        assert_eq!(body["rows"].as_array().unwrap().len(), 500);
        assert_eq!(body["rows_truncated"], json!(true));
        assert_eq!(body["values_truncated"], json!(false));

        let long = "x".repeat(600);
        let raw = vec![json!({"s": long, "j": {"inner": long}}).to_string()];
        let body = shape_rows(&cols(&["s", "j"]), &raw, 500, 500).unwrap();
        assert_eq!(body["rows_truncated"], json!(false));
        assert_eq!(body["values_truncated"], json!(true));
        assert_eq!(body["rows"][0][0].as_str().unwrap().chars().count(), 500);
        assert_eq!(body["rows"][0][1]["inner"].as_str().unwrap().len(), 500);
        assert!(body["notes"].as_array().unwrap().len() == 1);

        let body = shape_rows(&cols(&["n"]), &[r#"{"n":1}"#.to_string()], 500, 500).unwrap();
        assert!(body.get("notes").is_none());
        assert_eq!(body["rows_truncated"], json!(false));
    }

    #[test]
    fn a_route_point_row_keeps_its_coordinates_intact() {
        // What row_to_json renders for a raw.route_point row.
        let raw = vec![
            r#"{"idx":0,"lat":44.0587312345678,"lon":10.4618876543219,"alt":312.75}"#.to_string(),
            r#"{"idx":1,"lat":-33.8567844,"lon":151.2152967,"alt":null}"#.to_string(),
        ];
        let body = shape_rows(&cols(&["idx", "lat", "lon", "alt"]), &raw, 500, 500).unwrap();
        assert_eq!(
            body["rows"][0],
            json!([0, 44.0587312345678, 10.4618876543219, 312.75])
        );
        assert_eq!(body["rows"][1], json!([1, -33.8567844, 151.2152967, null]));
        assert_eq!(body["values_truncated"], json!(false));
    }

    #[test]
    fn columns_come_back_in_statement_order_and_a_shared_name_is_noted() {
        let body = shape_rows(
            &cols(&["z", "a", "a"]),
            &[r#"{"z":1,"a":2}"#.to_string()],
            500,
            500,
        )
        .unwrap();
        assert_eq!(body["rows"][0], json!([1, 2, 2]));
        assert!(body["notes"][0].as_str().unwrap().contains("share a name"));
    }

    #[test]
    fn tools_parse_by_name_and_only_query_takes_sql() {
        assert_eq!(
            HealthDbTool::parse("healthdb_query"),
            Some(HealthDbTool::Query)
        );
        assert_eq!(
            HealthDbTool::parse("healthdb_schema"),
            Some(HealthDbTool::Schema)
        );
        assert_eq!(HealthDbTool::parse("healthdb_write"), None);
        assert_eq!(
            HealthDbTool::Query.input_schema()["required"],
            json!(["sql"])
        );
        assert!(HealthDbTool::Schema.input_schema()["properties"]
            .as_object()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn the_guide_names_every_fixed_fact() {
        let g = HEALTHQ_GUIDE.join("\n");
        for needle in [
            "0 in bed, 1 asleep",
            "2 awake, 3 core, 4 deep, 5 REM",
            "2026-09-21",
            "18 percent",
            "TEXT",
            "timestamptz",
            "Europe/Rome",
            "analytics trail and terrain views first",
            "never select all of a route's points",
            "haversine in metres",
        ] {
            assert!(g.contains(needle), "{needle}");
        }
    }

    #[tokio::test]
    async fn a_missing_sql_argument_is_rejected_without_connecting() {
        // Port 1 on loopback: nothing listens, so reaching the connect step would fail as
        // `unavailable`, not `rejected`.
        let cfg = HealthQueryConfig::from_options(
            healthq_options_from_url("postgres://u:p@127.0.0.1:1/db").unwrap(),
        );
        let r = run_health_query(&cfg, &json!({})).await;
        assert_eq!(r.outcome, QueryOutcome::Rejected);
        let r = run_health_query(&cfg, &json!({"sql": "SELECT 1; SELECT 2"})).await;
        assert_eq!(r.outcome, QueryOutcome::Rejected);
        assert!(r.is_error);
    }

    #[test]
    fn the_ledger_tallies_by_tool_and_outcome_and_holds_no_content() {
        let dir = std::env::temp_dir().join(format!("healthq-ledger-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(HEALTHQ_LEDGER_FILE);
        let ledger = HealthQueryLedger::at(path.clone());
        let mk = |outcome| ToolReply {
            text: "SECRET RESULT".to_string(),
            is_error: false,
            outcome,
            rows: 3,
        };
        ledger.record(
            HealthDbTool::Query,
            &mk(QueryOutcome::Ok),
            Duration::from_millis(5),
        );
        ledger.record(
            HealthDbTool::Query,
            &mk(QueryOutcome::Ok),
            Duration::from_millis(5),
        );
        ledger.record(
            HealthDbTool::Query,
            &mk(QueryOutcome::Rejected),
            Duration::ZERO,
        );
        ledger.record(
            HealthDbTool::Query,
            &mk(QueryOutcome::Timeout),
            Duration::ZERO,
        );
        ledger.record(HealthDbTool::Schema, &mk(QueryOutcome::Ok), Duration::ZERO);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("SECRET"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let c = healthdb_mcp_counters(&path);
        assert_eq!(c[METRIC_MCP_QUERY]["ok"], 2);
        assert_eq!(c[METRIC_MCP_QUERY]["rejected"], 1);
        assert_eq!(c[METRIC_MCP_QUERY]["timeout"], 1);
        assert_eq!(c[METRIC_MCP_SCHEMA]["ok"], 1);
        assert!(healthdb_mcp_counters(&dir.join("absent")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
