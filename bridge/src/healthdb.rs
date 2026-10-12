//! **The health archive**: Apple Health samples and the diet CSVs, kept in Postgres.
//!
//! Two paths, both OFF unless the optional `[healthdb]` table is configured:
//!
//!   * `POST /jesse/health/ingest` takes one batch of HealthKit samples of one kind
//!     (`quantity`, `category`, `workout` or `route`) and upserts it, keyed by HealthKit UUID,
//!     in ONE transaction. `200` is sent only after that transaction commits, because the phone
//!     advances its HealthKit anchor on a `200` and on nothing else; and every write is an
//!     upsert (or a whole replacement, for a workout's events and a route's points), so a
//!     resend of a batch the phone never saw acknowledged is harmless.
//!   * The diet mirror: every [`DIET_MIRROR_INTERVAL`], each diet CSV whose sha256 changed is
//!     reloaded whole into `raw.diet_row`, one jsonb object per row keyed by the header.
//!
//! **A RAW ARCHIVE.** A value that is merely implausible (a heart rate of 250) is stored. What
//! is refused is a body that does not meet the contract: bad JSON, an unknown kind, a missing
//! field, a non-finite number, `start` after `end`, a coordinate off the globe. One bad sample
//! refuses the whole batch and nothing is written.
//!
//! **NOTHING CONTENT-BEARING LEAVES THIS MODULE EXCEPT INTO THE DATABASE.** No log line, error
//! body, metric label or panic message carries a sample value, a coordinate, a timestamp, the
//! connection URL or its password. Log lines name the kind, the HealthKit type and counts; an
//! error names the sample index and the field. A database error is reported by SQLSTATE only,
//! because Postgres messages can quote the offending value.
//!
//! **ABSENT IS INERT.** With no `[healthdb]` table, `AppState::healthdb` is `None`: no pool, no
//! task, no connection, and the route answers `503 {"error":"not_configured"}`.

use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgSslMode};
use sqlx::{ConnectOptions, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::*;

// ---- Limits -------------------------------------------------------------------------------

/// The most samples one non-route request may carry.
pub const INGEST_MAX_SAMPLES: usize = 2000;
/// The body cap for a `quantity`, `category` or `workout` request.
pub const INGEST_MAX_BODY: usize = 8 * 1024 * 1024;
/// The body cap for a `route` request, and so the most the route ever buffers.
pub const INGEST_MAX_ROUTE_BODY: usize = 16 * 1024 * 1024;
/// The most points one route may carry. The phone thins a longer route evenly and sets
/// `downsampled`.
pub const ROUTE_MAX_POINTS: usize = 100_000;
/// The pool size. One phone, one mirror task: four is headroom, not throughput.
pub const HEALTHDB_MAX_CONNECTIONS: u32 = 4;
/// How long a request waits for a connection before answering `503 unavailable`. A phone
/// syncing in the background should hear "try later" promptly rather than hold a socket.
pub const HEALTHDB_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the diet mirror looks for changed files.
pub const DIET_MIRROR_INTERVAL: Duration = Duration::from_secs(600);
/// How long the startup task waits between attempts to bring the schema current while
/// Postgres is unreachable.
const MIGRATE_RETRY: Duration = Duration::from_secs(60);
/// Postgres takes at most 65535 bind parameters per statement; every multi-row insert is
/// chunked to stay well under it.
const MAX_BINDS: usize = 30_000;

/// The diet CSVs the mirror keeps, by file name, in `diet-logs/`.
pub const DIET_MIRROR_FILES: [&str; 5] = [
    "food-log.csv",
    "exercise-log.csv",
    "weight-log.csv",
    VITALS_LOG,
    "daily-targets.csv",
];

/// The embedded schema. `migrate!` only reads `migrations/healthdb/` into the binary at build
/// time; it never connects anywhere, so the build needs no database.
pub static HEALTHDB_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/healthdb");

// ---- Config -------------------------------------------------------------------------------

/// The `[healthdb]` table as written, every field optional so a missing one is refused by name
/// rather than failing the whole overlay file's parse.
#[derive(Deserialize, Debug, Default, Clone)]
pub struct HealthDbToml {
    /// A file holding the `postgres://` connection URL, password included. Mode `0600`.
    pub url_file: Option<String>,
    /// The PEM certificate of the CA that signed the server's certificate.
    pub ca_file: Option<String>,
    /// Whether to run the diet mirror. Default true.
    pub mirror_diet: Option<bool>,
    /// A file holding the connection URL of the READ-ONLY role (`health_ro`), for the
    /// `jesse-healthdb-mcp` server a turn reads the archive through. Optional; same `0600` and
    /// owner checks as `url_file`, same `ca_file`. The bridge itself never connects with it.
    pub ro_url_file: Option<String>,
}

/// The connection URL. It carries the password, so its `Debug` prints a marker and it has no
/// `Display`: no `{:?}` of a config can put it in a log.
#[derive(Clone, PartialEq, Eq)]
pub struct HealthDbUrl(String);

impl HealthDbUrl {
    /// The URL's options, as written. Only ever built from a URL that already parsed.
    pub fn connect_options(&self) -> PgConnectOptions {
        PgConnectOptions::from_str(&self.0).unwrap_or_else(|_| PgConnectOptions::new())
    }
}

impl std::fmt::Debug for HealthDbUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HealthDbUrl(<redacted>)")
    }
}

/// A `[healthdb]` table that passed every check.
#[derive(Clone, Debug)]
pub struct HealthDbSettings {
    pub url: HealthDbUrl,
    /// Where the URL came from, for messages.
    pub url_file: PathBuf,
    pub ca_file: PathBuf,
    pub mirror_diet: bool,
    /// The read-only role's URL file, checked at load and handed to `jesse-healthdb-mcp` BY
    /// PATH ([`HealthDbSettings::healthdb_mcp_env`]); its contents are not kept here, so the
    /// bridge holds one credential and the server holds the other.
    pub ro_url_file: Option<PathBuf>,
}

impl HealthDbSettings {
    /// The connection options: the URL as written, with TLS forced to `verify-full` against
    /// `ca_file` whatever the URL's own `sslmode` says. Already parsed once at config load, so
    /// this cannot fail on a ready table.
    pub fn connect_options(&self) -> PgConnectOptions {
        PgConnectOptions::from_str(&self.url.0)
            .unwrap_or_else(|_| PgConnectOptions::new())
            .ssl_mode(PgSslMode::VerifyFull)
            .ssl_root_cert(&self.ca_file)
    }

    /// The environment a `jesse-healthdb-mcp` child is started with, or `None` when no
    /// `ro_url_file` is configured, in which case the server must not be registered at all.
    /// Paths only: the server reads and checks the URL file itself, so the read-only password
    /// never travels through an argv, an environment or a config string, and the owner's URL
    /// is never offered to it.
    pub fn healthdb_mcp_env(&self) -> Option<[(&'static str, String); 2]> {
        let ro = self.ro_url_file.as_ref()?;
        Some([
            (HEALTHQ_ENV_RO_URL_FILE, ro.to_string_lossy().into_owned()),
            (
                HEALTHQ_ENV_CA_FILE,
                self.ca_file.to_string_lossy().into_owned(),
            ),
        ])
    }
}

/// What the deployment's `[healthdb]` table amounts to.
#[derive(Clone, Debug, Default)]
pub enum HealthDbConfig {
    /// No table: the shipped default. Nothing is built and the route says `not_configured`.
    #[default]
    Absent,
    Ready(HealthDbSettings),
    /// Present and wrong. The startup gate refuses to boot on it ([`validate_healthdb`]).
    Refused(String),
}

impl HealthDbConfig {
    /// Resolve the table: absent stays absent; a present table is ready, or refused with every
    /// reason it was refused for.
    pub fn from_toml(table: Option<HealthDbToml>) -> HealthDbConfig {
        Self::from_toml_as(table, current_euid())
    }

    /// [`Self::from_toml`] for a given owner uid, so the owner check is testable.
    pub fn from_toml_as(table: Option<HealthDbToml>, uid: u32) -> HealthDbConfig {
        let Some(t) = table else {
            return HealthDbConfig::Absent;
        };
        let mut problems = Vec::new();
        let path = |v: Option<String>, key: &str, problems: &mut Vec<String>| match v
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(p) => Some(PathBuf::from(p)),
            None => {
                problems.push(format!("`{key}` is missing"));
                None
            }
        };
        let url_file = path(t.url_file, "url_file", &mut problems);
        let ca_file = path(t.ca_file, "ca_file", &mut problems);
        let url = url_file
            .as_deref()
            .and_then(|p| match read_url_file(p, uid) {
                Ok(u) => Some(u),
                Err(e) => {
                    problems.push(e);
                    None
                }
            });
        let ro_url_file = t
            .ro_url_file
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        if let Some(ro) = &ro_url_file {
            match read_url_file_keyed(ro, uid, "ro_url_file") {
                Ok(ro_url) => {
                    // The point of a second file is a second ROLE. The same role in both is
                    // the owner credential reaching the turn's server, which is refused.
                    let same_role = url.as_ref().is_some_and(|owner| {
                        owner.0 == ro_url.0
                            || ro_url.connect_options().get_username()
                                == owner.connect_options().get_username()
                    });
                    if same_role {
                        problems.push(
                            "`ro_url_file` names the same role as `url_file`; it must hold the \
                             read-only role's URL (health_ro), never the owner's"
                                .to_string(),
                        );
                    }
                }
                Err(e) => problems.push(e),
            }
        }
        if let Some(ca) = &ca_file {
            if let Err(e) = check_ca_file(ca) {
                problems.push(e);
            }
        }
        match (url, url_file, ca_file) {
            (Some(url), Some(url_file), Some(ca_file)) if problems.is_empty() => {
                HealthDbConfig::Ready(HealthDbSettings {
                    url,
                    url_file,
                    ca_file,
                    mirror_diet: t.mirror_diet.unwrap_or(true),
                    ro_url_file,
                })
            }
            _ => HealthDbConfig::Refused(problems.join("; ")),
        }
    }

