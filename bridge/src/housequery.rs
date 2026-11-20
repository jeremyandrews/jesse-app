//! **The house's history, read by a turn**: `jesse-housedb-mcp`, `housedb_schema` and
//! `housedb_query`.
//!
//! The same server as `jesse-healthdb-mcp`, over a different database: Home Assistant's recorder
//! on the home cluster (database `homeassistant`, `househistorydb.pozza`), with the daily
//! rollups the cluster's hourly job keeps in schema `analytics`. Every guard, limit and the
//! transport are [`crate::healthquery`]'s and are not repeated here: one statement, `BEGIN READ
//! ONLY`, a 15 s `statement_timeout`, at most 500 rows, a fresh connection per call, then
//! `ROLLBACK`. What this file adds is DATA: [`HOUSEDB`], the server's names and guide, the order
//! `housedb_schema` lists relations in, and the `[housedb]` table.
//!
//! # Read only in two places
//!
//! The server connects as `house_ro`, a role with `SELECT` on `public` and `analytics` and
//! nothing else. The bridge's own guards hold whatever that role turns out to be allowed.
//!
//! # The credential
//!
//! As `healthdb`'s: a URL file, `0600`, owned by the bridge's user, checked at startup and read
//! again by the server itself. The bridge hands the server the file's PATH; the URL never travels
//! through an argv, an environment or a config string, and is never logged.

use std::collections::BTreeMap;

use crate::*;

/// The house history's server, `jesse-housedb-mcp`.
pub static HOUSEDB: RoDbServer = RoDbServer {
    server: HOUSEDB_SERVER,
    command: HOUSEDB_COMMAND,
    schema_tool: "housedb_schema",
    query_tool: "housedb_query",
    schema_description: "Describe the house's history (Home Assistant's recorder in Postgres): \
                         first the analytics.house_* views, the daily rollups of energy, air and \
                         plant and the live lists of alarm changes, frozen sensors and unavailable \
                         entities, then Home Assistant's raw recorder tables, each with columns, \
                         types and row counts, and a guide to reading them. Prefer the house_* \
                         views: they already answer daily questions in Jeremy's local days. Call \
                         this before writing a housedb_query.",
    query_description: "Run ONE read-only SQL statement (SELECT, WITH, TABLE or VALUES) against \
                        the house's history (Home Assistant's recorder) and get the rows back as \
                        JSON. At most 500 rows, each text value cut at 500 characters, 15 second \
                        timeout; flags say when anything was cut. Query the analytics.house_* \
                        views first and reach for the raw recorder tables only for what they do \
                        not hold. Aggregate in SQL, and quote the numbers it returns.",
    archive: "the house history database",
    timeout_hint: "filter the recorder tables on metadata_id and last_updated_ts",
    env_ro_url_file: HOUSEQ_ENV_RO_URL_FILE,
    env_ca_file: HOUSEQ_ENV_CA_FILE,
    env_ledger: HOUSEQ_ENV_LEDGER,
    ledger_file: HOUSEQ_LEDGER_FILE,
    metric_query: METRIC_HOUSEDB_MCP_QUERY,
    metric_schema: METRIC_HOUSEDB_MCP_SCHEMA,
    guide: HOUSEQ_GUIDE,
    layout: SchemaLayout::House,
};

/// The server's name in every MCP set and grant: tools are `mcp__housedb__<tool>`.
pub const HOUSEDB_SERVER: &str = "housedb";
/// The bare command every harness starts, resolved from the child's `PATH`.
pub const HOUSEDB_COMMAND: &str = "jesse-housedb-mcp";

/// The URL file of the read-only role, by path. The server reads and checks it itself.
pub const HOUSEQ_ENV_RO_URL_FILE: &str = "JESSE_HOUSEDB_RO_URL_FILE";
/// The CA the server's certificate is verified against.
pub const HOUSEQ_ENV_CA_FILE: &str = "JESSE_HOUSEDB_CA_FILE";
/// An explicit ledger path, overriding `<JESSE_STATE_DIR>/housedb-mcp-calls.log`.
pub const HOUSEQ_ENV_LEDGER: &str = "JESSE_HOUSEDB_MCP_LEDGER";
/// The ledger's file name in the state directory.
pub const HOUSEQ_LEDGER_FILE: &str = "housedb-mcp-calls.log";

