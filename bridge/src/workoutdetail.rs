//! The **workout detail ledger**: `diet-logs/workout-detail.csv`, one row per workout keyed
//! by its HealthKit UUID with one column per figure the phone reads, and
//! `diet-logs/workout-splits.csv`, one row per split.
//!
//! WHY IT EXISTS. Until now the workout feed reached the bridge only as prose inside
//! `health_context`, one line per workout. The agent hand-copied a few of its numbers into
//! `exercise-log.csv` (the CALORIE ledger, 16 columns) and pasted the rest into the free-text
//! `Notes` cell, where no query could use them: on 2026-10-07 a single Runna run carried about
//! thirty figures and seven reached real columns. History beyond 48 hours carried only
//! minutes.
//!
//! So the phone now sends the same workouts as STRUCTURED DATA (`workout_records`, a JSON
//! array beside the prose, never instead of it) and the bridge writes them here itself,
//! before the model runs. `exercise-log.csv` and every reader of it are untouched; these two
//! files join to it on `Date` plus `Start_Time`.
//!
//! **Data, never prompt text.** Nothing in this module returns a string that reaches a
//! prompt; `workout_records` is read here and nowhere else.
//!
//! **Unknown is not zero.** An absent figure is a BLANK cell, never a 0.
//!
//! **Upsert, and only when something changed.** A UUID already in the file has its row
//! rewritten only when a figure differs, and a blank in the new record never erases a value
//! the file already holds (a route elevation that arrived on an earlier turn stays when a
//! later turn's route read missed its bound). Rows that do not change keep their bytes. A
//! request that changes nothing leaves both files byte-identical.
//!
//! **No positions.** The record carries only reduced numbers (route elevation is four of
//! them). There is no coordinate field to persist, and unknown JSON keys are dropped on read.

use crate::*;
use serde::Serialize;
use std::collections::BTreeMap;

/// The detail ledger's file name, beside `exercise-log.csv` in `diet-logs/`.
pub const WORKOUT_DETAIL_LOG: &str = "workout-detail.csv";
/// The splits ledger's file name.
pub const WORKOUT_SPLITS_LOG: &str = "workout-splits.csv";

/// THE detail header, fixed here and nowhere else. Column order is stable: a later field is
/// APPENDED at the end, never inserted, and an older file's header (a prefix of this one) is
/// extended in place by [`plan_log_header`].
pub const WORKOUT_DETAIL_HEADER: &str = "Workout_UUID,Date,Start_Time,TZ,Start,Activity,\
Workout_Name,Source_Name,Source_Bundle,Device_Product_Type,Indoor,Duration_s,Elapsed_s,\
Distance_m,Active_Energy_kcal,Avg_HR_bpm,Max_HR_bpm,Pace_s_per_km,Pace_Basis,Avg_Power_W,\
Ground_Contact_ms,Vertical_Oscillation_cm,Stride_Length_m,Step_Count,Cadence_spm,\
Cadence_Basis,Avg_METs,Effort_Score,Effort_Basis,Ascent_m,Ascent_Basis,Descent_m,\
Descent_Basis,Route_Altitude_Min_m,Route_Altitude_Max_m,Lap_Count,Split_Count,\
Weather_Temp_C,Weather_Humidity_percent,Weather_Condition,Swim_Location,\
Swim_Pool_Length_m,Swim_Lap_Count,Swim_Stroke_Count,Swim_Time_s,Swim_Pace_s_per_100m,\
Swim_Pace_Basis,Swim_Avg_SWOLF,Swim_Water_Temp_C,Swim_Stroke_Styles,Captured_At,\
Schema_Version";

/// The splits header: one row per split.
pub const WORKOUT_SPLITS_HEADER: &str = "Workout_UUID,Kind,Index,Distance_m,Duration_s";

/// The most records one request may carry: a turn carries at most five, and a backfill page
/// at most fifty.
pub const MAX_WORKOUT_RECORDS: usize = 50;
/// The most bytes the serialized `workout_records` value may take.
pub const MAX_WORKOUT_RECORDS_BYTES: usize = 512 * 1024;
/// The most splits one record may carry (a 100-mile run has 161).
pub const MAX_SPLITS_PER_RECORD: usize = 400;
/// The longest text cell (a workout name, a source name).
pub const MAX_TEXT_CELL_CHARS: usize = 200;

/// The record schema versions this bridge understands.
pub const WORKOUT_RECORD_SCHEMA_VERSIONS: std::ops::RangeInclusive<i64> = 1..=1;

/// One split, as the phone sends it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WorkoutSplit {
    pub kind: String,
    pub index: i64,
    pub distance_m: f64,
    pub duration_s: f64,
}

/// One workout, as the phone sends it (`WorkoutRecord` in JesseKit). Unknown keys are
/// ignored, so a newer phone can send a field this bridge does not keep yet.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct WorkoutRecord {
    pub schema_version: i64,
    pub workout_uuid: String,
    pub start: String,
    pub time_zone: Option<String>,
    pub captured_at: Option<String>,
    pub activity: String,
    pub workout_name: Option<String>,
    pub source_name: Option<String>,
    pub source_bundle: Option<String>,
    pub device_product_type: Option<String>,
    pub indoor: Option<bool>,
    pub duration_s: Option<f64>,
    pub elapsed_s: Option<f64>,
    pub distance_m: Option<f64>,
    pub active_energy_kcal: Option<f64>,
    pub avg_hr_bpm: Option<f64>,
    pub max_hr_bpm: Option<f64>,
    pub pace_s_per_km: Option<f64>,
    pub pace_basis: Option<String>,
    pub avg_power_w: Option<f64>,
    pub ground_contact_ms: Option<f64>,
    pub vertical_oscillation_cm: Option<f64>,
    pub stride_length_m: Option<f64>,
    pub step_count: Option<f64>,
    pub cadence_spm: Option<f64>,
    pub cadence_basis: Option<String>,
    pub avg_mets: Option<f64>,
    pub effort_score: Option<f64>,
    pub effort_basis: Option<String>,
    pub ascent_m: Option<f64>,
    pub ascent_basis: Option<String>,
    pub descent_m: Option<f64>,
    pub descent_basis: Option<String>,
    pub route_altitude_min_m: Option<f64>,
    pub route_altitude_max_m: Option<f64>,
    pub lap_count: Option<i64>,
    pub weather_temp_c: Option<f64>,
    pub weather_humidity_percent: Option<f64>,
    pub weather_condition: Option<String>,
    pub swim_location: Option<String>,
    pub swim_pool_length_m: Option<f64>,
    pub swim_lap_count: Option<i64>,
    pub swim_stroke_count: Option<f64>,
    pub swim_time_s: Option<f64>,
    pub swim_pace_s_per_100m: Option<f64>,
    pub swim_pace_basis: Option<String>,
    pub swim_avg_swolf: Option<f64>,
    pub swim_water_temp_c: Option<f64>,
    pub swim_stroke_styles: Option<BTreeMap<String, i64>>,
    pub splits: Option<Vec<WorkoutSplit>>,
}

