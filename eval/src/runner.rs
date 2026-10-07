//! The `run` subcommand: prepare each task's workspace, hand it to a [`Driver`],
//! evaluate assertions, and write `results.json` + `scorecard.md`.
//!
//! **NOTHING HERE KNOWS HOW A TASK IS EXECUTED.** Workspace preparation, scoring,
//! aggregation and the scorecard are the same for every driver; the driver is a
//! `Box<dyn Driver>` this module never inspects beyond its id, wire and model. That
//! split is what lets `compare` put two runs side by side and mean it.

use crate::assertions::{eval_all, AssertionResult};
use crate::driver::{Driver, Latency, PreparedWorkspace, TaskRun};
use crate::suite::{Suite, Task, Workspace};
use crate::transcript::{Transcript, Usage};
use crate::workspace::{prepare_fixture, resolve_task, BuiltRemote};
use jesse_agent::PriceDeck;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// Configuration for a run.
pub struct RunConfig {
    /// What actually runs each task.
    pub driver: Box<dyn Driver>,
    /// The price deck the per-task cost is computed with. `ZERO` by default, and a stated
    /// zero is honest where a plausible made-up rate is not.
    pub prices: PriceDeck,
    pub out_dir: PathBuf,
    /// Runs per task, k in pass^k. 1 runs every task once, as every run did before it.
    pub runs: u32,
    /// Where a task's `fixture_base` is resolved: `<suite dir>/../fixtures`.
    pub fixtures_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRecord {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
}

impl TokenRecord {
    /// Total tokens, for a comparison that wants one number.
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }
}

impl From<&Usage> for TokenRecord {
    fn from(u: &Usage) -> Self {
        TokenRecord {
            input: u.input_tokens,
            output: u.output_tokens,
            cache_read: u.cache_read_input_tokens,
            cache_creation: u.cache_creation_input_tokens,
        }
    }
}

/// The dollar cost of a token record under a deck.
fn cost_of(t: &TokenRecord, prices: &PriceDeck) -> f64 {
    (t.input as f64 * prices.in_per_m
        + t.cache_creation as f64 * prices.cache_write_per_m.unwrap_or(prices.in_per_m)
        + t.cache_read as f64 * prices.cached_per_m
        + t.output as f64 * prices.out_per_m)
        / 1_000_000.0
}

/// One task's full result record (serialized into `results.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub id: String,
    pub class: String,
    pub workspace: String,
    pub judged: bool,
    pub rubric: Option<String>,
    pub passed: bool,
    pub completed: bool,
    pub wall_ms: u64,
    /// Harness-measured time to first text delta.
    pub measured_ttft_ms: Option<u64>,
    /// Model-reported time to first token (from the result line).
    pub result_ttft_ms: Option<u64>,
    pub tool_calls: u32,
    /// The name of every tool call, in dispatch order.
    #[serde(default)]
    pub tool_names: Vec<String>,
    pub tokens: Option<TokenRecord>,
    /// The run's dollar cost under the run's price deck. `0.0` with the default deck.
    #[serde(default)]
    pub cost_usd: f64,
    pub final_answer: Option<String>,
    pub assertions: Vec<AssertionResult>,
    pub transcript_path: String,
    /// Harness-level error (spawn failure, timeout, mock miss). Not a model miss.
    pub error: Option<String>,
    /// Why the task did not run at all, when it did not: today, a `harnesses` list that does
    /// not name the driver's harness. A skipped task is neither a pass nor a fail and is
    /// left out of every pass rate and every comparison.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    /// Submit-relative latencies of the attempt this record describes (see `attempts`).
    #[serde(default)]
    pub latency: Latency,
    /// One entry per run of this task. A task with `runs: k` passes only when all k passed
    /// (pass^k); the fields above describe the FIRST FAILING attempt when there is one, and
    /// the last attempt otherwise, so a failure is never hidden behind a later pass. Empty
    /// in a results file written before pass^k existed, and for a skipped task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<AttemptRecord>,
}

/// One run of one task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub passed: bool,
    pub completed: bool,
    pub wall_ms: u64,
    #[serde(default)]
    pub latency: Latency,
    pub tool_calls: u32,
    pub transcript_path: String,
    /// The `kind` of every assertion that failed, for a glance at what broke.
    #[serde(default)]
    pub failed: Vec<String>,
    pub error: Option<String>,
}

