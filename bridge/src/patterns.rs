//! **Patterns**: what the owner's own logs say moves together, and just as usefully, what
//! they say does NOT.
//!
//! This replaced an app-side engine that correlated six intake variables against
//! day-over-day scale change and suppressed anything under a fixed |rho| of 0.30. Over the
//! real logs every one of its eight pairs had enough data and every one landed under the
//! floor, so the screen said "nothing worth flagging yet" forever. The design guaranteed it:
//! the outcome was the noisiest number in the stack (water, glycogen and weigh-in timing
//! swing it by a pound or two), days already flagged as hydration artifacts sat in the
//! sample, the floor ignored sample size, a well measured null was thrown away, and rho is
//! not a number anyone can act on.
//!
//! What this does instead:
//!
//! 1. **A preregistered catalogue** ([`PATTERN_CATALOGUE`]), not a sweep. Each question names a
//!    driver, an outcome, a lag, the direction a known mechanism predicts, and the smallest
//!    effect worth caring about, in the outcome's own units. Nothing outside it is tested.
//! 2. **Better outcomes.** Sleep, next-morning HRV and resting heart rate (from the vitals
//!    ledger the phone fills, see [`crate::vitals`]), and a weight RESIDUAL in place of raw
//!    day-over-day change: the morning weight minus its trailing seven-day mean, with days
//!    flagged `Hydration_Artifact` excluded from both.
//! 3. **Effects in units.** Days are split into two arms (a fixed threshold where the
//!    question has one, otherwise the driver's own median) and the effect is the difference
//!    in outcome means, with a 95% interval from a moving block bootstrap (seven-day blocks,
//!    so a run of similar days is not counted as independent evidence; fixed seed, so the
//!    report is identical on every request). Spearman's rho is kept as a secondary field.
//! 4. **Three verdicts, all shown.** A *finding* has an interval that excludes zero, passes
//!    Benjamini-Hochberg across the whole catalogue at q = 0.10, and points the same way in
//!    both halves of the window. *Ruled out* means the interval sits entirely inside plus or
//!    minus the meaningful effect: a measured null, which is a result. Everything else is
//!    *watching*, with its arm sizes and a rough count of the days still needed.
//! 5. **An energy audit** over the last 28 days: what the log says went in, net of logged
//!    exercise, against what the scale trend says the body did, giving an implied
//!    maintenance with its interval.
//!
//! **Associations, never causes.** Every sentence the app shows is built here, in units,
//! so no view can restate one as a cause. UNKNOWN IS NOT ZERO throughout: a day missing a
//! driver or an outcome is left out of that question, never filled. The one deliberate
//! exception is spelled out at [`PatternSeries::from_logs`]: a day inside the exercise log's
//! span with food logged and no session is a rest day, 0 kcal of training.
//!
//! Everything here is pure: four CSV strings in, one JSON value out.

use crate::*;
use chrono::{Datelike, NaiveDate, Weekday};
use std::collections::BTreeMap;

/// A daily quantity by ISO date. A date that is absent is UNKNOWN for that quantity.
pub type DaySeries = BTreeMap<String, f64>;

/// Minimum days in EACH arm before a question is evaluated at all.
pub const MIN_PER_ARM: usize = 8;
/// Bootstrap resamples per question.
pub const BOOT_REPS: usize = 2000;
/// Moving block length, in paired days.
pub const BLOCK_LEN: usize = 7;
/// The false discovery rate the catalogue is held to.
pub const FDR_Q: f64 = 0.10;
/// How far back the catalogue looks, in calendar days before the latest logged date.
pub const WINDOW_DAYS: i64 = 365;
/// The energy audit's window, and how many of its days must have calories known.
pub const AUDIT_DAYS: i64 = 28;
pub const AUDIT_MIN_DAYS: usize = 21;
/// ... and how many usable weigh-ins it needs for a slope.
pub const AUDIT_MIN_WEIGHINS: usize = 14;
/// What the audit cannot see, stated beside it every time.
pub const AUDIT_NOTE: &str = "A four-week scale trend still carries water: the weeks after a \
maintenance block or travel read as a bigger deficit than the tissue lost, and a partly logged \
day reads as a smaller intake than was eaten.";
/// Energy in a pound of body mass, the conventional figure.
pub const KCAL_PER_LB: f64 = 3500.0;

/// The standing caveat, fixed here so no view can soften it.
pub const PATTERNS_CAVEAT: &str = "These compare days in your own logs, and they are \
associations, not causes. Two things that move together may both follow something else: a \
long run, a travel week, a salty meal that was also a big one. A finding is a question worth \
asking; ruled out means the difference was measured and was too small to matter.";

// ---- Dates -------------------------------------------------------------------------------

fn parse_day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

fn day_str(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// The date `back` days before `date`, or None for a malformed date.
fn shift(date: &str, back: i64) -> Option<String> {
    parse_day(date).map(|d| day_str(d - chrono::Duration::days(back)))
}

/// Minutes since midnight for `HH:MM`, with the small hours (before 04:00, when a diet day
/// still belongs to the evening before) counted past midnight: `00:30` is 1470.
fn diet_clock_minutes(t: &str) -> Option<i64> {
    let (h, m) = t.trim().split_once(':')?;
    let (h, m) = (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?);
    if !(0..24).contains(&h) || !(0..60).contains(&m) {
        return None;
    }
    let mins = h * 60 + m;
    Some(if mins < 4 * 60 { mins + 24 * 60 } else { mins })
}

// ---- Daily series ------------------------------------------------------------------------

/// Every daily quantity the catalogue and the audit read.
#[derive(Debug, Default, Clone)]
pub struct PatternSeries {
    pub calories: DaySeries,
    pub carbs: DaySeries,
    pub sodium: DaySeries,
    pub sat_fat: DaySeries,
    pub alcohol_g: DaySeries,
    /// Caffeine (mg) in items eaten at 14:00 or later.
    pub caffeine_late: DaySeries,
    /// The diet-clock minute of the last item with calories (see [`diet_clock_minutes`]).
    pub last_food_min: DaySeries,
    /// Logged training kcal, with rest days (see [`PatternSeries::from_logs`]) as 0.
    pub exercise_kcal: DaySeries,
    /// 1 on a Saturday or Sunday, else 0, for every date with calories known.
    pub weekend: DaySeries,
    /// 1 on a day with any logged training kcal, else 0.
    pub training_day: DaySeries,
    /// Morning weigh-ins that are not hydration artifacts.
    pub weight: DaySeries,
    /// Morning weight minus the mean of the non-artifact weigh-ins over the seven days
    /// before it (at least four of them), for non-artifact mornings only.
    pub weight_residual: DaySeries,
    pub sleep_min: DaySeries,
    pub hrv: DaySeries,
    pub resting_hr: DaySeries,
}

fn header_map(h: &csv::StringRecord) -> HashMap<String, usize> {
    h.iter()
        .enumerate()
        .map(|(i, n)| (n.trim().to_string(), i))
        .collect()
}

fn cell<'a>(idx: &HashMap<String, usize>, rec: &'a csv::StringRecord, name: &str) -> &'a str {
    idx.get(name)
        .and_then(|&i| rec.get(i))
        .map(str::trim)
        .unwrap_or("")
}

fn num(idx: &HashMap<String, usize>, rec: &csv::StringRecord, name: &str) -> Option<f64> {
    let s = cell(idx, rec, name);
    if s.is_empty() {
        return None;
    }
    s.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// A log's header map and its row reader.
type CsvRows<'a> = (HashMap<String, usize>, csv::Reader<&'a [u8]>);

fn reader(content: &str) -> Option<CsvRows<'_>> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(content.as_bytes());
    let idx = header_map(rdr.headers().ok()?);
    Some((idx, rdr))
}

/// A sum over the items of one day that KNOWS whether it knows: `known` only once at least
/// one item carried a value.
#[derive(Default, Clone, Copy)]
struct Tally {
    sum: f64,
    known: bool,
}

impl Tally {
    fn add(&mut self, v: Option<f64>) {
        if let Some(x) = v {
            self.sum += x;
            self.known = true;
        }
    }
    fn get(self) -> Option<f64> {
        self.known.then_some(self.sum)
    }
}

#[derive(Default)]
struct FoodDay {
    calories: Tally,
    carbs: Tally,
    sodium: Tally,
    sat_fat: Tally,
    alcohol: Tally,
    caffeine_any: Tally,
    caffeine_late: Tally,
    last_food: Option<i64>,
}

