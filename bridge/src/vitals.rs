//! The **daily vitals ledger**: `diet-logs/vitals-log.csv`, one row per date of what Apple
//! Health measured that day (sleep and its stages, resting heart rate, HRV, steps, active
//! energy, respiratory rate, wrist temperature), and the one route that writes it.
//!
//! It exists because nothing on the Studio kept a daily history of any of these. The phone
//! has them in HealthKit and sends a summary with every turn, but that block is read once
//! and never persisted, so the only outcome anything here could ever correlate against was
//! the bathroom scale. Sleep, resting HR and HRV answer to alcohol, late meals and training
//! within a day and are far less noisy than a morning weight; this file is where they land.
//!
//! **The phone is the only writer.** `POST /jesse/diet/vitals` carries whole DAYS, and a
//! day that is already in the file is REPLACED IN PLACE: the row keeps its position and
//! every cell takes the new value, so a resend never adds a second row for a date and the
//! latest reading from HealthKit is the one kept. A new date is appended. The diet CSVs are
//! never read, rewritten or reordered by this module.
//!
//! **Unknown is not zero.** A metric HealthKit had no sample for arrives absent and is
//! written as an EMPTY cell; the series served on the diet snapshot omits the key. A day
//! with no metric at all is refused outright rather than written, because the one way such
//! a day reaches the bridge is a read that failed (a locked phone cannot read HealthKit),
//! and replacing a good row with an empty one would erase a night that was measured.

use crate::*;

/// The ledger's file name, beside `food-log.csv` and friends in `diet-logs/`.
pub const VITALS_LOG: &str = "vitals-log.csv";

/// How many dates `vitalsSeries` carries: the most recent ones, ascending. Matches the
/// backfill the phone sends on its first run.
pub const VITALS_SERIES_DAYS: usize = 120;

/// The most days one request may carry. The backfill is 120; the margin is for a phone
/// whose first send was interrupted and repeated.
pub const MAX_VITALS_DAYS_PER_POST: usize = 400;

/// One metric the ledger records: its CSV column, its wire key, the plausible range a
/// reading must fall in, and how many decimals the cell keeps.
pub struct VitalsMetric {
    pub column: &'static str,
    pub key: &'static str,
    pub min: f64,
    pub max: f64,
    pub decimals: u32,
}

/// Every metric, in column order. The order is the file's header; changing it would make
/// every existing row read under the wrong names, so a new metric only ever goes last.
pub const VITALS_METRICS: &[VitalsMetric] = &[
    VitalsMetric {
        column: "Sleep_min",
        key: "sleepMin",
        min: 0.0,
        max: 1440.0,
        decimals: 0,
    },
    VitalsMetric {
        column: "Deep_min",
        key: "deepMin",
        min: 0.0,
        max: 1440.0,
        decimals: 0,
    },
    VitalsMetric {
        column: "REM_min",
        key: "remMin",
        min: 0.0,
        max: 1440.0,
        decimals: 0,
    },
    VitalsMetric {
        column: "Awake_min",
        key: "awakeMin",
        min: 0.0,
        max: 1440.0,
        decimals: 0,
    },
    VitalsMetric {
        column: "Resting_HR_bpm",
        key: "restingHr",
        min: 20.0,
        max: 200.0,
        decimals: 1,
    },
    VitalsMetric {
        column: "HRV_SDNN_ms",
        key: "hrv",
        min: 1.0,
        max: 400.0,
        decimals: 1,
    },
    VitalsMetric {
        column: "Steps",
        key: "steps",
        min: 0.0,
        max: 200_000.0,
        decimals: 0,
    },
    VitalsMetric {
        column: "Active_kcal",
        key: "activeKcal",
        min: 0.0,
        max: 15_000.0,
        decimals: 0,
    },
    VitalsMetric {
        column: "Resp_rate_bpm",
        key: "respRate",
        min: 4.0,
        max: 60.0,
        decimals: 1,
    },
    VitalsMetric {
        column: "Wrist_temp_C",
        key: "wristTempC",
        min: 25.0,
        max: 45.0,
        decimals: 2,
    },
];

/// The header line, `Date` then every metric column.
pub fn vitals_header() -> String {
    std::iter::once("Date")
        .chain(VITALS_METRICS.iter().map(|m| m.column))
        .collect::<Vec<_>>()
        .join(",")
}