/// Top-level `results.json` document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub suite: String,
    /// Which driver ran it. `claude-cli` for every run recorded before D7 — absent from
    /// those files, and defaulted here so an old results dir still loads.
    #[serde(default = "legacy_driver")]
    pub driver: String,
    /// The wire, when the driver has one of its own.
    #[serde(default)]
    pub wire: Option<String>,
    /// The search index that answered, when the driver owns one. Absent from every run
    /// recorded before D12, which is why it defaults rather than being required.
    #[serde(default)]
    pub index: Option<String>,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    /// The harness the driver ran turns on, when it has one (`bridge`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Runs per task (k in pass^k). 1 for every results file written before it existed.
    #[serde(default = "one")]
    pub runs: u32,
    pub mock: bool,
    pub tasks: Vec<TaskResult>,
}

fn one() -> u32 {
    1
}

fn legacy_driver() -> String {
    "claude-cli".to_string()
}

/// Populate a fresh workspace for a task; return the dir to run in and the remotes built
/// for it. For vault tasks, returns the real vault path and writes nothing.
fn prepare_workspace(
    task: &Task,
    dir: &Path,
    fixtures_root: Option<&Path>,
) -> Result<(PreparedWorkspace, BTreeMap<String, BuiltRemote>), String> {
    let (dir, remotes) = match task.workspace {
        Workspace::VaultReadonly => (crate::suite::vault_dir(), BTreeMap::new()),
        Workspace::Fixture => {
            // The remotes live BESIDE the workspace, never in it: the turn reaches one only
            // by cloning it, which is the property a checkout task is testing.
            let remotes_root = dir.with_extension("remotes");
            let remotes = prepare_fixture(task, dir, fixtures_root, &remotes_root)?;
            (dir.to_path_buf(), remotes)
        }
    };
    Ok((
        PreparedWorkspace {
            kind: task.workspace,
            dir,
        },
        remotes,
    ))
}

/// Why a driver with harness `h` skips `task`, or `None` to run it.
pub fn skip_reason(task: &Task, harness: Option<&str>) -> Option<String> {
    let h = harness?;
    if task.harnesses.is_empty() || task.harnesses.iter().any(|t| t == h) {
        return None;
    }
    Some(format!(
        "harness {h} is not in this task's harnesses [{}]",
        task.harnesses.join(", ")
    ))
}

/// Run a whole suite. Returns the report (also written to `out_dir`).
///
/// SYNCHRONOUS on the outside and `block_on` inside: the harness runs one task at a time on
/// purpose (a latency number measured while three other tasks share the machine is not a
/// latency number), so a current-thread runtime driving one turn is the whole concurrency
/// story. The runtime exists because the agent loop is async, not because anything here is.
pub fn run_suite(suite: &Suite, cfg: &RunConfig) -> Result<RunReport, String> {
    std::fs::create_dir_all(&cfg.out_dir).map_err(|e| format!("could not create out dir: {e}"))?;
    let transcripts_dir = cfg.out_dir.join("transcripts");
    std::fs::create_dir_all(&transcripts_dir)
        .map_err(|e| format!("could not create transcripts dir: {e}"))?;
    let answers_dir = cfg.out_dir.join("answers");
    std::fs::create_dir_all(&answers_dir)
        .map_err(|e| format!("could not create answers dir: {e}"))?;

    // Fixture workspaces live under one temp root for the whole run.
    let temp_root = tempfile::Builder::new()
        .prefix("jesse-eval-")
        .tempdir()
        .map_err(|e| format!("could not create temp root: {e}"))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("could not start the runtime: {e}"))?;

    let runs = cfg.runs.max(1);
    let harness = cfg.driver.harness();
    let mut results = Vec::new();
    for task in &suite.tasks {
        // Load-bearing: refuse a vault task with a non-read tool before running.
        task.validate()?;

        if let Some(why) = skip_reason(task, harness.as_deref()) {
            results.push(skipped_result(task, why));
            continue;
        }

        let mut attempts: Vec<(AttemptRecord, TaskResult)> = Vec::new();
        for i in 0..runs {
            let dir = if runs == 1 {
                temp_root.path().join(&task.id)
            } else {
                temp_root.path().join(format!("{}.run{}", task.id, i + 1))
            };
            let suffix = if runs == 1 {
                String::new()
            } else {
                format!(".run{}", i + 1)
            };
            let record = match prepare_workspace(task, &dir, cfg.fixtures_root.as_deref())
                .and_then(|(ws, remotes)| Ok((ws, resolve_task(task, &remotes)?)))
            {
                Err(e) => harness_failure(task, &suffix, e),
                Ok((workspace, resolved)) => {
                    let run = runtime.block_on(cfg.driver.run_task(
                        &resolved,
                        &workspace,
                        CancellationToken::new(),
                    ));
                    score_attempt(&resolved, &workspace, run, cfg, &suffix, &answers_dir)
                }
            };
            attempts.push(record);
        }
        results.push(fold_attempts(attempts));
    }

    let report = RunReport {
        suite: suite.name.clone(),
        driver: cfg.driver.id().to_string(),
        wire: cfg.driver.wire(),
        index: cfg.driver.index(),
        endpoint: cfg.driver.endpoint(),
        model: cfg.driver.model(),
        harness,
        runs,
        mock: cfg.driver.is_mock(),
        tasks: results,
    };

    std::fs::write(
        cfg.out_dir.join("results.json"),
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("could not write results.json: {e}"))?;

    std::fs::write(cfg.out_dir.join("scorecard.md"), scorecard(&report))
        .map_err(|e| format!("could not write scorecard.md: {e}"))?;

    Ok(report)
}