/// `ro_url_file`'s default, in the bridge's state directory.
pub const HOUSEDB_DEFAULT_RO_URL_FILE: &str = "housedb-ro-url";
/// `ca_file`'s default, in the bridge's state directory.
pub const HOUSEDB_DEFAULT_CA_FILE: &str = "housedb-ca.crt";

/// The counter a `housedb_query` call lands in, labelled by outcome.
pub const METRIC_HOUSEDB_MCP_QUERY: &str = "housedb_mcp_query_total";
/// The counter a `housedb_schema` call lands in, labelled by outcome.
pub const METRIC_HOUSEDB_MCP_SCHEMA: &str = "housedb_mcp_schema_total";

/// The fixed guide `housedb_schema` returns beside the live description.
pub const HOUSEQ_GUIDE: &[&str] = &[
    "Read only: one SELECT, WITH, TABLE or VALUES statement per housedb_query call, at most 500 rows back, each text value cut at 500 characters, 15 second timeout. Aggregate in SQL rather than fetching rows to count them.",
    "Prefer the analytics.house_* views; they are listed first. house_daily_energy (one row per day: heat pump and pool heat pump kWh, pool heat pump run hours, outdoor mean, min and max C, heating degree days, kWh per degree day), house_daily_air (per day and floor: CO2 mean, max, hours above 1000 ppm and overnight peak, humidity mean and max), house_daily_plant (per day and item: pump, ventilation speed step and fan coil circuit run hours). house_alarm_changes (every on/off change of every alarm, error and fault sensor), house_alarm_sensors (those sensors, one row each, with the state now), house_frozen_sensors (numeric sensors unchanged for more than 48 hours, now) and house_unavailable (entities unknown or unavailable now, and since when) are live.",
    "A daily view's `day` is a calendar date in Europe/Rome, where Jeremy lives. `complete` is false for today and for the first day anything was recorded, which are partial: say so, or leave them out of a trend. The daily views are refreshed hourly and the last three days recomputed each time.",
    "heat_pump_kwh and pool_heat_pump_kwh come from counters that count whole kWh, so a single day is good to about 1 kWh. heat_pump_kwh_per_hdd is only set on days with at least one heating degree day (base 18 C).",
    "analytics.house_entity names which Home Assistant entity plays which part (role, label, entity_id) in the rollups.",
    "Raw recorder tables are in schema public and belong to Home Assistant. public.states: one row per state change, `state` is TEXT (a number, on, off, unknown, unavailable), `last_updated_ts` is epoch seconds (UTC, double precision; to_timestamp(last_updated_ts) for a timestamptz), `metadata_id` joins public.states_meta, whose `entity_id` names the entity. Always filter states on metadata_id and a last_updated_ts range: it gains tens of thousands of rows a day.",
    "public.statistics (hourly, never purged) and public.statistics_short_term (five minutes): `start_ts` epoch seconds, `mean`, `min`, `max`, and `state` and `sum` for counters; `metadata_id` joins public.statistics_meta, whose `statistic_id` is the entity id and `unit_of_measurement` its unit.",
    "analytics.num(text) turns a recorder state into a number, or null for on, unknown and the rest. analytics.day_start(date) is local midnight as a timestamptz and analytics.today() is today in Europe/Rome.",
    "Every call runs with TimeZone Europe/Rome, so to_timestamp(...)::date and date_trunc give his local days.",
    "Quote the numbers from the result when you answer; do not estimate what a query could have returned.",
];