/// One day as the phone sends it: the date and one optional value per metric, in
/// [`VITALS_METRICS`] order. `None` is UNKNOWN, never zero.
#[derive(Debug, Clone, PartialEq)]
pub struct VitalsDay {
    pub date: String,
    pub values: Vec<Option<f64>>,
}

impl VitalsDay {
    fn is_empty(&self) -> bool {
        self.values.iter().all(Option::is_none)
    }
}

/// Parse and validate the body of `POST /jesse/diet/vitals`: `{"days": [{"date": ...,
/// "sleepMin": ..., ...}]}`. Every problem is an `Err` naming it and nothing is written: a
/// malformed date, a non-number, a value outside its plausible range, a day with no metric
/// at all, the same date twice, or more than [`MAX_VITALS_DAYS_PER_POST`] days. An unknown
/// key is ignored, so a newer phone can send a metric an older bridge does not keep.
pub fn parse_vitals_body(body: &Value) -> Result<Vec<VitalsDay>, String> {
    let days = body
        .get("days")
        .and_then(Value::as_array)
        .ok_or("body has no \"days\" array")?;
    if days.is_empty() {
        return Err("\"days\" is empty".into());
    }
    if days.len() > MAX_VITALS_DAYS_PER_POST {
        return Err(format!(
            "{} days in one request; the cap is {MAX_VITALS_DAYS_PER_POST}",
            days.len()
        ));
    }
    let mut out: Vec<VitalsDay> = Vec::with_capacity(days.len());
    for (i, day) in days.iter().enumerate() {
        let obj = day
            .as_object()
            .ok_or_else(|| format!("day {i} is not an object"))?;
        let date = obj
            .get("date")
            .and_then(Value::as_str)
            .filter(|d| valid_iso_date(d).is_some())
            .ok_or_else(|| format!("day {i} has no valid YYYY-MM-DD \"date\""))?
            .to_string();
        if out.iter().any(|d| d.date == date) {
            return Err(format!("{date} appears twice"));
        }
        let mut values = Vec::with_capacity(VITALS_METRICS.len());
        for m in VITALS_METRICS {
            let v = match obj.get(m.key) {
                None | Some(Value::Null) => None,
                Some(v) => {
                    let n = v
                        .as_f64()
                        .filter(|n| n.is_finite())
                        .ok_or_else(|| format!("{date}: {} is not a number", m.key))?;
                    if n < m.min || n > m.max {
                        return Err(format!(
                            "{date}: {} = {n} is outside {}..={}",
                            m.key, m.min, m.max
                        ));
                    }
                    Some(n)
                }
            };
            values.push(v);
        }
        let parsed = VitalsDay { date, values };
        if parsed.is_empty() {
            return Err(format!(
                "{}: no metric at all; an empty day is never written",
                parsed.date
            ));
        }
        out.push(parsed);
    }
    Ok(out)
}

/// A value as its CSV cell: rounded to the metric's decimals, shortest form (`431`, `55.5`).
fn vitals_cell(v: Option<f64>, decimals: u32) -> String {
    match v {
        None => String::new(),
        Some(n) => {
            let scale = 10f64.powi(decimals as i32);
            let r = (n * scale).round() / scale;
            // `-0` would read as a sign on a value that has none.
            let r = if r == 0.0 { 0.0 } else { r };
            format!("{r}")
        }
    }
}

/// What an upsert did, for the response and the log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VitalsUpsert {
    pub replaced: usize,
    pub added: usize,
}

