//! The health archive against a real Postgres.
//!
//! Every test here runs only when `JESSE_TEST_PG_URL` names a server it may create databases
//! on (CI's `postgres:16` service container); without it each test returns at once, so a bare
//! `cargo test` needs no database. Each test gets a fresh database with an empty `raw` schema,
//! as the deployment provides one, and drops it at the end.
//!
//! The pool here is built with `HealthDb::from_options`, the same constructor the production
//! pool goes through, minus the verify-full TLS the service container does not offer. The TLS
//! options themselves are unit tested in `src/healthdb.rs`.

mod common;

use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use jesse_bridge::*;
use serde_json::{json, Value};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tower::ServiceExt;
use uuid::Uuid;

const HR: &str = "HKQuantityTypeIdentifierHeartRate";

fn pg_url() -> Option<String> {
    std::env::var("JESSE_TEST_PG_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// A fresh database with an empty `raw` schema, and its options.
struct Scratch {
    admin: PgConnectOptions,
    name: String,
}

impl Scratch {
    async fn new() -> Option<Scratch> {
        let admin = PgConnectOptions::from_str(&pg_url()?).expect("JESSE_TEST_PG_URL parses");
        let name = format!("hdb_{}", Uuid::new_v4().simple());
        let mut conn = PgConnection::connect_with(&admin).await.expect("connect");
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&mut conn)
            .await
            .expect("create database");
        conn.close().await.ok();
        let mut conn = PgConnection::connect_with(&admin.clone().database(&name))
            .await
            .expect("connect scratch");
        sqlx::query("CREATE SCHEMA raw")
            .execute(&mut conn)
            .await
            .expect("create schema");
        conn.close().await.ok();
        Some(Scratch { admin, name })
    }

    fn options(&self) -> PgConnectOptions {
        self.admin.clone().database(&self.name)
    }

    fn db(&self) -> Arc<HealthDb> {
        Arc::new(HealthDb::from_options(self.options()))
    }

    async fn drop_db(self, db: &HealthDb) {
        db.pool().close().await;
        if let Ok(mut conn) = PgConnection::connect_with(&self.admin).await {
            let _ = sqlx::query(&format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                self.name
            ))
            .execute(&mut conn)
            .await;
        }
    }
}

fn state_with(db: Arc<HealthDb>) -> AppState {
    let mut st = common::test_state();
    st.healthdb = Some(db);
    st
}

async fn post(st: &AppState, body: &Value) -> (StatusCode, Value) {
    let res = app(st.clone())
        .oneshot(
            Request::post("/jesse/health/ingest")
                .header("authorization", "Bearer test-token")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn count(db: &HealthDb, sql: &str, uuid: Uuid) -> i64 {
    sqlx::query_scalar(sql)
        .bind(uuid)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

fn hr(uuid: Uuid, seq: i64, value: f64) -> Value {
    json!({
        "uuid": uuid.to_string(), "seq": seq,
        "start": "2026-10-09T08:59:00+02:00", "end": "2026-10-09T08:59:05+02:00",
        "value": value, "unit": "count/min", "source": "com.apple.health.x",
        "source_name": "Watch", "device": null,
        "metadata": {"HKMetadataKeyHeartRateMotionContext": 2},
    })
}

fn quantity(samples: Vec<Value>, deleted: Vec<Uuid>) -> Value {
    json!({
        "schema": 1, "kind": "quantity", "type": HR, "samples": samples,
        "deleted": deleted.iter().map(Uuid::to_string).collect::<Vec<_>>(),
    })
}

#[tokio::test]
async fn migrations_apply_twice_cleanly() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let db = s.db();
    HEALTHDB_MIGRATOR.run(db.pool()).await.expect("first run");
    HEALTHDB_MIGRATOR.run(db.pool()).await.expect("second run");
    db.ensure_migrated().await.expect("ensure_migrated");
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'raw' \
         ORDER BY 1",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        tables,
        [
            "_sqlx_migrations",
            "category_sample",
            "deleted_sample",
            "diet_file",
            "diet_row",
            "quantity_sample",
            "route",
            "route_point",
            "workout",
            "workout_event"
        ]
    );
    let analytics: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(analytics, 0, "nothing lands outside `raw`");
    s.drop_db(&db).await;
}

#[tokio::test]
async fn an_identical_batch_twice_leaves_one_row_and_a_changed_value_updates_in_place() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let db = s.db();
    let st = state_with(db.clone());
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let batch = quantity(
        vec![hr(a, 0, 142.0), hr(a, 1, 143.0), hr(b, 0, 250.0)],
        vec![],
    );
    for _ in 0..2 {
        let (status, body) = post(&st, &batch).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"upserted": 3, "deleted": 0}));
    }
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM raw.quantity_sample")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(total, 3, "one row per (uuid, seq)");

    let (status, _) = post(&st, &quantity(vec![hr(a, 1, 99.5)], vec![])).await;
    assert_eq!(status, StatusCode::OK);
    let value: f64 =
        sqlx::query_scalar("SELECT value FROM raw.quantity_sample WHERE uuid = $1 AND seq = 1")
            .bind(a)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(value, 99.5);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM raw.quantity_sample WHERE uuid = $1",
            a
        )
        .await,
        2
    );
    let start: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT start_ts FROM raw.quantity_sample WHERE uuid = $1 AND seq = 0")
            .bind(a)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(start.to_rfc3339(), "2026-10-09T06:59:00+00:00");

    // A category batch: the same upsert, integer values, no seq.
    let c = Uuid::new_v4();
    let mut sleep = hr(c, 0, 0.0);
    sleep.as_object_mut().unwrap().remove("seq");
    sleep.as_object_mut().unwrap().remove("unit");
    sleep["value"] = json!(3);
    let cat = json!({"schema": 1, "kind": "category",
        "type": "HKCategoryTypeIdentifierSleepAnalysis", "samples": [sleep.clone()]});
    assert_eq!(post(&st, &cat).await.0, StatusCode::OK);
    sleep["value"] = json!(4);
    let cat = json!({"schema": 1, "kind": "category",
        "type": "HKCategoryTypeIdentifierSleepAnalysis", "samples": [sleep]});
    assert_eq!(post(&st, &cat).await.0, StatusCode::OK);
    let v: i32 = sqlx::query_scalar("SELECT value FROM raw.category_sample WHERE uuid = $1")
        .bind(c)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(v, 4);
    s.drop_db(&db).await;
}