    pub fn ready(&self) -> Option<&HealthDbSettings> {
        match self {
            HealthDbConfig::Ready(s) => Some(s),
            HealthDbConfig::Absent | HealthDbConfig::Refused(_) => None,
        }
    }
}

fn current_euid() -> u32 {
    // SAFETY: `geteuid` takes no arguments, cannot fail, and touches no memory of ours.
    unsafe { libc::geteuid() }
}

/// Read the connection URL, refusing a file anyone but its owner can read or change, one owned
/// by another user, and one that does not hold a `postgres://` URL. No message here quotes the
/// file's contents: it holds a password.
pub fn read_url_file(path: &Path, uid: u32) -> Result<HealthDbUrl, String> {
    read_url_file_keyed(path, uid, "url_file")
}

/// [`read_url_file`] for any key, so `ro_url_file` gets the same checks and messages that name
/// it rather than `url_file`.
pub fn read_url_file_keyed(path: &Path, uid: u32, key: &str) -> Result<HealthDbUrl, String> {
    let shown = path.display();
    let meta =
        std::fs::metadata(path).map_err(|e| format!("`{key}` {shown} cannot be read: {e}"))?;
    if !meta.is_file() {
        return Err(format!("`{key}` {shown} is not a regular file"));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "`{key}` {shown} has mode {mode:04o}, open to group or other; it holds a \
             password and must be 0600 (chmod 600 {shown})"
        ));
    }
    if meta.uid() != uid {
        return Err(format!(
            "`{key}` {shown} is owned by uid {}, not by the uid {uid} reading it",
            meta.uid()
        ));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("`{key}` {shown} cannot be read: {e}"))?;
    let url = text.trim();
    if url.is_empty() {
        return Err(format!("`{key}` {shown} is empty"));
    }
    if !(url.starts_with("postgres://") || url.starts_with("postgresql://"))
        || url.contains(char::is_whitespace)
        || PgConnectOptions::from_str(url).is_err()
    {
        // The parser's own message is not passed on: it can echo part of the URL.
        return Err(format!(
            "`{key}` {shown} must hold one postgres:// connection URL on one line"
        ));
    }
    Ok(HealthDbUrl(url.to_string()))
}

/// The CA must be readable and hold a PEM certificate.
pub fn check_ca_file(path: &Path) -> Result<(), String> {
    let shown = path.display();
    let pem = std::fs::read(path).map_err(|e| format!("`ca_file` {shown} cannot be read: {e}"))?;
    if !String::from_utf8_lossy(&pem).contains("-----BEGIN CERTIFICATE-----") {
        return Err(format!("`ca_file` {shown} holds no PEM certificate"));
    }
    reqwest::Certificate::from_pem(&pem)
        .map(|_| ())
        .map_err(|e| format!("`ca_file` {shown} is not a PEM certificate: {e}"))
}

/// The startup gate's view of `[healthdb]`: a refused table stops the bridge, by name.
pub fn validate_healthdb(cfg: &Config) -> Vec<ConfigError> {
    match &cfg.healthdb {
        HealthDbConfig::Refused(why) => vec![ConfigError {
            model: None,
            message: format!(
                "the [healthdb] table is present but unusable: {why}. Fix it in \
                 jesse.local.toml, or remove the table to run without the health archive."
            ),
        }],
        HealthDbConfig::Absent | HealthDbConfig::Ready(_) => Vec::new(),
    }
}

// ---- Counters -----------------------------------------------------------------------------

/// The archive's counters, by metric name and then by label value. Process lifetime, served
/// under `healthdb.counters` on the auth-gated `GET /health`. Every label is a fixed word (a
/// kind, a reason, a diet file name), never a value.
static COUNTERS: Mutex<BTreeMap<&'static str, BTreeMap<String, u64>>> = Mutex::new(BTreeMap::new());

pub const METRIC_INGEST_SAMPLES: &str = "healthdb_ingest_samples_total";
pub const METRIC_INGEST_ERRORS: &str = "healthdb_ingest_errors_total";
pub const METRIC_DIET_RELOAD: &str = "healthdb_diet_reload_total";

pub fn healthdb_count(metric: &'static str, label: &str, n: u64) {
    let mut m = COUNTERS.lock().unwrap_or_else(|p| p.into_inner());
    *m.entry(metric)
        .or_default()
        .entry(label.to_string())
        .or_default() += n;
}

pub fn healthdb_counter(metric: &'static str, label: &str) -> u64 {
    let m = COUNTERS.lock().unwrap_or_else(|p| p.into_inner());
    m.get(metric)
        .and_then(|l| l.get(label))
        .copied()
        .unwrap_or(0)
}

pub fn healthdb_counters_json() -> Value {
    let m = COUNTERS.lock().unwrap_or_else(|p| p.into_inner());
    json!(*m)
}

// ---- Database -----------------------------------------------------------------------------

/// Why a database step failed, as far as a caller may know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbFailure {
    /// Postgres could not be reached, or the connection broke: `503 unavailable`.
    Unavailable(&'static str),
    /// Postgres answered with an error. The SQLSTATE only; its message can quote a value.
    Database(String),
}

impl std::fmt::Display for DbFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbFailure::Unavailable(why) => write!(f, "unavailable ({why})"),
            DbFailure::Database(code) => write!(f, "database error SQLSTATE {code}"),
        }
    }
}

/// Classify a driver error without ever formatting it.
pub fn classify_db_error(e: &sqlx::Error) -> DbFailure {
    match e {
        sqlx::Error::Database(d) => {
            let code = d.code().map(|c| c.into_owned()).unwrap_or_default();
            // Class 08 is a connection exception; 57P01..57P03 are an administrator
            // shutdown, a crash shutdown and "cannot connect now". All mean "not reachable".
            if code.starts_with("08") || matches!(code.as_str(), "57P01" | "57P02" | "57P03") {
                DbFailure::Unavailable("connection lost")
            } else {
                DbFailure::Database(code)
            }
        }
        sqlx::Error::PoolTimedOut => DbFailure::Unavailable("no connection within the timeout"),
        sqlx::Error::PoolClosed => DbFailure::Unavailable("pool closed"),
        sqlx::Error::Io(_) => DbFailure::Unavailable("io"),
        sqlx::Error::Tls(_) => DbFailure::Unavailable("tls"),
        sqlx::Error::Protocol(_) => DbFailure::Unavailable("protocol"),
        sqlx::Error::WorkerCrashed => DbFailure::Unavailable("worker crashed"),
        _ => DbFailure::Database("driver".to_string()),
    }
}

fn classify_migrate_error(e: &sqlx::migrate::MigrateError) -> DbFailure {
    match e {
        sqlx::migrate::MigrateError::Execute(inner) => classify_db_error(inner),
        sqlx::migrate::MigrateError::ExecuteMigration(inner, _) => classify_db_error(inner),
        _ => DbFailure::Database("migration".to_string()),
    }
}

/// The lazy pool and whether the schema is known to be current.
pub struct HealthDb {
    pool: PgPool,
    migrated: tokio::sync::OnceCell<()>,
}

impl HealthDb {
    /// The production pool: verify-full TLS against the configured CA. LAZY: no connection
    /// is opened here, so a Postgres that is down at startup costs nothing until something
    /// asks, and each ask after it comes back simply connects.
    pub fn connect_lazy(s: &HealthDbSettings) -> HealthDb {
        HealthDb::from_options(s.connect_options())
    }

    /// The pool over any options. Every connection runs with `search_path=raw`, so sqlx's own
    /// `_sqlx_migrations` table lands in the schema the bridge owns rather than in `public`,
    /// and statement logging is off.
    pub fn from_options(options: PgConnectOptions) -> HealthDb {
        let options = options
            .application_name("jesse-bridge")
            .options([("search_path", "raw")])
            .disable_statement_logging();
        let pool = PgPoolOptions::new()
            .max_connections(HEALTHDB_MAX_CONNECTIONS)
            .min_connections(0)
            .acquire_timeout(HEALTHDB_ACQUIRE_TIMEOUT)
            .connect_lazy_with(options);
        HealthDb {
            pool,
            migrated: tokio::sync::OnceCell::new(),
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Bring the schema current, once per process. Concurrent callers share one attempt; a
    /// failed attempt leaves it to the next caller.
    pub async fn ensure_migrated(&self) -> Result<(), DbFailure> {
        self.migrated
            .get_or_try_init(|| async {
                HEALTHDB_MIGRATOR
                    .run(&self.pool)
                    .await
                    .map_err(|e| classify_migrate_error(&e))
            })
            .await
            .map(|_| ())
    }
}

/// At startup, in the background: bring the schema current, retrying while Postgres is
/// unreachable. The bridge serves meanwhile; the route says `unavailable` until it is.
pub fn spawn_healthdb_migrations(db: Arc<HealthDb>) {
    tokio::spawn(async move {
        loop {
            match db.ensure_migrated().await {
                Ok(()) => {
                    eprintln!("jesse-bridge: health archive schema is current");
                    return;
                }
                Err(e) => {
                    eprintln!(
                        "jesse-bridge: health archive {e}; retrying in {}s",
                        MIGRATE_RETRY.as_secs()
                    );
                    tokio::time::sleep(MIGRATE_RETRY).await;
                }
            }
        }
    });
}

// ---- The wire contract --------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestKind {
    Quantity,
    Category,
    Workout,
    Route,
}

impl IngestKind {
    pub fn as_str(self) -> &'static str {
        match self {
            IngestKind::Quantity => "quantity",
            IngestKind::Category => "category",
            IngestKind::Workout => "workout",
            IngestKind::Route => "route",
        }
    }