/// Apply `days` to the ledger's current `content` and return the new file.
///
/// A date already in the file is replaced where it stands; a new date is appended, in
/// the order sent. Rows for other dates are carried through cell for cell. The result has
/// the header first, LF line endings, RFC 4180 quoting and a single trailing newline.
///
/// Refused, with the file unchanged: a header that is not this ledger's (a file someone
/// else wrote under this name is not ours to rewrite), and a row that does not parse.
/// Two rows for one date in an existing file collapse to one, at the first one's position,
/// carrying the later one's cells: the file is one row per date, and the later row is the
/// later reading.
pub fn upsert_vitals_csv(
    content: &str,
    days: &[VitalsDay],
) -> Result<(String, VitalsUpsert), String> {
    let header = vitals_header();
    let width = VITALS_METRICS.len() + 1;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    if !content.trim().is_empty() {
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .flexible(false)
            .from_reader(content.as_bytes());
        let found = reader
            .headers()
            .map_err(|e| format!("cannot read the header: {e}"))?
            .iter()
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(",");
        if found != header {
            return Err(format!(
                "the header is {found:?}, not this ledger's {header:?}; refusing to rewrite it"
            ));
        }
        for (n, rec) in reader.records().enumerate() {
            let rec = rec.map_err(|e| format!("row {} does not parse: {e}", n + 2))?;
            let cells: Vec<String> = rec.iter().map(str::to_string).collect();
            if cells.len() != width {
                return Err(format!(
                    "row {} has {} cells, not {width}",
                    n + 2,
                    cells.len()
                ));
            }
            let date = cells[0].trim().to_string();
            match index.get(&date) {
                Some(&at) => rows[at] = cells,
                None => {
                    index.insert(date, rows.len());
                    rows.push(cells);
                }
            }
        }
    }

    let mut counts = VitalsUpsert {
        replaced: 0,
        added: 0,
    };
    for day in days {
        let cells: Vec<String> = std::iter::once(day.date.clone())
            .chain(
                VITALS_METRICS
                    .iter()
                    .zip(&day.values)
                    .map(|(m, v)| vitals_cell(*v, m.decimals)),
            )
            .collect();
        match index.get(&day.date) {
            Some(&at) => {
                rows[at] = cells;
                counts.replaced += 1;
            }
            None => {
                index.insert(day.date.clone(), rows.len());
                rows.push(cells);
                counts.added += 1;
            }
        }
    }

    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new());
    let head: Vec<&str> = header.split(',').collect();
    writer.write_record(&head).map_err(|e| e.to_string())?;
    for row in &rows {
        writer.write_record(row).map_err(|e| e.to_string())?;
    }
    let bytes = writer.into_inner().map_err(|e| e.to_string())?;
    let out = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    Ok((out, counts))
}

/// Build `vitalsSeries` from the ledger: one object per date, the most recent
/// [`VITALS_SERIES_DAYS`] dates ascending, each carrying `date` and only the metrics the
/// row KNOWS (a blank cell omits the key; it is never a 0). Rows that do not parse, carry
/// a malformed date, or know no metric are skipped. A missing or empty file is `[]`.
pub fn vitals_series(content: &str) -> Vec<Value> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(content.as_bytes());
    let cols: HashMap<String, usize> = match reader.headers() {
        Ok(h) => h
            .iter()
            .enumerate()
            .map(|(i, n)| (n.trim().to_string(), i))
            .collect(),
        Err(_) => return Vec::new(),
    };
    let Some(&date_col) = cols.get("Date") else {
        return Vec::new();
    };
    let mut by_date: std::collections::BTreeMap<String, Value> = Default::default();
    for rec in reader.records().flatten() {
        let Some(date) = rec.get(date_col).map(str::trim) else {
            continue;
        };
        if valid_iso_date(date).is_none() {
            continue;
        }
        let mut obj = serde_json::Map::new();
        obj.insert("date".into(), Value::String(date.to_string()));
        for m in VITALS_METRICS {
            let v = cols
                .get(m.column)
                .and_then(|&i| rec.get(i))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|n| n.is_finite());
            if let Some(n) = v {
                obj.insert(m.key.into(), json!(n));
            }
        }
        if obj.len() > 1 {
            by_date.insert(date.to_string(), Value::Object(obj));
        }
    }
    let skip = by_date.len().saturating_sub(VITALS_SERIES_DAYS);
    by_date.into_values().skip(skip).collect()
}