/// Score one finished run and persist its transcript (and answer, for a judged task).
fn score_attempt(
    task: &Task,
    workspace: &PreparedWorkspace,
    run: TaskRun,
    cfg: &RunConfig,
    suffix: &str,
    answers_dir: &Path,
) -> (AttemptRecord, TaskResult) {
    let transcript_rel = format!("transcripts/{}{suffix}.ndjson", task.id);
    let _ = std::fs::write(cfg.out_dir.join(&transcript_rel), run.lines.join("\n"));
    let parsed: &Transcript = &run.transcript;
    if task.judged && !run.answer.is_empty() {
        let _ = std::fs::write(
            answers_dir.join(format!("{}{suffix}.txt", task.id)),
            &run.answer,
        );
    }
    let (passed, assertion_results) = eval_all(
        &task.assertions,
        parsed,
        &workspace.dir,
        task.persona.as_ref(),
    );
    // A harness error (couldn't even run) is not a legitimate pass.
    let passed = passed && run.error.is_none();
    let tokens = TokenRecord::from(&run.usage);
    let cost_usd = cost_of(&tokens, &cfg.prices);
    let attempt = AttemptRecord {
        passed,
        completed: run.completed,
        wall_ms: run.wall_ms,
        latency: run.latency,
        tool_calls: run.tool_calls as u32,
        transcript_path: transcript_rel.clone(),
        failed: assertion_results
            .iter()
            .filter(|a| !a.passed)
            .map(|a| a.kind.clone())
            .collect(),
        error: run.error.clone(),
    };
    let result = TaskResult {
        id: task.id.clone(),
        class: task.class.clone(),
        workspace: format!("{:?}", task.workspace).to_lowercase(),
        judged: task.judged,
        rubric: task.rubric.clone(),
        passed,
        completed: run.completed,
        wall_ms: run.wall_ms,
        measured_ttft_ms: run.ttft_ms,
        result_ttft_ms: parsed.result_ttft_ms,
        tool_calls: run.tool_calls as u32,
        tool_names: run.tool_names.clone(),
        tokens: Some(tokens),
        cost_usd,
        final_answer: parsed.final_answer.clone(),
        assertions: assertion_results,
        transcript_path: transcript_rel,
        error: run.error.clone(),
        skipped: None,
        latency: run.latency,
        attempts: Vec::new(),
    };
    (attempt, result)
}

/// A workspace that could not be prepared: a harness failure, recorded like one.
fn harness_failure(task: &Task, suffix: &str, why: String) -> (AttemptRecord, TaskResult) {
    let mut r = skipped_result(task, String::new());
    r.skipped = None;
    r.error = Some(why.clone());
    r.transcript_path = format!("transcripts/{}{suffix}.ndjson", task.id);
    (
        AttemptRecord {
            passed: false,
            completed: false,
            wall_ms: 0,
            latency: Latency::default(),
            tool_calls: 0,
            transcript_path: r.transcript_path.clone(),
            failed: Vec::new(),
            error: Some(why),
        },
        r,
    )
}