    /// The table a `deleted` UUID of this kind is removed from.
    fn table(self) -> &'static str {
        match self {
            IngestKind::Quantity => "raw.quantity_sample",
            IngestKind::Category => "raw.category_sample",
            IngestKind::Workout => "raw.workout",
            IngestKind::Route => "raw.route",
        }
    }
}

pub const WORKOUT_TYPE: &str = "HKWorkoutTypeIdentifier";
pub const ROUTE_TYPE: &str = "HKWorkoutRouteTypeIdentifier";
const EVENT_TYPES: [&str; 6] = ["lap", "segment", "pause", "resume", "marker", "other"];

#[derive(Debug, Clone, PartialEq)]
pub struct QuantityRow {
    pub uuid: Uuid,
    pub seq: i32,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub value: f64,
    pub unit: String,
    pub source: String,
    pub source_name: String,
    pub device: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CategoryRow {
    pub uuid: Uuid,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub value: i32,
    pub source: String,
    pub source_name: String,
    pub device: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkoutEvent {
    pub kind: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkoutRow {
    pub uuid: Uuid,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub duration_s: f64,
    pub activity_type: i32,
    pub activity_name: String,
    pub distance_m: Option<f64>,
    pub energy_kcal: Option<f64>,
    pub avg_hr: Option<f64>,
    pub max_hr: Option<f64>,
    pub effort: Option<f64>,
    pub indoor: Option<bool>,
    pub source: String,
    pub source_name: String,
    pub device: Option<String>,
    pub metadata: Option<Value>,
    pub events: Vec<WorkoutEvent>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoutePoint {
    pub t: DateTime<Utc>,
    pub lat: f64,
    pub lon: f64,
    pub alt: Option<f64>,
    pub h_acc: Option<f64>,
    pub v_acc: Option<f64>,
    pub speed: Option<f64>,
    pub course: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteRow {
    pub uuid: Uuid,
    pub workout_uuid: Uuid,
    pub source: String,
    pub source_name: String,
    pub downsampled: bool,
    pub points: Vec<RoutePoint>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IngestSamples {
    Quantity(Vec<QuantityRow>),
    Category(Vec<CategoryRow>),
    Workout(Vec<WorkoutRow>),
    Route(Option<RouteRow>),
}

impl IngestSamples {
    pub fn len(&self) -> usize {
        match self {
            IngestSamples::Quantity(v) => v.len(),
            IngestSamples::Category(v) => v.len(),
            IngestSamples::Workout(v) => v.len(),
            IngestSamples::Route(r) => usize::from(r.is_some()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One validated request.
#[derive(Debug, Clone, PartialEq)]
pub struct IngestBatch {
    pub kind: IngestKind,
    pub hk_type: String,
    pub samples: IngestSamples,
    pub deleted: Vec<Uuid>,
}

/// A refused request: the status, a fixed reason word for the error counter, and a message
/// that names a sample index and a field and never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestRefusal {
    pub status: StatusCode,
    pub reason: &'static str,
    pub message: String,
}

fn bad(message: impl Into<String>) -> IngestRefusal {
    IngestRefusal {
        status: StatusCode::BAD_REQUEST,
        reason: "bad_request",
        message: message.into(),
    }
}

fn too_large(message: impl Into<String>) -> IngestRefusal {
    IngestRefusal {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        reason: "too_large",
        message: message.into(),
    }
}

type Obj = serde_json::Map<String, Value>;

fn as_obj<'a>(v: &'a Value, ctx: &str) -> Result<&'a Obj, IngestRefusal> {
    v.as_object()
        .ok_or_else(|| bad(format!("{ctx} is not an object")))
}

/// Present and not null.
fn field<'a>(o: &'a Obj, key: &str) -> Option<&'a Value> {
    o.get(key).filter(|v| !v.is_null())
}

fn req_str(o: &Obj, key: &str, ctx: &str) -> Result<String, IngestRefusal> {
    match field(o, key) {
        None => Err(bad(format!("{ctx}: `{key}` is missing"))),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(bad(format!("{ctx}: `{key}` is not a string"))),
    }
}

fn opt_str(o: &Obj, key: &str, ctx: &str) -> Result<Option<String>, IngestRefusal> {
    match field(o, key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(bad(format!("{ctx}: `{key}` is not a string"))),
    }
}

fn req_uuid(o: &Obj, key: &str, ctx: &str) -> Result<Uuid, IngestRefusal> {
    let s = req_str(o, key, ctx)?;
    Uuid::parse_str(&s).map_err(|_| bad(format!("{ctx}: `{key}` is not a UUID")))
}

fn req_ts(o: &Obj, key: &str, ctx: &str) -> Result<DateTime<Utc>, IngestRefusal> {
    let s = req_str(o, key, ctx)?;
    DateTime::parse_from_rfc3339(&s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| {
            bad(format!(
                "{ctx}: `{key}` is not an RFC 3339 timestamp with offset"
            ))
        })
}

fn opt_f64(o: &Obj, key: &str, ctx: &str) -> Result<Option<f64>, IngestRefusal> {
    match field(o, key) {
        None => Ok(None),
        Some(v) => match v.as_f64() {
            Some(n) if n.is_finite() => Ok(Some(n)),
            Some(_) => Err(bad(format!("{ctx}: `{key}` is not finite"))),
            None => Err(bad(format!("{ctx}: `{key}` is not a number"))),
        },
    }
}

fn req_f64(o: &Obj, key: &str, ctx: &str) -> Result<f64, IngestRefusal> {
    opt_f64(o, key, ctx)?.ok_or_else(|| bad(format!("{ctx}: `{key}` is missing")))
}

fn req_i32(o: &Obj, key: &str, ctx: &str) -> Result<i32, IngestRefusal> {
    match field(o, key) {
        None => Err(bad(format!("{ctx}: `{key}` is missing"))),
        Some(v) => v
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| bad(format!("{ctx}: `{key}` is not a 32-bit integer"))),
    }
}

fn opt_bool(o: &Obj, key: &str, ctx: &str) -> Result<Option<bool>, IngestRefusal> {
    match field(o, key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(bad(format!("{ctx}: `{key}` is not a boolean"))),
    }
}

fn metadata(o: &Obj, ctx: &str) -> Result<Option<Value>, IngestRefusal> {
    match field(o, "metadata") {
        None => Ok(None),
        Some(v @ Value::Object(_)) => Ok(Some(v.clone())),
        Some(_) => Err(bad(format!("{ctx}: `metadata` is not an object"))),
    }
}

fn ordered(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    ctx: &str,
) -> Result<(DateTime<Utc>, DateTime<Utc>), IngestRefusal> {
    if start > end {
        return Err(bad(format!("{ctx}: `start` is after `end`")));
    }
    Ok((start, end))
}

/// Keep the LAST of any rows sharing a key, in first-seen order. One `INSERT … ON CONFLICT DO
/// UPDATE` cannot touch the same row twice, and the later copy is the later reading.
fn dedup_last<T, K: std::hash::Hash + Eq>(rows: Vec<T>, key: impl Fn(&T) -> K) -> Vec<T> {
    let mut at: HashMap<K, usize> = HashMap::new();
    let mut out: Vec<T> = Vec::with_capacity(rows.len());
    for r in rows {
        match at.get(&key(&r)) {
            Some(&i) => out[i] = r,
            None => {
                at.insert(key(&r), out.len());
                out.push(r);
            }
        }
    }
    out
}

fn parse_quantity(i: usize, v: &Value) -> Result<QuantityRow, IngestRefusal> {
    let ctx = format!("sample {i}");
    let o = as_obj(v, &ctx)?;
    let (start, end) = ordered(req_ts(o, "start", &ctx)?, req_ts(o, "end", &ctx)?, &ctx)?;
    let seq = req_i32(o, "seq", &ctx)?;
    if seq < 0 {
        return Err(bad(format!("{ctx}: `seq` is negative")));
    }
    Ok(QuantityRow {
        uuid: req_uuid(o, "uuid", &ctx)?,
        seq,
        start,
        end,
        value: req_f64(o, "value", &ctx)?,
        unit: req_str(o, "unit", &ctx)?,
        source: req_str(o, "source", &ctx)?,
        source_name: req_str(o, "source_name", &ctx)?,
        device: opt_str(o, "device", &ctx)?,
        metadata: metadata(o, &ctx)?,
    })
}

fn parse_category(i: usize, v: &Value) -> Result<CategoryRow, IngestRefusal> {
    let ctx = format!("sample {i}");
    let o = as_obj(v, &ctx)?;
    let (start, end) = ordered(req_ts(o, "start", &ctx)?, req_ts(o, "end", &ctx)?, &ctx)?;
    Ok(CategoryRow {
        uuid: req_uuid(o, "uuid", &ctx)?,
        start,
        end,
        value: req_i32(o, "value", &ctx)?,
        source: req_str(o, "source", &ctx)?,
        source_name: req_str(o, "source_name", &ctx)?,
        device: opt_str(o, "device", &ctx)?,
        metadata: metadata(o, &ctx)?,
    })
}

fn parse_workout(i: usize, v: &Value) -> Result<WorkoutRow, IngestRefusal> {
    let ctx = format!("sample {i}");
    let o = as_obj(v, &ctx)?;
    let (start, end) = ordered(req_ts(o, "start", &ctx)?, req_ts(o, "end", &ctx)?, &ctx)?;
    let events = match field(o, "events") {
        None => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .enumerate()
            .map(|(j, e)| {
                let ectx = format!("{ctx} event {j}");
                let eo = as_obj(e, &ectx)?;
                let kind = req_str(eo, "type", &ectx)?;
                if !EVENT_TYPES.contains(&kind.as_str()) {
                    return Err(bad(format!(
                        "{ectx}: `type` is not one of {}",
                        EVENT_TYPES.join(", ")
                    )));
                }
                let (start, end) = ordered(
                    req_ts(eo, "start", &ectx)?,
                    req_ts(eo, "end", &ectx)?,
                    &ectx,
                )?;
                Ok(WorkoutEvent {
                    kind,
                    start,
                    end,
                    metadata: metadata(eo, &ectx)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(bad(format!("{ctx}: `events` is not an array"))),
    };
    Ok(WorkoutRow {
        uuid: req_uuid(o, "uuid", &ctx)?,
        start,
        end,
        duration_s: req_f64(o, "duration_s", &ctx)?,
        activity_type: req_i32(o, "activity_type", &ctx)?,
        activity_name: req_str(o, "activity_name", &ctx)?,
        distance_m: opt_f64(o, "distance_m", &ctx)?,
        energy_kcal: opt_f64(o, "energy_kcal", &ctx)?,
        avg_hr: opt_f64(o, "avg_hr", &ctx)?,
        max_hr: opt_f64(o, "max_hr", &ctx)?,
        effort: opt_f64(o, "effort", &ctx)?,
        indoor: opt_bool(o, "indoor", &ctx)?,
        source: req_str(o, "source", &ctx)?,
        source_name: req_str(o, "source_name", &ctx)?,
        device: opt_str(o, "device", &ctx)?,
        metadata: metadata(o, &ctx)?,
        events,
    })
}

fn parse_route(v: &Value) -> Result<RouteRow, IngestRefusal> {
    let ctx = "sample 0";
    let o = as_obj(v, ctx)?;
    let points = match field(o, "points") {
        Some(Value::Array(a)) => a,
        None => return Err(bad(format!("{ctx}: `points` is missing"))),
        Some(_) => return Err(bad(format!("{ctx}: `points` is not an array"))),
    };
    if points.len() > ROUTE_MAX_POINTS {
        return Err(too_large(format!(
            "{} points; a route carries at most {ROUTE_MAX_POINTS}",
            points.len()
        )));
    }
    let points = points
        .iter()
        .enumerate()
        .map(|(j, p)| {
            let pctx = format!("point {j}");
            let po = as_obj(p, &pctx)?;
            let lat = req_f64(po, "lat", &pctx)?;
            let lon = req_f64(po, "lon", &pctx)?;
            if !(-90.0..=90.0).contains(&lat) {
                return Err(bad(format!("{pctx}: `lat` is outside -90..90")));
            }
            if !(-180.0..=180.0).contains(&lon) {
                return Err(bad(format!("{pctx}: `lon` is outside -180..180")));
            }
            Ok(RoutePoint {
                t: req_ts(po, "t", &pctx)?,
                lat,
                lon,
                alt: opt_f64(po, "alt", &pctx)?,
                h_acc: opt_f64(po, "h_acc", &pctx)?,
                v_acc: opt_f64(po, "v_acc", &pctx)?,
                speed: opt_f64(po, "speed", &pctx)?,
                course: opt_f64(po, "course", &pctx)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let downsampled = match field(o, "downsampled") {
        Some(Value::Bool(b)) => *b,
        None => return Err(bad(format!("{ctx}: `downsampled` is missing"))),
        Some(_) => return Err(bad(format!("{ctx}: `downsampled` is not a boolean"))),
    };
    Ok(RouteRow {
        uuid: req_uuid(o, "uuid", ctx)?,
        workout_uuid: req_uuid(o, "workout_uuid", ctx)?,
        source: req_str(o, "source", ctx)?,
        source_name: req_str(o, "source_name", ctx)?,
        downsampled,
        points,
    })
}

/// Parse and validate one request body against the contract. Everything is checked before
/// anything is written, and any failure refuses the whole batch.
pub fn parse_ingest(body: &[u8]) -> Result<IngestBatch, IngestRefusal> {
    if body.len() > INGEST_MAX_ROUTE_BODY {
        return Err(too_large(format!(
            "the body exceeds {INGEST_MAX_ROUTE_BODY} bytes"
        )));
    }
    // serde_json's message names a line and column, never the text it choked on.
    let root: Value =
        serde_json::from_slice(body).map_err(|e| bad(format!("the body is not JSON: {e}")))?;
    let o = as_obj(&root, "the body")?;
    match field(o, "schema") {
        Some(v) if v.as_i64() == Some(1) => {}
        Some(_) => return Err(bad("`schema` is not 1")),
        None => return Err(bad("`schema` is missing")),
    }
    let kind = match req_str(o, "kind", "the body")?.as_str() {
        "quantity" => IngestKind::Quantity,
        "category" => IngestKind::Category,
        "workout" => IngestKind::Workout,
        "route" => IngestKind::Route,
        _ => return Err(bad("`kind` is not quantity, category, workout or route")),
    };
    if kind != IngestKind::Route && body.len() > INGEST_MAX_BODY {
        return Err(too_large(format!(
            "the body exceeds {INGEST_MAX_BODY} bytes; only a route may be larger"
        )));
    }
    let hk_type = req_str(o, "type", "the body")?;
    if hk_type.trim().is_empty() {
        return Err(bad("`type` is empty"));
    }
    let expected = match kind {
        IngestKind::Workout => Some(WORKOUT_TYPE),
        IngestKind::Route => Some(ROUTE_TYPE),
        _ => None,
    };
    if let Some(t) = expected {
        if hk_type != t {
            return Err(bad(format!("a {} batch has `type` {t}", kind.as_str())));
        }
    }
    let samples = match field(o, "samples") {
        Some(Value::Array(a)) => a,
        None => return Err(bad("`samples` is missing")),
        Some(_) => return Err(bad("`samples` is not an array")),
    };
    if kind == IngestKind::Route {
        if samples.len() > 1 {
            return Err(bad("a route request carries exactly one route"));
        }
    } else if samples.len() > INGEST_MAX_SAMPLES {
        return Err(too_large(format!(
            "{} samples; a request carries at most {INGEST_MAX_SAMPLES}",
            samples.len()
        )));
    }
    let deleted = match field(o, "deleted") {
        None => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .enumerate()
            .map(|(i, v)| {
                v.as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .ok_or_else(|| bad(format!("deleted {i} is not a UUID")))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(bad("`deleted` is not an array")),
    };
    let deleted = dedup_last(deleted, |u| *u);
    let samples = match kind {
        IngestKind::Quantity => IngestSamples::Quantity(dedup_last(
            samples
                .iter()
                .enumerate()
                .map(|(i, v)| parse_quantity(i, v))
                .collect::<Result<Vec<_>, _>>()?,
            |r| (r.uuid, r.seq),
        )),
        IngestKind::Category => IngestSamples::Category(dedup_last(
            samples
                .iter()
                .enumerate()
                .map(|(i, v)| parse_category(i, v))
                .collect::<Result<Vec<_>, _>>()?,
            |r| r.uuid,
        )),
        IngestKind::Workout => IngestSamples::Workout(dedup_last(
            samples
                .iter()
                .enumerate()
                .map(|(i, v)| parse_workout(i, v))
                .collect::<Result<Vec<_>, _>>()?,
            |r| r.uuid,
        )),
        IngestKind::Route => IngestSamples::Route(samples.first().map(parse_route).transpose()?),
    };
    Ok(IngestBatch {
        kind,
        hk_type,
        samples,
        deleted,
    })
}

// ---- Writing a batch ----------------------------------------------------------------------

/// What a committed batch did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestOutcome {
    /// Rows upserted: samples, workouts, or the route.
    pub upserted: u64,
    /// UUIDs the batch reported deleted.
    pub deleted: u64,
}

fn rows_per_chunk(columns: usize) -> usize {
    (MAX_BINDS / columns).max(1)
}

type Tx<'c> = sqlx::Transaction<'c, Postgres>;

async fn upsert_quantity(tx: &mut Tx<'_>, hk_type: &str, rows: &[QuantityRow]) -> sqlx::Result<()> {
    for chunk in rows.chunks(rows_per_chunk(11)) {
        let mut q = QueryBuilder::<Postgres>::new(
            "INSERT INTO raw.quantity_sample (uuid, seq, type, start_ts, end_ts, value, unit, \
             source, source_name, device, metadata) ",
        );
        q.push_values(chunk, |mut b, r| {
            b.push_bind(r.uuid)
                .push_bind(r.seq)
                .push_bind(hk_type)
                .push_bind(r.start)
                .push_bind(r.end)
                .push_bind(r.value)
                .push_bind(&r.unit)
                .push_bind(&r.source)
                .push_bind(&r.source_name)
                .push_bind(&r.device)
                .push_bind(&r.metadata);
        });
        q.push(
            " ON CONFLICT (uuid, seq) DO UPDATE SET type = EXCLUDED.type, \
             start_ts = EXCLUDED.start_ts, end_ts = EXCLUDED.end_ts, value = EXCLUDED.value, \
             unit = EXCLUDED.unit, source = EXCLUDED.source, \
             source_name = EXCLUDED.source_name, device = EXCLUDED.device, \
             metadata = EXCLUDED.metadata, received_at = now()",
        );
        q.build().execute(&mut **tx).await?;
    }
    Ok(())
}

async fn upsert_category(tx: &mut Tx<'_>, hk_type: &str, rows: &[CategoryRow]) -> sqlx::Result<()> {
    for chunk in rows.chunks(rows_per_chunk(9)) {
        let mut q = QueryBuilder::<Postgres>::new(
            "INSERT INTO raw.category_sample (uuid, type, start_ts, end_ts, value, source, \
             source_name, device, metadata) ",
        );
        q.push_values(chunk, |mut b, r| {
            b.push_bind(r.uuid)
                .push_bind(hk_type)
                .push_bind(r.start)
                .push_bind(r.end)
                .push_bind(r.value)
                .push_bind(&r.source)
                .push_bind(&r.source_name)
                .push_bind(&r.device)
                .push_bind(&r.metadata);
        });
        q.push(
            " ON CONFLICT (uuid) DO UPDATE SET type = EXCLUDED.type, \
             start_ts = EXCLUDED.start_ts, end_ts = EXCLUDED.end_ts, value = EXCLUDED.value, \
             source = EXCLUDED.source, source_name = EXCLUDED.source_name, \
             device = EXCLUDED.device, metadata = EXCLUDED.metadata, received_at = now()",
        );
        q.build().execute(&mut **tx).await?;
    }
    Ok(())
}

/// Upsert the workouts, then REPLACE their events: a resend carries the workout's whole event
/// list, so whatever was stored for it before is deleted first.
async fn upsert_workouts(tx: &mut Tx<'_>, rows: &[WorkoutRow]) -> sqlx::Result<()> {
    for chunk in rows.chunks(rows_per_chunk(16)) {
        let mut q = QueryBuilder::<Postgres>::new(
            "INSERT INTO raw.workout (uuid, start_ts, end_ts, duration_s, activity_type, \
             activity_name, distance_m, energy_kcal, avg_hr, max_hr, effort, indoor, source, \
             source_name, device, metadata) ",
        );
        q.push_values(chunk, |mut b, r| {
            b.push_bind(r.uuid)
                .push_bind(r.start)
                .push_bind(r.end)
                .push_bind(r.duration_s)
                .push_bind(r.activity_type)
                .push_bind(&r.activity_name)
                .push_bind(r.distance_m)
                .push_bind(r.energy_kcal)
                .push_bind(r.avg_hr)
                .push_bind(r.max_hr)
                .push_bind(r.effort)
                .push_bind(r.indoor)
                .push_bind(&r.source)
                .push_bind(&r.source_name)
                .push_bind(&r.device)
                .push_bind(&r.metadata);
        });
        q.push(
            " ON CONFLICT (uuid) DO UPDATE SET start_ts = EXCLUDED.start_ts, \
             end_ts = EXCLUDED.end_ts, duration_s = EXCLUDED.duration_s, \
             activity_type = EXCLUDED.activity_type, activity_name = EXCLUDED.activity_name, \
             distance_m = EXCLUDED.distance_m, energy_kcal = EXCLUDED.energy_kcal, \
             avg_hr = EXCLUDED.avg_hr, max_hr = EXCLUDED.max_hr, effort = EXCLUDED.effort, \
             indoor = EXCLUDED.indoor, source = EXCLUDED.source, \
             source_name = EXCLUDED.source_name, device = EXCLUDED.device, \
             metadata = EXCLUDED.metadata, received_at = now()",
        );
        q.build().execute(&mut **tx).await?;
    }
    let uuids: Vec<Uuid> = rows.iter().map(|r| r.uuid).collect();
    sqlx::query("DELETE FROM raw.workout_event WHERE workout_uuid = ANY($1)")
        .bind(&uuids)
        .execute(&mut **tx)
        .await?;
    let events: Vec<(Uuid, i32, &WorkoutEvent)> = rows
        .iter()
        .flat_map(|w| {
            w.events
                .iter()
                .enumerate()
                .map(move |(i, e)| (w.uuid, i as i32, e))
        })
        .collect();
    for chunk in events.chunks(rows_per_chunk(6)) {
        let mut q = QueryBuilder::<Postgres>::new(
            "INSERT INTO raw.workout_event (workout_uuid, idx, type, start_ts, end_ts, metadata) ",
        );
        q.push_values(chunk, |mut b, (w, i, e)| {
            b.push_bind(*w)
                .push_bind(*i)
                .push_bind(&e.kind)
                .push_bind(e.start)
                .push_bind(e.end)
                .push_bind(&e.metadata);
        });
        q.build().execute(&mut **tx).await?;
    }
    Ok(())
}

/// One point as a `COPY … (FORMAT csv)` line. Every field is a number, a UUID or an RFC 3339
/// timestamp, none of which can hold a comma or a quote; an empty field is NULL.
fn route_point_csv(out: &mut String, route: Uuid, idx: usize, p: &RoutePoint) {
    use std::fmt::Write as _;
    let opt = |v: Option<f64>| v.map(|n| n.to_string()).unwrap_or_default();
    let _ = writeln!(
        out,
        "{route},{idx},{},{},{},{},{},{},{},{}",
        p.t.to_rfc3339_opts(SecondsFormat::AutoSi, true),
        p.lat,
        p.lon,
        opt(p.alt),
        opt(p.h_acc),
        opt(p.v_acc),
        opt(p.speed),
        opt(p.course),
    );
}

/// Upsert the route and REPLACE all of its points, streamed with `COPY` so a 100000-point
/// route is one round of bulk load rather than a hundred statements.
async fn upsert_route(tx: &mut Tx<'_>, r: &RouteRow) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO raw.route (uuid, workout_uuid, source, source_name, n_points, downsampled) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (uuid) DO UPDATE SET workout_uuid = EXCLUDED.workout_uuid, \
         source = EXCLUDED.source, source_name = EXCLUDED.source_name, \
         n_points = EXCLUDED.n_points, downsampled = EXCLUDED.downsampled, received_at = now()",
    )
    .bind(r.uuid)
    .bind(r.workout_uuid)
    .bind(&r.source)
    .bind(&r.source_name)
    .bind(r.points.len() as i32)
    .bind(r.downsampled)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM raw.route_point WHERE route_uuid = $1")
        .bind(r.uuid)
        .execute(&mut **tx)
        .await?;
    if r.points.is_empty() {
        return Ok(());
    }
    let mut copy = tx
        .copy_in_raw(
            "COPY raw.route_point (route_uuid, idx, ts, lat, lon, alt, h_acc, v_acc, speed, \
             course) FROM STDIN WITH (FORMAT csv)",
        )
        .await?;
    let mut buf = String::with_capacity(1 << 20);
    for (i, p) in r.points.iter().enumerate() {
        route_point_csv(&mut buf, r.uuid, i, p);
        if buf.len() >= 1 << 20 {
            copy.send(buf.as_bytes()).await?;
            buf.clear();
        }
    }
    if !buf.is_empty() {
        copy.send(buf.as_bytes()).await?;
    }
    copy.finish().await?;
    Ok(())
}

/// Delete every row of `kind` under the given UUIDs (every `seq` of a quantity series, a
/// workout's events and a route's points by cascade) and record a tombstone for each.
async fn apply_deletes(
    tx: &mut Tx<'_>,
    kind: IngestKind,
    hk_type: &str,
    uuids: &[Uuid],
) -> sqlx::Result<()> {
    if uuids.is_empty() {
        return Ok(());
    }
    let sql = format!("DELETE FROM {} WHERE uuid = ANY($1)", kind.table());
    sqlx::query(&sql).bind(uuids).execute(&mut **tx).await?;
    sqlx::query(
        "INSERT INTO raw.deleted_sample (uuid, type) SELECT u, $2 FROM unnest($1::uuid[]) AS u \
         ON CONFLICT (uuid) DO NOTHING",
    )
    .bind(uuids)
    .bind(hk_type)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Write one validated batch in ONE transaction. `Ok` means it committed.
pub async fn write_batch(db: &HealthDb, batch: &IngestBatch) -> Result<IngestOutcome, DbFailure> {
    async fn run(db: &HealthDb, batch: &IngestBatch) -> sqlx::Result<()> {
        let mut tx = db.pool().begin().await?;
        match &batch.samples {
            IngestSamples::Quantity(rows) => upsert_quantity(&mut tx, &batch.hk_type, rows).await?,
            IngestSamples::Category(rows) => upsert_category(&mut tx, &batch.hk_type, rows).await?,
            IngestSamples::Workout(rows) => upsert_workouts(&mut tx, rows).await?,
            IngestSamples::Route(Some(r)) => upsert_route(&mut tx, r).await?,
            IngestSamples::Route(None) => {}
        }
        apply_deletes(&mut tx, batch.kind, &batch.hk_type, &batch.deleted).await?;
        tx.commit().await
    }
    run(db, batch).await.map_err(|e| classify_db_error(&e))?;
    Ok(IngestOutcome {
        upserted: batch.samples.len() as u64,
        deleted: batch.deleted.len() as u64,
    })
}

// ---- The route ----------------------------------------------------------------------------

fn ingest_error(status: StatusCode, reason: &'static str, message: &str) -> Response {
    healthdb_count(METRIC_INGEST_ERRORS, reason, 1);
    (status, Json(json!({ "error": message }))).into_response()
}

/// `POST /jesse/health/ingest`. Bearer auth, then `503 not_configured` without `[healthdb]`,
/// then the contract, then one transaction. The body is read here, under the route's own
/// 16 MiB cap, rather than by an extractor, so auth and configuration are answered first.
pub async fn jesse_health_ingest(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    if let Err(e) = check_auth(&headers, &st.cfg.token) {
        healthdb_count(METRIC_INGEST_ERRORS, "unauthorized", 1);
        return e.into_response();
    }
    let Some(db) = st.healthdb.clone() else {
        return ingest_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "not_configured",
            "not_configured",
        );
    };
    let bytes = match axum::body::to_bytes(body, INGEST_MAX_ROUTE_BODY).await {
        Ok(b) => b,
        Err(_) => {
            return ingest_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                &format!("the body exceeds {INGEST_MAX_ROUTE_BODY} bytes or could not be read"),
            )
        }
    };
    let batch = match parse_ingest(&bytes) {
        Ok(b) => b,
        Err(r) => {
            eprintln!("healthdb: refused a batch ({})", r.reason);
            return ingest_error(r.status, r.reason, &r.message);
        }
    };
    let result = match db.ensure_migrated().await {
        Ok(()) => write_batch(&db, &batch).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(out) => {
            healthdb_count(METRIC_INGEST_SAMPLES, batch.kind.as_str(), out.upserted);
            eprintln!(
                "healthdb: {} {} upserted={} deleted={}",
                batch.kind.as_str(),
                batch.hk_type,
                out.upserted,
                out.deleted
            );
            (
                StatusCode::OK,
                Json(json!({ "upserted": out.upserted, "deleted": out.deleted })),
            )
                .into_response()
        }
        Err(DbFailure::Unavailable(why)) => {
            eprintln!(
                "healthdb: {} {} not written: unavailable ({why})",
                batch.kind.as_str(),
                batch.hk_type
            );
            ingest_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "unavailable",
            )
        }
        Err(DbFailure::Database(code)) => {
            eprintln!(
                "healthdb: {} {} not written: SQLSTATE {code}",
                batch.kind.as_str(),
                batch.hk_type
            );
            ingest_error(StatusCode::INTERNAL_SERVER_ERROR, "database", "database")
        }
    }
}

// ---- The diet mirror ----------------------------------------------------------------------

/// The diet logs directory, resolved exactly as `POST /jesse/diet/vitals` resolves
/// `vitals-log.csv`: `<vault>/diet-logs`.
pub fn diet_logs_dir(cfg: &Config) -> PathBuf {
    Path::new(&cfg.vault).join("diet-logs")
}

/// One parsed diet row: its 1-based data row number, the `Date` column if it is a date, and
/// every cell keyed by its header.
#[derive(Debug, Clone, PartialEq)]
pub struct DietRow {
    pub row_no: i32,
    pub date: Option<NaiveDate>,
    pub data: Value,
}

/// Parse a diet CSV with a real RFC 4180 reader (quoted fields hold commas and newlines).
/// `Err` carries the file line the reader failed on, and nothing of the content.
pub fn parse_diet_csv(content: &[u8]) -> Result<Vec<DietRow>, u64> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_reader(content);
    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| e.position().map(|p| p.line()).unwrap_or(1))?
        .iter()
        .map(|h| h.trim().to_string())
        .collect();
    let date_col = headers.iter().position(|h| h == "Date");
    let mut out = Vec::new();
    for (i, rec) in reader.records().enumerate() {
        let rec = rec.map_err(|e| e.position().map(|p| p.line()).unwrap_or(i as u64 + 2))?;
        let mut data = serde_json::Map::new();
        for (h, cell) in headers.iter().zip(rec.iter()) {
            data.insert(h.clone(), Value::String(cell.to_string()));
        }
        let date = date_col
            .and_then(|c| rec.get(c))
            .and_then(|d| NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d").ok());
        out.push(DietRow {
            row_no: i as i32 + 1,
            date,
            data: Value::Object(data),
        });
    }
    Ok(out)
}

/// What one mirror pass did with one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DietMirrorOutcome {
    /// Rows replaced with this many.
    Reloaded(usize),
    /// Same sha256 as the mirror: nothing touched.
    Unchanged,
    /// Not on disk: nothing touched.
    Missing,
    /// Did not parse at this file line: the previous mirror is kept.
    ParseFailed(u64),
    /// The file could not be read: the previous mirror is kept.
    Unreadable,
    /// The database step failed: the previous mirror is kept (the transaction rolled back).
    Failed(DbFailure),
}

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn reload_diet_file(
    db: &HealthDb,
    file: &str,
    sha: &str,
    rows: &[DietRow],
) -> sqlx::Result<()> {
    let mut tx = db.pool().begin().await?;
    sqlx::query("DELETE FROM raw.diet_row WHERE file = $1")
        .bind(file)
        .execute(&mut *tx)
        .await?;
    for chunk in rows.chunks(rows_per_chunk(4)) {
        let mut q =
            QueryBuilder::<Postgres>::new("INSERT INTO raw.diet_row (file, row_no, date, data) ");
        q.push_values(chunk, |mut b, r| {
            b.push_bind(file)
                .push_bind(r.row_no)
                .push_bind(r.date)
                .push_bind(&r.data);
        });
        q.build().execute(&mut *tx).await?;
    }
    sqlx::query(
        "INSERT INTO raw.diet_file (file, sha256, loaded_at) VALUES ($1, $2, now()) \
         ON CONFLICT (file) DO UPDATE SET sha256 = EXCLUDED.sha256, loaded_at = now()",
    )
    .bind(file)
    .bind(sha)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// One mirror pass over `files` in `dir`. Each changed file is reloaded in its own
/// transaction, so one bad file never holds back another.
pub async fn mirror_diet_once(
    db: &HealthDb,
    dir: &Path,
    files: &[&str],
) -> Vec<(String, DietMirrorOutcome)> {
    let mut out = Vec::with_capacity(files.len());
    for &file in files {
        let outcome = mirror_one(db, dir, file).await;
        match &outcome {
            DietMirrorOutcome::Reloaded(n) => {
                healthdb_count(METRIC_DIET_RELOAD, file, 1);
                eprintln!("healthdb: diet mirror reloaded {file} ({n} rows)");
            }
            DietMirrorOutcome::ParseFailed(line) => eprintln!(
                "healthdb: diet mirror skipped {file}: line {line} does not parse; the previous \
                 mirror is kept"
            ),
            DietMirrorOutcome::Unreadable => {
                eprintln!("healthdb: diet mirror skipped {file}: it cannot be read")
            }
            DietMirrorOutcome::Failed(e) => {
                eprintln!("healthdb: diet mirror did not reload {file}: {e}")
            }
            DietMirrorOutcome::Unchanged | DietMirrorOutcome::Missing => {}
        }
        out.push((file.to_string(), outcome));
    }
    out
}

async fn mirror_one(db: &HealthDb, dir: &Path, file: &str) -> DietMirrorOutcome {
    let bytes = match std::fs::read(dir.join(file)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return DietMirrorOutcome::Missing,
        Err(_) => return DietMirrorOutcome::Unreadable,
    };
    let sha = sha256_hex(&bytes);
    let stored: Option<String> =
        match sqlx::query_scalar("SELECT sha256 FROM raw.diet_file WHERE file = $1")
            .bind(file)
            .fetch_optional(db.pool())
            .await
        {
            Ok(s) => s,
            Err(e) => return DietMirrorOutcome::Failed(classify_db_error(&e)),
        };
    if stored.as_deref() == Some(sha.as_str()) {
        return DietMirrorOutcome::Unchanged;
    }
    let rows = match parse_diet_csv(&bytes) {
        Ok(r) => r,
        Err(line) => return DietMirrorOutcome::ParseFailed(line),
    };
    match reload_diet_file(db, file, &sha, &rows).await {
        Ok(()) => DietMirrorOutcome::Reloaded(rows.len()),
        Err(e) => DietMirrorOutcome::Failed(classify_db_error(&e)),
    }
}

/// The mirror task: a pass at startup and every [`DIET_MIRROR_INTERVAL`] after. A pass that
/// finds Postgres unreachable says so and waits for the next.
pub fn spawn_diet_mirror(db: Arc<HealthDb>, dir: PathBuf) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(DIET_MIRROR_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            match db.ensure_migrated().await {
                Ok(()) => {
                    mirror_diet_once(&db, &dir, &DIET_MIRROR_FILES).await;
                }
                Err(e) => eprintln!("healthdb: diet mirror pass skipped: {e}"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    const U1: &str = "6f1c2c1e-6a43-4c39-9a51-2b7b0e5d6c11";
    const U2: &str = "0b7c8f0a-3d1d-4f6c-8f7e-6c1e0e6a5b22";

    fn quantity(samples: Value) -> Value {
        json!({
            "schema": 1, "kind": "quantity", "type": "HKQuantityTypeIdentifierHeartRate",
            "samples": samples,
        })
    }

    fn hr(uuid: &str, seq: i64, value: Value) -> Value {
        json!({
            "uuid": uuid, "seq": seq,
            "start": "2026-10-09T08:59:00+02:00", "end": "2026-10-09T08:59:05+02:00",
            "value": value, "unit": "count/min", "source": "com.apple.health.x",
            "source_name": "Watch", "device": "Watch8,1",
            "metadata": {"HKMetadataKeyHeartRateMotionContext": 2},
        })
    }

    fn parse(v: &Value) -> Result<IngestBatch, IngestRefusal> {
        parse_ingest(serde_json::to_vec(v).unwrap().as_slice())
    }

    fn refused(v: &Value) -> IngestRefusal {
        parse(v).expect_err("should be refused")
    }

    #[test]
    fn a_well_formed_quantity_batch_parses() {
        let b = parse(&quantity(json!([
            hr(U1, 0, json!(142.0)),
            hr(U2, 0, json!(60))
        ])))
        .unwrap();
        assert_eq!(b.kind, IngestKind::Quantity);
        assert_eq!(b.samples.len(), 2);
        let IngestSamples::Quantity(rows) = &b.samples else {
            panic!()
        };
        assert_eq!(rows[0].start.to_rfc3339(), "2026-10-09T06:59:00+00:00");
    }

    #[test]
    fn an_implausible_value_is_stored_not_refused() {
        assert!(parse(&quantity(json!([hr(U1, 0, json!(250.0))]))).is_ok());
    }

    #[test]
    fn bad_json_is_a_400() {
        let r = parse_ingest(b"{\"schema\": 1, ").unwrap_err();
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn an_unknown_kind_is_a_400() {
        let mut v = quantity(json!([]));
        v["kind"] = json!("ecg");
        assert_eq!(refused(&v).status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_missing_required_field_is_a_400_naming_it() {
        for key in [
            "uuid",
            "seq",
            "start",
            "end",
            "value",
            "unit",
            "source",
            "source_name",
        ] {
            let mut s = hr(U1, 0, json!(1.0));
            s.as_object_mut().unwrap().remove(key);
            let r = refused(&quantity(json!([s])));
            assert_eq!(r.status, StatusCode::BAD_REQUEST, "{key}");
            assert!(r.message.contains(key), "{key}: {}", r.message);
        }
        let mut v = quantity(json!([]));
        v.as_object_mut().unwrap().remove("samples");
        assert_eq!(refused(&v).status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn optional_fields_may_be_absent_or_null() {
        let mut s = hr(U1, 0, json!(1.0));
        s["device"] = Value::Null;
        s.as_object_mut().unwrap().remove("metadata");
        assert!(parse(&quantity(json!([s]))).is_ok());
    }

    #[test]
    fn a_non_finite_number_is_a_400() {
        // JSON has no NaN; a number too large for f64 is what a non-finite value looks like
        // on the wire, and serde_json refuses it as malformed JSON.
        let body = serde_json::to_string(&quantity(json!([hr(U1, 0, json!(1.0))])))
            .unwrap()
            .replace("\"value\":1.0", "\"value\":1e999");
        let r = parse_ingest(body.as_bytes()).unwrap_err();
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
        // A string where a number belongs is refused as not a number.
        let r = refused(&quantity(json!([hr(U1, 0, json!("NaN"))])));
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn start_after_end_is_a_400() {
        let mut s = hr(U1, 0, json!(1.0));
        s["start"] = json!("2026-10-09T09:00:00+02:00");
        assert!(refused(&quantity(json!([s]))).message.contains("after"));
    }

    #[test]
    fn a_timestamp_without_offset_is_a_400() {
        let mut s = hr(U1, 0, json!(1.0));
        s["start"] = json!("2026-10-09T08:59:00");
        assert_eq!(
            refused(&quantity(json!([s]))).status,
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn no_refusal_message_quotes_a_value() {
        let mut s = hr(U1, 0, json!("142.5"));
        s["start"] = json!("2026-10-09T09:00:00+02:00");
        let r = refused(&quantity(json!([s])));
        assert!(!r.message.contains("142.5") && !r.message.contains("2026"));
    }

    #[test]
    fn a_category_value_must_be_an_integer() {
        let mut s = hr(U1, 0, json!(2.5));
        s.as_object_mut().unwrap().remove("unit");
        let v = json!({"schema": 1, "kind": "category",
            "type": "HKCategoryTypeIdentifierSleepAnalysis", "samples": [s.clone()]});
        assert_eq!(refused(&v).status, StatusCode::BAD_REQUEST);
        s["value"] = json!(3);
        let v = json!({"schema": 1, "kind": "category",
            "type": "HKCategoryTypeIdentifierSleepAnalysis", "samples": [s]});
        assert!(parse(&v).is_ok());
    }

    #[test]
    fn the_sample_limit_is_2000() {
        let at = |n: usize| {
            quantity(Value::Array(
                (0..n).map(|i| hr(U1, i as i64, json!(1.0))).collect(),
            ))
        };
        assert!(parse(&at(INGEST_MAX_SAMPLES)).is_ok());
        let r = refused(&at(INGEST_MAX_SAMPLES + 1));
        assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn the_body_limit_is_8_mib_except_for_a_route() {
        let mut v = quantity(json!([hr(U1, 0, json!(1.0))]));
        v["samples"][0]["metadata"]["pad"] = json!("x".repeat(INGEST_MAX_BODY));
        let r = refused(&v);
        assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
        // The same padding under a route is within its own cap.
        let mut route = route_body(10);
        route["pad"] = json!("x".repeat(INGEST_MAX_BODY));
        assert!(parse(&route).is_ok());
        let big = vec![b' '; INGEST_MAX_ROUTE_BODY + 1];
        assert_eq!(
            parse_ingest(&big).unwrap_err().status,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    fn route_body(n: usize) -> Value {
        let points: Vec<Value> = (0..n)
            .map(|i| {
                json!({"t": "2026-10-09T08:59:00+02:00", "lat": 45.0 + i as f64 * 1e-5,
                    "lon": 11.0, "alt": null, "h_acc": 5.0, "v_acc": null,
                    "speed": 2.5, "course": 180.0})
            })
            .collect();
        json!({"schema": 1, "kind": "route", "type": ROUTE_TYPE, "samples": [{
            "uuid": U1, "workout_uuid": U2, "source": "s", "source_name": "n",
            "downsampled": false, "points": points}]})
    }

    #[test]
    fn a_route_parses_and_its_limits_hold() {
        let b = parse(&route_body(3)).unwrap();
        let IngestSamples::Route(Some(r)) = &b.samples else {
            panic!()
        };
        assert_eq!(r.points.len(), 3);
        assert_eq!(r.points[0].alt, None);
        let mut v = route_body(1);
        v["samples"][0]["points"][0]["lat"] = json!(90.5);
        assert!(refused(&v).message.contains("lat"));
        let mut v = route_body(1);
        v["samples"][0]["points"][0]["lon"] = json!(-180.01);
        assert!(refused(&v).message.contains("lon"));
        let mut v = route_body(1);
        let s = v["samples"][0].clone();
        v["samples"] = json!([s.clone(), s]);
        assert_eq!(refused(&v).status, StatusCode::BAD_REQUEST);
        let r = refused(&route_body(ROUTE_MAX_POINTS + 1));
        assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn a_workout_parses_with_events_and_refuses_an_unknown_event_type() {
        let w = json!({"schema": 1, "kind": "workout", "type": WORKOUT_TYPE, "samples": [{
            "uuid": U1, "start": "2026-10-09T07:00:00Z", "end": "2026-10-09T08:00:00Z",
            "duration_s": 3600.0, "activity_type": 37, "activity_name": "running",
            "distance_m": 10000.0, "energy_kcal": null, "indoor": false,
            "source": "s", "source_name": "n",
            "events": [{"type": "lap", "start": "2026-10-09T07:00:00Z",
                "end": "2026-10-09T07:05:00Z", "metadata": {"stroke": 1}}]}]});
        let b = parse(&w).unwrap();
        let IngestSamples::Workout(rows) = &b.samples else {
            panic!()
        };
        assert_eq!(rows[0].events.len(), 1);
        assert_eq!(rows[0].energy_kcal, None);
        let mut bad = w.clone();
        bad["samples"][0]["events"][0]["type"] = json!("split");
        assert_eq!(refused(&bad).status, StatusCode::BAD_REQUEST);
        let mut wrong_type = w;
        wrong_type["type"] = json!("HKQuantityTypeIdentifierHeartRate");
        assert_eq!(refused(&wrong_type).status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn deleted_must_be_uuids_and_a_duplicate_sample_keeps_the_last() {
        let mut v = quantity(json!([hr(U1, 0, json!(1.0)), hr(U1, 0, json!(2.0))]));
        v["deleted"] = json!([U2, U2]);
        let b = parse(&v).unwrap();
        let IngestSamples::Quantity(rows) = &b.samples else {
            panic!()
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value, 2.0);
        assert_eq!(b.deleted.len(), 1);
        v["deleted"] = json!(["not-a-uuid"]);
        assert_eq!(refused(&v).status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn without_healthdb_the_route_says_not_configured() {
        let st = crate::testutil::test_state();
        assert!(st.healthdb.is_none());
        let token = st.cfg.token.clone();
        let res = app(st)
            .oneshot(
                Request::post("/jesse/health/ingest")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&quantity(json!([hr(U1, 0, json!(1.0))]))).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(res.into_body(), 1 << 16)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v, json!({"error": "not_configured"}));
    }

    #[tokio::test]
    async fn a_bad_token_is_a_401_before_anything_else() {
        let st = crate::testutil::test_state();
        let res = app(st)
            .oneshot(
                Request::post("/jesse/health/ingest")
                    .header("authorization", "Bearer wrong")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    // ---- config --------------------------------------------------------------------------

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("healthdb-{name}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // A throwaway self signed certificate, generated for these tests; its key was discarded.
    const TEST_CA: &str = "-----BEGIN CERTIFICATE-----
MIIBhTCCASugAwIBAgIUf7+srFnwxd1aXzCFFKxUaJzBry4wCgYIKoZIzj0EAwIw
GDEWMBQGA1UEAwwNamVzc2UtdGVzdC1jYTAeFw0yNjEwMTIwNTUxMjFaFw0zNjEw
MDkwNTUxMjFaMBgxFjAUBgNVBAMMDWplc3NlLXRlc3QtY2EwWTATBgcqhkjOPQIB
BggqhkjOPQMBBwNCAASdD4O1d/5BFppaL8IGuf89onvHaY8nZ2PmKskMDnTaKuJx
xs5jauHhL17bA9RQNfgLIR2leo9EKdiGwoocYDSio1MwUTAdBgNVHQ4EFgQUd3J/
0Dk/160GRHuhV09rr5CQ4KIwHwYDVR0jBBgwFoAUd3J/0Dk/160GRHuhV09rr5CQ
4KIwDwYDVR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAgNIADBFAiEAnbML+lQyKoiV
FyhnU3eUVjnxaYmYvgKxZxvZ8MGUeSUCICGoxhH3xvPW0Nv7U6X6/tz83oZ0JeyD
Htby8Y5sp501
-----END CERTIFICATE-----
";

    fn table(dir: &Path, url_mode: u32) -> HealthDbToml {
        let url = dir.join("url");
        std::fs::write(
            &url,
            "postgres://health:PASSWORD@healthdb.example:5432/health?sslmode=verify-full\n",
        )
        .unwrap();
        std::fs::set_permissions(&url, std::fs::Permissions::from_mode(url_mode)).unwrap();
        let ca = dir.join("ca.pem");
        std::fs::write(&ca, TEST_CA).unwrap();
        HealthDbToml {
            url_file: Some(url.to_string_lossy().into_owned()),
            ca_file: Some(ca.to_string_lossy().into_owned()),
            mirror_diet: None,
            ro_url_file: None,
        }
    }

    #[test]
    fn an_absent_table_is_absent() {
        assert!(matches!(
            HealthDbConfig::from_toml(None),
            HealthDbConfig::Absent
        ));
    }

    #[test]
    fn a_0600_url_file_and_a_ca_make_a_ready_table_with_mirror_on() {
        let d = scratch("ready");
        match HealthDbConfig::from_toml(Some(table(&d, 0o600))) {
            HealthDbConfig::Ready(s) => {
                assert!(s.mirror_diet);
                assert!(!format!("{s:?}").contains("PASSWORD"));
            }
            other => panic!("expected ready, got {other:?}"),
        }
    }

    #[test]
    fn a_group_readable_url_file_is_refused_without_quoting_it() {
        let d = scratch("mode");
        for mode in [0o640, 0o604, 0o644] {
            match HealthDbConfig::from_toml(Some(table(&d, mode))) {
                HealthDbConfig::Refused(why) => {
                    assert!(why.contains("chmod 600"), "{why}");
                    assert!(!why.contains("PASSWORD"), "{why}");
                }
                other => panic!("mode {mode:o}: expected refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_missing_file_is_refused_by_name() {
        let d = scratch("missing");
        let mut t = table(&d, 0o600);
        t.ca_file = Some(d.join("nope.pem").to_string_lossy().into_owned());
        let HealthDbConfig::Refused(why) = HealthDbConfig::from_toml(Some(t)) else {
            panic!("a missing ca_file must refuse");
        };
        assert!(why.contains("ca_file"), "{why}");
        let mut t = table(&d, 0o600);
        t.url_file = Some(d.join("nope").to_string_lossy().into_owned());
        let HealthDbConfig::Refused(why) = HealthDbConfig::from_toml(Some(t)) else {
            panic!("a missing url_file must refuse");
        };
        assert!(why.contains("url_file"), "{why}");
        let mut t = table(&d, 0o600);
        t.url_file = None;
        assert!(matches!(
            HealthDbConfig::from_toml(Some(t)),
            HealthDbConfig::Refused(_)
        ));
    }

    #[test]
    fn a_url_file_owned_by_another_uid_is_refused() {
        let d = scratch("owner");
        let t = table(&d, 0o600);
        assert!(matches!(
            HealthDbConfig::from_toml_as(Some(t), current_euid().wrapping_add(1)),
            HealthDbConfig::Refused(_)
        ));
    }

    #[test]
    fn a_url_file_that_is_not_a_postgres_url_is_refused_without_quoting_it() {
        let d = scratch("url");
        let t = table(&d, 0o600);
        let path = PathBuf::from(t.url_file.clone().unwrap());
        std::fs::write(&path, "mysql://u:SECRETPW@h/db\n").unwrap();
        let HealthDbConfig::Refused(why) = HealthDbConfig::from_toml(Some(t)) else {
            panic!()
        };
        assert!(!why.contains("SECRETPW"), "{why}");
    }

    #[test]
    fn a_refused_table_stops_the_startup_gate() {
        let mut cfg = crate::testutil::test_config();
        cfg.healthdb = HealthDbConfig::Refused("`url_file` is missing".into());
        let errs = validate_healthdb(&cfg);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].message.contains("[healthdb]"));
        cfg.healthdb = HealthDbConfig::Absent;
        assert!(validate_healthdb(&cfg).is_empty());
    }

    #[test]
    fn connect_options_force_verify_full() {
        let d = scratch("tls");
        let HealthDbConfig::Ready(s) = HealthDbConfig::from_toml(Some(HealthDbToml {
            url_file: table(&d, 0o600).url_file,
            ca_file: table(&d, 0o600).ca_file,
            mirror_diet: Some(false),
            ro_url_file: None,
        })) else {
            panic!()
        };
        std::fs::write(&s.url_file, "postgres://h:p@x/db?sslmode=disable").unwrap();
        let s = HealthDbSettings {
            url: read_url_file(&s.url_file, current_euid()).unwrap(),
            ..s
        };
        assert!(!s.mirror_diet);
        assert!(matches!(
            s.connect_options().get_ssl_mode(),
            PgSslMode::VerifyFull
        ));
    }

    /// A table with an `ro_url_file` holding `url`, at `mode`.
    fn table_with_ro(dir: &Path, url: &str, mode: u32) -> HealthDbToml {
        let ro = dir.join("ro-url");
        std::fs::write(&ro, url).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(mode)).unwrap();
        HealthDbToml {
            ro_url_file: Some(ro.to_string_lossy().into_owned()),
            ..table(dir, 0o600)
        }
    }

    const RO_URL: &str =
        "postgres://health_ro:ROPASSWORD@healthdb.example:5432/health?sslmode=verify-full\n";

    #[test]
    fn without_ro_url_file_there_is_no_server_environment() {
        let d = scratch("ro-absent");
        let HealthDbConfig::Ready(s) = HealthDbConfig::from_toml(Some(table(&d, 0o600))) else {
            panic!("expected ready");
        };
        assert!(s.ro_url_file.is_none());
        assert!(s.healthdb_mcp_env().is_none());
    }

    #[test]
    fn a_0600_ro_url_file_gives_the_server_paths_and_never_a_url() {
        let d = scratch("ro-ready");
        let HealthDbConfig::Ready(s) =
            HealthDbConfig::from_toml(Some(table_with_ro(&d, RO_URL, 0o600)))
        else {
            panic!("expected ready");
        };
        let env = s.healthdb_mcp_env().expect("configured");
        assert_eq!(env[0].0, HEALTHQ_ENV_RO_URL_FILE);
        assert_eq!(env[0].1, d.join("ro-url").to_string_lossy());
        assert_eq!(env[1].0, HEALTHQ_ENV_CA_FILE);
        assert_eq!(PathBuf::from(&env[1].1), s.ca_file);
        let all = format!("{env:?} {s:?}");
        assert!(!all.contains("PASSWORD"), "{all}");
    }

    #[test]
    fn a_group_readable_ro_url_file_is_refused_by_name_without_quoting_it() {
        let d = scratch("ro-mode");
        for mode in [0o640, 0o604, 0o644] {
            match HealthDbConfig::from_toml(Some(table_with_ro(&d, RO_URL, mode))) {
                HealthDbConfig::Refused(why) => {
                    assert!(why.contains("`ro_url_file`"), "{why}");
                    assert!(why.contains("chmod 600"), "{why}");
                    assert!(!why.contains("PASSWORD"), "{why}");
                }
                other => panic!("mode {mode:o}: expected refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn an_ro_url_file_naming_the_owner_role_is_refused() {
        let d = scratch("ro-same");
        let owner = "postgres://health:OTHER@healthdb.example:5432/health\n";
        let HealthDbConfig::Refused(why) =
            HealthDbConfig::from_toml(Some(table_with_ro(&d, owner, 0o600)))
        else {
            panic!("the owner's role in ro_url_file must refuse");
        };
        assert!(why.contains("same role"), "{why}");
        assert!(!why.contains("OTHER"), "{why}");
    }

    // ---- diet CSV ------------------------------------------------------------------------

    #[test]
    fn a_quoted_field_with_a_comma_and_a_newline_is_one_cell() {
        let csv = "Date,Item,Notes\n2026-10-01,eggs,\"two, scrambled\nwith butter\"\n\
                   2026-10-02,toast,plain\n";
        let rows = parse_diet_csv(csv.as_bytes()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].row_no, 1);
        assert_eq!(rows[0].data["Notes"], json!("two, scrambled\nwith butter"));
        assert_eq!(rows[0].date, NaiveDate::from_ymd_opt(2026, 10, 1));
        assert_eq!(rows[1].data["Item"], json!("toast"));
    }

    #[test]
    fn a_ragged_row_fails_with_its_line_and_a_bad_date_is_null() {
        let rows = parse_diet_csv(b"Date,A\nnot-a-date,1\n").unwrap();
        assert_eq!(rows[0].date, None);
        let line = parse_diet_csv(b"Date,A\n2026-10-01,1\n2026-10-02,1,extra\n").unwrap_err();
        assert_eq!(line, 3);
    }

    #[test]
    fn the_mirror_reads_the_vitals_ledger_from_where_vitals_writes_it() {
        let cfg = crate::testutil::test_config();
        assert_eq!(
            diet_logs_dir(&cfg).join(VITALS_LOG),
            Path::new(&cfg.vault).join("diet-logs").join(VITALS_LOG)
        );
        assert!(DIET_MIRROR_FILES.contains(&VITALS_LOG));
    }

    #[test]
    fn a_route_point_line_is_csv_with_empty_nulls() {
        let mut s = String::new();
        let p = RoutePoint {
            t: DateTime::parse_from_rfc3339("2026-10-09T08:59:00.5+02:00")
                .unwrap()
                .with_timezone(&Utc),
            lat: 45.5,
            lon: -11.25,
            alt: None,
            h_acc: Some(5.0),
            v_acc: None,
            speed: None,
            course: Some(180.0),
        };
        route_point_csv(&mut s, Uuid::parse_str(U1).unwrap(), 7, &p);
        assert_eq!(
            s,
            format!("{U1},7,2026-10-09T06:59:00.500Z,45.5,-11.25,,5,,,180\n")
        );
    }

    #[test]
    fn counters_accumulate_by_label() {
        let before = healthdb_counter(METRIC_INGEST_ERRORS, "test_label");
        healthdb_count(METRIC_INGEST_ERRORS, "test_label", 2);
        assert_eq!(
            healthdb_counter(METRIC_INGEST_ERRORS, "test_label"),
            before + 2
        );
        assert!(healthdb_counters_json()[METRIC_INGEST_ERRORS]["test_label"].is_u64());
    }
}