/// What `housedb_schema` lists, in its order: the `analytics.house_*` views, then the rest of
/// `analytics` this role can read, then Home Assistant's recorder tables in `public`. Objects
/// whose name starts with `_` (the tables behind the daily views) are left out: the views are
/// what to query. The shape matches the health layout's, plus a `rank` the caller removes.
pub const HOUSE_RELATIONS_SQL: &str = "\
SELECT coalesce(json_agg(r ORDER BY r.rank, r.schema, r.name), '[]')::text FROM (
  SELECT n.nspname AS schema, c.relname AS name,
    CASE c.relkind WHEN 'r' THEN 'table' WHEN 'p' THEN 'table' WHEN 'v' THEN 'view'
      WHEN 'm' THEN 'materialized view' ELSE 'foreign table' END AS kind,
    (SELECT coalesce(json_agg(json_build_object('name', a.attname,
              'type', format_type(a.atttypid, a.atttypmod)) ORDER BY a.attnum), '[]')
       FROM pg_attribute a
      WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped) AS columns,
    CASE WHEN c.relkind IN ('r', 'p', 'm')
      THEN greatest(c.reltuples::bigint, coalesce(s.n_live_tup, 0)) END AS estimate,
    CASE WHEN n.nspname = 'analytics' AND c.relname LIKE 'house\\_%' AND c.relkind = 'v' THEN 0
      WHEN n.nspname = 'analytics' THEN 1
      ELSE 2 END AS rank
  FROM pg_class c
  JOIN pg_namespace n ON n.oid = c.relnamespace
  LEFT JOIN pg_stat_all_tables s ON s.relid = c.oid
  WHERE n.nspname IN ('analytics', 'public')
    AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
    AND c.relname NOT LIKE '\\_%'
    AND has_table_privilege(c.oid, 'SELECT')
) r";

// ---- Config -------------------------------------------------------------------------------

/// The `[housedb]` table as written. Both keys are optional and default to files in the
/// bridge's state directory ([`HOUSEDB_DEFAULT_RO_URL_FILE`], [`HOUSEDB_DEFAULT_CA_FILE`]).
#[derive(Deserialize, Debug, Default, Clone)]
pub struct HouseDbToml {
    /// A file holding the connection URL of the READ-ONLY role (`house_ro`), password included.
    /// Mode `0600`, owned by the bridge's user. The bridge never connects with it.
    pub ro_url_file: Option<String>,
    /// The PEM certificate of the CA that signed the server's certificate.
    pub ca_file: Option<String>,
}

/// A `[housedb]` table that passed every check. Paths only: the URL is read, checked and
/// dropped at load, and read again by the server itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HouseDbSettings {
    pub ro_url_file: PathBuf,
    pub ca_file: PathBuf,
}

impl HouseDbSettings {
    /// The environment a `jesse-housedb-mcp` child is started with. Paths only.
    pub fn housedb_mcp_env(&self) -> [(&'static str, String); 2] {
        [
            (
                HOUSEQ_ENV_RO_URL_FILE,
                self.ro_url_file.to_string_lossy().into_owned(),
            ),
            (
                HOUSEQ_ENV_CA_FILE,
                self.ca_file.to_string_lossy().into_owned(),
            ),
        ]
    }
}

/// What the deployment's `[housedb]` table amounts to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum HouseDbConfig {
    /// No table: the shipped default. No server is ever started.
    #[default]
    Absent,
    Ready(HouseDbSettings),
    /// Present and wrong. The startup gate refuses to boot on it ([`validate_housedb`]).
    Refused(String),
}

impl HouseDbConfig {
    /// Resolve the table against the bridge's state directory, which holds the default files.
    pub fn from_toml(table: Option<HouseDbToml>, state_dir: Option<&str>) -> HouseDbConfig {
        // SAFETY: `geteuid` takes no arguments, cannot fail, and touches no memory of ours.
        Self::from_toml_as(table, state_dir, unsafe { libc::geteuid() })
    }