/// The record of a task that did not run.
fn skipped_result(task: &Task, why: String) -> TaskResult {
    TaskResult {
        id: task.id.clone(),
        class: task.class.clone(),
        workspace: format!("{:?}", task.workspace).to_lowercase(),
        judged: task.judged,
        rubric: task.rubric.clone(),
        passed: false,
        completed: false,
        wall_ms: 0,
        measured_ttft_ms: None,
        result_ttft_ms: None,
        tool_calls: 0,
        tool_names: Vec::new(),
        tokens: None,
        cost_usd: 0.0,
        final_answer: None,
        assertions: Vec::new(),
        transcript_path: String::new(),
        error: None,
        skipped: Some(why),
        latency: Latency::default(),
        attempts: Vec::new(),
    }
}

/// pass^k: every attempt must pass. The task's own fields describe the first failing
/// attempt, or the last one when all passed.
fn fold_attempts(attempts: Vec<(AttemptRecord, TaskResult)>) -> TaskResult {
    let all = attempts.iter().all(|(a, _)| a.passed);
    let pick = attempts
        .iter()
        .position(|(a, _)| !a.passed)
        .unwrap_or(attempts.len().saturating_sub(1));
    let records: Vec<AttemptRecord> = attempts.iter().map(|(a, _)| a.clone()).collect();
    let mut r = attempts
        .into_iter()
        .nth(pick)
        .map(|(_, r)| r)
        .expect("at least one attempt");
    r.passed = all;
    if records.len() > 1 {
        r.attempts = records;
    }
    r
}

/// Nearest-rank percentile of `xs` (`p` in 0..=100). `None` for an empty set.
pub fn percentile(xs: &[u64], p: u32) -> Option<u64> {
    if xs.is_empty() {
        return None;
    }
    let mut v = xs.to_vec();
    v.sort_unstable();
    let rank = ((p as f64 / 100.0) * v.len() as f64).ceil() as usize;
    Some(v[rank.clamp(1, v.len()) - 1])
}

/// Every attempt's latency in a report: the attempts list when present, else the task's own.
fn all_latencies(report: &RunReport) -> Vec<Latency> {
    report
        .tasks
        .iter()
        .filter(|t| t.skipped.is_none())
        .flat_map(|t| {
            if t.attempts.is_empty() {
                vec![t.latency]
            } else {
                t.attempts.iter().map(|a| a.latency).collect()
            }
        })
        .collect()
}

/// Render the per-class + totals scorecard.
pub fn scorecard(report: &RunReport) -> String {
    struct Agg {
        n: u32,
        passed: u32,
        latency_sum: u64,
        tool_sum: u64,
    }
    let mut by_class: BTreeMap<String, Agg> = BTreeMap::new();
    let mut total = Agg {
        n: 0,
        passed: 0,
        latency_sum: 0,
        tool_sum: 0,
    };
    for t in report.tasks.iter().filter(|t| t.skipped.is_none()) {
        let a = by_class.entry(t.class.clone()).or_insert(Agg {
            n: 0,
            passed: 0,
            latency_sum: 0,
            tool_sum: 0,
        });
        for agg in [a, &mut total] {
            agg.n += 1;
            if t.passed {
                agg.passed += 1;
            }
            agg.latency_sum += t.wall_ms;
            agg.tool_sum += t.tool_calls as u64;
        }
    }

    let mut out = String::new();
    out.push_str(&format!("# Scorecard — {}\n\n", report.suite));
    let target = match (&report.endpoint, &report.model) {
        (Some(e), Some(m)) => format!("endpoint `{e}`, model `{m}`"),
        (Some(e), None) => format!("endpoint `{e}`, default model"),
        (None, _) if report.mock => "mock (canned responses)".to_string(),
        (None, _) => "ambient auth + default model".to_string(),
    };
    // THE HEADER NAMES THE RUNNER, not only the endpoint. Two runs of one suite that differ
    // only in which driver produced them are the comparison this harness now exists to
    // make, and a scorecard that did not say which is which would be unreadable a week later.
    out.push_str(&format!(
        "Driver: `{}` · wire: {} · model: {} · index: {}\n\n",
        report.driver,
        report.wire.as_deref().unwrap_or("n/a"),
        report.model.as_deref().unwrap_or("default"),
        report.index.as_deref().unwrap_or("n/a"),
    ));
    out.push_str(&format!("Target: {target}\n\n"));
    if report.harness.is_some() || report.runs > 1 {
        out.push_str(&format!(
            "Harness: {} · runs per task: {} (pass^{})\n\n",
            report.harness.as_deref().unwrap_or("n/a"),
            report.runs,
            report.runs
        ));
    }
    out.push_str("| Class | Pass rate | Mean latency | Mean tool calls |\n");
    out.push_str("|---|---|---|---|\n");
    for (class, a) in &by_class {
        out.push_str(&format!(
            "| {} | {}/{} ({:.0}%) | {} ms | {:.1} |\n",
            class,
            a.passed,
            a.n,
            100.0 * a.passed as f64 / a.n as f64,
            a.latency_sum / a.n as u64,
            a.tool_sum as f64 / a.n as f64,
        ));
    }
    if total.n > 0 {
        out.push_str(&format!(
            "| **TOTAL** | **{}/{} ({:.0}%)** | **{} ms** | **{:.1}** |\n",
            total.passed,
            total.n,
            100.0 * total.passed as f64 / total.n as f64,
            total.latency_sum / total.n as u64,
            total.tool_sum as f64 / total.n as f64,
        ));
    }
    out.push_str(&pass_k_section(report));
    out.push_str(&skips_section(report));
    out.push_str(&latency_section(report));
    out
}

