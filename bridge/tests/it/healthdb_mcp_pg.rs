//! `jesse-healthdb-mcp`'s two tools against a real Postgres, connected as a read-only role.
//!
//! Runs only when `JESSE_TEST_PG_URL` names a server it may create databases and roles on
//! (CI's `postgres:16` service container); without it each test returns at once. Each test
//! gets a fresh database with the archive's schema, a few seeded rows, and its own `health_ro`
//! style role that may `SELECT` from `raw` and nothing else, and drops all of it at the end.

use std::str::FromStr;
use std::time::{Duration, Instant};

use jesse_bridge::*;
use serde_json::{json, Value};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

const HR: &str = "HKQuantityTypeIdentifierHeartRate";
const SLEEP: &str = "HKCategoryTypeIdentifierSleepAnalysis";

fn pg_url() -> Option<String> {
    std::env::var("JESSE_TEST_PG_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

struct Scratch {
    admin: PgConnectOptions,
    name: String,
    role: String,
    route: Uuid,
}

impl Scratch {
    /// A database with the migrated `raw` schema, a route of 1000 points, a few samples, and a
    /// role with `SELECT` on `raw` only.
    async fn new() -> Option<Scratch> {
        let admin = PgConnectOptions::from_str(&pg_url()?).expect("JESSE_TEST_PG_URL parses");
        let id = Uuid::new_v4().simple().to_string();
        let name = format!("hmcp_{id}");
        let role = format!("hro_{}", &id[..12]);
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
        sqlx::raw_sql("CREATE SCHEMA raw")
            .execute(&mut conn)
            .await
            .unwrap();
        conn.close().await.ok();
        let db = HealthDb::from_options(opts.clone());
        db.ensure_migrated().await.expect("migrate");
        db.pool().close().await;

        let route = Uuid::new_v4();
        let mut conn = PgConnection::connect_with(&opts).await.expect("connect");
        let seed = format!(
            "INSERT INTO raw.route (uuid, workout_uuid, source, source_name, n_points, downsampled)
               VALUES ('{route}', '{w}', 'watch', 'Watch', 1000, false);
             INSERT INTO raw.route_point (route_uuid, idx, ts, lat, lon, alt)
               SELECT '{route}', g, '2026-05-01T08:00:00Z'::timestamptz + g * interval '1 second',
                      44.0587312345678 + g * 0.00001, 10.4618876543219 + g * 0.00001, 300.5 + g
                 FROM generate_series(0, 999) g;
             INSERT INTO raw.quantity_sample
               (uuid, seq, type, start_ts, end_ts, value, unit, source, source_name)
               VALUES ('{a}', 0, '{HR}', '2026-01-02T10:00:00Z', '2026-01-02T10:00:00Z', 61, 'count/min', 'w', 'W'),
                      ('{b}', 0, '{HR}', '2026-03-04T10:00:00Z', '2026-03-04T10:00:00Z', 72, 'count/min', 'w', 'W');
             INSERT INTO raw.category_sample
               (uuid, type, start_ts, end_ts, value, source, source_name)
               VALUES ('{c}', '{SLEEP}', '2026-02-01T23:00:00Z', '2026-02-02T01:00:00Z', 4, 'w', 'W');
             GRANT CONNECT ON DATABASE {name} TO {role};
             GRANT USAGE ON SCHEMA raw TO {role};
             GRANT SELECT ON ALL TABLES IN SCHEMA raw TO {role};
             ANALYZE;",
            w = Uuid::new_v4(),
            a = Uuid::new_v4(),
            b = Uuid::new_v4(),
            c = Uuid::new_v4(),
        );
        sqlx::raw_sql(&seed).execute(&mut conn).await.expect("seed");
        conn.close().await.ok();
        Some(Scratch {
            admin,
            name,
            role,
            route,
        })
    }

    /// The server's config, as the read-only role.
    fn ro(&self) -> HealthQueryConfig {
        HealthQueryConfig::from_options(
            self.admin
                .clone()
                .database(&self.name)
                .username(&self.role)
                .password("ro"),
        )
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

async fn query(cfg: &HealthQueryConfig, sql: &str) -> ToolReply {
    run_health_query(cfg, &json!({ "sql": sql })).await
}

fn body(r: &ToolReply) -> Value {
    assert!(!r.is_error, "expected success, got: {}", r.text);
    serde_json::from_str(&r.text).expect("a success body is JSON")
}

#[tokio::test]
async fn writes_ddl_and_role_changes_all_fail() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let cfg = s.ro();

    // Refused by the server's own guard, before Postgres sees them.
    for sql in [
        "INSERT INTO raw.diet_file (file, sha256) VALUES ('x', 'y')",
        "UPDATE raw.route_point SET lat = 0",
        "CREATE TABLE raw.evil (a int)",
        "SET ROLE postgres",
        "SELECT 1; DELETE FROM raw.route_point",
    ] {
        let r = query(&cfg, sql).await;
        assert_eq!(r.outcome, QueryOutcome::Rejected, "{sql}: {}", r.text);
        assert!(r.is_error);
    }

    // Past the guard, a write in a read statement is refused: as the read-only role, by its
    // grants; and as the OWNER, who may write, by the read-only transaction. The second is the
    // point: the server's guard holds whatever the role turns out to be allowed.
    let owner = HealthQueryConfig::from_options(s.admin.clone().database(&s.name));
    for sql in [
        "WITH x AS (INSERT INTO raw.diet_file (file, sha256) VALUES ('x', 'y') RETURNING 1) SELECT * FROM x",
        "WITH x AS (UPDATE raw.route_point SET lat = 0 RETURNING 1) SELECT count(*) FROM x",
        "SELECT * FROM raw.route_point FOR UPDATE",
    ] {
        let r = query(&cfg, sql).await;
        assert_eq!(r.outcome, QueryOutcome::Error, "{sql}: {}", r.text);
        assert!(r.text.contains("42501"), "{sql}: {}", r.text);
        // As the owner: a data-modifying CTE cannot sit inside the row cap's subquery
        // (0A000), and anything that could write is refused by the transaction (25006).
        let r = query(&owner, sql).await;
        assert_eq!(r.outcome, QueryOutcome::Error, "{sql}: {}", r.text);
        assert!(
            r.text.contains("25006") || r.text.contains("0A000"),
            "{sql}: {}",
            r.text
        );
    }
    // A write that IS a plain SELECT reaches the transaction, and the transaction refuses it.
    let r = query(&owner, "SELECT lo_create(0)").await;
    assert_eq!(r.outcome, QueryOutcome::Error, "{}", r.text);
    assert!(r.text.contains("25006"), "{}", r.text);
    // The statement timeout is not the owner's to lift either: SET inside the statement is
    // refused by the guard, and set_config is rolled back with everything else.
    let r = query(&owner, "SELECT set_config('statement_timeout', '0', false)").await;
    assert!(!r.is_error, "{}", r.text);
    let r = query(&owner, "SELECT current_setting('statement_timeout')").await;
    assert_eq!(body(&r)["rows"][0][0], json!("15s"));

    // A role change through a function is refused by Postgres: the role is not a member.
    let r = query(&cfg, "SELECT set_config('role', 'postgres', false)").await;
    assert_eq!(r.outcome, QueryOutcome::Error, "{}", r.text);
    assert!(r.text.contains("Postgres error"), "{}", r.text);

    // And nothing moved.
    let r = query(
        &cfg,
        "SELECT count(*) AS n, min(lat) AS lat FROM raw.route_point",
    )
    .await;
    let b = body(&r);
    assert_eq!(b["rows"][0][0], json!(1000));
    assert!(b["rows"][0][1].as_f64().unwrap() > 44.0);
    let r = query(&cfg, "SELECT count(*) FROM raw.diet_file").await;
    assert_eq!(body(&r)["rows"][0][0], json!(0));

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
        took >= Duration::from_secs(14) && took < Duration::from_secs(20),
        "{took:?}"
    );
    s.drop_all().await;
}

#[tokio::test]
async fn a_thousand_row_select_returns_five_hundred_with_the_flag() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let r = query(
        &s.ro(),
        "SELECT idx, lat, lon FROM raw.route_point ORDER BY idx",
    )
    .await;
    assert_eq!(r.rows, 500);
    let b = body(&r);
    assert_eq!(b["row_count"], json!(500));
    assert_eq!(b["rows"].as_array().unwrap().len(), 500);
    assert_eq!(b["rows_truncated"], json!(true));
    assert_eq!(b["rows"][499][0], json!(499));
    assert!(b["notes"][0].as_str().unwrap().contains("500"));

    let r = query(&s.ro(), "SELECT repeat('x', 700) AS long").await;
    let b = body(&r);
    assert_eq!(b["values_truncated"], json!(true));
    assert_eq!(b["rows"][0][0].as_str().unwrap().len(), 500);
    s.drop_all().await;
}

#[tokio::test]
async fn route_points_come_back_with_their_coordinates_intact() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let r = query(
        &s.ro(),
        &format!(
            "SELECT idx, lat, lon, alt FROM raw.route_point \
             WHERE route_uuid = '{}' AND idx IN (0, 7) ORDER BY idx",
            s.route
        ),
    )
    .await;
    let b = body(&r);
    assert_eq!(
        b["columns"],
        json!([
            {"name": "idx", "type": "INT4"},
            {"name": "lat", "type": "FLOAT8"},
            {"name": "lon", "type": "FLOAT8"},
            {"name": "alt", "type": "FLOAT8"},
        ])
    );
    assert_eq!(
        b["rows"][0],
        json!([0, 44.0587312345678, 10.4618876543219, 300.5])
    );
    let lat7 = b["rows"][1][1].as_f64().unwrap();
    assert!(
        (lat7 - (44.0587312345678 + 7.0 * 0.00001)).abs() < 1e-12,
        "{lat7}"
    );
    s.drop_all().await;
}