/// Basis words a record may carry, per field. Anything else is refused.
const PACE_BASES: &[&str] = &["computed"];
const EFFORT_BASES: &[&str] = &["rated", "estimated"];
const ELEVATION_BASES: &[&str] = &["metadata", "route"];
const SWIM_PACE_BASES: &[&str] = &["computed", "computed_swim_time"];
const SWIM_LOCATIONS: &[&str] = &["pool", "open_water"];
const SPLIT_KINDS: &[&str] = &["km", "lap"];

/// A UUID-ish key: 1..=64 ASCII hex digits and hyphens.
fn valid_uuid(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// A text cell: control characters (a newline would split the row) become spaces, then
/// trimmed and capped. Blank is `None`.
fn clean_text(s: &Option<String>) -> Option<String> {
    let t: String = s
        .as_deref()?
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_TEXT_CELL_CHARS)
        .collect();
    (!t.is_empty()).then_some(t)
}

/// Parse and validate a `workout_records` value. Every problem is an `Err` naming it, and
/// none of the records is used: an array that is not one, too many records, too many bytes,
/// a record that does not decode, a bad UUID or start instant, a number that is not finite
/// or is negative where nothing can be, a basis or location outside its vocabulary, a split
/// list too long, or the same UUID twice.
pub fn parse_workout_records(value: &Value) -> Result<Vec<WorkoutRecord>, String> {
    let arr = value.as_array().ok_or("workout_records is not an array")?;
    if arr.len() > MAX_WORKOUT_RECORDS {
        return Err(format!(
            "{} records; the cap is {MAX_WORKOUT_RECORDS}",
            arr.len()
        ));
    }
    let bytes = serde_json::to_vec(value)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    if bytes > MAX_WORKOUT_RECORDS_BYTES {
        return Err(format!(
            "{bytes} bytes; the cap is {MAX_WORKOUT_RECORDS_BYTES}"
        ));
    }
    let mut out: Vec<WorkoutRecord> = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        if !v.is_object() {
            return Err(format!("record {i} is not an object"));
        }
        let r: WorkoutRecord = serde_json::from_value(v.clone())
            .map_err(|e| format!("record {i} does not decode: {e}"))?;
        validate_record(&r).map_err(|why| format!("record {i}: {why}"))?;
        if out.iter().any(|o| o.workout_uuid == r.workout_uuid) {
            return Err(format!("record {i}: workout_uuid appears twice"));
        }
        out.push(r);
    }
    Ok(out)
}

fn validate_record(r: &WorkoutRecord) -> Result<(), String> {
    if !WORKOUT_RECORD_SCHEMA_VERSIONS.contains(&r.schema_version) {
        return Err(format!(
            "schema_version {} is not understood",
            r.schema_version
        ));
    }
    if !valid_uuid(&r.workout_uuid) {
        return Err("workout_uuid is not a UUID".into());
    }
    if chrono::DateTime::parse_from_rfc3339(r.start.trim()).is_err() {
        return Err("start is not an RFC 3339 instant".into());
    }
    if r.activity.trim().is_empty() {
        return Err("activity is blank".into());
    }
    let duration = r.duration_s.ok_or("duration_s is missing")?;
    // Non-negative figures. Altitudes and temperatures may be below zero (checked below).
    let non_negative: &[(&str, Option<f64>)] = &[
        ("duration_s", Some(duration)),
        ("elapsed_s", r.elapsed_s),
        ("distance_m", r.distance_m),
        ("active_energy_kcal", r.active_energy_kcal),
        ("avg_hr_bpm", r.avg_hr_bpm),
        ("max_hr_bpm", r.max_hr_bpm),
        ("pace_s_per_km", r.pace_s_per_km),
        ("avg_power_w", r.avg_power_w),
        ("ground_contact_ms", r.ground_contact_ms),
        ("vertical_oscillation_cm", r.vertical_oscillation_cm),
        ("stride_length_m", r.stride_length_m),
        ("step_count", r.step_count),
        ("cadence_spm", r.cadence_spm),
        ("avg_mets", r.avg_mets),
        ("effort_score", r.effort_score),
        ("ascent_m", r.ascent_m),
        ("descent_m", r.descent_m),
        ("weather_humidity_percent", r.weather_humidity_percent),
        ("swim_pool_length_m", r.swim_pool_length_m),
        ("swim_stroke_count", r.swim_stroke_count),
        ("swim_time_s", r.swim_time_s),
        ("swim_pace_s_per_100m", r.swim_pace_s_per_100m),
        ("swim_avg_swolf", r.swim_avg_swolf),
        ("lap_count", r.lap_count.map(|n| n as f64)),
        ("swim_lap_count", r.swim_lap_count.map(|n| n as f64)),
    ];
    for (name, v) in non_negative {
        if let Some(n) = *v {
            if !n.is_finite() || n < 0.0 {
                return Err(format!("{name} = {n} is not a finite non-negative number"));
            }
        }
    }
    for (name, v) in [
        ("route_altitude_min_m", r.route_altitude_min_m),
        ("route_altitude_max_m", r.route_altitude_max_m),
        ("weather_temp_c", r.weather_temp_c),
        ("swim_water_temp_c", r.swim_water_temp_c),
    ] {
        if v.is_some_and(|n| !n.is_finite()) {
            return Err(format!("{name} is not finite"));
        }
    }
    let vocab: [(&str, &Option<String>, &[&str]); 7] = [
        ("pace_basis", &r.pace_basis, PACE_BASES),
        ("cadence_basis", &r.cadence_basis, PACE_BASES),
        ("effort_basis", &r.effort_basis, EFFORT_BASES),
        ("ascent_basis", &r.ascent_basis, ELEVATION_BASES),
        ("descent_basis", &r.descent_basis, ELEVATION_BASES),
        ("swim_pace_basis", &r.swim_pace_basis, SWIM_PACE_BASES),
        ("swim_location", &r.swim_location, SWIM_LOCATIONS),
    ];
    for (name, v, words) in vocab {
        if let Some(w) = v {
            if !words.contains(&w.as_str()) {
                return Err(format!("{name} {w:?} is not one of {words:?}"));
            }
        }
    }
    if let Some(tz) = &r.time_zone {
        if parse_iana(tz).is_none() {
            return Err("time_zone is not an IANA zone".into());
        }
    }
    if let Some(at) = &r.captured_at {
        if chrono::DateTime::parse_from_rfc3339(at.trim()).is_err() {
            return Err("captured_at is not an RFC 3339 instant".into());
        }
    }
    if let Some(styles) = &r.swim_stroke_styles {
        if styles.len() > 16 {
            return Err("swim_stroke_styles has more than 16 styles".into());
        }
        for (name, n) in styles {
            if *n < 0
                || name.is_empty()
                || name.len() > 32
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ' ')
            {
                return Err("swim_stroke_styles has a malformed entry".into());
            }
        }
    }
    if let Some(splits) = &r.splits {
        if splits.len() > MAX_SPLITS_PER_RECORD {
            return Err(format!(
                "{} splits; the cap is {MAX_SPLITS_PER_RECORD}",
                splits.len()
            ));
        }
        for s in splits {
            if !SPLIT_KINDS.contains(&s.kind.as_str())
                || s.index < 1
                || !s.distance_m.is_finite()
                || s.distance_m < 0.0
                || !s.duration_s.is_finite()
                || s.duration_s < 0.0
            {
                return Err("a split is malformed".into());
            }
        }
    }
    Ok(())
}