/// pass^k per task: only when a task ran more than once, since with k = 1 it is the table
/// above.
fn pass_k_section(report: &RunReport) -> String {
    if report.runs <= 1 {
        return String::new();
    }
    let k = report.runs;
    let mut out =
        format!("\n## pass^{k} per task\n\nA task passes only when all {k} runs passed.\n\n");
    out.push_str(
        "| Task | Class | pass^k | Runs passed | Failed assertion kinds |\n|---|---|---|---|---|\n",
    );
    for t in report.tasks.iter().filter(|t| t.skipped.is_none()) {
        let n_pass = t.attempts.iter().filter(|a| a.passed).count();
        let n = t.attempts.len().max(1);
        let mut kinds: Vec<String> = t
            .attempts
            .iter()
            .flat_map(|a| {
                a.failed
                    .iter()
                    .cloned()
                    .chain(a.error.as_ref().map(|_| "harness-error".to_string()))
            })
            .collect();
        kinds.sort();
        kinds.dedup();
        out.push_str(&format!(
            "| {} | {} | {} | {}/{} | {} |\n",
            t.id,
            t.class,
            if t.passed { "PASS" } else { "FAIL" },
            n_pass,
            n,
            if kinds.is_empty() {
                "".to_string()
            } else {
                kinds.join(", ")
            },
        ));
    }
    out
}

/// The tasks this run skipped, under the harness that skipped them. A skip is never a fail.
fn skips_section(report: &RunReport) -> String {
    let skipped: Vec<&TaskResult> = report
        .tasks
        .iter()
        .filter(|t| t.skipped.is_some())
        .collect();
    if skipped.is_empty() {
        return String::new();
    }
    let h = report.harness.as_deref().unwrap_or("this driver");
    let mut out = format!(
        "\n## Skipped on {h}: {}\n\nNot counted as passes or fails.\n\n",
        skipped.len()
    );
    for t in skipped {
        out.push_str(&format!(
            "- `{}`: {}\n",
            t.id,
            t.skipped.as_deref().unwrap_or("")
        ));
    }
    out
}