#[tokio::test]
async fn a_deleted_uuid_removes_every_seq_and_writes_one_tombstone() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let db = s.db();
    let st = state_with(db.clone());
    let (series, other) = (Uuid::new_v4(), Uuid::new_v4());
    let samples = (0..5).map(|i| hr(series, i, 60.0 + i as f64)).collect();
    assert_eq!(
        post(&st, &quantity(samples, vec![])).await.0,
        StatusCode::OK
    );
    assert_eq!(
        post(&st, &quantity(vec![hr(other, 0, 70.0)], vec![]))
            .await
            .0,
        StatusCode::OK
    );
    for _ in 0..2 {
        let (status, body) = post(&st, &quantity(vec![], vec![series])).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"upserted": 0, "deleted": 1}));
    }
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM raw.quantity_sample WHERE uuid = $1",
            series
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM raw.quantity_sample WHERE uuid = $1",
            other
        )
        .await,
        1
    );
    let tomb: Vec<String> =
        sqlx::query_scalar("SELECT type FROM raw.deleted_sample WHERE uuid = $1")
            .bind(series)
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        tomb,
        [HR],
        "one tombstone, typed, however often it is resent"
    );
    s.drop_db(&db).await;
}

fn workout(uuid: Uuid, events: usize) -> Value {
    let events: Vec<Value> = (0..events)
        .map(|i| {
            json!({"type": "lap", "start": "2026-10-09T07:00:00Z", "end": "2026-10-09T07:05:00Z",
                "metadata": {"HKSwimmingStrokeStyle": i}})
        })
        .collect();
    json!({"schema": 1, "kind": "workout", "type": "HKWorkoutTypeIdentifier", "samples": [{
        "uuid": uuid.to_string(), "start": "2026-10-09T07:00:00Z", "end": "2026-10-09T08:00:00Z",
        "duration_s": 3600.0, "activity_type": 46, "activity_name": "swimming",
        "distance_m": 2000.0, "energy_kcal": 500.0, "avg_hr": 130.0, "max_hr": null,
        "effort": null, "indoor": true, "source": "s", "source_name": "n", "device": "Watch8,1",
        "metadata": null, "events": events}]})
}

#[tokio::test]
async fn a_workout_resend_replaces_its_events_and_a_delete_cascades_them() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let db = s.db();
    let st = state_with(db.clone());
    let w = Uuid::new_v4();
    let events = "SELECT count(*) FROM raw.workout_event WHERE workout_uuid = $1";
    assert_eq!(post(&st, &workout(w, 4)).await.0, StatusCode::OK);
    assert_eq!(count(&db, events, w).await, 4);
    assert_eq!(post(&st, &workout(w, 1)).await.0, StatusCode::OK);
    assert_eq!(count(&db, events, w).await, 1, "replaced, not appended");
    let stroke: Value = sqlx::query_scalar(
        "SELECT metadata FROM raw.workout_event WHERE workout_uuid = $1 AND idx = 0",
    )
    .bind(w)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(stroke, json!({"HKSwimmingStrokeStyle": 0}));

    let mut del = workout(w, 0);
    del["samples"] = json!([]);
    del["deleted"] = json!([w.to_string()]);
    assert_eq!(post(&st, &del).await.0, StatusCode::OK);
    assert_eq!(
        count(&db, "SELECT count(*) FROM raw.workout WHERE uuid = $1", w).await,
        0
    );
    assert_eq!(count(&db, events, w).await, 0);
    s.drop_db(&db).await;
}