// ---- Cells -------------------------------------------------------------------

/// A number cell: blank when absent, else rounded to `decimals` in its shortest form.
fn num(v: Option<f64>, decimals: u32) -> String {
    match v {
        None => String::new(),
        Some(n) => {
            let scale = 10f64.powi(decimals as i32);
            let r = (n * scale).round() / scale;
            let r = if r == 0.0 { 0.0 } else { r };
            format!("{r}")
        }
    }
}

fn int(v: Option<i64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_default()
}

fn text(v: &Option<String>) -> String {
    clean_text(v).unwrap_or_default()
}

/// The zone a record's wall-clock cells are read in: its own IANA zone, else the fixed
/// offset its `start` carries.
pub fn record_zone(r: &WorkoutRecord) -> SchedulerZone {
    if let Some(z) = r.time_zone.as_deref().and_then(parse_iana) {
        return z;
    }
    chrono::DateTime::parse_from_rfc3339(r.start.trim())
        .map(|dt| SchedulerZone::Fixed(*dt.offset()))
        .unwrap_or(SchedulerZone::Host)
}

/// The detail row for one record: (column, UNQUOTED value) in header order. `now` is the
/// `Captured_At` used when the record carries none.
pub fn detail_cells(r: &WorkoutRecord, now: &str) -> Vec<(&'static str, String)> {
    let zone = record_zone(r);
    let date = diet_day_of(&r.start, &zone).unwrap_or_default();
    let clock = clock_hhmm_in(&r.start, &zone).unwrap_or_default();
    let indoor = r
        .indoor
        .map(|b| if b { "true" } else { "false" }.to_string());
    let styles = r.swim_stroke_styles.as_ref().map(|m| {
        // By lap count descending, then name, so the bytes never depend on map order.
        let mut v: Vec<(&String, &i64)> = m.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        v.iter()
            .map(|(k, n)| format!("{k}:{n}"))
            .collect::<Vec<_>>()
            .join(";")
    });
    let split_count = r
        .splits
        .as_ref()
        .map(|s| s.iter().filter(|x| x.kind == "km").count())
        .filter(|n| *n > 0)
        .map(|n| n as i64);
    vec![
        ("Workout_UUID", r.workout_uuid.trim().to_ascii_uppercase()),
        ("Date", date),
        ("Start_Time", clock),
        ("TZ", tz_label(&zone)),
        ("Start", r.start.trim().to_string()),
        ("Activity", text(&Some(r.activity.clone()))),
        ("Workout_Name", text(&r.workout_name)),
        ("Source_Name", text(&r.source_name)),
        ("Source_Bundle", text(&r.source_bundle)),
        ("Device_Product_Type", text(&r.device_product_type)),
        ("Indoor", indoor.unwrap_or_default()),
        ("Duration_s", num(r.duration_s, 1)),
        ("Elapsed_s", num(r.elapsed_s, 1)),
        ("Distance_m", num(r.distance_m, 1)),
        ("Active_Energy_kcal", num(r.active_energy_kcal, 1)),
        ("Avg_HR_bpm", num(r.avg_hr_bpm, 1)),
        ("Max_HR_bpm", num(r.max_hr_bpm, 1)),
        ("Pace_s_per_km", num(r.pace_s_per_km, 1)),
        ("Pace_Basis", text(&r.pace_basis)),
        ("Avg_Power_W", num(r.avg_power_w, 1)),
        ("Ground_Contact_ms", num(r.ground_contact_ms, 1)),
        ("Vertical_Oscillation_cm", num(r.vertical_oscillation_cm, 2)),
        ("Stride_Length_m", num(r.stride_length_m, 3)),
        ("Step_Count", num(r.step_count, 0)),
        ("Cadence_spm", num(r.cadence_spm, 1)),
        ("Cadence_Basis", text(&r.cadence_basis)),
        ("Avg_METs", num(r.avg_mets, 2)),
        ("Effort_Score", num(r.effort_score, 1)),
        ("Effort_Basis", text(&r.effort_basis)),
        ("Ascent_m", num(r.ascent_m, 1)),
        ("Ascent_Basis", text(&r.ascent_basis)),
        ("Descent_m", num(r.descent_m, 1)),
        ("Descent_Basis", text(&r.descent_basis)),
        ("Route_Altitude_Min_m", num(r.route_altitude_min_m, 1)),
        ("Route_Altitude_Max_m", num(r.route_altitude_max_m, 1)),
        ("Lap_Count", int(r.lap_count)),
        ("Split_Count", int(split_count)),
        ("Weather_Temp_C", num(r.weather_temp_c, 1)),
        (
            "Weather_Humidity_percent",
            num(r.weather_humidity_percent, 1),
        ),
        ("Weather_Condition", text(&r.weather_condition)),
        ("Swim_Location", text(&r.swim_location)),
        ("Swim_Pool_Length_m", num(r.swim_pool_length_m, 1)),
        ("Swim_Lap_Count", int(r.swim_lap_count)),
        ("Swim_Stroke_Count", num(r.swim_stroke_count, 0)),
        ("Swim_Time_s", num(r.swim_time_s, 1)),
        ("Swim_Pace_s_per_100m", num(r.swim_pace_s_per_100m, 1)),
        ("Swim_Pace_Basis", text(&r.swim_pace_basis)),
        ("Swim_Avg_SWOLF", num(r.swim_avg_swolf, 1)),
        ("Swim_Water_Temp_C", num(r.swim_water_temp_c, 1)),
        ("Swim_Stroke_Styles", styles.unwrap_or_default()),
        (
            "Captured_At",
            r.captured_at
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(now)
                .to_string(),
        ),
        ("Schema_Version", r.schema_version.to_string()),
    ]
}