/// p50 and p95 of the three submit-relative latencies, over every attempt that has them.
fn latency_section(report: &RunReport) -> String {
    let all = all_latencies(report);
    let pick = |f: fn(&Latency) -> Option<u64>| -> Vec<u64> { all.iter().filter_map(f).collect() };
    let rows = [
        ("submit to first streamed event", pick(|l| l.first_event_ms)),
        ("submit to first model token", pick(|l| l.first_token_ms)),
        ("submit to result", pick(|l| l.result_ms)),
    ];
    if rows.iter().all(|(_, v)| v.is_empty()) {
        return String::new();
    }
    let fmt = |x: Option<u64>| x.map(|v| format!("{v} ms")).unwrap_or_else(|| "n/a".into());
    let mut out = String::from("\n## Latency\n\n| Measure | p50 | p95 | n |\n|---|---|---|---|\n");
    for (name, v) in rows {
        out.push_str(&format!(
            "| {name} | {} | {} | {} |\n",
            fmt(percentile(&v, 50)),
            fmt(percentile(&v, 95)),
            v.len()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(cost: f64) -> TaskResult {
        TaskResult {
            id: "t".into(),
            class: "c".into(),
            workspace: "fixture".into(),
            judged: false,
            rubric: None,
            passed: true,
            completed: true,
            wall_ms: 10,
            measured_ttft_ms: None,
            result_ttft_ms: None,
            tool_calls: 0,
            tool_names: vec![],
            tokens: None,
            cost_usd: cost,
            final_answer: None,
            assertions: vec![],
            transcript_path: "x".into(),
            error: None,
            skipped: None,
            latency: Latency::default(),
            attempts: vec![],
        }
    }

    #[test]
    fn the_scorecard_header_names_the_driver_wire_and_model() {
        let report = RunReport {
            suite: "s".into(),
            driver: "direct".into(),
            wire: Some("chat".into()),
            index: Some("grep".into()),
            endpoint: Some("http://example".into()),
            model: Some("m".into()),
            harness: None,
            runs: 1,
            mock: false,
            tasks: vec![record(0.0)],
        };
        let card = scorecard(&report);
        assert!(card.contains("Driver: `direct`"), "{card}");
        assert!(card.contains("wire: chat"), "{card}");
        assert!(card.contains("model: m"), "{card}");
    }

    #[test]
    fn a_results_file_without_a_driver_field_loads_as_the_cli() {
        let json = serde_json::json!({
            "suite": "s", "endpoint": null, "model": null, "mock": true, "tasks": []
        });
        let report: RunReport = serde_json::from_value(json).expect("legacy results parse");
        assert_eq!(report.driver, "claude-cli");
        assert_eq!(report.wire, None);
    }

    #[test]
    fn cost_uses_the_deck_and_is_zero_by_default() {
        let t = TokenRecord {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_creation: 0,
        };
        assert_eq!(cost_of(&t, &PriceDeck::ZERO), 0.0);
        let deck = PriceDeck {
            in_per_m: 3.0,
            cached_per_m: 0.3,
            cache_write_per_m: None,
            out_per_m: 15.0,
        };
        assert!((cost_of(&t, &deck) - 18.3).abs() < 1e-9);
    }

    // ---- pass^k, skips, latency ------------------------------------------------------

    use crate::driver::BoxFuture;
    use crate::suite::Suite;
    use std::cell::Cell;

    /// A driver that knows its harness and answers from a script: run n passes when
    /// `pass_on(n)` says so (by writing the file the task asserts on), with fixed latencies.
    struct FakeDriver {
        harness: &'static str,
        calls: Cell<u32>,
        pass_on: fn(u32) -> bool,
    }

    impl Driver for FakeDriver {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn is_mock(&self) -> bool {
            true
        }
        fn harness(&self) -> Option<String> {
            Some(self.harness.to_string())
        }
        fn run_task<'a>(
            &'a self,
            task: &'a Task,
            workspace: &'a PreparedWorkspace,
            _cancel: CancellationToken,
        ) -> BoxFuture<'a, TaskRun> {
            Box::pin(async move {
                let n = self.calls.get();
                self.calls.set(n + 1);
                assert!(
                    workspace.dir.join("vault/Today.md").is_file(),
                    "the base was copied before the driver ran"
                );
                if (self.pass_on)(n) {
                    std::fs::write(workspace.dir.join("out.txt"), "done").unwrap();
                }
                let mut run = TaskRun::from_lines(
                    vec![format!(
                        r#"{{"type":"result","subtype":"success","result":"ok {}"}}"#,
                        task.id
                    )],
                    100 + u64::from(n),
                    None,
                );
                run.latency = Latency {
                    first_event_ms: Some(10 * u64::from(n + 1)),
                    first_token_ms: Some(20 * u64::from(n + 1)),
                    result_ms: Some(100 * u64::from(n + 1)),
                };
                run
            })
        }
    }

    fn suite_with_base() -> (tempfile::TempDir, Suite) {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("fixtures/base/vault");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("Today.md"), "- [ ] one\n").unwrap();
        let suite = Suite::from_json(
            serde_json::json!({
                "name": "wf", "runs": 3,
                "tasks": [
                    {"id": "a", "class": "c", "prompt": "p", "workspace": "fixture",
                     "fixture_base": "base", "git_init": true,
                     "assertions": [{"type": "file_exists", "path": "out.txt"}]},
                    {"id": "b-direct-only", "class": "c", "prompt": "p", "workspace": "fixture",
                     "harnesses": ["direct"], "assertions": []}
                ]
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        (root, suite)
    }

    fn cfg(root: &Path, out: &Path, pass_on: fn(u32) -> bool) -> RunConfig {
        RunConfig {
            driver: Box::new(FakeDriver {
                harness: "codex",
                calls: Cell::new(0),
                pass_on,
            }),
            prices: PriceDeck::ZERO,
            out_dir: out.to_path_buf(),
            runs: 3,
            fixtures_root: Some(root.join("fixtures")),
        }
    }

    #[test]
    fn pass_k_needs_every_run_and_a_skip_is_not_a_fail() {
        let (root, suite) = suite_with_base();
        let out = tempfile::tempdir().unwrap();
        let report = run_suite(&suite, &cfg(root.path(), out.path(), |_| true)).unwrap();
        let a = &report.tasks[0];
        assert!(a.passed);
        assert_eq!(a.attempts.len(), 3);
        let b = &report.tasks[1];
        assert!(b.skipped.as_deref().unwrap().contains("codex"));
        assert!(!b.passed && b.attempts.is_empty());
        let card = scorecard(&report);
        assert!(card.contains("| **TOTAL** | **1/1 (100%)**"), "{card}");
        assert!(card.contains("## pass^3 per task"), "{card}");
        assert!(card.contains("| a | c | PASS | 3/3 |"), "{card}");
        assert!(card.contains("## Skipped on codex: 1"), "{card}");
        assert!(card.contains("`b-direct-only`"), "{card}");
        // Three attempts with result 100, 200, 300 ms: p50 200, p95 300.
        assert!(
            card.contains("| submit to result | 200 ms | 300 ms | 3 |"),
            "{card}"
        );
        assert!(
            card.contains("| submit to first model token | 40 ms | 60 ms | 3 |"),
            "{card}"
        );
        // Each attempt left its own transcript.
        assert!(out.path().join("transcripts/a.run3.ndjson").is_file());
    }

    #[test]
    fn one_failing_run_fails_the_task_and_the_record_describes_that_run() {
        let (root, suite) = suite_with_base();
        let out = tempfile::tempdir().unwrap();
        // The second of three runs fails.
        let report = run_suite(&suite, &cfg(root.path(), out.path(), |n| n != 1)).unwrap();
        let a = &report.tasks[0];
        assert!(!a.passed);
        assert_eq!(
            a.attempts.iter().map(|x| x.passed).collect::<Vec<_>>(),
            [true, false, true]
        );
        assert_eq!(a.transcript_path, "transcripts/a.run2.ndjson");
        assert_eq!(a.attempts[1].failed, ["file_exists"]);
        let card = scorecard(&report);
        assert!(
            card.contains("| a | c | FAIL | 2/3 | file_exists |"),
            "{card}"
        );
        assert!(card.contains("| **TOTAL** | **0/1 (0%)**"), "{card}");
    }

    #[test]
    fn a_harnessless_driver_runs_every_task() {
        let mut t = record(0.0);
        t.id = "x".into();
        let task: Task = serde_json::from_value(serde_json::json!({
            "id": "x", "class": "c", "prompt": "p", "workspace": "fixture",
            "harnesses": ["direct"], "assertions": []
        }))
        .unwrap();
        assert_eq!(skip_reason(&task, None), None);
        assert_eq!(skip_reason(&task, Some("direct")), None);
        assert!(skip_reason(&task, Some("claude-code")).is_some());
    }

    #[test]
    fn percentile_is_nearest_rank() {
        assert_eq!(percentile(&[], 50), None);
        assert_eq!(percentile(&[7], 95), Some(7));
        let xs: Vec<u64> = (1..=20).collect();
        assert_eq!(percentile(&xs, 50), Some(10));
        assert_eq!(percentile(&xs, 95), Some(19));
        assert_eq!(percentile(&[300, 100, 200], 50), Some(200));
    }

    #[test]
    fn an_old_results_file_loads_with_one_run_and_no_attempts() {
        let json = serde_json::json!({
            "suite": "s", "driver": "direct", "endpoint": null, "model": null,
            "mock": true, "tasks": [serde_json::to_value(record(0.0)).unwrap()]
        });
        let mut v = json.clone();
        // Strip the fields this change added, as a pre-change file would not have them.
        for k in ["skipped", "latency", "attempts"] {
            v["tasks"][0].as_object_mut().unwrap().remove(k);
        }
        let report: RunReport = serde_json::from_value(v).expect("old results parse");
        assert_eq!(report.runs, 1);
        assert!(report.tasks[0].attempts.is_empty());
    }
}
