//! `tool-usage`: which allowlist grants real turns used, from the bridge's content-free
//! `turn-timings.jsonl`, joined against `bridge/capability-map.toml`.
//!
//! A separate binary rather than a `jesse-bridge` subcommand, following the bridge's other
//! read-only audits (`vaultqa-audit`, `shadow-audit`): the serving binary parses no
//! subcommands, and this never needs the server's config, credentials or state beyond the
//! one file it reads. The logic lives in the library (`jesse_bridge::tool_usage_report`) so it
//! is tested there.
//!
//!   tool-usage --since <days> [--json] [--file <path>]
//!
//! `--file` defaults to `$JESSE_STATE_DIR/turn-timings.jsonl`, else
//! `~/.jesse-bridge/turn-timings.jsonl`. The map is the one compiled into this binary.
//! Read-only: nothing is written anywhere.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use jesse_bridge::{
    parse_timing_log, render_tool_usage, rfc3339_utc, shipped_capability_map, tool_usage_report,
    TURN_TIMING_FILE,
};

const USAGE: &str = "usage: tool-usage --since <days> [--json] [--file <turn-timings.jsonl>]

Counts calls and distinct turns per tool name over the window, joins them against
bridge/capability-map.toml, and prints: grants used, grants never used, and trace
names that match no grant (with why each was callable). Bash and Skill are class
rows: the trace cannot tell their scoped grants apart.";

struct Args {
    since_days: u64,
    json: bool,
    file: PathBuf,
}

fn default_file() -> PathBuf {
    let state = std::env::var("JESSE_STATE_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".jesse-bridge")
        });
    state.join(TURN_TIMING_FILE)
}

fn parse() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut since: Option<u64> = None;
    let mut json = false;
    let mut file: Option<PathBuf> = None;
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--since" => {
                let v = val()?;
                since = Some(
                    v.parse::<u64>()
                        .ok()
                        .filter(|d| *d > 0)
                        .ok_or_else(|| format!("--since takes a whole number of days, not {v}"))?,
                );
            }
            "--json" => json = true,
            "--file" => file = Some(PathBuf::from(val()?)),
            "-h" | "--help" => return Err("help".to_string()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        since_days: since.ok_or_else(|| "--since is required".to_string())?,
        json,
        file: file.unwrap_or_else(default_file),
    })
}

fn main() {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            if e != "help" {
                eprintln!("tool-usage: {e}\n");
            }
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    let body = match std::fs::read_to_string(&args.file) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("tool-usage: cannot read {}: {e}", args.file.display());
            std::process::exit(1);
        }
    };
    let (records, unparsed) = parse_timing_log(&body);
    let window_start = rfc3339_utc(
        SystemTime::now() - Duration::from_secs(args.since_days.saturating_mul(86_400)),
    );
    let report = tool_usage_report(
        &shipped_capability_map(),
        &records,
        unparsed,
        args.since_days,
        &window_start,
    );
    if args.json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("tool-usage: {e}");
                std::process::exit(1);
            }
        }
    } else {
        print!("{}", render_tool_usage(&report));
    }
}