impl PatternSeries {
    /// Build every series from the four logs' contents (any may be empty).
    ///
    /// Food: each nutrient is the day's sum of the items that KNOW it, and a day where no
    /// item knows it is absent. Calories fall back to `Cal_per_100g x Grams / 100`, as the
    /// day view does. Late caffeine is known for a day on which any item carries a caffeine
    /// figure. The last food time is the latest item with calories.
    ///
    /// Exercise, and the one place a gap becomes a 0: the exercise log has no row for a
    /// rest day, so correlating training against anything used to compare training days
    /// only with other training days. A date on which food was logged, falling between the
    /// exercise log's first and last dates and carrying no session, is read as a REST DAY
    /// with 0 training kcal. Outside that span nothing is inferred.
    ///
    /// Weight: `Hydration_Artifact = true` mornings are excluded outright, from the
    /// residual's own day and from every trailing mean. Two rows for a date keep the last.
    pub fn from_logs(food: &str, exercise: &str, weight: &str, vitals: &str) -> Self {
        let mut s = PatternSeries::default();

        let mut days: BTreeMap<String, FoodDay> = BTreeMap::new();
        if let Some((idx, mut rdr)) = reader(food) {
            for rec in rdr.records().flatten() {
                let date = cell(&idx, &rec, "Date");
                if valid_iso_date(date).is_none() {
                    continue;
                }
                let d = days.entry(date.to_string()).or_default();
                let cal = num(&idx, &rec, "Calories").or_else(|| {
                    match (num(&idx, &rec, "Cal_per_100g"), num(&idx, &rec, "Grams")) {
                        (Some(c), Some(g)) => Some((c * g / 100.0).round()),
                        _ => None,
                    }
                });
                d.calories.add(cal);
                d.carbs.add(num(&idx, &rec, "Carbs_g"));
                d.sodium.add(num(&idx, &rec, "Sodium_mg"));
                d.sat_fat.add(num(&idx, &rec, "SatFat_g"));
                d.alcohol.add(num(&idx, &rec, "Alcohol_g"));
                let caf = num(&idx, &rec, "Caffeine_mg");
                d.caffeine_any.add(caf);
                let clock = diet_clock_minutes(cell(&idx, &rec, "Time"));
                if clock.is_some_and(|m| m >= 14 * 60) {
                    d.caffeine_late.add(caf);
                }
                if let (Some(m), Some(c)) = (clock, cal) {
                    if c > 0.0 {
                        d.last_food = Some(d.last_food.map_or(m, |p| p.max(m)));
                    }
                }
            }
        }
        for (date, d) in &days {
            let put = |series: &mut DaySeries, v: Option<f64>| {
                if let Some(v) = v {
                    series.insert(date.clone(), v);
                }
            };
            put(&mut s.calories, d.calories.get());
            put(&mut s.carbs, d.carbs.get());
            put(&mut s.sodium, d.sodium.get());
            put(&mut s.sat_fat, d.sat_fat.get());
            put(&mut s.alcohol_g, d.alcohol.get());
            // Late caffeine is 0 on a day that tracked caffeine and had none after 14:00.
            put(
                &mut s.caffeine_late,
                d.caffeine_any
                    .get()
                    .map(|_| d.caffeine_late.get().unwrap_or(0.0)),
            );
            put(&mut s.last_food_min, d.last_food.map(|m| m as f64));
        }
        for date in s.calories.keys() {
            if let Some(d) = parse_day(date) {
                let w = matches!(d.weekday(), Weekday::Sat | Weekday::Sun);
                s.weekend.insert(date.clone(), if w { 1.0 } else { 0.0 });
            }
        }

        if let Some((idx, mut rdr)) = reader(exercise) {
            let mut kcal: DaySeries = BTreeMap::new();
            for rec in rdr.records().flatten() {
                let date = cell(&idx, &rec, "Date");
                if valid_iso_date(date).is_none() {
                    continue;
                }
                *kcal.entry(date.to_string()).or_insert(0.0) +=
                    num(&idx, &rec, "Calories").unwrap_or(0.0);
            }
            if let (Some(first), Some(last)) = (
                kcal.keys().next().cloned(),
                kcal.keys().next_back().cloned(),
            ) {
                for date in days.keys() {
                    if *date >= first && *date <= last && !kcal.contains_key(date) {
                        kcal.insert(date.clone(), 0.0);
                    }
                }
            }
            for (date, k) in &kcal {
                s.training_day
                    .insert(date.clone(), if *k > 0.0 { 1.0 } else { 0.0 });
            }
            s.exercise_kcal = kcal;
        }

        if let Some((idx, mut rdr)) = reader(weight) {
            let mut rows: BTreeMap<String, (f64, bool)> = BTreeMap::new();
            for rec in rdr.records().flatten() {
                let date = cell(&idx, &rec, "Date");
                let Some(lbs) = num(&idx, &rec, "Weight_lbs") else {
                    continue;
                };
                if valid_iso_date(date).is_none() {
                    continue;
                }
                let artifact = cell(&idx, &rec, "Hydration_Artifact").eq_ignore_ascii_case("true");
                rows.insert(date.to_string(), (lbs, artifact));
            }
            for (date, (lbs, artifact)) in &rows {
                if !artifact {
                    s.weight.insert(date.clone(), *lbs);
                }
            }
            for (date, lbs) in &s.weight {
                let prior: Vec<f64> = (1..=7)
                    .filter_map(|back| shift(date, back).and_then(|d| s.weight.get(&d).copied()))
                    .collect();
                if prior.len() >= 4 {
                    let mean = prior.iter().sum::<f64>() / prior.len() as f64;
                    s.weight_residual.insert(date.clone(), lbs - mean);
                }
            }
        }

        if let Some((idx, mut rdr)) = reader(vitals) {
            for rec in rdr.records().flatten() {
                let date = cell(&idx, &rec, "Date");
                if valid_iso_date(date).is_none() {
                    continue;
                }
                let put = |series: &mut DaySeries, col: &str| {
                    if let Some(v) = num(&idx, &rec, col) {
                        series.insert(date.to_string(), v);
                    }
                };
                put(&mut s.sleep_min, "Sleep_min");
                put(&mut s.hrv, "HRV_SDNN_ms");
                put(&mut s.resting_hr, "Resting_HR_bpm");
            }
        }
        s
    }

    /// Drop the diet day still being logged from every intake and training series. Its
    /// food and exercise are PARTIAL until it closes, and a half-eaten day read as a whole
    /// one would sit in the low-calorie arm of every question it touches. Its morning
    /// weight and last night's vitals are complete, so they stay.
    pub fn close_open_day(&mut self, open_day: &str) {
        for series in [
            &mut self.calories,
            &mut self.carbs,
            &mut self.sodium,
            &mut self.sat_fat,
            &mut self.alcohol_g,
            &mut self.caffeine_late,
            &mut self.last_food_min,
            &mut self.exercise_kcal,
            &mut self.weekend,
            &mut self.training_day,
        ] {
            series.retain(|d, _| d.as_str() < open_day);
        }
    }

    /// The latest date any log knows, which anchors the window.
    fn latest_date(&self) -> Option<String> {
        [
            &self.calories,
            &self.exercise_kcal,
            &self.weight,
            &self.sleep_min,
            &self.hrv,
        ]
        .iter()
        .filter_map(|s| s.keys().next_back())
        .max()
        .cloned()
    }
}

// ---- The catalogue -----------------------------------------------------------------------

/// Which daily series a question reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternVar {
    Calories,
    Carbs,
    Sodium,
    SatFat,
    Alcohol,
    CaffeineLate,
    LastFood,
    ExerciseKcal,
    TrainingDay,
    Weekend,
    WeightResidual,
    Sleep,
    Hrv,
    RestingHr,
}

impl PatternVar {
    fn series(self, s: &PatternSeries) -> &DaySeries {
        match self {
            PatternVar::Calories => &s.calories,
            PatternVar::Carbs => &s.carbs,
            PatternVar::Sodium => &s.sodium,
            PatternVar::SatFat => &s.sat_fat,
            PatternVar::Alcohol => &s.alcohol_g,
            PatternVar::CaffeineLate => &s.caffeine_late,
            PatternVar::LastFood => &s.last_food_min,
            PatternVar::ExerciseKcal => &s.exercise_kcal,
            PatternVar::TrainingDay => &s.training_day,
            PatternVar::Weekend => &s.weekend,
            PatternVar::WeightResidual => &s.weight_residual,
            PatternVar::Sleep => &s.sleep_min,
            PatternVar::Hrv => &s.hrv,
            PatternVar::RestingHr => &s.resting_hr,
        }
    }
}