#[tokio::test]
async fn the_schema_lists_the_seeded_tables_types_and_guide() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let r = run_health_schema(&s.ro()).await;
    let b = body(&r);
    let rel = |name: &str| {
        b["relations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["schema"] == "raw" && r["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("raw.{name} missing: {b}"))
    };
    let points = rel("route_point");
    assert_eq!(points["kind"], json!("table"));
    assert_eq!(points["rows"], json!(1000));
    assert_eq!(points["rows_estimated"], json!(false));
    assert!(points["columns"]
        .as_array()
        .unwrap()
        .contains(&json!({"name": "lat", "type": "double precision"})));
    let q = rel("quantity_sample");
    assert!(q["columns"]
        .as_array()
        .unwrap()
        .contains(&json!({"name": "start_ts", "type": "timestamp with time zone"})));
    assert_eq!(q["rows"], json!(2));
    for name in ["category_sample", "workout", "route", "diet_row"] {
        rel(name);
    }
    assert!(!b["relations"].to_string().contains("_sqlx_migrations"));

    assert_eq!(
        b["quantity_types"],
        json!([{"type": HR, "units": ["count/min"], "first": "2026-01-02", "last": "2026-03-04"}])
    );
    assert_eq!(
        b["category_types"],
        json!([{"type": SLEEP, "first": "2026-02-02", "last": "2026-02-02"}])
    );
    assert!(b["guide"].to_string().contains("5 REM"));
    s.drop_all().await;
}