fn route(uuid: Uuid, workout: Uuid, n: usize) -> Value {
    // Realistic precision, so the 100000-point body is the size the phone will send.
    let points: Vec<Value> = (0..n)
        .map(|i| {
            let f = i as f64;
            json!({"t": format!("2026-10-09T07:{:02}:{:02}.{:03}+02:00",
                    (i / 1000) % 60, (i / 17) % 60, i % 1000),
                "lat": 45.437_191_234_567 + f * 1.234_567e-6,
                "lon": 12.334_590_123_456 - f * 2.345_678e-6,
                "alt": 12.5 + (f * 0.01).sin(), "h_acc": 4.7, "v_acc": 3.2,
                "speed": 3.27, "course": if i % 7 == 0 { Value::Null } else { json!(181.25) }})
        })
        .collect();
    json!({"schema": 1, "kind": "route", "type": "HKWorkoutRouteTypeIdentifier", "samples": [{
        "uuid": uuid.to_string(), "workout_uuid": workout.to_string(), "source": "s",
        "source_name": "n", "downsampled": n == ROUTE_MAX_POINTS, "points": points}]})
}

#[tokio::test]
async fn a_route_of_100000_points_commits_and_a_resend_replaces_it() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let db = s.db();
    let st = state_with(db.clone());
    db.ensure_migrated().await.unwrap();
    let (r, w) = (Uuid::new_v4(), Uuid::new_v4());
    let points = "SELECT count(*) FROM raw.route_point WHERE route_uuid = $1";

    let big = route(r, w, ROUTE_MAX_POINTS);
    let size = serde_json::to_vec(&big).unwrap().len();
    assert!(
        size > INGEST_MAX_BODY && size <= INGEST_MAX_ROUTE_BODY,
        "{size}"
    );
    let began = Instant::now();
    let (status, body) = post(&st, &big).await;
    let took = began.elapsed();
    eprintln!("100000-point route ({size} bytes) committed in {took:?}");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(took < Duration::from_secs(30), "{took:?}");
    assert_eq!(count(&db, points, r).await, ROUTE_MAX_POINTS as i64);
    let (n, down): (i32, bool) =
        sqlx::query_as("SELECT n_points, downsampled FROM raw.route WHERE uuid = $1")
            .bind(r)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!((n, down), (ROUTE_MAX_POINTS as i32, true));
    let (lat, course): (f64, Option<f64>) =
        sqlx::query_as("SELECT lat, course FROM raw.route_point WHERE route_uuid = $1 AND idx = 7")
            .bind(r)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(lat, 45.437_191_234_567 + 7.0 * 1.234_567e-6);
    assert_eq!(course, None);

    assert_eq!(post(&st, &route(r, w, 500)).await.0, StatusCode::OK);
    assert_eq!(
        count(&db, points, r).await,
        500,
        "a resend replaces every point"
    );

    let mut off = route(r, w, 3);
    off["samples"][0]["points"][1]["lat"] = json!(91.0);
    let (status, body) = post(&st, &off).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!body.to_string().contains("91"), "{body}");
    assert_eq!(
        count(&db, points, r).await,
        500,
        "a refused batch writes nothing"
    );

    let del = json!({"schema": 1, "kind": "route", "type": "HKWorkoutRouteTypeIdentifier",
        "samples": [], "deleted": [r.to_string()]});
    assert_eq!(post(&st, &del).await.0, StatusCode::OK);
    assert_eq!(count(&db, points, r).await, 0, "points cascade");
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM raw.deleted_sample WHERE uuid = $1",
            r
        )
        .await,
        1
    );
    s.drop_db(&db).await;
}