/// How a question's days are split into a HIGH arm and a LOW arm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PatternSplit {
    /// High is strictly above the driver's own median over the paired days.
    Median,
    /// High is strictly above this value (0 reads as "any").
    Above(f64),
    /// High is at or above this value.
    AtLeast(f64),
}

/// The direction a known mechanism predicts for HIGH minus LOW.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternExpect {
    Higher,
    Lower,
    Either,
}

/// One preregistered question. Its outcome on date `d` is paired with its driver on date
/// `d - lag`.
#[derive(Debug, Clone, Copy)]
pub struct PatternQuestion {
    pub id: &'static str,
    pub title: &'static str,
    pub driver: PatternVar,
    pub outcome: PatternVar,
    pub lag: i64,
    pub split: PatternSplit,
    /// The arms, as the sentence names them ("days with alcohol", "days without").
    pub high: &'static str,
    pub low: &'static str,
    /// The comparison preposition: "after" for a lagged driver, "on" for same-day.
    pub relation: &'static str,
    /// The outcome as a sentence subject ("next-morning HRV").
    pub outcome_label: &'static str,
    pub unit: &'static str,
    pub decimals: usize,
    /// What one paired day is called ("mornings", "nights", "days").
    pub noun: &'static str,
    /// The smallest difference worth caring about, in `unit`.
    pub meaningful: f64,
    pub expect: PatternExpect,
}

/// The catalogue. Order is display order within a verdict and never changes a result.
pub const PATTERN_CATALOGUE: &[PatternQuestion] = &[
    PatternQuestion {
        id: "sodium-weight",
        title: "Sodium and next-morning weight",
        driver: PatternVar::Sodium,
        outcome: PatternVar::WeightResidual,
        lag: 1,
        split: PatternSplit::Median,
        high: "saltier days",
        low: "less salty days",
        relation: "after",
        outcome_label: "morning weight (against its 7-day trend)",
        unit: "lb",
        decimals: 1,
        noun: "mornings",
        meaningful: 0.5,
        expect: PatternExpect::Higher,
    },
    PatternQuestion {
        id: "carbs-weight",
        title: "Carbs and next-morning weight",
        driver: PatternVar::Carbs,
        outcome: PatternVar::WeightResidual,
        lag: 1,
        split: PatternSplit::Median,
        high: "higher-carb days",
        low: "lower-carb days",
        relation: "after",
        outcome_label: "morning weight (against its 7-day trend)",
        unit: "lb",
        decimals: 1,
        noun: "mornings",
        meaningful: 0.5,
        expect: PatternExpect::Higher,
    },
    PatternQuestion {
        id: "alcohol-weight",
        title: "Alcohol and next-morning weight",
        driver: PatternVar::Alcohol,
        outcome: PatternVar::WeightResidual,
        lag: 1,
        split: PatternSplit::Above(0.0),
        high: "days with alcohol",
        low: "days without",
        relation: "after",
        outcome_label: "morning weight (against its 7-day trend)",
        unit: "lb",
        decimals: 1,
        noun: "mornings",
        meaningful: 0.5,
        expect: PatternExpect::Either,
    },
    PatternQuestion {
        id: "alcohol-sleep",
        title: "Alcohol and that night's sleep",
        driver: PatternVar::Alcohol,
        outcome: PatternVar::Sleep,
        lag: 1,
        split: PatternSplit::Above(0.0),
        high: "evenings with alcohol",
        low: "evenings without",
        relation: "after",
        outcome_label: "sleep",
        unit: "min",
        decimals: 0,
        noun: "nights",
        meaningful: 20.0,
        expect: PatternExpect::Lower,
    },
    PatternQuestion {
        id: "alcohol-hrv",
        title: "Alcohol and next-morning HRV",
        driver: PatternVar::Alcohol,
        outcome: PatternVar::Hrv,
        lag: 1,
        split: PatternSplit::Above(0.0),
        high: "days with alcohol",
        low: "days without",
        relation: "after",
        outcome_label: "next-day HRV",
        unit: "ms",
        decimals: 0,
        noun: "mornings",
        meaningful: 5.0,
        expect: PatternExpect::Lower,
    },
    PatternQuestion {
        id: "alcohol-rhr",
        title: "Alcohol and next-morning resting heart rate",
        driver: PatternVar::Alcohol,
        outcome: PatternVar::RestingHr,
        lag: 1,
        split: PatternSplit::Above(0.0),
        high: "days with alcohol",
        low: "days without",
        relation: "after",
        outcome_label: "next-day resting heart rate",
        unit: "bpm",
        decimals: 1,
        noun: "mornings",
        meaningful: 2.0,
        expect: PatternExpect::Higher,
    },
    PatternQuestion {
        id: "latefood-sleep",
        title: "A late last meal and that night's sleep",
        driver: PatternVar::LastFood,
        outcome: PatternVar::Sleep,
        lag: 1,
        split: PatternSplit::AtLeast(21.0 * 60.0),
        high: "days eating at 21:00 or later",
        low: "days finished earlier",
        relation: "after",
        outcome_label: "sleep",
        unit: "min",
        decimals: 0,
        noun: "nights",
        meaningful: 20.0,
        expect: PatternExpect::Lower,
    },
    PatternQuestion {
        id: "caffeine-sleep",
        title: "Caffeine after 14:00 and that night's sleep",
        driver: PatternVar::CaffeineLate,
        outcome: PatternVar::Sleep,
        lag: 1,
        split: PatternSplit::Above(0.0),
        high: "days with caffeine after 14:00",
        low: "days without",
        relation: "after",
        outcome_label: "sleep",
        unit: "min",
        decimals: 0,
        noun: "nights",
        meaningful: 20.0,
        expect: PatternExpect::Lower,
    },
    PatternQuestion {
        id: "training-hrv",
        title: "Training load and next-morning HRV",
        driver: PatternVar::ExerciseKcal,
        outcome: PatternVar::Hrv,
        lag: 1,
        split: PatternSplit::Median,
        high: "harder training days",
        low: "lighter or rest days",
        relation: "after",
        outcome_label: "next-day HRV",
        unit: "ms",
        decimals: 0,
        noun: "mornings",
        meaningful: 5.0,
        expect: PatternExpect::Lower,
    },
    PatternQuestion {
        id: "training-rhr",
        title: "Training load and next-morning resting heart rate",
        driver: PatternVar::ExerciseKcal,
        outcome: PatternVar::RestingHr,
        lag: 1,
        split: PatternSplit::Median,
        high: "harder training days",
        low: "lighter or rest days",
        relation: "after",
        outcome_label: "next-day resting heart rate",
        unit: "bpm",
        decimals: 1,
        noun: "mornings",
        meaningful: 2.0,
        expect: PatternExpect::Higher,
    },
    PatternQuestion {
        id: "sleep-calories",
        title: "Sleep and the next day's calories",
        driver: PatternVar::Sleep,
        outcome: PatternVar::Calories,
        lag: 0,
        split: PatternSplit::Median,
        high: "longer nights",
        low: "shorter nights",
        relation: "after",
        outcome_label: "calorie intake",
        unit: "kcal",
        decimals: 0,
        noun: "days",
        meaningful: 200.0,
        expect: PatternExpect::Lower,
    },
    PatternQuestion {
        id: "sleep-satfat",
        title: "Sleep and the next day's saturated fat",
        driver: PatternVar::Sleep,
        outcome: PatternVar::SatFat,
        lag: 0,
        split: PatternSplit::Median,
        high: "longer nights",
        low: "shorter nights",
        relation: "after",
        outcome_label: "saturated fat intake",
        unit: "g",
        decimals: 0,
        noun: "days",
        meaningful: 5.0,
        expect: PatternExpect::Lower,
    },
    PatternQuestion {
        id: "trainingday-calories",
        title: "Training days and calories",
        driver: PatternVar::TrainingDay,
        outcome: PatternVar::Calories,
        lag: 0,
        split: PatternSplit::Above(0.0),
        high: "training days",
        low: "rest days",
        relation: "on",
        outcome_label: "calorie intake",
        unit: "kcal",
        decimals: 0,
        noun: "days",
        meaningful: 200.0,
        expect: PatternExpect::Higher,
    },
    PatternQuestion {
        id: "weekend-calories",
        title: "Weekends and calories",
        driver: PatternVar::Weekend,
        outcome: PatternVar::Calories,
        lag: 0,
        split: PatternSplit::Above(0.0),
        high: "weekend days",
        low: "weekdays",
        relation: "on",
        outcome_label: "calorie intake",
        unit: "kcal",
        decimals: 0,
        noun: "days",
        meaningful: 200.0,
        expect: PatternExpect::Higher,
    },
    PatternQuestion {
        id: "alcohol-calories",
        title: "Alcohol and the next day's calories",
        driver: PatternVar::Alcohol,
        outcome: PatternVar::Calories,
        lag: 1,
        split: PatternSplit::Above(0.0),
        high: "days with alcohol",
        low: "days without",
        relation: "after",
        outcome_label: "calorie intake",
        unit: "kcal",
        decimals: 0,
        noun: "days",
        meaningful: 200.0,
        expect: PatternExpect::Higher,
    },
];