    /// [`Self::from_toml`] for a given owner uid, so the owner check is testable.
    pub fn from_toml_as(
        table: Option<HouseDbToml>,
        state_dir: Option<&str>,
        uid: u32,
    ) -> HouseDbConfig {
        let Some(t) = table else {
            return HouseDbConfig::Absent;
        };
        let mut problems = Vec::new();
        let path = |v: Option<String>, key: &str, default: &str, problems: &mut Vec<String>| match v
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(p) => Some(PathBuf::from(p)),
            None => match state_dir.filter(|d| !d.trim().is_empty()) {
                Some(d) => Some(PathBuf::from(d).join(default)),
                None => {
                    problems.push(format!(
                        "`{key}` is missing and there is no state directory to default it to"
                    ));
                    None
                }
            },
        };
        let ro_url_file = path(
            t.ro_url_file,
            "ro_url_file",
            HOUSEDB_DEFAULT_RO_URL_FILE,
            &mut problems,
        );
        let ca_file = path(t.ca_file, "ca_file", HOUSEDB_DEFAULT_CA_FILE, &mut problems);
        if let Some(ro) = &ro_url_file {
            // Read and checked, then dropped: the server reads it again itself.
            if let Err(e) = read_url_file_keyed(ro, uid, "ro_url_file") {
                problems.push(e);
            }
        }
        if let Some(ca) = &ca_file {
            if let Err(e) = check_ca_file(ca) {
                problems.push(e);
            }
        }
        match (ro_url_file, ca_file) {
            (Some(ro_url_file), Some(ca_file)) if problems.is_empty() => {
                HouseDbConfig::Ready(HouseDbSettings {
                    ro_url_file,
                    ca_file,
                })
            }
            _ => HouseDbConfig::Refused(problems.join("; ")),
        }
    }

    pub fn ready(&self) -> Option<&HouseDbSettings> {
        match self {
            HouseDbConfig::Ready(s) => Some(s),
            HouseDbConfig::Absent | HouseDbConfig::Refused(_) => None,
        }
    }
}

/// The startup gate's view of `[housedb]`: a refused table stops the bridge, by name.
pub fn validate_housedb(cfg: &Config) -> Vec<ConfigError> {
    match &cfg.housedb {
        HouseDbConfig::Refused(why) => vec![ConfigError {
            model: None,
            message: format!(
                "the [housedb] table is present but unusable: {why}. Fix it in \
                 jesse.local.toml, or remove the table to run without the house history."
            ),
        }],
        HouseDbConfig::Absent | HouseDbConfig::Ready(_) => Vec::new(),
    }
}

// ---- Registration -------------------------------------------------------------------------

/// Whether this deployment starts the server at all: a ready `[housedb]` table. Without one, no
/// harness registers it and no process is started.
pub fn housedb_mcp_ready(cfg: &Config) -> Option<&HouseDbSettings> {
    cfg.housedb.ready()
}

/// The `--mcp-config` a child is given, as far as `housedb` goes: a config that does not carry
/// the shipped entry ([`HOUSEDB_MCP_ENTRY`]) is returned unchanged, byte for byte; one that does
/// keeps it on a deployment that starts the server and loses it on any other. The same surgery
/// as [`render_healthdb_mcp`].
pub fn render_housedb_mcp<'a>(cfg: &Config, config: &'a str) -> std::borrow::Cow<'a, str> {
    if !config.contains(HOUSEDB_MCP_ENTRY.as_str()) || housedb_mcp_ready(cfg).is_some() {
        return std::borrow::Cow::Borrowed(config);
    }
    std::borrow::Cow::Owned(config.replacen(HOUSEDB_MCP_ENTRY.as_str(), "", 1))
}