/// Serializes read-modify-write of the ledger inside this process. The phone's foreground
/// resend and a background refresh can land together; without this the second write would
/// be computed from the file as it stood before the first.
static VITALS_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Upsert `days` into `<logs_dir>/vitals-log.csv`, atomically. The whole read, merge and
/// replace runs under [`VITALS_WRITE`].
pub fn write_vitals(logs_dir: &Path, days: &[VitalsDay]) -> Result<VitalsUpsert, String> {
    let _guard = VITALS_WRITE.lock().unwrap_or_else(|p| p.into_inner());
    let path = logs_dir.join(VITALS_LOG);
    let current = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let (next, counts) = upsert_vitals_csv(&current, days)?;
    write_atomic(&path, next.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(counts)
}

/// `POST /jesse/diet/vitals` — the phone's daily vitals, upserted into the ledger. Same
/// bearer auth as every route. `400` for a body [`parse_vitals_body`] refuses, `500` when
/// the file cannot be written or carries a header that is not the ledger's; nothing is
/// written in either case.
pub async fn jesse_diet_vitals(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &st.cfg.token)?;
    let days = parse_vitals_body(&body).map_err(|why| {
        eprintln!("vitals: rejected a malformed body ({why})");
        (StatusCode::BAD_REQUEST, format!("malformed vitals: {why}"))
    })?;
    let logs = Path::new(&st.cfg.vault).join("diet-logs");
    let counts = write_vitals(&logs, &days).map_err(|why| {
        eprintln!("vitals: write refused ({why})");
        (StatusCode::INTERNAL_SERVER_ERROR, why)
    })?;
    eprintln!(
        "vitals: {} day(s) upserted ({} replaced, {} added)",
        days.len(),
        counts.replaced,
        counts.added
    );
    Ok(Json(json!({
        "status": "ok",
        "replaced": counts.replaced,
        "added": counts.added,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(date: &str, sleep: Option<f64>, hrv: Option<f64>) -> VitalsDay {
        let mut values = vec![None; VITALS_METRICS.len()];
        values[0] = sleep;
        values[5] = hrv;
        VitalsDay {
            date: date.into(),
            values,
        }
    }

    #[test]
    fn the_header_is_date_then_every_metric_in_order() {
        assert_eq!(
            vitals_header(),
            "Date,Sleep_min,Deep_min,REM_min,Awake_min,Resting_HR_bpm,HRV_SDNN_ms,Steps,\
             Active_kcal,Resp_rate_bpm,Wrist_temp_C"
        );
    }

    #[test]
    fn a_new_date_into_an_empty_file_writes_the_header_and_one_row() {
        let (out, counts) =
            upsert_vitals_csv("", &[day("2026-09-27", Some(431.0), Some(94.0))]).unwrap();
        assert_eq!(
            counts,
            VitalsUpsert {
                replaced: 0,
                added: 1
            }
        );
        assert_eq!(
            out,
            format!("{}\n2026-09-27,431,,,,,94,,,,\n", vitals_header())
        );
    }

    #[test]
    fn the_same_date_twice_replaces_the_row_in_place_never_a_second_row() {
        let (first, _) = upsert_vitals_csv(
            "",
            &[
                day("2026-09-26", Some(400.0), Some(79.0)),
                day("2026-09-27", Some(431.0), Some(94.0)),
            ],
        )
        .unwrap();
        let (second, counts) =
            upsert_vitals_csv(&first, &[day("2026-09-26", Some(410.0), Some(90.0))]).unwrap();
        assert_eq!(
            counts,
            VitalsUpsert {
                replaced: 1,
                added: 0
            }
        );
        let lines: Vec<&str> = second.lines().collect();
        assert_eq!(lines.len(), 3, "header plus one row per date: {second}");
        assert_eq!(
            lines[1], "2026-09-26,410,,,,,90,,,,",
            "replaced where it stood"
        );
        assert_eq!(
            lines[2], "2026-09-27,431,,,,,94,,,,",
            "the other row untouched"
        );
    }

    #[test]
    fn blank_metrics_stay_blank_and_are_omitted_from_the_series() {
        let (out, _) = upsert_vitals_csv("", &[day("2026-09-27", Some(431.0), None)]).unwrap();
        assert!(out.contains("2026-09-27,431,,,,,,,,,\n"), "{out}");
        let series = vitals_series(&out);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0]["sleepMin"], 431.0);
        assert!(series[0].get("hrv").is_none(), "unknown is absent, never 0");
    }

    #[test]
    fn a_resend_may_blank_a_metric_the_new_reading_lacks() {
        let (first, _) =
            upsert_vitals_csv("", &[day("2026-09-27", Some(431.0), Some(94.0))]).unwrap();
        let (second, _) =
            upsert_vitals_csv(&first, &[day("2026-09-27", Some(455.0), None)]).unwrap();
        assert!(second.ends_with("2026-09-27,455,,,,,,,,,\n"), "{second}");
    }

    #[test]
    fn values_round_to_each_metrics_decimals() {
        let mut d = day("2026-09-27", Some(430.6), Some(94.04));
        d.values[4] = Some(50.96);
        d.values[9] = Some(36.4249);
        let (out, _) = upsert_vitals_csv("", &[d]).unwrap();
        assert!(out.ends_with("2026-09-27,431,,,,51,94,,,,36.42\n"), "{out}");
    }

    #[test]
    fn a_foreign_header_is_refused_and_nothing_is_produced() {
        let err = upsert_vitals_csv(
            "Date,Weight_lbs\n2026-09-27,183\n",
            &[day("2026-09-27", Some(1.0), None)],
        )
        .unwrap_err();
        assert!(err.contains("refusing"), "{err}");
    }

    #[test]
    fn duplicate_rows_in_an_existing_file_collapse_to_one() {
        let h = vitals_header();
        let content = format!(
            "{h}\n2026-09-26,400,,,,,,,,,\n2026-09-27,431,,,,,,,,,\n2026-09-26,405,,,,,,,,,\n"
        );
        let (out, counts) =
            upsert_vitals_csv(&content, &[day("2026-09-28", Some(420.0), None)]).unwrap();
        assert_eq!(counts.added, 1);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            vec![
                h.as_str(),
                "2026-09-26,405,,,,,,,,,",
                "2026-09-27,431,,,,,,,,,",
                "2026-09-28,420,,,,,,,,,"
            ]
        );
    }

    #[test]
    fn parse_refuses_bad_dates_ranges_empty_days_and_duplicates() {
        let bad = [
            json!({}),
            json!({"days": []}),
            json!({"days": [{"date": "2026-9-27", "sleepMin": 400}]}),
            json!({"days": [{"date": "2026-09-27", "sleepMin": 2000}]}),
            json!({"days": [{"date": "2026-09-27", "sleepMin": "long"}]}),
            json!({"days": [{"date": "2026-09-27"}]}),
            json!({"days": [{"date": "2026-09-27", "hrv": 50}, {"date": "2026-09-27", "hrv": 51}]}),
        ];
        for b in bad {
            assert!(parse_vitals_body(&b).is_err(), "should refuse {b}");
        }
    }

    #[test]
    fn parse_keeps_known_metrics_ignores_unknown_keys_and_null_is_unknown() {
        let days = parse_vitals_body(&json!({"days": [
            {"date": "2026-09-27", "sleepMin": 431, "hrv": null, "restingHr": 55, "futureMetric": 3}
        ]}))
        .unwrap();
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].values[0], Some(431.0));
        assert_eq!(days[0].values[4], Some(55.0));
        assert_eq!(days[0].values[5], None);
    }

    #[test]
    fn the_series_keeps_the_most_recent_days_ascending() {
        let h = vitals_header();
        let mut content = format!("{h}\n");
        // 130 dates across Jun to Oct, written newest first to prove the series sorts.
        let start = chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let dates: Vec<String> = (0..130)
            .map(|i| {
                (start + chrono::Duration::days(i))
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .collect();
        for d in dates.iter().rev() {
            content.push_str(&format!("{d},400,,,,,,,,,\n"));
        }
        let series = vitals_series(&content);
        assert_eq!(series.len(), VITALS_SERIES_DAYS);
        assert_eq!(series[0]["date"], dates[10].as_str());
        assert_eq!(series[119]["date"], dates[129].as_str());
    }

    #[test]
    fn write_vitals_creates_the_file_and_upserts_it() {
        let dir =
            std::env::temp_dir().join(format!("vitals-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        write_vitals(&dir, &[day("2026-09-27", Some(431.0), None)]).unwrap();
        let counts = write_vitals(&dir, &[day("2026-09-27", Some(460.0), None)]).unwrap();
        assert_eq!(
            counts,
            VitalsUpsert {
                replaced: 1,
                added: 0
            }
        );
        let content = std::fs::read_to_string(dir.join(VITALS_LOG)).unwrap();
        assert_eq!(content.lines().count(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn rand_suffix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }
}