// ---- Statistics --------------------------------------------------------------------------

/// SplitMix64: a tiny, well-mixed, deterministic generator. The bootstrap needs
/// reproducibility (the same logs give the same report on every request), not
/// cryptographic quality, and a dependency for twelve lines would be the wrong trade.
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// A stable seed per question id (FNV-1a), so adding a question never reshuffles another.
fn seed_for(id: &str) -> u64 {
    id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn mean(xs: impl Iterator<Item = f64>) -> Option<f64> {
    let (s, n) = xs.fold((0.0, 0usize), |(s, n), x| (s + x, n + 1));
    (n > 0).then(|| s / n as f64)
}

fn median(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

/// The `p`-quantile of sorted data, linear between order statistics.
fn quantile(sorted: &[f64], p: f64) -> f64 {
    let pos = p * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

/// Fractional ranks (1-based), ties averaged.
fn ranks(xs: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..xs.len()).collect();
    idx.sort_by(|&a, &b| xs[a].total_cmp(&xs[b]));
    let mut out = vec![0.0; xs.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && xs[idx[j + 1]] == xs[idx[i]] {
            j += 1;
        }
        let r = (i + j + 2) as f64 / 2.0;
        for &k in &idx[i..=j] {
            out[k] = r;
        }
        i = j + 1;
    }
    out
}

/// Spearman's rho, or None for fewer than three points or a constant side.
pub fn spearman(xs: &[f64], ys: &[f64]) -> Option<f64> {
    if xs.len() != ys.len() || xs.len() < 3 {
        return None;
    }
    let (rx, ry) = (ranks(xs), ranks(ys));
    let n = rx.len() as f64;
    let (mx, my) = (rx.iter().sum::<f64>() / n, ry.iter().sum::<f64>() / n);
    let (mut num, mut dx, mut dy) = (0.0, 0.0, 0.0);
    for (a, b) in rx.iter().zip(&ry) {
        num += (a - mx) * (b - my);
        dx += (a - mx) * (a - mx);
        dy += (b - my) * (b - my);
    }
    (dx > 0.0 && dy > 0.0).then(|| (num / (dx * dy).sqrt()).clamp(-1.0, 1.0))
}

/// HIGH-arm mean minus LOW-arm mean over `pairs` (driver-is-high, outcome), or None when
/// either arm is empty.
fn arm_diff<'a>(pairs: impl Iterator<Item = &'a (bool, f64)>) -> Option<f64> {
    let (mut sh, mut nh, mut sl, mut nl) = (0.0, 0usize, 0.0, 0usize);
    for &(high, y) in pairs {
        if high {
            sh += y;
            nh += 1;
        } else {
            sl += y;
            nl += 1;
        }
    }
    (nh > 0 && nl > 0).then(|| sh / nh as f64 - sl / nl as f64)
}

/// Moving block bootstrap of `stat` over `n` ordered items: each resample concatenates
/// blocks of [`BLOCK_LEN`] consecutive indices (wrapping at the end) from uniformly drawn
/// starts until it holds `n`, so the day-to-day dependence inside a block survives
/// resampling. Resamples where `stat` is undefined are dropped.
fn block_bootstrap(n: usize, seed: u64, stat: impl Fn(&[usize]) -> Option<f64>) -> Vec<f64> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(BOOT_REPS);
    let mut sample = Vec::with_capacity(n + BLOCK_LEN);
    let block = BLOCK_LEN.min(n.max(1));
    for _ in 0..BOOT_REPS {
        sample.clear();
        while sample.len() < n {
            let start = rng.below(n);
            for k in 0..block {
                if sample.len() == n {
                    break;
                }
                sample.push((start + k) % n);
            }
        }
        if let Some(v) = stat(&sample) {
            out.push(v);
        }
    }
    out.sort_by(|a, b| a.total_cmp(b));
    out
}

/// Benjamini-Hochberg: which of `ps` are discoveries at level `q`, with `m = ps.len()`.
pub fn benjamini_hochberg(ps: &[f64], q: f64) -> Vec<bool> {
    let m = ps.len();
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|&a, &b| ps[a].total_cmp(&ps[b]));
    let cutoff = order
        .iter()
        .enumerate()
        .filter(|(rank, &i)| ps[i] <= (rank + 1) as f64 / m as f64 * q)
        .map(|(rank, _)| rank + 1)
        .max()
        .unwrap_or(0);
    let mut out = vec![false; m];
    for &i in order.iter().take(cutoff) {
        out[i] = true;
    }
    out
}

// ---- Evaluating one question -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternVerdict {
    Finding,
    RuledOut,
    Watching,
}

impl PatternVerdict {
    fn key(self) -> &'static str {
        match self {
            PatternVerdict::Finding => "finding",
            PatternVerdict::RuledOut => "ruledOut",
            PatternVerdict::Watching => "watching",
        }
    }
}

/// The arithmetic for one question, before the catalogue-wide FDR step.
#[derive(Debug, Clone)]
pub struct PatternEvaluation {
    pub n_high: usize,
    pub n_low: usize,
    pub mean_high: Option<f64>,
    pub mean_low: Option<f64>,
    /// HIGH minus LOW, when both arms reach [`MIN_PER_ARM`].
    pub effect: Option<f64>,
    pub ci: Option<(f64, f64)>,
    pub p: Option<f64>,
    pub rho: Option<f64>,
    /// The effect in the earlier and the later half of the paired days.
    pub halves: Option<(f64, f64)>,
    /// The split point, in the driver's units.
    pub threshold: Option<f64>,
    pub first: Option<String>,
    pub last: Option<String>,
}

/// Pair a question's driver and outcome inside `[from, to]`: every outcome date `d` whose
/// driver is known on `d - lag`, ascending by `d`.
fn pairs_for(
    q: &PatternQuestion,
    s: &PatternSeries,
    from: &str,
    to: &str,
) -> Vec<(String, f64, f64)> {
    let (xs, ys) = (q.driver.series(s), q.outcome.series(s));
    ys.range(from.to_string()..=to.to_string())
        .filter_map(|(d, &y)| {
            let x = *xs.get(&shift(d, q.lag)?)?;
            Some((d.clone(), x, y))
        })
        .collect()
}