/// The split rows for one record, `(kind, cells)` in order, cells under
/// [`WORKOUT_SPLITS_HEADER`].
pub fn split_rows(r: &WorkoutRecord) -> Vec<(String, Vec<String>)> {
    let uuid = r.workout_uuid.trim().to_ascii_uppercase();
    r.splits
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|s| {
            (
                s.kind.clone(),
                vec![
                    uuid.clone(),
                    s.kind.clone(),
                    s.index.to_string(),
                    num(Some(s.distance_m), 1),
                    num(Some(s.duration_s), 1),
                ],
            )
        })
        .collect()
}

// ---- The upsert ------------------------------------------------------------

/// One existing CSV row: its parsed cells (padded to the planned columns) and its exact
/// original bytes, terminator included, so an untouched row is written back unchanged.
struct RawRow {
    cells: Vec<String>,
    raw: String,
}

/// A ledger as read: its planned columns, its header line's raw bytes, and its rows.
type Ledger = (Vec<String>, Option<String>, Vec<RawRow>);

/// Parse a ledger into its planned columns, its header line's raw bytes (terminator
/// included, `None` for an empty file) and its rows with their original bytes.
fn read_ledger(content: &str, canonical: &str) -> Result<Ledger, String> {
    if content.trim().is_empty() {
        return Ok((
            canonical.split(',').map(str::to_string).collect(),
            None,
            Vec::new(),
        ));
    }
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(content.as_bytes());
    let found = reader
        .headers()
        .map_err(|e| format!("cannot read the header: {e}"))?
        .iter()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(",");
    let columns = plan_log_header(&found, canonical)?;
    let mut starts: Vec<usize> = Vec::new();
    let mut parsed: Vec<Vec<String>> = Vec::new();
    for (n, rec) in reader.records().enumerate() {
        let rec = rec.map_err(|e| format!("row {} does not parse: {e}", n + 2))?;
        let start = rec
            .position()
            .map(|p| p.byte() as usize)
            .ok_or_else(|| format!("row {} has no position", n + 2))?;
        let mut cells: Vec<String> = rec.iter().map(str::to_string).collect();
        if cells.len() > columns.len() {
            return Err(format!(
                "row {} has {} cells, more than the {} columns",
                n + 2,
                cells.len(),
                columns.len()
            ));
        }
        cells.resize(columns.len(), String::new());
        starts.push(start);
        parsed.push(cells);
    }
    let header_end = starts.first().copied().unwrap_or(content.len());
    let header_raw = content[..header_end].to_string();
    let mut rows = Vec::with_capacity(parsed.len());
    for (i, cells) in parsed.into_iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(content.len());
        rows.push(RawRow {
            cells,
            raw: content[starts[i]..end].to_string(),
        });
    }
    Ok((columns, Some(header_raw), rows))
}

/// Render one row under `columns` as an RFC 4180 line with an LF terminator.
fn render_line(cells: &[String]) -> String {
    let mut s = cells
        .iter()
        .map(|c| csv_field(c))
        .collect::<Vec<_>>()
        .join(",");
    s.push('\n');
    s
}