/// The variables a `jesse-housedb-mcp` child reads, as `(name, value)`: the two paths, and the
/// ledger's path when the bridge has a state directory, so the server's calls land where
/// `GET /health` tallies them. `None` when the deployment does not start the server.
pub fn housedb_child_env(cfg: &Config) -> Option<Vec<(&'static str, String)>> {
    let s = housedb_mcp_ready(cfg)?;
    let mut env: Vec<(&'static str, String)> = s.housedb_mcp_env().into_iter().collect();
    if let Some(dir) = cfg.state_dir.as_deref() {
        env.push((
            HOUSEQ_ENV_LEDGER,
            PathBuf::from(dir)
                .join(HOUSEQ_LEDGER_FILE)
                .to_string_lossy()
                .into_owned(),
        ));
    }
    Some(env)
}

/// Put the server's variables on a child whose rendered set loads it, and on no other. Paths
/// only: nothing here is a credential.
pub fn apply_housedb_env(cmd: &mut Command, cfg: &Config, config: &str) {
    if !config.contains(HOUSEDB_MCP_ENTRY.as_str()) {
        return;
    }
    if let Some(env) = housedb_child_env(cfg) {
        for (k, v) in env {
            cmd.env(k, v);
        }
    }
}

/// Whether a main turn on `harness` registers `housedb` on this deployment: the server is
/// started here, and the harness's main set declares it, or it is `direct`, which connects it
/// through [`direct_housedb_grant`].
pub fn main_turn_loads_housedb(cfg: &Config, harness: &dyn Harness) -> bool {
    if housedb_mcp_ready(cfg).is_none() {
        return false;
    }
    match harness.runner() {
        Runner::Spawned(h) => main_mcp_config(cfg, h).contains(HOUSEDB_MCP_ENTRY.as_str()),
        Runner::InProcess(_) => harness.id() == DIRECT_ID,
    }
}

/// The `housedb` grant as the `direct` record commits it, with or without the table, as
/// [`healthdb_grant_shape`] is: names and classes, nothing a turn could connect with. Both tools
/// are `egress`, for `healthdb`'s reason.
pub fn housedb_grant_shape() -> DirectMcpGrant {
    DirectMcpGrant {
        name: HOUSEDB_SERVER.to_string(),
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
        tools: HOUSEDB
            .tool_names()
            .into_iter()
            .map(str::to_string)
            .collect(),
        class: jesse_agent::tools::ActionClass::Egress,
        per_tool_class: BTreeMap::new(),
    }
}

/// The `housedb` grant a direct turn connects with on this deployment, or `None` when the server
/// is not started here. A stdio grant whose environment is the direct harness's forwarded
/// variables plus the server's own paths, and nothing else.
pub fn direct_housedb_grant(cfg: &Config) -> Option<jesse_agent::mcp::ServerGrant> {
    let env = housedb_child_env(cfg)?;
    let shape = housedb_grant_shape();
    let mut vars: BTreeMap<String, String> = DIRECT_MCP_FORWARDED_ENV
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    vars.extend(env.into_iter().map(|(k, v)| (k.to_string(), v)));
    Some(jesse_agent::mcp::ServerGrant {
        name: HOUSEDB_SERVER.to_string(),
        transport: jesse_agent::mcp::McpTransport::Stdio {
            command: HOUSEDB_COMMAND.to_string(),
            args: Vec::new(),
            env: vars,
        },
        tools: shape.tools,
        action_class_default: shape.class,
        per_tool_class: Default::default(),
    })
}

/// The `housedb.counters` object `GET /health` serves: the server's call counters, tallied
/// from its ledger in `state_dir`.
pub fn housedb_counters_json(state_dir: Option<&str>) -> Value {
    let mut body = json!({});
    if let (Some(dir), Value::Object(o)) = (state_dir, &mut body) {
        for (metric, labels) in
            ro_db_mcp_counters(&HOUSEDB, &PathBuf::from(dir).join(HOUSEQ_LEDGER_FILE))
        {
            o.insert(metric.to_string(), json!(labels));
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    // A throwaway self signed certificate, generated for these tests; its key was discarded.
    const CA: &str = "-----BEGIN CERTIFICATE-----
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

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(tag: &str) -> Dir {
        let d = std::env::temp_dir().join(format!(
            "housedb-cfg-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn write(path: &Path, body: &str, mode: u32) {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn uid() -> u32 {
        // SAFETY: as above.
        unsafe { libc::geteuid() }
    }

    #[test]
    fn no_table_is_absent_and_starts_nothing() {
        assert_eq!(
            HouseDbConfig::from_toml_as(None, Some("/srv/state"), uid()),
            HouseDbConfig::Absent
        );
        assert!(validate_housedb(&crate::testutil::test_config()).is_empty());
    }

    #[test]
    fn an_empty_table_defaults_both_files_to_the_state_dir() {
        let d = scratch("defaults");
        let ro = d.0.join(HOUSEDB_DEFAULT_RO_URL_FILE);
        let ca = d.0.join(HOUSEDB_DEFAULT_CA_FILE);
        write(
            &ro,
            "postgres://house_ro:SECRETPW@househistorydb.example:5432/homeassistant\n",
            0o600,
        );
        write(&ca, CA, 0o644);
        let got = HouseDbConfig::from_toml_as(
            Some(HouseDbToml::default()),
            Some(d.0.to_str().unwrap()),
            uid(),
        );
        assert_eq!(
            got,
            HouseDbConfig::Ready(HouseDbSettings {
                ro_url_file: ro,
                ca_file: ca,
            })
        );
        assert!(!format!("{got:?}").contains("SECRETPW"));
    }

    #[test]
    fn a_url_file_open_to_others_is_refused_without_quoting_it() {
        let d = scratch("mode");
        let ro = d.0.join("ro");
        write(
            &ro,
            "postgres://house_ro:SECRETPW@househistorydb.example/homeassistant",
            0o644,
        );
        let t = HouseDbToml {
            ro_url_file: Some(ro.to_string_lossy().into_owned()),
            ca_file: Some(d.0.join("absent.crt").to_string_lossy().into_owned()),
        };
        let HouseDbConfig::Refused(why) = HouseDbConfig::from_toml_as(Some(t), None, uid()) else {
            panic!("refused");
        };
        assert!(why.contains("0644"), "{why}");
        assert!(why.contains("`ca_file`"), "{why}");
        assert!(!why.contains("SECRETPW"), "{why}");
        let mut cfg = crate::testutil::test_config();
        cfg.housedb = HouseDbConfig::Refused(why);
        let errs = validate_housedb(&cfg);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].message.contains("[housedb]"));
    }

    #[test]
    fn missing_keys_without_a_state_dir_are_refused_by_name() {
        let HouseDbConfig::Refused(why) =
            HouseDbConfig::from_toml_as(Some(HouseDbToml::default()), None, uid())
        else {
            panic!("refused");
        };
        assert!(why.contains("`ro_url_file` is missing"), "{why}");
        assert!(why.contains("`ca_file` is missing"), "{why}");
    }

    #[test]
    fn the_server_environment_is_two_paths_and_never_the_url() {
        let s = HouseDbSettings {
            ro_url_file: PathBuf::from("/srv/state/housedb-ro-url"),
            ca_file: PathBuf::from("/srv/state/housedb-ca.crt"),
        };
        assert_eq!(
            s.housedb_mcp_env(),
            [
                (
                    "JESSE_HOUSEDB_RO_URL_FILE",
                    "/srv/state/housedb-ro-url".to_string()
                ),
                (
                    "JESSE_HOUSEDB_CA_FILE",
                    "/srv/state/housedb-ca.crt".to_string()
                ),
            ]
        );
    }

    #[test]
    fn the_guide_and_descriptions_steer_to_the_views() {
        let g = HOUSEQ_GUIDE.join("\n");
        for needle in [
            "Prefer the analytics.house_* views",
            "house_daily_energy",
            "house_daily_air",
            "house_daily_plant",
            "house_alarm_changes",
            "house_frozen_sensors",
            "house_unavailable",
            "Europe/Rome",
            "`complete` is false",
            "last_updated_ts",
            "states_meta",
        ] {
            assert!(g.contains(needle), "{needle}");
        }
        assert!(HOUSEDB
            .schema_description
            .contains("Prefer the house_* views"));
        assert!(HOUSEDB
            .query_description
            .contains("analytics.house_* views first"));
        // The views sort before everything else, and the tables behind them are left out.
        assert!(HOUSE_RELATIONS_SQL.contains("LIKE 'house\\_%' AND c.relkind = 'v' THEN 0"));
        assert!(HOUSE_RELATIONS_SQL.contains("NOT LIKE '\\_%'"));
    }

    #[test]
    fn the_counters_tally_only_housedb_lines() {
        let d = scratch("ledger");
        let ledger = RoDbLedger::at(&HOUSEDB, d.0.join(HOUSEQ_LEDGER_FILE));
        let reply = ToolReply {
            text: "SECRET".into(),
            is_error: false,
            outcome: QueryOutcome::Ok,
            rows: 3,
        };
        ledger.record(DbTool::Query, &reply, Duration::from_millis(4));
        ledger.record(DbTool::Schema, &reply, Duration::from_millis(4));
        // A healthdb line in the same file is not this server's.
        RoDbLedger::at(&HEALTHDB, d.0.join(HOUSEQ_LEDGER_FILE)).record(
            DbTool::Query,
            &reply,
            Duration::ZERO,
        );
        let c = housedb_counters_json(d.0.to_str());
        assert_eq!(
            c,
            json!({"housedb_mcp_query_total": {"ok": 1}, "housedb_mcp_schema_total": {"ok": 1}})
        );
        let text = std::fs::read_to_string(d.0.join(HOUSEQ_LEDGER_FILE)).unwrap();
        assert!(!text.contains("SECRET"));
    }
}

/// Registration: which harness carries the server, and with what, configured and not. The
/// same shape as `healthdb`'s (`healthquery::registration_tests`).
#[cfg(test)]
mod registration_tests {
    use super::*;
    use crate::testutil::*;

    const RO_FILE: &str = "/srv/jesse/state/housedb-ro-url";
    const CA_FILE: &str = "/srv/jesse/state/housedb-ca.crt";

    fn configured() -> Config {
        let mut cfg = test_config();
        cfg.housedb = HouseDbConfig::Ready(HouseDbSettings {
            ro_url_file: PathBuf::from(RO_FILE),
            ca_file: PathBuf::from(CA_FILE),
        });
        cfg.state_dir = Some("/srv/jesse/state".to_string());
        cfg
    }

    fn unconfigured() -> Vec<Config> {
        [HouseDbConfig::Absent, HouseDbConfig::Refused("x".into())]
            .into_iter()
            .map(|h| {
                let mut cfg = test_config();
                cfg.housedb = h;
                cfg
            })
            .collect()
    }

    fn servers_on_argv(args: &[String]) -> serde_json::Map<String, Value> {
        let at = args.iter().position(|a| a == "--mcp-config").unwrap();
        let v: Value = serde_json::from_str(&args[at + 1]).unwrap();
        v["mcpServers"].as_object().unwrap().clone()
    }

    fn claude_main_args(cfg: &Config) -> Vec<String> {
        build_claude_args(
            cfg,
            "hi",
            None,
            Capability::Write,
            main_mcp_config(cfg, &ClaudeCode),
            None,
            None,
        )
    }

    #[test]
    fn the_rendered_set_keeps_housedb_exactly_when_the_table_is_ready() {
        let with = render_child_mcp(&configured(), MAIN_CHILD_MCP_CONFIG.as_str());
        assert!(with.contains(HOUSEDB_MCP_ENTRY.as_str()));
        for cfg in unconfigured() {
            let without = render_child_mcp(&cfg, MAIN_CHILD_MCP_CONFIG.as_str());
            assert!(!without.contains("housedb"), "{without}");
        }
        // Every set that does not declare it is untouched by its surgery either way.
        for set in McpSet::ALL.into_iter().filter(|s| !s.contains_housedb()) {
            for cfg in [configured(), test_config()] {
                assert_eq!(
                    render_housedb_mcp(&cfg, set.config()).as_ref(),
                    set.config()
                );
            }
        }
    }

    #[test]
    fn claude_code_registers_housedb_when_configured_and_not_otherwise() {
        let args = claude_main_args(&configured());
        assert_eq!(
            servers_on_argv(&args)["housedb"],
            json!({"type": "stdio", "command": HOUSEDB_COMMAND, "args": []})
        );
        assert!(args.join("\n").contains("mcp__housedb__housedb_query"));
        assert!(main_turn_loads_housedb(&configured(), &ClaudeCode));
        for cfg in unconfigured() {
            let args = claude_main_args(&cfg);
            assert!(!servers_on_argv(&args).contains_key("housedb"));
            assert!(!main_turn_loads_housedb(&cfg, &ClaudeCode));
            assert!(!main_turn_loads_housedb(&cfg, &Codex));
            assert!(!main_turn_loads_housedb(&cfg, &Direct));
        }
    }

    #[test]
    fn codex_registers_housedb_with_its_paths_forwarded_by_name() {
        let args = codex_mcp_args(
            CODEX_ID,
            &render_child_mcp(&configured(), MAIN_CHILD_MCP_CONFIG.as_str()),
            DEFAULT_ALLOWED_TOOLS,
        )
        .expect("renders");
        let flat = args.join("\n");
        assert!(
            flat.contains(r#"mcp_servers.housedb.command="jesse-housedb-mcp""#),
            "{flat}"
        );
        assert!(
            flat.contains(
                r#"mcp_servers.housedb.enabled_tools=["housedb_query", "housedb_schema"]"#
            ),
            "{flat}"
        );
        let env_vars = args
            .iter()
            .find(|a| a.starts_with("mcp_servers.housedb.env_vars="))
            .expect("Codex forwards the server's variables by name");
        for name in [
            HOUSEQ_ENV_RO_URL_FILE,
            HOUSEQ_ENV_CA_FILE,
            HOUSEQ_ENV_LEDGER,
        ] {
            assert!(env_vars.contains(name), "{env_vars}");
        }
        assert!(main_turn_loads_housedb(&configured(), &Codex));
    }

    #[test]
    fn direct_gets_a_stdio_grant_with_paths_only_when_configured() {
        let g = direct_housedb_grant(&configured()).expect("configured");
        assert_eq!(g.name, HOUSEDB_SERVER);
        assert_eq!(g.tools, ["housedb_query", "housedb_schema"]);
        assert_eq!(
            g.action_class_default,
            jesse_agent::tools::ActionClass::Egress
        );
        let jesse_agent::mcp::McpTransport::Stdio { command, env, .. } = &g.transport else {
            panic!("housedb is stdio");
        };
        assert_eq!(command, HOUSEDB_COMMAND);
        assert_eq!(env[HOUSEQ_ENV_RO_URL_FILE], RO_FILE);
        assert_eq!(env[HOUSEQ_ENV_CA_FILE], CA_FILE);
        assert_eq!(
            env[HOUSEQ_ENV_LEDGER],
            "/srv/jesse/state/housedb-mcp-calls.log"
        );
        assert!(main_turn_loads_housedb(&configured(), &Direct));
        for cfg in unconfigured() {
            assert!(direct_housedb_grant(&cfg).is_none());
        }
    }

    #[test]
    fn only_a_child_that_loads_housedb_gets_its_paths() {
        let names = [
            HOUSEQ_ENV_RO_URL_FILE,
            HOUSEQ_ENV_CA_FILE,
            HOUSEQ_ENV_LEDGER,
        ];
        let keys = |cmd: &Command| -> Vec<String> {
            cmd.as_std()
                .get_envs()
                .map(|(k, _)| k.to_string_lossy().into_owned())
                .collect()
        };
        let mut cmd = Command::new("true");
        apply_child_mcp_env(&mut cmd, &configured(), MAIN_CHILD_MCP_CONFIG.as_str());
        for n in names {
            assert!(keys(&cmd).iter().any(|k| k == n), "{n}");
        }
        let mut cmd = Command::new("true");
        apply_child_mcp_env(&mut cmd, &configured(), McpSet::Replies.config());
        assert!(keys(&cmd).iter().all(|k| !names.contains(&k.as_str())));
        for cfg in unconfigured() {
            let mut cmd = Command::new("true");
            apply_child_mcp_env(&mut cmd, &cfg, MAIN_CHILD_MCP_CONFIG.as_str());
            assert!(keys(&cmd).iter().all(|k| !names.contains(&k.as_str())));
        }
    }
}