/// Evaluate one question over its paired days.
pub fn evaluate_question(
    q: &PatternQuestion,
    s: &PatternSeries,
    from: &str,
    to: &str,
) -> PatternEvaluation {
    let pairs = pairs_for(q, s, from, to);
    let xs: Vec<f64> = pairs.iter().map(|p| p.1).collect();
    let ys: Vec<f64> = pairs.iter().map(|p| p.2).collect();
    let threshold = match q.split {
        PatternSplit::Median => median(&xs),
        PatternSplit::Above(t) | PatternSplit::AtLeast(t) => Some(t),
    };
    let is_high = |x: f64| match (q.split, threshold) {
        (PatternSplit::AtLeast(t), _) => x >= t,
        (_, Some(t)) => x > t,
        _ => false,
    };
    let arms: Vec<(bool, f64)> = pairs.iter().map(|p| (is_high(p.1), p.2)).collect();
    let n_high = arms.iter().filter(|a| a.0).count();
    let n_low = arms.len() - n_high;
    let mean_high = mean(arms.iter().filter(|a| a.0).map(|a| a.1));
    let mean_low = mean(arms.iter().filter(|a| !a.0).map(|a| a.1));
    let mut e = PatternEvaluation {
        n_high,
        n_low,
        mean_high,
        mean_low,
        effect: None,
        ci: None,
        p: None,
        rho: spearman(&xs, &ys),
        halves: None,
        threshold,
        first: pairs.first().map(|p| p.0.clone()),
        last: pairs.last().map(|p| p.0.clone()),
    };
    if n_high < MIN_PER_ARM || n_low < MIN_PER_ARM {
        return e;
    }
    let Some(effect) = arm_diff(arms.iter()) else {
        return e;
    };
    e.effect = Some(effect);
    let boots = block_bootstrap(arms.len(), seed_for(q.id), |idx| {
        arm_diff(idx.iter().map(|&i| &arms[i]))
    });
    if boots.len() >= BOOT_REPS / 2 {
        e.ci = Some((quantile(&boots, 0.025), quantile(&boots, 0.975)));
        // Two-sided p from the bootstrap distribution recentred on the estimate: how often
        // a resample lands at least as far from the estimate as the estimate is from 0.
        let extreme = boots
            .iter()
            .filter(|b| (*b - effect).abs() >= effect.abs())
            .count();
        e.p = Some((extreme + 1) as f64 / (boots.len() + 1) as f64);
    }
    let mid = arms.len() / 2;
    if let (Some(a), Some(b)) = (arm_diff(arms[..mid].iter()), arm_diff(arms[mid..].iter())) {
        e.halves = Some((a, b));
    }
    e
}

/// The verdict, given whether the question survived the catalogue-wide FDR cut.
pub fn question_verdict(
    q: &PatternQuestion,
    e: &PatternEvaluation,
    fdr_pass: bool,
) -> PatternVerdict {
    let (Some(effect), Some((lo, hi))) = (e.effect, e.ci) else {
        return PatternVerdict::Watching;
    };
    let excludes_zero = lo > 0.0 || hi < 0.0;
    let halves_agree = e
        .halves
        .is_some_and(|(a, b)| a.signum() == effect.signum() && b.signum() == effect.signum());
    if excludes_zero && fdr_pass && halves_agree {
        PatternVerdict::Finding
    } else if lo > -q.meaningful && hi < q.meaningful {
        PatternVerdict::RuledOut
    } else {
        PatternVerdict::Watching
    }
}

/// A rough count of further paired days before a watching question could settle either
/// way, assuming the interval narrows with the square root of the sample. None when there
/// is no honest estimate.
pub fn question_days_needed(q: &PatternQuestion, e: &PatternEvaluation) -> Option<usize> {
    let n = (e.n_high + e.n_low) as f64;
    if n == 0.0 {
        return None;
    }
    let round5 = |x: f64| ((x / 5.0).ceil() * 5.0).max(5.0) as usize;
    if e.n_high < MIN_PER_ARM || e.n_low < MIN_PER_ARM {
        // Reach the per-arm minimum at the arms' current rates.
        let need = |have: usize| -> Option<f64> {
            if have >= MIN_PER_ARM {
                return Some(0.0);
            }
            if have == 0 {
                return None;
            }
            Some((MIN_PER_ARM - have) as f64 / (have as f64 / n))
        };
        let more = need(e.n_high)?.max(need(e.n_low)?);
        return Some(round5(more));
    }
    let (Some(effect), Some((lo, hi))) = (e.effect, e.ci) else {
        return None;
    };
    let half = (hi - lo) / 2.0;
    // The widest interval that would settle it: narrow enough to sit inside the meaningful
    // band, or narrow enough to exclude zero, whichever comes first.
    let target = (q.meaningful - effect.abs()).max(effect.abs());
    if target <= 0.0 || half <= target {
        return None;
    }
    let more = n * ((half / target).powi(2) - 1.0);
    (more <= 10.0 * n).then(|| round5(more))
}

// ---- Wording -----------------------------------------------------------------------------

/// A number, thousands separated, with `decimals` places and no sign.
fn number(v: f64, decimals: usize) -> String {
    let s = format!("{:.*}", decimals, v.abs());
    let (int, frac) = s
        .split_once('.')
        .map_or((s.as_str(), None), |(i, f)| (i, Some(f)));
    let mut grouped = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    match frac {
        Some(f) => format!("{grouped}.{f}"),
        None => grouped,
    }
}

/// A number in a unit: "1,183 kcal", "0.7 lb".
fn amount(v: f64, decimals: usize, unit: &str) -> String {
    format!("{} {unit}", number(v, decimals))
}

/// A signed difference in words: "7 ms lower", "0.6 lb higher", or "about the same" when it
/// rounds to nothing.
fn direction(v: f64, decimals: usize, unit: &str) -> String {
    if number(v, decimals)
        .trim_start_matches(['0', '.', ','])
        .is_empty()
    {
        return "about the same".to_string();
    }
    format!(
        "{} {}",
        amount(v, decimals, unit),
        if v > 0.0 { "higher" } else { "lower" }
    )
}

