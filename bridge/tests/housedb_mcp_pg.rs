//! `jesse-housedb-mcp`'s two tools against a real Postgres, connected as a read-only role.
//!
//! Runs only when `JESSE_TEST_PG_URL` names a server it may create databases and roles on
//! (CI's `postgres:16` service container); without it each test returns at once. Each test
//! gets a fresh database shaped like Home Assistant's recorder on the home cluster: a few
//! recorder tables in `public`, and in `analytics` the `house_daily_energy` view over its
//! `_house_daily_energy` table, a live `house_unavailable` view and the `house_entity` table.
//! Its own `house_ro` style role may `SELECT` from both schemas and nothing else. All of it is
//! dropped at the end.
//!
//! The guards are the ones `healthdb` has (`tests/healthdb_mcp_pg.rs`): this proves they hold
//! on the house server too, because it is the same code with another configuration.

use std::str::FromStr;
use std::time::{Duration, Instant};

use jesse_bridge::*;
use serde_json::{json, Value};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

fn pg_url() -> Option<String> {
    std::env::var("JESSE_TEST_PG_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

struct Scratch {
    admin: PgConnectOptions,
    name: String,
    role: String,
}

impl Scratch {
    async fn new() -> Option<Scratch> {
        let admin = PgConnectOptions::from_str(&pg_url()?).expect("JESSE_TEST_PG_URL parses");
        let id = Uuid::new_v4().simple().to_string();
        let name = format!("hsmcp_{id}");
        let role = format!("hsro_{}", &id[..12]);
        let mut conn = PgConnection::connect_with(&admin).await.expect("connect");
        for sql in [
            format!("CREATE DATABASE {name}"),
            format!("CREATE ROLE {role} LOGIN PASSWORD 'ro'"),
        ] {
            sqlx::raw_sql(&sql).execute(&mut conn).await.expect(&sql);
        }
        conn.close().await.ok();

        let opts = admin.clone().database(&name);
        let mut conn = PgConnection::connect_with(&opts).await.expect("connect");
        let seed = format!(
            "CREATE TABLE public.states_meta (metadata_id bigserial PRIMARY KEY, entity_id text);
             CREATE TABLE public.states (state_id bigserial PRIMARY KEY, state text,
               last_updated_ts double precision, metadata_id bigint);
             INSERT INTO public.states_meta (entity_id) VALUES ('sensor.stato_temperatura_esterna');
             INSERT INTO public.states (state, last_updated_ts, metadata_id)
               SELECT (10 + g % 7)::text, 1795000000 + g * 60, 1 FROM generate_series(0, 999) g;
             CREATE SCHEMA analytics;
             CREATE TABLE analytics._house_daily_energy (day date PRIMARY KEY,
               heat_pump_kwh numeric, outdoor_mean_c double precision, complete boolean);
             INSERT INTO analytics._house_daily_energy VALUES
               ('2026-11-18', 31, 7.5, true), ('2026-11-19', 29, 8.25, true),
               ('2026-11-20', 12, 9.5, false);
             CREATE VIEW analytics.house_daily_energy AS SELECT * FROM analytics._house_daily_energy;
             CREATE VIEW analytics.house_unavailable AS
               SELECT m.entity_id, s.state FROM public.states s
                 JOIN public.states_meta m USING (metadata_id) WHERE s.state = 'unavailable';
             CREATE TABLE analytics.house_entity (role text, label text, entity_id text);
             GRANT CONNECT ON DATABASE {name} TO {role};
             GRANT USAGE ON SCHEMA public, analytics TO {role};
             GRANT SELECT ON ALL TABLES IN SCHEMA public, analytics TO {role};
             ALTER ROLE {role} SET search_path = analytics, public;
             ANALYZE;"
        );
        sqlx::raw_sql(&seed).execute(&mut conn).await.expect("seed");
        conn.close().await.ok();
        Some(Scratch { admin, name, role })
    }

    /// The server's config, as the read-only role.
    fn ro(&self) -> RoDbConfig {
        RoDbConfig::from_options(
            &HOUSEDB,
            self.admin
                .clone()
                .database(&self.name)
                .username(&self.role)
                .password("ro"),
        )
    }

    /// As the OWNER, who may write: proves the bridge's own guard holds whatever the role is.
    fn owner(&self) -> RoDbConfig {
        RoDbConfig::from_options(&HOUSEDB, self.admin.clone().database(&self.name))
    }

    async fn drop_all(self) {
        if let Ok(mut conn) = PgConnection::connect_with(&self.admin).await {
            let _ = sqlx::raw_sql(&format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE); DROP ROLE IF EXISTS {}",
                self.name, self.role
            ))
            .execute(&mut conn)
            .await;
        }
    }
}

async fn query(cfg: &RoDbConfig, sql: &str) -> ToolReply {
    run_db_tool(cfg, DbTool::Query, &json!({ "sql": sql })).await
}