/// Reassemble a ledger: the header (its own bytes when the plan kept it, else rewritten) and
/// every row (its own bytes unless `changed`).
fn assemble(columns: &[String], header_raw: Option<String>, rows: &[(RawRow, bool)]) -> String {
    let planned = columns.join(",");
    let mut out = match header_raw {
        Some(h) if h.trim_end_matches(['\n', '\r']) == planned => h,
        _ => format!("{planned}\n"),
    };
    if !out.ends_with('\n') {
        out.push('\n');
    }
    for (row, changed) in rows {
        if *changed {
            out.push_str(&render_line(&row.cells));
        } else {
            out.push_str(&row.raw);
            if !row.raw.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out
}

/// What one upsert did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct WorkoutUpsert {
    /// Workouts whose detail row was added.
    pub added: usize,
    /// Workouts whose detail row changed.
    pub replaced: usize,
    /// Workouts whose detail row was already identical.
    pub unchanged: usize,
    /// Split rows written (added or replaced).
    pub splits_written: usize,
}

impl WorkoutUpsert {
    pub fn written(&self) -> usize {
        self.added + self.replaced
    }
}

/// Apply `records` to the detail ledger's `content`. Returns the new content, or `None`
/// when nothing changed (the file must then be left byte-identical), and the counts.
///
/// A known UUID's row is MERGED: every non-blank new cell overwrites, a blank new cell keeps
/// what the file holds. If no cell other than `Captured_At` differs, the row is unchanged
/// and keeps its bytes; otherwise it is re-rendered with the new `Captured_At`. A new UUID is
/// appended. Columns this bridge does not know keep their cells.
pub fn upsert_detail_csv(
    content: &str,
    records: &[WorkoutRecord],
    now: &str,
) -> Result<(Option<String>, WorkoutUpsert), String> {
    let canonical = canonical_header(WORKOUT_DETAIL_LOG).expect("a ledger this bridge owns");
    let (columns, header_raw, rows) = read_ledger(content, canonical)?;
    let col = |name: &str| columns.iter().position(|c| c == name);
    let key_col = col("Workout_UUID").ok_or("no Workout_UUID column")?;
    let captured_col = col("Captured_At");
    let mut rows: Vec<(RawRow, bool)> = rows.into_iter().map(|r| (r, false)).collect();
    let mut counts = WorkoutUpsert::default();
    for r in records {
        let cells = detail_cells(r, now);
        let uuid = cells[0].1.clone();
        let at = rows
            .iter()
            .position(|(row, _)| row.cells[key_col].trim().eq_ignore_ascii_case(&uuid));
        match at {
            Some(i) => {
                let mut merged = rows[i].0.cells.clone();
                for (name, value) in &cells {
                    if value.is_empty() {
                        continue;
                    }
                    if let Some(c) = col(name) {
                        merged[c] = value.clone();
                    }
                }
                let differs = merged
                    .iter()
                    .zip(&rows[i].0.cells)
                    .enumerate()
                    .any(|(c, (a, b))| Some(c) != captured_col && a != b);
                if differs {
                    rows[i].0.cells = merged;
                    rows[i].1 = true;
                    counts.replaced += 1;
                } else {
                    counts.unchanged += 1;
                }
            }
            None => {
                let mut fresh = vec![String::new(); columns.len()];
                for (name, value) in &cells {
                    if let Some(c) = col(name) {
                        fresh[c] = value.clone();
                    }
                }
                rows.push((
                    RawRow {
                        cells: fresh,
                        raw: String::new(),
                    },
                    true,
                ));
                counts.added += 1;
            }
        }
    }
    // Nothing to write: the file stays byte-identical (a legacy header is extended only
    // when a row is written under it).
    if counts.written() == 0 {
        return Ok((None, counts));
    }
    Ok((Some(assemble(&columns, header_raw, &rows)), counts))
}

/// Apply `records`' splits to the splits ledger's `content`. A record with no splits leaves
/// its rows alone. A record whose splits of one kind equal the file's rows for that UUID and
/// kind changes nothing; otherwise those rows are replaced, at the position of the first old
/// one (appended for a new workout). `None` when nothing changed.
pub fn upsert_splits_csv(
    content: &str,
    records: &[WorkoutRecord],
) -> Result<(Option<String>, usize), String> {
    let canonical = canonical_header(WORKOUT_SPLITS_LOG).expect("a ledger this bridge owns");
    let (columns, header_raw, rows) = read_ledger(content, canonical)?;
    let col = |name: &str| columns.iter().position(|c| c == name);
    let (uuid_col, kind_col) = (
        col("Workout_UUID").ok_or("no Workout_UUID column")?,
        col("Kind").ok_or("no Kind column")?,
    );
    let mut rows: Vec<(RawRow, bool)> = rows.into_iter().map(|r| (r, false)).collect();
    let mut written = 0usize;
    for r in records {
        let wanted = split_rows(r);
        if wanted.is_empty() {
            continue;
        }
        let mut kinds: Vec<String> = wanted.iter().map(|(k, _)| k.clone()).collect();
        kinds.dedup();
        for kind in kinds {
            let uuid = r.workout_uuid.trim().to_ascii_uppercase();
            let new_rows: Vec<Vec<String>> = wanted
                .iter()
                .filter(|(k, _)| *k == kind)
                .map(|(_, cells)| {
                    let mut full = vec![String::new(); columns.len()];
                    for (name, v) in WORKOUT_SPLITS_HEADER.split(',').zip(cells) {
                        if let Some(c) = col(name) {
                            full[c] = v.clone();
                        }
                    }
                    full
                })
                .collect();
            let is_ours = |row: &RawRow| {
                row.cells[uuid_col].trim().eq_ignore_ascii_case(&uuid)
                    && row.cells[kind_col].trim() == kind
            };
            let old: Vec<&Vec<String>> = rows
                .iter()
                .filter(|(row, _)| is_ours(row))
                .map(|(row, _)| &row.cells)
                .collect();
            if old.len() == new_rows.len() && old.iter().zip(&new_rows).all(|(a, b)| *a == b) {
                continue;
            }
            let insert_at = rows
                .iter()
                .position(|(row, _)| is_ours(row))
                .unwrap_or(rows.len());
            rows.retain(|(row, _)| !is_ours(row));
            let insert_at = insert_at.min(rows.len());
            written += new_rows.len();
            for (offset, cells) in new_rows.into_iter().enumerate() {
                rows.insert(
                    insert_at + offset,
                    (
                        RawRow {
                            cells,
                            raw: String::new(),
                        },
                        true,
                    ),
                );
            }
        }
    }
    if written == 0 {
        return Ok((None, 0));
    }
    Ok((Some(assemble(&columns, header_raw, &rows)), written))
}

// ---- The write path ----------------------------------------------------------

/// Serializes the read-modify-write of both ledgers inside this process, under (not
/// instead of) the broker's file locks.
static WORKOUT_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Upsert `records` into both ledgers under `logs_dir`. Synchronous and lock-free by itself;
/// [`write_workout_records`] is the locked, committing entry point. Returns the counts and
/// the diet days of the rows that changed.
pub fn upsert_workout_files(
    logs_dir: &Path,
    records: &[WorkoutRecord],
    now: &str,
) -> Result<(WorkoutUpsert, Vec<String>), String> {
    let _guard = WORKOUT_WRITE.lock().unwrap_or_else(|p| p.into_inner());
    let read = |name: &str| -> Result<String, String> {
        let path = logs_dir.join(name);
        match std::fs::read_to_string(&path) {
            Ok(c) => Ok(c),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    };
    let detail = read(WORKOUT_DETAIL_LOG)?;
    let splits = read(WORKOUT_SPLITS_LOG)?;
    // Both computed before either is written, so a refused header writes neither.
    let (new_detail, mut counts) = upsert_detail_csv(&detail, records, now)?;
    let (new_splits, split_count) = upsert_splits_csv(&splits, records)?;
    counts.splits_written = split_count;
    for (name, content) in [
        (WORKOUT_DETAIL_LOG, new_detail),
        (WORKOUT_SPLITS_LOG, new_splits),
    ] {
        if let Some(c) = content {
            let path = logs_dir.join(name);
            write_atomic(&path, c.as_bytes())
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        }
    }
    let mut days: Vec<String> = records
        .iter()
        .filter_map(|r| diet_day_of(&r.start, &record_zone(r)))
        .collect();
    days.sort();
    days.dedup();
    Ok((counts, days))
}

/// The locked, committing write: take the broker's file locks on both ledgers and the git
/// lock (the bridge's one acquisition order: per-file, then git), upsert, and when anything
/// changed commit through [`commit_diet_logs`]. A commit failure is logged and does not undo
/// the write (the rows are right; the vault's own autocommit picks them up).
pub async fn write_workout_records(
    vault: &Path,
    broker: &LockBroker,
    turn: &str,
    records: &[WorkoutRecord],
) -> Result<WorkoutUpsert, String> {
    let logs = vault.join("diet-logs");
    let call = "workout-records";
    let keys = [
        LockKey::Path(resolve_lock_path(&logs.join(WORKOUT_DETAIL_LOG), "/")),
        LockKey::Path(resolve_lock_path(&logs.join(WORKOUT_SPLITS_LOG), "/")),
        LockKey::Git,
    ];
    for key in &keys {
        if !broker
            .acquire_direct_op(key, turn, call, crate::activity::VaultOp::Edit)
            .await
        {
            broker.release_call_direct(turn, call);
            return Err("timed out waiting for the workout ledger's write lock".into());
        }
    }
    let now = now_rfc3339_in(&SchedulerZone::Host);
    let result = upsert_workout_files(&logs, records, &now);
    let result = match result {
        Ok((counts, days)) => {
            if counts.written() > 0 || counts.splits_written > 0 {
                let day = days.first().cloned().unwrap_or_else(local_today);
                if let Err(e) = commit_diet_logs(
                    vault,
                    &day,
                    &commit_stamp_in(&SchedulerZone::Host),
                    broker.ledger().map(|l| l.as_ref()),
                    turn,
                )
                .await
                {
                    eprintln!("workout ledger: commit failed ({e}); the rows are written");
                }
            }
            Ok(counts)
        }
        Err(e) => Err(e),
    };
    broker.release_call_direct(turn, call);
    result
}

/// The turn path: validate a request's `workout_records` and write them. Never fails the
/// turn: a malformed value or a write failure is logged with its reason and the turn runs.
/// Logs counts only, never a figure.
pub async fn ingest_turn_workout_records(
    vault: &Path,
    broker: &LockBroker,
    turn: &str,
    records: &[WorkoutRecord],
) {
    if records.is_empty() {
        return;
    }
    match write_workout_records(vault, broker, turn, records).await {
        Ok(c) => eprintln!(
            "workout ledger: turn {turn}: {} record(s), {} added, {} replaced, {} unchanged, {} split row(s)",
            records.len(),
            c.added,
            c.replaced,
            c.unchanged,
            c.splits_written
        ),
        Err(why) => eprintln!("workout ledger: turn {turn}: not written ({why})"),
    }
}

/// Validate a turn's raw `workout_records` value. `None` (absent) and a rejected value both
/// yield no records; a rejection is logged with its reason. The turn runs either way.
pub fn turn_workout_records(value: Option<&Value>) -> Vec<WorkoutRecord> {
    match value {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => {
            match parse_workout_records(v) {
                Ok(r) => r,
                Err(why) => {
                    eprintln!("workout ledger: rejected workout_records ({why}); the turn runs without them");
                    Vec::new()
                }
            }
        }
    }
}

/// `POST /jesse/diet/workouts` — the backfill: one page of records upserted into the
/// ledgers exactly as a turn's are. Starts no model turn. Same bearer auth as every route;
/// `400` for a body that does not validate (nothing written), `500` when the files cannot be
/// written.
pub async fn jesse_diet_workouts(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&headers, &st.cfg.token)?;
    let records = body
        .get("records")
        .ok_or_else(|| "body has no \"records\" array".to_string())
        .and_then(parse_workout_records)
        .map_err(|why| {
            eprintln!("workout ledger: rejected a backfill page ({why})");
            (
                StatusCode::BAD_REQUEST,
                format!("malformed workout records: {why}"),
            )
        })?;
    let turn = format!("workout-backfill-{}", random_hex());
    let counts = write_workout_records(Path::new(&st.cfg.vault), &st.broker, &turn, &records)
        .await
        .map_err(|why| {
            eprintln!("workout ledger: backfill write refused ({why})");
            (StatusCode::INTERNAL_SERVER_ERROR, why)
        })?;
    eprintln!(
        "workout ledger: backfill page of {}: {} added, {} replaced, {} unchanged, {} split row(s)",
        records.len(),
        counts.added,
        counts.replaced,
        counts.unchanged,
        counts.splits_written
    );
    Ok(Json(json!({
        "status": "ok",
        "written": counts.written(),
        "unchanged": counts.unchanged,
        "added": counts.added,
        "replaced": counts.replaced,
        "splits_written": counts.splits_written,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    const U1: &str = "6F1C2A4E-0000-4000-8000-000000000001";
    const U2: &str = "6F1C2A4E-0000-4000-8000-000000000002";
    const NOW: &str = "2026-10-07T10:00:00+02:00";

    /// The Runna run of 2026-10-07, as the phone sends it.
    fn run(uuid: &str) -> Value {
        json!({
            "schema_version": 1, "workout_uuid": uuid,
            "start": "2026-10-07T07:12:03+02:00", "time_zone": "Europe/Rome",
            "captured_at": "2026-10-07T09:00:00+02:00",
            "activity": "Run", "workout_name": "7.5km Long Run",
            "source_name": "Runna", "source_bundle": "com.runna.app",
            "device_product_type": "Watch7,5", "indoor": false,
            "duration_s": 2894.0, "elapsed_s": 2950.0, "distance_m": 7500.0,
            "active_energy_kcal": 500.4, "avg_hr_bpm": 150.2, "max_hr_bpm": 172.0,
            "pace_s_per_km": 385.866666, "pace_basis": "computed",
            "avg_power_w": 245.0, "ground_contact_ms": 240.0,
            "vertical_oscillation_cm": 8.1, "stride_length_m": 1.15,
            "step_count": 7910.0, "cadence_spm": 163.99, "cadence_basis": "computed",
            "avg_mets": 9.4, "effort_score": 8.0, "effort_basis": "estimated",
            "lap_count": 5, "weather_temp_c": 14.0, "weather_humidity_percent": 72.0,
            "weather_condition": "partly cloudy",
            "splits": [
                {"kind": "km", "index": 1, "distance_m": 1000.0, "duration_s": 358.0},
                {"kind": "km", "index": 2, "distance_m": 1000.0, "duration_s": 361.0}
            ]
        })
    }

    fn records(v: Value) -> Vec<WorkoutRecord> {
        parse_workout_records(&v).unwrap()
    }

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("workout-ledger-{}", random_hex()));
        std::fs::create_dir_all(d.join("diet-logs")).unwrap();
        d
    }

    fn rows(content: &str) -> Vec<Vec<String>> {
        csv::ReaderBuilder::new()
            .has_headers(true)
            .from_reader(content.as_bytes())
            .records()
            .map(|r| r.unwrap().iter().map(str::to_string).collect())
            .collect()
    }

    fn cell(content: &str, row: usize, name: &str) -> String {
        let i = WORKOUT_DETAIL_HEADER
            .split(',')
            .position(|c| c == name)
            .unwrap();
        rows(content)[row][i].clone()
    }

    #[test]
    fn the_header_is_one_constant_and_every_cell_lands_under_it() {
        let r = &records(json!([run(U1)]))[0];
        let names: Vec<&str> = detail_cells(r, NOW).iter().map(|(n, _)| *n).collect();
        assert_eq!(names.join(","), WORKOUT_DETAIL_HEADER);
        assert_eq!(
            canonical_header(WORKOUT_DETAIL_LOG),
            Some(WORKOUT_DETAIL_HEADER)
        );
        assert_eq!(
            canonical_header(WORKOUT_SPLITS_LOG),
            Some(WORKOUT_SPLITS_HEADER)
        );
        // The calorie ledger's header is untouched.
        assert_eq!(
            canonical_header("exercise-log.csv"),
            Some(EXERCISE_LOG_HEADER)
        );
    }

    #[test]
    fn a_new_uuid_appends_one_row_with_the_header() {
        let (out, c) = upsert_detail_csv("", &records(json!([run(U1)])), NOW).unwrap();
        let out = out.unwrap();
        assert_eq!(
            c,
            WorkoutUpsert {
                added: 1,
                ..Default::default()
            }
        );
        assert!(out.starts_with(&format!("{WORKOUT_DETAIL_HEADER}\n")));
        assert_eq!(rows(&out).len(), 1);
        assert!(!out.contains('\r'), "LF line endings");
        assert_eq!(cell(&out, 0, "Workout_UUID"), U1);
        assert_eq!(cell(&out, 0, "Date"), "2026-10-07");
        assert_eq!(cell(&out, 0, "Start_Time"), "07:12");
        assert_eq!(cell(&out, 0, "TZ"), "Europe/Rome");
        assert_eq!(cell(&out, 0, "Avg_Power_W"), "245");
        assert_eq!(cell(&out, 0, "Pace_s_per_km"), "385.9");
        assert_eq!(cell(&out, 0, "Effort_Basis"), "estimated");
        assert_eq!(cell(&out, 0, "Split_Count"), "2");
        assert_eq!(cell(&out, 0, "Schema_Version"), "1");
        // Not reported is blank, never 0.
        assert_eq!(cell(&out, 0, "Ascent_m"), "");
        assert_eq!(cell(&out, 0, "Swim_Lap_Count"), "");

        // A second, different UUID appends exactly one more row.
        let (out2, c2) = upsert_detail_csv(&out, &records(json!([run(U2)])), NOW).unwrap();
        let out2 = out2.unwrap();
        assert_eq!(c2.added, 1);
        assert_eq!(rows(&out2).len(), 2);
        assert!(out2.starts_with(&out), "the first row's bytes are kept");
    }

    #[test]
    fn a_repeat_with_identical_values_leaves_both_files_byte_identical() {
        let vault = scratch();
        let logs = vault.join("diet-logs");
        let recs = records(json!([run(U1), run(U2)]));
        let (first, _) = upsert_workout_files(&logs, &recs, NOW).unwrap();
        assert_eq!(first.added, 2);
        assert_eq!(first.splits_written, 4);
        let detail = std::fs::read(logs.join(WORKOUT_DETAIL_LOG)).unwrap();
        let splits = std::fs::read(logs.join(WORKOUT_SPLITS_LOG)).unwrap();
        let mtime = std::fs::metadata(logs.join(WORKOUT_DETAIL_LOG))
            .unwrap()
            .modified()
            .unwrap();

        // Same records, a later capture time and a later clock: nothing changes.
        let mut again = json!([run(U1), run(U2)]);
        again[0]["captured_at"] = json!("2026-10-08T09:00:00+02:00");
        let (second, _) =
            upsert_workout_files(&logs, &records(again), "2026-10-08T10:00:00+02:00").unwrap();
        assert_eq!(
            second,
            WorkoutUpsert {
                unchanged: 2,
                ..Default::default()
            }
        );
        assert_eq!(
            std::fs::read(logs.join(WORKOUT_DETAIL_LOG)).unwrap(),
            detail
        );
        assert_eq!(
            std::fs::read(logs.join(WORKOUT_SPLITS_LOG)).unwrap(),
            splits
        );
        assert_eq!(
            std::fs::metadata(logs.join(WORKOUT_DETAIL_LOG))
                .unwrap()
                .modified()
                .unwrap(),
            mtime,
            "not even rewritten"
        );
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_newly_filled_elevation_rewrites_only_that_row() {
        let (base, _) = upsert_detail_csv("", &records(json!([run(U1), run(U2)])), NOW).unwrap();
        let base = base.unwrap();
        let mut late = run(U2);
        late["ascent_m"] = json!(84.0);
        late["ascent_basis"] = json!("route");
        late["route_altitude_min_m"] = json!(12.0);
        late["route_altitude_max_m"] = json!(61.0);
        late["captured_at"] = json!("2026-10-07T12:00:00+02:00");
        // A later turn whose route read missed: blanks never erase what the file holds.
        let mut missed = run(U1);
        missed.as_object_mut().unwrap().remove("avg_power_w");
        let (out, c) = upsert_detail_csv(&base, &records(json!([missed, late])), NOW).unwrap();
        let out = out.unwrap();
        assert_eq!(
            c,
            WorkoutUpsert {
                replaced: 1,
                unchanged: 1,
                ..Default::default()
            }
        );
        let before: Vec<&str> = base.lines().collect();
        let after: Vec<&str> = out.lines().collect();
        assert_eq!(after.len(), before.len());
        assert_eq!(after[0], before[0], "header");
        assert_eq!(
            after[1], before[1],
            "the other workout's row is byte-identical"
        );
        assert_ne!(after[2], before[2]);
        assert_eq!(cell(&out, 0, "Avg_Power_W"), "245", "a blank did not erase");
        assert_eq!(cell(&out, 1, "Ascent_m"), "84");
        assert_eq!(cell(&out, 1, "Ascent_Basis"), "route");
        assert_eq!(cell(&out, 1, "Route_Altitude_Max_m"), "61");
        assert_eq!(cell(&out, 1, "Captured_At"), "2026-10-07T12:00:00+02:00");
    }

    #[test]
    fn a_0030_start_falls_on_the_previous_diet_day() {
        let mut late = run(U1);
        late["start"] = json!("2026-10-08T00:30:00+02:00");
        let r = &records(json!([late]))[0];
        let cells = detail_cells(r, NOW);
        let get = |n: &str| cells.iter().find(|(c, _)| *c == n).unwrap().1.clone();
        assert_eq!(get("Date"), "2026-10-07");
        assert_eq!(get("Start_Time"), "00:30");
        // With no zone named, the start's own offset decides.
        let mut no_zone = run(U1);
        no_zone["start"] = json!("2026-10-08T00:30:00+01:00");
        no_zone.as_object_mut().unwrap().remove("time_zone");
        let r = &records(json!([no_zone]))[0];
        let cells = detail_cells(r, NOW);
        let get = |n: &str| cells.iter().find(|(c, _)| *c == n).unwrap().1.clone();
        assert_eq!(get("Date"), "2026-10-07");
        assert_eq!(get("TZ"), "+01:00");
    }

    #[test]
    fn a_name_with_a_comma_and_a_quote_round_trips() {
        let mut r = run(U1);
        r["workout_name"] = json!("Tempo, \"hard\" 5x1km");
        let (out, _) = upsert_detail_csv("", &records(json!([r.clone()])), NOW).unwrap();
        let out = out.unwrap();
        assert!(
            out.contains("\"Tempo, \"\"hard\"\" 5x1km\""),
            "RFC 4180 quoted"
        );
        assert_eq!(cell(&out, 0, "Workout_Name"), "Tempo, \"hard\" 5x1km");
        // And reads back as the same row: a repeat changes nothing.
        let (again, c) = upsert_detail_csv(&out, &records(json!([r])), NOW).unwrap();
        assert!(again.is_none());
        assert_eq!(c.unchanged, 1);
        // A newline in a name cannot split the row.
        let mut nl = run(U2);
        nl["workout_name"] = json!("two\nlines");
        let (out, _) = upsert_detail_csv("", &records(json!([nl])), NOW).unwrap();
        assert_eq!(out.unwrap().lines().count(), 2);
    }

    #[test]
    fn splits_replace_only_their_own_workout_when_they_change() {
        let (base, n) = upsert_splits_csv("", &records(json!([run(U1), run(U2)]))).unwrap();
        let base = base.unwrap();
        assert_eq!(n, 4);
        assert!(base.starts_with(&format!("{WORKOUT_SPLITS_HEADER}\n")));
        assert!(base.contains(&format!("{U1},km,1,1000,358\n")));
        // Unchanged → None.
        let (same, _) = upsert_splits_csv(&base, &records(json!([run(U1)]))).unwrap();
        assert!(same.is_none());
        // A third split arrives for U1: its rows are replaced in place, U2's kept.
        let mut more = run(U1);
        more["splits"]
            .as_array_mut()
            .unwrap()
            .push(json!({"kind": "km", "index": 3, "distance_m": 1000.0, "duration_s": 364.0}));
        let (out, n) = upsert_splits_csv(&base, &records(json!([more]))).unwrap();
        let out = out.unwrap();
        assert_eq!(n, 3);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[3], format!("{U1},km,3,1000,364"));
        assert_eq!(lines[4], format!("{U2},km,1,1000,358"));
        // A record with no splits leaves the file alone.
        let mut none = run(U1);
        none.as_object_mut().unwrap().remove("splits");
        assert!(upsert_splits_csv(&out, &records(json!([none])))
            .unwrap()
            .0
            .is_none());
    }

    #[test]
    fn malformed_or_oversized_records_are_rejected_with_a_reason() {
        let cases: Vec<(Value, &str)> = vec![
            (json!({"not": "an array"}), "not an array"),
            (json!([run("not a uuid!")]), "workout_uuid"),
            (json!([{"workout_uuid": U1}]), "schema_version"),
            (json!([run(U1), run(U1)]), "twice"),
            (
                Value::Array((0..51).map(|i| run(&format!("{i:X}"))).collect()),
                "cap",
            ),
        ];
        for (v, why) in cases {
            let err = parse_workout_records(&v).unwrap_err();
            assert!(err.contains(why), "{err} should mention {why}");
        }
        let mut neg = run(U1);
        neg["distance_m"] = json!(-1.0);
        assert!(parse_workout_records(&json!([neg]))
            .unwrap_err()
            .contains("distance_m"));
        let mut basis = run(U1);
        basis["pace_basis"] = json!("guessed");
        assert!(parse_workout_records(&json!([basis])).is_err());
        let mut huge = run(U1);
        huge["workout_name"] = json!("x".repeat(MAX_WORKOUT_RECORDS_BYTES));
        assert!(parse_workout_records(&json!([huge]))
            .unwrap_err()
            .contains("bytes"));
        // The turn path logs and carries on with none.
        assert!(turn_workout_records(Some(&json!("garbage"))).is_empty());
    }

    /// Unknown keys, including any position a future phone might add, are dropped on read:
    /// nothing outside the known columns can reach a file.
    #[test]
    fn unknown_keys_never_reach_a_file() {
        let mut r = run(U1);
        r["latitude"] = json!(45.4642);
        r["longitude"] = json!(9.19);
        let (out, _) = upsert_detail_csv("", &records(json!([r])), NOW).unwrap();
        let out = out.unwrap();
        assert!(!out.contains("45.4642"));
        assert!(!out.contains("9.19"));
    }

    #[tokio::test]
    async fn a_request_without_workout_records_writes_nothing() {
        assert!(turn_workout_records(None).is_empty());
        assert!(turn_workout_records(Some(&Value::Null)).is_empty());
        let vault = scratch();
        let broker = LockBroker::new();
        ingest_turn_workout_records(&vault, &broker, "turn-1", &[]).await;
        assert!(!vault.join("diet-logs").join(WORKOUT_DETAIL_LOG).exists());
        assert!(!vault.join("diet-logs").join(WORKOUT_SPLITS_LOG).exists());
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[tokio::test]
    async fn the_locked_write_upserts_and_releases_its_locks() {
        let vault = scratch();
        let broker = LockBroker::new();
        let recs = records(json!([run(U1)]));
        let c = write_workout_records(&vault, &broker, "turn-1", &recs)
            .await
            .unwrap();
        assert_eq!(c.added, 1);
        assert_eq!(broker.held_count(), 0, "every lock released");
        let c = write_workout_records(&vault, &broker, "turn-2", &recs)
            .await
            .unwrap();
        assert_eq!(c.unchanged, 1);
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[tokio::test]
    async fn the_endpoint_rejects_an_unauthenticated_request() {
        let st = crate::testutil::test_state();
        for auth in [None, Some("Bearer wrong")] {
            let mut req =
                Request::post("/jesse/diet/workouts").header("content-type", "application/json");
            if let Some(a) = auth {
                req = req.header("authorization", a);
            }
            let res = app(st.clone())
                .oneshot(
                    req.body(Body::from(json!({"records": [run(U1)]}).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn the_endpoint_refuses_a_malformed_page_and_writes_nothing() {
        let st = crate::testutil::test_state();
        let token = st.cfg.token.clone();
        let res = app(st)
            .oneshot(
                Request::post("/jesse/diet/workouts")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::from(
                        json!({"records": [run("bad uuid")]}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
}