/// An interval in words, relative to zero: "3 to 11 lower", "2 lower to 3 higher".
fn interval(lo: f64, hi: f64, decimals: usize) -> String {
    let n = |v: f64| number(v, decimals);
    if lo >= 0.0 {
        format!("{} to {} higher", n(lo), n(hi))
    } else if hi <= 0.0 {
        format!("{} to {} lower", n(hi), n(lo))
    } else {
        format!("{} lower to {} higher", n(lo), n(hi))
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Why a question whose interval already excludes zero is still only watching, when that is
/// the case, in words for the row.
pub fn watching_reason(e: &PatternEvaluation, fdr_pass: bool) -> Option<&'static str> {
    let (effect, (lo, hi)) = (e.effect?, e.ci?);
    if !(lo > 0.0 || hi < 0.0) {
        return None;
    }
    let agree = e
        .halves
        .is_some_and(|(a, b)| a.signum() == effect.signum() && b.signum() == effect.signum());
    if !agree {
        Some("the earlier and later halves of the window disagree")
    } else if !fdr_pass {
        Some("not yet clear of the check for asking many questions at once")
    } else {
        None
    }
}

/// The row's sentence. Associational by construction: every verdict states the same
/// comparison of two kinds of day ("after days with alcohol, next-day HRV was 7 ms lower
/// than after days without") and none says one did anything to the other.
pub fn question_sentence(
    q: &PatternQuestion,
    e: &PatternEvaluation,
    v: PatternVerdict,
    more: Option<usize>,
    reason: Option<&str>,
) -> String {
    let arms = format!("{} vs {} {}", e.n_high, e.n_low, q.noun);
    let more = match (reason, more) {
        (Some(r), _) => format!("; {r}"),
        (None, Some(m)) => format!("; roughly {m} more days"),
        (None, None) => String::new(),
    };
    let (Some(eff), Some((lo, hi))) = (e.effect, e.ci) else {
        return format!("Not enough days yet: {arms} so far, {MIN_PER_ARM} needed in each{more}.");
    };
    let diff = direction(eff, q.decimals, q.unit);
    let than = if diff == "about the same" {
        "as"
    } else {
        "than"
    };
    let compared = format!(
        "{} {}, {} was {diff} {than} {} {}",
        q.relation, q.high, q.outcome_label, q.relation, q.low
    );
    let range = interval(lo, hi, q.decimals);
    match v {
        PatternVerdict::Finding => format!("{} ({range}), {arms}.", capitalize(&compared)),
        PatternVerdict::RuledOut => format!(
            "No meaningful difference: {compared} ({range}, inside the {} that would matter), \
             {arms}.",
            amount(q.meaningful, q.decimals, q.unit)
        ),
        PatternVerdict::Watching => format!("Not settled: {compared} ({range}), {arms}{more}."),
    }
}

// ---- The energy audit --------------------------------------------------------------------

/// A deficit interval in words, with a surplus named as one: "310 to 870", "a surplus of 50
/// to a deficit of 400", "a surplus of 100 to 300".
fn balance_range(lo: f64, hi: f64) -> String {
    let n = |v: f64| number(v, 0);
    if lo >= 0.0 {
        format!("{} to {}", n(lo), n(hi))
    } else if hi <= 0.0 {
        format!("a surplus of {} to {}", n(hi), n(lo))
    } else {
        format!("a surplus of {} to a deficit of {}", n(lo), n(hi))
    }
}

/// OLS slope of `ys` on `ts`, or None for fewer than two distinct `ts`.
fn ols_slope(pts: &[(f64, f64)]) -> Option<f64> {
    let n = pts.len() as f64;
    if pts.len() < 2 {
        return None;
    }
    let mt = pts.iter().map(|p| p.0).sum::<f64>() / n;
    let my = pts.iter().map(|p| p.1).sum::<f64>() / n;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (t, y) in pts {
        sxy += (t - mt) * (y - my);
        sxx += (t - mt) * (t - mt);
    }
    (sxx > 0.0).then(|| sxy / sxx)
}

/// The energy audit over the [`AUDIT_DAYS`] ending on `to`: net logged intake against the
/// scale trend. Withheld (a JSON object carrying only `withheld`) when fewer than
/// [`AUDIT_MIN_DAYS`] of those days have calories known or the window has too few
/// weigh-ins for a slope.
pub fn energy_audit(s: &PatternSeries, to: &str) -> Value {
    let Some(end) = parse_day(to) else {
        return json!({ "withheld": "no dated logs" });
    };
    let start = end - chrono::Duration::days(AUDIT_DAYS - 1);
    // Each day of the window: (index, net intake if calories known, weight if weighed).
    let days: Vec<(f64, Option<f64>, Option<f64>)> = (0..AUDIT_DAYS)
        .map(|i| {
            let d = day_str(start + chrono::Duration::days(i));
            let net = s
                .calories
                .get(&d)
                .map(|c| c - s.exercise_kcal.get(&d).copied().unwrap_or(0.0));
            (i as f64, net, s.weight.get(&d).copied())
        })
        .collect();
    let known = days.iter().filter(|d| d.1.is_some()).count();
    let weighed = days.iter().filter(|d| d.2.is_some()).count();
    let window =
        json!({ "from": day_str(start), "to": to, "daysWithCalories": known, "weighIns": weighed });
    if known < AUDIT_MIN_DAYS {
        return json!({
            "window": window,
            "withheld": format!(
                "{known} of the last {AUDIT_DAYS} days have calories logged; the audit needs {AUDIT_MIN_DAYS}"
            ),
        });
    }
    if weighed < AUDIT_MIN_WEIGHINS {
        return json!({
            "window": window,
            "withheld": format!(
                "{weighed} usable weigh-ins in the last {AUDIT_DAYS} days; the audit needs {AUDIT_MIN_WEIGHINS}"
            ),
        });
    }
    let stats = |idx: &[usize]| -> Option<(f64, f64)> {
        let net = mean(idx.iter().filter_map(|&i| days[i].1))?;
        let pts: Vec<(f64, f64)> = idx
            .iter()
            .filter_map(|&i| days[i].2.map(|w| (days[i].0, w)))
            .collect();
        let slope = ols_slope(&pts)?;
        Some((net, slope))
    };
    let all: Vec<usize> = (0..days.len()).collect();
    let Some((net, slope)) = stats(&all) else {
        return json!({ "window": window, "withheld": "the weigh-ins give no trend" });
    };
    let deficit = -slope * KCAL_PER_LB;
    let maintenance = net + deficit;
    let mut boot_def = Vec::new();
    let mut boot_maint = Vec::new();
    let mut rng = Rng::new(seed_for("energy-audit"));
    let n = days.len();
    for _ in 0..BOOT_REPS {
        let mut idx = Vec::with_capacity(n + BLOCK_LEN);
        while idx.len() < n {
            let startb = rng.below(n);
            for k in 0..BLOCK_LEN {
                if idx.len() == n {
                    break;
                }
                // Blocks do NOT wrap here: a block that ran from the last days back into the
                // first would join two ends of a trend and bend the slope.
                idx.push((startb + k).min(n - 1));
            }
        }
        if let Some((bn, bs)) = stats(&idx) {
            boot_def.push(-bs * KCAL_PER_LB);
            boot_maint.push(bn - bs * KCAL_PER_LB);
        }
    }
    boot_def.sort_by(|a, b| a.total_cmp(b));
    boot_maint.sort_by(|a, b| a.total_cmp(b));
    let ci = |v: &[f64]| (quantile(v, 0.025), quantile(v, 0.975));
    let (dlo, dhi) = ci(&boot_def);
    let (mlo, mhi) = ci(&boot_maint);
    let r = |v: f64| (v / 10.0).round() * 10.0;
    let lbs_week = slope * 7.0;
    let trend = if lbs_week.abs() < 0.05 {
        "held flat".to_string()
    } else {
        format!(
            "{} {:.1} lb a week",
            if lbs_week < 0.0 { "fell" } else { "rose" },
            lbs_week.abs()
        )
    };
    let balance = if deficit >= 0.0 { "deficit" } else { "surplus" };
    let k = |v: f64| number(r(v), 0);
    let sentence = format!(
        "Last {AUDIT_DAYS} days, {known} with food logged: intake minus logged exercise averaged \
         {} kcal a day. The weight trend {trend}, which at 3,500 kcal per lb is a daily {balance} \
         of about {} kcal ({}). Together they put maintenance near {} kcal ({} to {}).",
        k(net),
        k(deficit.abs()),
        balance_range(r(dlo), r(dhi)),
        k(maintenance),
        k(mlo),
        k(mhi),
    );
    json!({
        "window": window,
        "netIntakeKcal": r(net),
        "trendLbsPerWeek": (lbs_week * 100.0).round() / 100.0,
        "scaleDeficitKcal": r(deficit),
        "scaleDeficitLow": r(dlo),
        "scaleDeficitHigh": r(dhi),
        "maintenanceKcal": r(maintenance),
        "maintenanceLow": r(mlo),
        "maintenanceHigh": r(mhi),
        "sentence": sentence,
        "note": AUDIT_NOTE,
    })
}

// ---- The report --------------------------------------------------------------------------

/// The whole Patterns report from the four logs' contents, for `patterns` on the diet
/// snapshot.
pub fn patterns_report(
    food: &str,
    exercise: &str,
    weight: &str,
    vitals: &str,
    open_day: Option<&str>,
) -> Value {
    let mut s = PatternSeries::from_logs(food, exercise, weight, vitals);
    if let Some(day) = open_day {
        s.close_open_day(day);
    }
    report_from_series(&s)
}

/// The report from already-built series (the seam the tests plant effects through).
pub fn report_from_series(s: &PatternSeries) -> Value {
    let Some(to) = s.latest_date() else {
        return json!({
            "questions": [],
            "counts": { "findings": 0, "ruledOut": 0, "watching": 0 },
            "energyAudit": { "withheld": "no dated logs" },
            "caveat": PATTERNS_CAVEAT,
        });
    };
    let from = shift(&to, WINDOW_DAYS - 1).unwrap_or_else(|| to.clone());
    let evals: Vec<PatternEvaluation> = PATTERN_CATALOGUE
        .iter()
        .map(|q| evaluate_question(q, s, &from, &to))
        .collect();
    // Every catalogue question is in the family: one that could not be evaluated counts as
    // p = 1, so an unmeasurable question can never make the others easier to pass.
    let ps: Vec<f64> = evals.iter().map(|e| e.p.unwrap_or(1.0)).collect();
    let pass = benjamini_hochberg(&ps, FDR_Q);
    let mut counts = (0, 0, 0);
    let questions: Vec<Value> = PATTERN_CATALOGUE
        .iter()
        .zip(&evals)
        .zip(&pass)
        .map(|((q, e), &fdr)| {
            let v = question_verdict(q, e, fdr);
            match v {
                PatternVerdict::Finding => counts.0 += 1,
                PatternVerdict::RuledOut => counts.1 += 1,
                PatternVerdict::Watching => counts.2 += 1,
            }
            let reason = (v == PatternVerdict::Watching)
                .then(|| watching_reason(e, fdr))
                .flatten();
            let more = (v == PatternVerdict::Watching && reason.is_none())
                .then(|| question_days_needed(q, e))
                .flatten();
            let as_expected = e.effect.map(|eff| match q.expect {
                PatternExpect::Higher => eff > 0.0,
                PatternExpect::Lower => eff < 0.0,
                PatternExpect::Either => true,
            });
            let round = |x: f64| {
                let f = 10f64.powi(q.decimals as i32 + 1);
                (x * f).round() / f
            };
            json!({
                "id": q.id,
                "title": q.title,
                "verdict": v.key(),
                "sentence": question_sentence(q, e, v, more, reason),
                "watchingReason": reason,
                "short": e.effect.map(|eff| format!(
                    "{} {} {} {}",
                    q.outcome_label,
                    direction(eff, q.decimals, q.unit),
                    q.relation,
                    q.high
                )),
                "unit": q.unit,
                "decimals": q.decimals,
                "meaningful": q.meaningful,
                "lagDays": q.lag,
                "expected": match q.expect {
                    PatternExpect::Higher => "higher",
                    PatternExpect::Lower => "lower",
                    PatternExpect::Either => "either",
                },
                "asExpected": as_expected,
                "high": { "label": q.high, "days": e.n_high, "mean": e.mean_high.map(round) },
                "low": { "label": q.low, "days": e.n_low, "mean": e.mean_low.map(round) },
                "effect": e.effect.map(round),
                "ciLow": e.ci.map(|c| round(c.0)),
                "ciHigh": e.ci.map(|c| round(c.1)),
                "p": e.p.map(|p| (p * 10_000.0).round() / 10_000.0),
                "fdrPass": fdr,
                "halvesAgree": e.halves.zip(e.effect).map(|((a, b), eff)| {
                    a.signum() == eff.signum() && b.signum() == eff.signum()
                }),
                "rho": e.rho.map(|r| (r * 100.0).round() / 100.0),
                "threshold": e.threshold,
                "daysNeeded": more,
                "from": e.first,
                "to": e.last,
            })
        })
        .collect();
    json!({
        "window": { "from": from, "to": to },
        "questions": questions,
        "counts": { "findings": counts.0, "ruledOut": counts.1, "watching": counts.2 },
        "energyAudit": match s.calories.keys().next_back() {
            Some(last) => energy_audit(s, last),
            None => json!({ "withheld": "no calories logged" }),
        },
        "caveat": PATTERNS_CAVEAT,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` consecutive dates from 2026-03-01.
    fn dates(n: usize) -> Vec<String> {
        let start = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
        (0..n)
            .map(|i| day_str(start + chrono::Duration::days(i as i64)))
            .collect()
    }

    /// A deterministic noise source for synthetic series, independent of the bootstrap's.
    fn noise(seed: u64, n: usize, scale: f64) -> Vec<f64> {
        let mut r = Rng::new(seed);
        (0..n)
            .map(|_| ((r.next() % 10_000) as f64 / 10_000.0 - 0.5) * 2.0 * scale)
            .collect()
    }

    fn question(id: &str) -> &'static PatternQuestion {
        PATTERN_CATALOGUE.iter().find(|q| q.id == id).unwrap()
    }

    /// Alcohol on every third day; next-morning HRV built by `hrv(alcohol_yesterday, i)`.
    fn alcohol_hrv_series(n: usize, hrv: impl Fn(bool, usize) -> f64) -> PatternSeries {
        let mut s = PatternSeries::default();
        let ds = dates(n);
        for (i, d) in ds.iter().enumerate() {
            let drank = i % 3 == 0;
            s.alcohol_g
                .insert(d.clone(), if drank { 28.0 } else { 0.0 });
            if i > 0 {
                let yesterday = (i - 1) % 3 == 0;
                s.hrv.insert(d.clone(), hrv(yesterday, i));
            }
        }
        s
    }

    fn run(q: &PatternQuestion, s: &PatternSeries) -> (PatternEvaluation, PatternVerdict) {
        let to = s.latest_date().unwrap();
        let from = shift(&to, WINDOW_DAYS - 1).unwrap();
        let e = evaluate_question(q, s, &from, &to);
        let fdr = e.p.is_some_and(|p| p < 0.01);
        let v = question_verdict(q, &e, fdr);
        (e, v)
    }

    #[test]
    fn a_planted_effect_is_found_in_units() {
        let n = 120;
        let jitter = noise(1, n, 4.0);
        let s = alcohol_hrv_series(n, |drank, i| {
            70.0 + jitter[i] - if drank { 12.0 } else { 0.0 }
        });
        let q = question("alcohol-hrv");
        let (e, v) = run(q, &s);
        assert_eq!(v, PatternVerdict::Finding, "{e:?}");
        let eff = e.effect.unwrap();
        assert!(
            (eff + 12.0).abs() < 2.0,
            "the effect is about -12 ms: {eff}"
        );
        let (lo, hi) = e.ci.unwrap();
        assert!(hi < 0.0 && lo < eff && eff < hi);
        let text = question_sentence(q, &e, v, None, None);
        assert!(text.contains("ms lower"), "{text}");
        assert!(
            text.contains(&format!("{} vs {} mornings", e.n_high, e.n_low)),
            "{text}"
        );
    }

    #[test]
    fn a_planted_null_with_enough_days_is_ruled_out() {
        let n = 150;
        let jitter = noise(2, n, 3.0);
        let s = alcohol_hrv_series(n, |_, i| 70.0 + jitter[i]);
        let (e, v) = run(question("alcohol-hrv"), &s);
        assert_eq!(v, PatternVerdict::RuledOut, "{e:?}");
        let (lo, hi) = e.ci.unwrap();
        assert!(lo > -5.0 && hi < 5.0);
    }

    #[test]
    fn a_short_series_is_watching_with_its_arms_and_no_interval() {
        let jitter = noise(3, 15, 3.0);
        let s = alcohol_hrv_series(15, |drank, i| {
            70.0 + jitter[i] - if drank { 12.0 } else { 0.0 }
        });
        let q = question("alcohol-hrv");
        let (e, v) = run(q, &s);
        assert_eq!(v, PatternVerdict::Watching);
        assert!(e.n_high < MIN_PER_ARM);
        assert!(
            e.effect.is_none() && e.ci.is_none(),
            "no number below the minimum"
        );
        let more = question_days_needed(q, &e);
        assert!(more.is_some_and(|m| m > 0), "{more:?}");
        assert!(question_sentence(q, &e, v, more, None).starts_with("Not enough days yet"));
    }

    #[test]
    fn an_effect_that_flips_between_halves_is_never_a_finding() {
        let n = 160;
        let jitter = noise(4, n, 1.0);
        // -20 ms in the first half, +4 ms in the second: clearly negative overall (the interval
        // excludes zero), but the later half points the other way.
        let s = alcohol_hrv_series(n, |drank, i| {
            let e = if i < n / 2 { -20.0 } else { 4.0 };
            70.0 + jitter[i] + if drank { e } else { 0.0 }
        });
        let q = question("alcohol-hrv");
        let to = s.latest_date().unwrap();
        let e = evaluate_question(q, &s, "2000-01-01", &to);
        let (a, b) = e.halves.unwrap();
        assert!(a < 0.0 && b > 0.0, "{a} {b}");
        let (_, hi) = e.ci.unwrap();
        assert!(hi < 0.0, "the interval alone would call it: {:?}", e.ci);
        assert_ne!(
            question_verdict(q, &e, true),
            PatternVerdict::Finding,
            "even when FDR passes"
        );
    }

    #[test]
    fn missing_days_are_excluded_never_filled() {
        let mut s = alcohol_hrv_series(60, |_, _| 70.0);
        // Drop every alcohol value on even days: their next mornings lose their pair.
        let ds = dates(60);
        for d in ds.iter().step_by(2) {
            s.alcohol_g.remove(d);
        }
        let pairs = pairs_for(question("alcohol-hrv"), &s, "2000-01-01", "2100-01-01");
        assert!(pairs.len() < 35, "{}", pairs.len());
        for (d, _, _) in &pairs {
            assert!(s.alcohol_g.contains_key(&shift(d, 1).unwrap()));
        }
    }

    #[test]
    fn the_report_is_identical_on_every_run() {
        let jitter = noise(5, 90, 4.0);
        let s = alcohol_hrv_series(90, |drank, i| {
            70.0 + jitter[i] - if drank { 6.0 } else { 0.0 }
        });
        assert_eq!(report_from_series(&s), report_from_series(&s));
    }

    #[test]
    fn benjamini_hochberg_passes_the_step_up_set() {
        // m = 5, q = 0.10: thresholds 0.02, 0.04, 0.06, 0.08, 0.10.
        let pass = benjamini_hochberg(&[0.01, 0.5, 0.05, 0.03, 0.9], 0.10);
        assert_eq!(pass, vec![true, false, true, true, false]);
        assert_eq!(benjamini_hochberg(&[0.2, 0.3], 0.10), vec![false, false]);
    }

    #[test]
    fn tied_values_take_their_average_rank() {
        assert_eq!(ranks(&[10.0, 20.0, 20.0, 30.0]), vec![1.0, 2.5, 2.5, 4.0]);
        // A monotone relation with ties is still a perfect rank association.
        assert_eq!(
            spearman(&[1.0, 2.0, 2.0, 3.0], &[5.0, 6.0, 6.0, 9.0]),
            Some(1.0)
        );
    }

    #[test]
    fn the_caveat_says_associations_not_causes() {
        assert!(PATTERNS_CAVEAT.contains("associations, not causes"));
        assert!(PATTERNS_CAVEAT.contains("ruled out means"));
    }

    #[test]
    fn spearman_matches_known_values() {
        assert_eq!(
            spearman(&[1.0, 2.0, 3.0, 4.0], &[10.0, 20.0, 30.0, 40.0]),
            Some(1.0)
        );
        assert_eq!(
            spearman(&[1.0, 2.0, 3.0, 4.0], &[4.0, 3.0, 2.0, 1.0]),
            Some(-1.0)
        );
        assert_eq!(spearman(&[1.0, 1.0, 1.0], &[1.0, 2.0, 3.0]), None);
    }

    #[test]
    fn logs_become_series_with_unknowns_left_out() {
        let food =
            "Date,Meal,Item,Calories,Carbs_g,Sodium_mg,SatFat_g,Time,Alcohol_g,Caffeine_mg\n\
            2026-03-01,Lunch,Soup,300,20,,2,12:30,0,\n\
            2026-03-01,Dinner,Wine,120,4,,0,21:30,14,\n\
            2026-03-02,Breakfast,Coffee,5,0,5,0,08:00,0,95\n\
            2026-03-02,Snack,Espresso,2,0,1,0,15:10,0,63\n\
            2026-03-02,Late,Toast,90,15,150,0.5,00:40,0,0\n";
        let exercise = "Date,Type,Calories\n2026-03-01,Run,500\n2026-03-03,Swim,300\n";
        let weight =
            "Date,Weight_lbs,Hydration_Artifact\n2026-03-01,190,false\n2026-03-02,188,true\n";
        let s = PatternSeries::from_logs(food, exercise, weight, "");
        assert_eq!(s.calories["2026-03-01"], 420.0);
        assert!(
            !s.sodium.contains_key("2026-03-01"),
            "no item knew sodium: a gap, not 0"
        );
        assert_eq!(s.sodium["2026-03-02"], 156.0);
        assert_eq!(s.alcohol_g["2026-03-01"], 14.0);
        assert!(
            !s.caffeine_late.contains_key("2026-03-01"),
            "caffeine untracked that day"
        );
        assert_eq!(s.caffeine_late["2026-03-02"], 63.0);
        assert_eq!(s.last_food_min["2026-03-01"], (21 * 60 + 30) as f64);
        assert_eq!(
            s.last_food_min["2026-03-02"],
            (24 * 60 + 40) as f64,
            "00:40 is after dinner"
        );
        assert_eq!(
            s.exercise_kcal["2026-03-02"], 0.0,
            "a logged day inside the span is rest"
        );
        assert_eq!(s.training_day["2026-03-02"], 0.0);
        assert_eq!(s.training_day["2026-03-01"], 1.0);
        assert!(s.weight.contains_key("2026-03-01"));
        assert!(
            !s.weight.contains_key("2026-03-02"),
            "a hydration artifact is excluded"
        );
    }

    #[test]
    fn the_weight_residual_is_against_the_trailing_week_without_artifacts() {
        let mut weight = String::from("Date,Weight_lbs,Hydration_Artifact\n");
        for (i, d) in dates(8).iter().enumerate() {
            // Flat 190 for a week, except an artifact 180 on day 3; the 8th morning is 191.
            let (w, a) = match i {
                3 => (180.0, true),
                7 => (191.0, false),
                _ => (190.0, false),
            };
            weight.push_str(&format!("{d},{w},{a}\n"));
        }
        let s = PatternSeries::from_logs("", "", &weight, "");
        let last = dates(8)[7].clone();
        assert!(
            (s.weight_residual[&last] - 1.0).abs() < 1e-9,
            "{:?}",
            s.weight_residual
        );
    }

    #[test]
    fn the_energy_audit_recovers_a_planted_deficit_and_withholds_when_thin() {
        let mut s = PatternSeries::default();
        let ds = dates(28);
        let jitter = noise(6, 28, 0.3);
        for (i, d) in ds.iter().enumerate() {
            s.calories.insert(d.clone(), 2000.0);
            s.exercise_kcal.insert(d.clone(), 300.0);
            // 1 lb a week down: a 500 kcal a day deficit on a 1,700 net intake.
            s.weight
                .insert(d.clone(), 190.0 - i as f64 / 7.0 + jitter[i]);
        }
        let a = energy_audit(&s, ds.last().unwrap());
        assert!(a.get("withheld").is_none(), "{a}");
        assert_eq!(a["netIntakeKcal"], 1700.0);
        let deficit = a["scaleDeficitKcal"].as_f64().unwrap();
        assert!((deficit - 500.0).abs() < 60.0, "{deficit}");
        let m = a["maintenanceKcal"].as_f64().unwrap();
        assert!(
            a["maintenanceLow"].as_f64().unwrap() <= m
                && m <= a["maintenanceHigh"].as_f64().unwrap()
        );

        for d in ds.iter().take(8) {
            s.calories.remove(d);
        }
        let thin = energy_audit(&s, ds.last().unwrap());
        assert!(
            thin["withheld"]
                .as_str()
                .unwrap()
                .contains("20 of the last 28"),
            "{thin}"
        );
    }

    #[test]
    fn the_open_diet_day_drops_its_partial_intake_but_keeps_its_morning() {
        let food = "Date,Calories,Time\n2026-03-01,2100,19:00\n2026-03-02,450,08:00\n";
        let weight = "Date,Weight_lbs,Hydration_Artifact\n2026-03-02,190,false\n";
        let mut s = PatternSeries::from_logs(food, "", weight, "");
        s.close_open_day("2026-03-02");
        assert!(s.calories.contains_key("2026-03-01"));
        assert!(
            !s.calories.contains_key("2026-03-02"),
            "a half-eaten day is not a day"
        );
        assert!(!s.last_food_min.contains_key("2026-03-02"));
        assert!(
            s.weight.contains_key("2026-03-02"),
            "this morning's weigh-in is complete"
        );
    }

    #[test]
    fn every_verdict_states_the_comparison_in_units() {
        let q = question("alcohol-hrv");
        let e = PatternEvaluation {
            n_high: 20,
            n_low: 50,
            mean_high: Some(63.0),
            mean_low: Some(70.0),
            effect: Some(-7.0),
            ci: Some((-11.2, -3.4)),
            p: Some(0.001),
            rho: Some(-0.3),
            halves: Some((-6.0, -8.0)),
            threshold: Some(0.0),
            first: None,
            last: None,
        };
        assert_eq!(
            question_sentence(q, &e, PatternVerdict::Finding, None, None),
            "After days with alcohol, next-day HRV was 7 ms lower than after days without \
             (3 to 11 lower), 20 vs 50 mornings."
        );
        let null = PatternEvaluation {
            effect: Some(0.4),
            ci: Some((-2.1, 2.9)),
            ..e.clone()
        };
        assert_eq!(
            question_sentence(q, &null, PatternVerdict::RuledOut, None, None),
            "No meaningful difference: after days with alcohol, next-day HRV was about the same \
             as after days without (2 lower to 3 higher, inside the 5 ms that would matter), \
             20 vs 50 mornings."
        );
    }

    #[test]
    fn no_sentence_reads_as_a_cause() {
        let jitter = noise(7, 120, 4.0);
        let s = alcohol_hrv_series(120, |drank, i| {
            70.0 + jitter[i] - if drank { 12.0 } else { 0.0 }
        });
        let report = report_from_series(&s);
        for q in report["questions"].as_array().unwrap() {
            let text = q["sentence"].as_str().unwrap().to_lowercase();
            for banned in [
                "cause", "because", "raises", "lowers", "makes", "leads to", "due to",
            ] {
                assert!(!text.contains(banned), "{banned:?} in {text:?}");
            }
        }
    }
}