fn body(r: &ToolReply) -> Value {
    assert!(!r.is_error, "expected success, got: {}", r.text);
    serde_json::from_str(&r.text).expect("a success body is JSON")
}

#[tokio::test]
async fn an_insert_fails_and_nothing_moves() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let cfg = s.ro();

    // Refused by the server's own guard, before Postgres sees it.
    for sql in [
        "INSERT INTO public.states (state) VALUES ('x')",
        "insert into analytics._house_daily_energy (day) values ('2026-01-01')",
        "DELETE FROM public.states",
        "SELECT 1; INSERT INTO public.states (state) VALUES ('x')",
    ] {
        let r = query(&cfg, sql).await;
        assert_eq!(r.outcome, QueryOutcome::Rejected, "{sql}: {}", r.text);
        assert!(r.is_error);
    }

    // An INSERT that gets past the guard inside a read: refused by the role's grants as
    // house_ro (42501), and by the read-only transaction as the owner (25006, or 0A000 for a
    // data-modifying CTE under the row cap's subquery).
    let sql =
        "WITH x AS (INSERT INTO public.states (state) VALUES ('x') RETURNING 1) SELECT * FROM x";
    let r = query(&cfg, sql).await;
    assert_eq!(r.outcome, QueryOutcome::Error, "{}", r.text);
    assert!(r.text.contains("42501"), "{}", r.text);
    let r = query(&s.owner(), sql).await;
    assert_eq!(r.outcome, QueryOutcome::Error, "{}", r.text);
    assert!(
        r.text.contains("25006") || r.text.contains("0A000"),
        "{}",
        r.text
    );
    let r = query(&s.owner(), "SELECT lo_create(0)").await;
    assert_eq!(r.outcome, QueryOutcome::Error, "{}", r.text);
    assert!(r.text.contains("25006"), "{}", r.text);

    let r = query(&cfg, "SELECT count(*) FROM public.states").await;
    assert_eq!(body(&r)["rows"][0][0], json!(1000));
    s.drop_all().await;
}

#[tokio::test]
async fn the_row_cap_stops_at_five_hundred_with_the_flag() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let r = query(
        &s.ro(),
        "SELECT state_id, state FROM public.states ORDER BY state_id",
    )
    .await;
    assert_eq!(r.rows, 500);
    let b = body(&r);
    assert_eq!(b["row_count"], json!(500));
    assert_eq!(b["rows_truncated"], json!(true));
    assert!(b["notes"][0].as_str().unwrap().contains("500"));
    s.drop_all().await;
}

#[tokio::test]
async fn a_long_statement_times_out_at_fifteen_seconds() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let started = Instant::now();
    let r = query(&s.ro(), "SELECT pg_sleep(20)").await;
    let took = started.elapsed();
    assert_eq!(r.outcome, QueryOutcome::Timeout, "{}", r.text);
    assert!(r.text.contains("57014"), "{}", r.text);
    assert!(
        r.text.contains("metadata_id and last_updated_ts"),
        "{}",
        r.text
    );
    assert!(
        took >= Duration::from_secs(14) && took < Duration::from_secs(20),
        "{took:?}"
    );
    s.drop_all().await;
}

#[tokio::test]
async fn the_morning_query_reads_the_daily_energy_view() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    // The query a morning run asks, unqualified, as the role's search_path resolves it.
    let r = query(
        &s.ro(),
        "select * from house_daily_energy order by 1 desc limit 3",
    )
    .await;
    let b = body(&r);
    assert_eq!(b["rows"][0], json!(["2026-11-20", 12, 9.5, false]));
    assert_eq!(b["rows"][2][0], json!("2026-11-18"));
    s.drop_all().await;
}

#[tokio::test]
async fn the_schema_lists_the_house_views_first_then_the_recorder() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let r = run_db_tool(&s.ro(), DbTool::Schema, &json!({})).await;
    let b = body(&r);
    let order: Vec<String> = b["relations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            format!(
                "{}.{}",
                r["schema"].as_str().unwrap(),
                r["name"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            "analytics.house_daily_energy",
            "analytics.house_unavailable",
            "analytics.house_entity",
            "public.states",
            "public.states_meta",
        ]
    );
    let view = &b["relations"][0];
    assert_eq!(view["kind"], json!("view"));
    assert_eq!(view["rows"], json!(null));
    assert!(view.get("rank").is_none(), "{view}");
    assert!(view["columns"]
        .as_array()
        .unwrap()
        .contains(&json!({"name": "heat_pump_kwh", "type": "numeric"})));
    assert_eq!(b["relations"][3]["rows"], json!(1000));
    // The health layout's sample types are not the house's.
    assert!(b.get("quantity_types").is_none());
    assert!(b["guide"]
        .to_string()
        .contains("Prefer the analytics.house_* views"));
    s.drop_all().await;
}