#[tokio::test]
async fn a_changed_diet_file_replaces_its_rows_and_an_unchanged_one_is_skipped() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let db = s.db();
    db.ensure_migrated().await.unwrap();
    let dir = std::env::temp_dir().join(format!("healthdb-diet-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = "food-log.csv";
    let rows = "SELECT count(*) FROM raw.diet_row WHERE file = $1";
    let n = |db: Arc<HealthDb>| async move {
        sqlx::query_scalar::<_, i64>(rows)
            .bind(file)
            .fetch_one(db.pool())
            .await
            .unwrap()
    };

    std::fs::write(
        dir.join(file),
        "Date,Item,Notes\n2026-10-01,eggs,\"two, scrambled\nwith butter\"\n2026-10-01,toast,\n\
         2026-10-02,soup,x\n",
    )
    .unwrap();
    let before = healthdb_counter(METRIC_DIET_RELOAD, file);
    let out = mirror_diet_once(&db, &dir, &[file, "weight-log.csv"]).await;
    assert_eq!(out[0].1, DietMirrorOutcome::Reloaded(3));
    assert_eq!(out[1].1, DietMirrorOutcome::Missing);
    assert_eq!(n(db.clone()).await, 3);
    let (date, notes): (chrono::NaiveDate, Value) =
        sqlx::query_as("SELECT date, data FROM raw.diet_row WHERE file = $1 AND row_no = 1")
            .bind(file)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(date, chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
    assert_eq!(notes["Notes"], json!("two, scrambled\nwith butter"));

    let out = mirror_diet_once(&db, &dir, &[file]).await;
    assert_eq!(out[0].1, DietMirrorOutcome::Unchanged);
    assert_eq!(healthdb_counter(METRIC_DIET_RELOAD, file), before + 1);

    std::fs::write(dir.join(file), "Date,Item,Notes\n2026-10-03,rice,\n").unwrap();
    let out = mirror_diet_once(&db, &dir, &[file]).await;
    assert_eq!(out[0].1, DietMirrorOutcome::Reloaded(1));
    assert_eq!(n(db.clone()).await, 1, "rows replaced, none left over");

    std::fs::write(
        dir.join(file),
        "Date,Item\n2026-10-04,a\n2026-10-05,b,extra\n",
    )
    .unwrap();
    let out = mirror_diet_once(&db, &dir, &[file]).await;
    assert_eq!(out[0].1, DietMirrorOutcome::ParseFailed(3));
    assert_eq!(
        n(db.clone()).await,
        1,
        "a parse failure keeps the previous mirror"
    );

    let _ = std::fs::remove_dir_all(&dir);
    s.drop_db(&db).await;
}

/// A TCP relay in front of the real server, so a test can take Postgres away mid-test.
struct Relay {
    port: u16,
    accept: JoinHandle<()>,
    conns: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Relay {
    async fn start(target: String) -> Relay {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let conns: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::default();
        let held = conns.clone();
        let accept = tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                let target = target.clone();
                let h = tokio::spawn(async move {
                    if let Ok(mut out) = TcpStream::connect(target).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut out).await;
                    }
                });
                held.lock().unwrap().push(h);
            }
        });
        Relay {
            port,
            accept,
            conns,
        }
    }

    /// Stop listening and cut every open connection: the server is gone.
    fn stop(&self) {
        self.accept.abort();
        for h in self.conns.lock().unwrap().drain(..) {
            h.abort();
        }
    }
}

#[tokio::test]
async fn postgres_stopped_mid_test_is_a_503_unavailable_never_a_200() {
    let Some(s) = Scratch::new().await else {
        return;
    };
    let direct = s.options();
    let relay = Relay::start(format!("{}:{}", direct.get_host(), direct.get_port())).await;
    let db = Arc::new(HealthDb::from_options(
        direct.clone().host("127.0.0.1").port(relay.port),
    ));
    let st = state_with(db.clone());
    let a = Uuid::new_v4();
    let (status, _) = post(&st, &quantity(vec![hr(a, 0, 60.0)], vec![])).await;
    assert_eq!(status, StatusCode::OK, "through the relay while it is up");

    relay.stop();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let b = Uuid::new_v4();
    let before = healthdb_counter(METRIC_INGEST_ERRORS, "unavailable");
    let (status, body) = post(&st, &quantity(vec![hr(b, 0, 61.0)], vec![])).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, json!({"error": "unavailable"}));
    assert!(healthdb_counter(METRIC_INGEST_ERRORS, "unavailable") > before);

    // Nothing was written behind the 503.
    let check = Arc::new(HealthDb::from_options(direct));
    assert_eq!(
        count(
            &check,
            "SELECT count(*) FROM raw.quantity_sample WHERE uuid = $1",
            b
        )
        .await,
        0
    );
    db.pool().close().await;
    s.drop_db(&check).await;
}

#[tokio::test]
async fn a_server_that_was_never_up_is_a_503_unavailable() {
    if pg_url().is_none() {
        return;
    }
    // A port nothing listens on: bind one, learn its number, close it.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let db = Arc::new(HealthDb::from_options(
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("nobody")
            .database("health"),
    ));
    let st = state_with(db);
    let (status, body) = post(&st, &quantity(vec![hr(Uuid::new_v4(), 0, 60.0)], vec![])).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, json!({"error": "unavailable"}));
}
