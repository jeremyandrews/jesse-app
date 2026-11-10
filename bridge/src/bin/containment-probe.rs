//! `containment-probe` — the containment battery, as a merge gate.
//!
//! [`jesse_bridge::claude_capability_args`] records what this codebase learned the hard way: an empty
//! `--allowedTools` was believed to mean "no tools", and a live probe against the pinned CLI
//! disproved it. The conclusion drawn there is the rule this binary enforces — enumerated
//! denial is not a boundary, and the acceptance gate is a live probe battery re-run against
//! the pinned binary on every change.
//!
//! Run it two ways:
//!
//! ```text
//! cargo run --bin containment-probe            # re-run the battery, compare to the record
//! cargo run --bin containment-probe -- --write # re-run and RE-RECORD bridge/containment.toml
//! cargo run --bin containment-probe -- --show  # print the committed record, run nothing
//! cargo run --bin containment-probe -- --sign  # sign this build and exit (every run signs first)
//! ```
//!
//! Every form but `--show` first takes the LIVE BRIDGE'S ENVIRONMENT from its launchd plist
//! (`~/Library/LaunchAgents/<bridge label>.plist`, `EnvironmentVariables`): the plist
//! wins for every variable, and PATH is the plist's followed by any shell entries it lacks.
//! `--env-plist <path>` names another plist; `--no-env-plist` keeps the shell's environment
//! (CI and tests). `--write` refuses to start when the plist cannot be read. Before any model
//! turn, a pre-flight starts every MCP server the chosen rows load and stops the run, naming
//! them, if any registers zero tools. See [`jesse_bridge::plistenv`].
//!
//! The default form is the gate: it fails (exit 1) when a probe drifts from the committed
//! record in EITHER direction, when a hard gate is not met, or when a probe could not be
//! concluded from. `--write` is the deliberate human act of re-recording a drift on purpose —
//! it is not something CI does.
//!
//! Re-run it on every bump of the pinned agent binary, on every change to the containment
//! posture (the capability→flags mapping, the tool lists, the MCP server sets), and before
//! shipping a new (capability, MCP server set) pair. Each probe is a real headless turn, so a
//! full run costs real money and a few minutes of wall clock; that is the price of a boundary
//! that is proven rather than assumed.

use jesse_bridge::{
    compare_results, export_mcp_server_env, parse_results, plistenv, render_results, run_battery,
    BatteryOptions, Config, ContainmentRow, McpSet,
};
use std::path::PathBuf;

/// The committed record. Resolved against the crate root at COMPILE time so the binary always
/// means this repo's file no matter where it is run from — and so the next step (embedding the
/// record with `include_str!` and validating config against it at startup) names one path.
const RESULTS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/containment.toml");
const CODEX_RESULTS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/containment-codex.toml");
const DIRECT_RESULTS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/containment-direct.toml");
/// Where the agent crate's structural battery leaves its machine-readable summary. See
/// [`jesse_bridge::direct_results_from_battery`] for the two-command chain.
const DIRECT_SUMMARY_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../target/containment-direct.json"
);

/// The record file for a harness. ONE FILE PER HARNESS, because a containment verdict
/// describes a (harness, capability, MCP set) triple: nothing recorded for one harness says
/// anything about another, and a single file with a `harness` field could only ever describe
/// the last one probed.
fn results_path(harness: &str) -> &'static str {
    match harness {
        jesse_bridge::CODEX_ID => CODEX_RESULTS_PATH,
        jesse_bridge::DIRECT_ID => DIRECT_RESULTS_PATH,
        _ => RESULTS_PATH,
    }
}

fn usage() -> ! {
    eprintln!(
        "containment-probe — run the live containment battery against the pinned agent binary\n\
         \n\
         USAGE:\n\
         \x20 containment-probe                 run the battery and compare it to the record\n\
         \x20 containment-probe --write         run the battery and RE-RECORD the results file\n\
         \x20 containment-probe --show          print the committed record without running\n\
         \x20 containment-probe --sign          sign this binary (as every run does first) and exit\n\
         \n\
         OPTIONS:\n\
         \x20 --rows <a/b,...>   only these rows (e.g. read/qmd,write/qmd). Never with --write.\n\
         \x20 --probes <id,...>  only these probes. Never with --write.\n\
         \x20 --timeout <secs>   per-probe timeout (default 300)\n\
         \x20 --keep             keep the scratch trees for inspection\n\
         \x20 --harness <id>     which harness to probe (claude-code | codex | direct)\n\
         \x20 --model <id>       probe AS this registry model (default: the ambient one)\n\
         \x20 --env-plist <path> take the environment from this launchd plist (default: the\n\
         \x20                    live bridge's own LaunchAgent plist)\n\
         \x20 --no-env-plist     keep this shell's environment as is (CI and tests)\n"
    );
    std::process::exit(2)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // THE BRIDGE'S OWN ENVIRONMENT, FROM ITS PLIST, BEFORE ANYTHING ELSE RUNS. The live bridge
    // runs with its launchd plist's `EnvironmentVariables`, PATH included; an interactive
    // shell has none of it. Without the plist's PATH (the nvm node bin, `~/.local/bin`) qmd,
    // github, google, google-perseido, imcp, proxmox, routeros, unifi and whatsapp cannot find
    // `qmd`, `node`, `npx` or `uvx`, start with ZERO tools, and the battery still finishes and
    // writes a record vouching for a posture the bridge never runs. Loaded here, in a plain
    // `main` before the runtime exists, so no other thread can be reading the environment.
    let source = match take_env_source(&mut args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("containment-probe: {e}");
            usage()
        }
    };
    let write = args.iter().any(|a| a == "--write");
    // `--show` reads a committed file and starts nothing, so it needs no environment.
    if !args.iter().any(|a| a == "--show") {
        let plan =
            plistenv::bridge_env_plan(&source, plistenv::read_plist_env, |k| std::env::var(k).ok());
        match plan.verdict(write) {
            Ok(warning) => {
                if let Some(w) = warning {
                    eprintln!("containment-probe: {w}");
                }
            }
            Err(refusal) => {
                eprintln!("containment-probe: {refusal}");
                std::process::exit(2);
            }
        }
        plan.apply();
        eprintln!("containment-probe: {}", plan.summary());
    }

    // THE BATTERY MUST SPAWN CHILDREN IN THE SAME ENVIRONMENT THE BRIDGE DOES, or it records
    // a posture that was never actually probed. Without this the MCP servers that read a
    // credential — Google, GitHub, Fastmail, UniFi — start, find nothing, and register ZERO
    // tools; the run then looks clean while proving nothing about the servers it names, and
    // the resulting record would vouch for a set the child never really loaded. Same call,
    // same reason, as `main.rs`. AFTER the plist, whose `JESSE_*` names it maps.
    export_mcp_server_env();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    rt.block_on(run(args));
}

/// `--env-plist <path>` and `--no-env-plist`, removed from `args`. Default: the live bridge's
/// plist under `$HOME`.
fn take_env_source(args: &mut Vec<String>) -> Result<plistenv::EnvSource, String> {
    let mut explicit: Option<PathBuf> = None;
    let mut off = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-env-plist" => {
                off = true;
                args.remove(i);
            }
            "--env-plist" => {
                args.remove(i);
                if i >= args.len() {
                    return Err("--env-plist needs a path".into());
                }
                explicit = Some(PathBuf::from(args.remove(i)));
            }
            _ => i += 1,
        }
    }
    match (off, explicit) {
        (true, Some(_)) => Err("--env-plist and --no-env-plist contradict each other".into()),
        (true, None) => Ok(plistenv::EnvSource::Off),
        (false, Some(p)) => Ok(plistenv::EnvSource::Plist(p)),
        (false, None) => default_bridge_plist(),
    }
}

/// The live bridge's LaunchAgent plist: `<bridge label>.plist` (the label resolved as the
/// battery's signing does, [`jesse_bridge::probesign::bridge_label`]), else the one
/// `*.jesse-bridge.plist` there. When neither is found, the label's path, which then fails to
/// read and is reported as such.
fn default_bridge_plist() -> Result<plistenv::EnvSource, String> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        "HOME is unset, so the bridge's plist cannot be found; name it with --env-plist \
         <path>, or pass --no-env-plist"
            .to_string()
    })?;
    let agents = PathBuf::from(home).join("Library/LaunchAgents");
    let label = jesse_bridge::probesign::bridge_label();
    Ok(plistenv::EnvSource::Plist(
        plistenv::find_bridge_plist(&agents, Some(&label))
            .unwrap_or_else(|| agents.join(format!("{label}.plist"))),
    ))
}

async fn run(args: Vec<String>) {
    // `--rows`, if it was given. APPLIED AFTER PARSING, not during: the rows depend on
    // `--harness`, and a flag must not mean different things depending on which side of
    // another flag it was written.
    let mut explicit_rows: Option<Vec<jesse_bridge::ContainmentRow>> = None;
    let mut opts = BatteryOptions::default();
    let mut write = false;
    let mut show = false;
    let mut sign_only = false;
    // Resolved AFTER `Config::from_env()` below, because the registry it names lives there.
    let mut model_id: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--write" => write = true,
            "--show" => show = true,
            "--sign" => sign_only = true,
            "--keep" => opts.keep_scratch = true,
            "--harness" => {
                i += 1;
                opts.harness = args.get(i).cloned().unwrap_or_else(|| usage());
            }
            "--model" => {
                i += 1;
                model_id = Some(args.get(i).cloned().unwrap_or_else(|| usage()));
            }
            "--rows" => {
                i += 1;
                let list = args.get(i).cloned().unwrap_or_else(|| usage());
                explicit_rows = Some(list.split(',').map(parse_row).collect::<Vec<_>>());
            }
            "--probes" => {
                i += 1;
                let list = args.get(i).cloned().unwrap_or_else(|| usage());
                opts.probes = Some(list.split(',').map(|s| s.trim().to_string()).collect());
            }
            "--timeout" => {
                i += 1;
                opts.timeout_secs = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| usage());
            }
            "-h" | "--help" => usage(),
            other => {
                eprintln!("containment-probe: unknown argument {other}");
                usage()
            }
        }
        i += 1;
    }

    // THE ROWS FOLLOW THE HARNESS. `--harness` used to set only `opts.harness`, leaving
    // `rows` at its default — Claude Code's — so `--harness codex --write` probed Codex
    // children against a set Codex does not ship and wrote it into Codex's record. See
    // `shipped_rows_for`, and `BatteryOptions::for_harness` for the pairing every
    // programmatic caller gets.
    //
    // ASSIGNED, NOT REBUILT. An earlier spelling of this rebuilt the whole struct with
    // `..BatteryOptions::for_harness(...)`, which silently reset `--probes`, `--timeout` and
    // `--keep` to their defaults — a fix that broke three other flags. One field is wrong, so
    // one field is set.
    //
    // AFTER PARSING, so `--harness` and `--rows` may be written in either order.
    opts.rows =
        explicit_rows.unwrap_or_else(|| jesse_bridge::shipped_rows_for(&opts.harness).to_vec());
    if let Err(e) = jesse_bridge::validate_rows_for_harness(&opts.harness, &opts.rows) {
        eprintln!("containment-probe: {e}");
        std::process::exit(2);
    }

    let results_path = results_path(&opts.harness);
    let recorded_text = std::fs::read_to_string(results_path).ok();

    // ONE CODE IDENTITY ACROSS REBUILDS, before any network call: every run but `--show`
    // (which reads a file and opens nothing). See `jesse_bridge::probesign`.
    if !show {
        sign_self();
        if sign_only {
            return;
        }
    }

    if show {
        match recorded_text {
            Some(text) => print_record(&text),
            None => {
                eprintln!("containment-probe: no record at {results_path}");
                std::process::exit(1);
            }
        }
        return;
    }

    // ---- THE DIRECT HARNESS TAKES A DIFFERENT ROUTE ENTIRELY ------------
    //
    // Everything below this block is about driving a CHILD PROCESS: resolving a binary,
    // planting a credential, spawning per row, scoring what came back. None of it applies to
    // a harness that spawns nothing. Its battery lives in the agent crate (where the tools
    // are) and runs with a scripted provider and no model at all, so this mode reads that
    // battery's out-of-band summary and renders the record from it.
    //
    // Handled here, before the model/binary resolution, rather than by threading a second
    // shape through the run loop: the two have almost nothing in common beyond the file they
    // write, and pretending otherwise would mean a `--model` flag that silently means nothing
    // and a `--rows` flag with nothing to select.
    if opts.harness == jesse_bridge::DIRECT_ID {
        if model_id.is_some() {
            eprintln!(
                "containment-probe: --model means nothing for harness 'direct' — its battery \
                 runs with a scripted provider and no model, which is what makes every escape \
                 definitely attempted. Drop --model."
            );
            std::process::exit(2);
        }
        let cfg = Config::from_env();
        // The gateway's probes, run here against the real gateway and probe upstream.
        let mut gateway_probes = std::collections::BTreeMap::new();
        for level in ["basic", "read", "write"] {
            gateway_probes.insert(
                level.to_string(),
                jesse_bridge::direct_gateway_probes(level).await,
            );
        }
        let fresh = match jesse_bridge::direct_results_from_battery(
            &cfg,
            std::path::Path::new(DIRECT_SUMMARY_PATH),
            &gateway_probes,
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("containment-probe: {e}");
                std::process::exit(1);
            }
        };
        print_record(&render_results(&fresh));
        report_local_network_denials();
        if !write {
            eprintln!(
                "\ncontainment-probe: nothing written (add --write to record {})",
                results_path
            );
            return;
        }
        let mut fresh = fresh;
        // Carry the committed acceptances across, exactly as the spawned path does: a fresh
        // run knows what the boundary did and cannot know who agreed to ship it.
        if let Some(prev) = recorded_text.as_deref().and_then(|t| parse_results(t).ok()) {
            fresh.accepted = prev.accepted.clone();
            let drift = compare_results(&prev, &fresh);
            if drift.is_empty() {
                eprintln!(
                    "\ncontainment-probe: re-recording — nothing moved since {}.",
                    prev.recorded
                );
            } else {
                eprintln!("\nRE-RECORDING, {} probe(s) moved:", drift.len());
                for d in &drift {
                    eprintln!("  ~ {d}");
                }
            }
        }
        let text = render_results(&fresh);
        if let Err(e) = std::fs::write(results_path, &text) {
            eprintln!("containment-probe: could not write {results_path}: {e}");
            std::process::exit(1);
        }
        eprintln!("\ncontainment-probe: recorded {results_path}");
        return;
    }

    // A partial run cannot be a record: a file missing rows or probes reads as a battery that
    // covered everything and found nothing, which is the exact lie this gate exists to stop.
    //
    // IT COMPARES THE ROWS, NOT HOW MANY THERE ARE. The count it used to compare was the one
    // property a WRONG list is most likely to share with the right one — both spawned
    // harnesses ship four rows, so four of Claude Code's rows sailed through a check that was
    // meant to be certifying Codex's. See `records_the_whole_battery`.
    if write {
        if let Err(e) = jesse_bridge::records_the_whole_battery(
            &opts.harness,
            &opts.rows,
            opts.probes.as_deref(),
        ) {
            eprintln!(
                "containment-probe: --write records the WHOLE battery — {e}. Drop \
                 --rows/--probes, or drop --write."
            );
            std::process::exit(2);
        }
    }

    let cfg = Config::from_env();

    // Resolve `--model` against the SAME registry the serving bridge builds, and demand it be
    // CONFIGURED. An unarmed entry has no credential to give the child, so every probe would
    // come back inconclusive and the run would record a battery that proved nothing — the
    // exact lie the `--write` guard above exists to stop, arriving by a different door.
    if let Some(id) = &model_id {
        let Some(m) = cfg.model_registry.get(id) else {
            eprintln!(
                "containment-probe: unknown model '{id}' — it must be a registered entry \
                 (a [[models]] block or a JESSE_MODEL_* triple)."
            );
            std::process::exit(2);
        };
        if !m.configured {
            eprintln!(
                "containment-probe: model '{id}' is registered but NOT configured (its token \
                 env var is unset), so every probe would run without a credential and prove \
                 nothing. Arm it and re-run."
            );
            std::process::exit(2);
        }
        if m.harness != opts.harness {
            eprintln!(
                "containment-probe: model '{id}' runs on harness '{}' but this run probes \
                 '{}' — a record must be written by a model the harness actually serves.",
                m.harness, opts.harness
            );
            std::process::exit(2);
        }
        opts.model = Some(jesse_bridge::ActiveModel::from_registry(m));
    }

    let bin = if opts.harness == jesse_bridge::CODEX_ID {
        &cfg.codex_bin
    } else {
        &cfg.claude_bin
    };
    eprintln!(
        "containment-probe: {} rows x {} probes against {} ({}) [harness {}, model {}]",
        opts.rows.len(),
        opts.probes
            .as_ref()
            .map(|p| p.len())
            .unwrap_or(jesse_bridge::PROBES.len()),
        bin,
        jesse_bridge::probe_binary_version(bin),
        opts.harness,
        opts.model
            .as_ref()
            .map(|m| m.id.as_str())
            .unwrap_or("the ambient default"),
    );

    // THE PRE-FLIGHT: every server these rows load must come up with at least one tool, in
    // this environment, before a single model turn is spent. A server that cannot find its
    // binary or its credential registers zero tools, and a battery run over it proves nothing
    // about the set it names.
    let servers = jesse_bridge::preflight_servers(&opts.rows);
    if !servers.is_empty() {
        let counts = jesse_bridge::count_server_tools(&cfg, &servers).await;
        let listed: Vec<String> = counts
            .iter()
            .map(|(n, r)| match r {
                Ok(c) => format!("{n} {c}"),
                Err(_) => format!("{n} -"),
            })
            .collect();
        eprintln!(
            "containment-probe: pre-flight, tools per MCP server: {}",
            listed.join(", ")
        );
        if let Err(e) = jesse_bridge::preflight_verdict(&counts) {
            eprintln!("containment-probe: {e}");
            let failures: Vec<(String, String)> = counts
                .iter()
                .filter_map(|(n, r)| r.as_ref().err().map(|e| (n.clone(), e.clone())))
                .collect();
            let exe = std::env::current_exe()
                .map(|e| e.display().to_string())
                .unwrap_or_else(|_| "containment-probe".to_string());
            for l in jesse_bridge::probesign::local_network_lines(&failures, &exe) {
                eprintln!("  ✗ {l}");
            }
            std::process::exit(1);
        }
        eprintln!(
            "containment-probe: pre-flight passed, all {} MCP server(s) registered tools",
            counts.len()
        );
    }

    let outcome = match run_battery(&cfg, &opts).await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("containment-probe: the battery could not run: {e}");
            std::process::exit(1);
        }
    };
    let fresh = &outcome.results;
    eprintln!(
        "\ncontainment-probe: run finished — gate {}, ${:.2} spent{}",
        fresh.gate,
        outcome.cost_usd,
        if opts.keep_scratch {
            format!(", scratch kept at {}", outcome.scratch.display())
        } else {
            String::new()
        }
    );

    // A LAN server macOS refused is a missing grant, not a moved boundary: said FIRST, so the
    // drift list below is never read as a tool set change.
    report_local_network_denials();

    // The findings, always printed, whatever the mode: a run that scrolled past is a run
    // nobody read.
    let failures = fresh.hard_gate_failures();
    let open = fresh.known_open();
    if !failures.is_empty() {
        eprintln!("\nHARD GATES NOT MET ({}):", failures.len());
        for f in &failures {
            eprintln!("  ✗ {f}");
        }
    }
    if !open.is_empty() {
        eprintln!("\nKNOWN-OPEN BASELINES ({}):", open.len());
        for o in &open {
            eprintln!("  ! {o}");
        }
    }

    if write {
        // A fresh run knows what the boundary did; it cannot know who agreed to ship it.
        // Carry the committed acceptances across, or `--write` would erase the decision
        // record at exactly the moment an operator most needs to read it.
        let mut fresh = fresh.clone();
        // Re-recording is a deliberate act, so say what it is about to change. A silent
        // overwrite is how a posture regression gets committed as "the new baseline" without
        // anyone deciding that it should be.
        if let Some(prev) = recorded_text.as_deref().and_then(|t| parse_results(t).ok()) {
            fresh.accepted = prev.accepted.clone();
            let drift = compare_results(&prev, &fresh);
            if drift.is_empty() {
                eprintln!(
                    "\ncontainment-probe: re-recording — nothing moved since {} (this run \
                     CONFIRMS the previous one).",
                    prev.recorded
                );
            } else {
                eprintln!("\nRE-RECORDING, {} probe(s) moved:", drift.len());
                for d in &drift {
                    eprintln!("  ~ {d}");
                }
            }
        }
        // Which of the open doors has nobody signed for? "We accepted the read opens" must
        // not quietly come to mean "we accepted everything the battery found".
        let unsigned = fresh.unaccepted_known_open();
        if !unsigned.is_empty() {
            eprintln!(
                "\nKNOWN-OPEN AND UNACCEPTED ({}) — no [[accepted]] entry covers these:",
                unsigned.len()
            );
            for u in &unsigned {
                eprintln!("  ! {u}");
            }
        }
        // An acceptance that outlived its finding is not an error — the door may simply have
        // closed — but it must not be left to imply a risk nobody is carrying any more.
        let stale = fresh.stale_acceptances();
        if !stale.is_empty() {
            eprintln!(
                "\nSTALE ACCEPTANCES ({}) — remove them on purpose:",
                stale.len()
            );
            for s in &stale {
                eprintln!("  ~ {s}");
            }
        }
        let text = render_results(&fresh);
        if let Err(e) = std::fs::write(results_path, &text) {
            eprintln!("containment-probe: could not write {results_path}: {e}");
            std::process::exit(1);
        }
        eprintln!("\ncontainment-probe: recorded {results_path}");
        if !failures.is_empty() {
            eprintln!(
                "containment-probe: the record says gate = fail — {} hard gate(s) are not met. \
                 Recording the truth is right; shipping this pair is a decision.",
                failures.len()
            );
            std::process::exit(1);
        }
        return;
    }

    // Gate mode: drift against the record, plus the record's own health.
    let Some(text) = recorded_text else {
        eprintln!(
            "\ncontainment-probe: no record at {results_path} — nothing to compare against. \
             Re-run with --write to record this battery."
        );
        std::process::exit(1);
    };
    let recorded = match parse_results(&text) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("containment-probe: {results_path} does not parse: {e}");
            std::process::exit(1);
        }
    };
    if recorded.binary_version != fresh.binary_version {
        eprintln!(
            "\ncontainment-probe: the record was taken against {} and this is {} — the battery \
             is re-run on every binary version bump.",
            recorded.binary_version, fresh.binary_version
        );
    }
    // Said out loud for the same reason as the binary version: the turn-behavior half of a
    // row is model-dependent, so comparing a K3 run against an ambient-probed record is
    // comparing two different questions. Not a failure by itself — the drift list below is
    // still the verdict — but a reader must not have to infer it.
    if recorded.model != fresh.model {
        let name = |m: &Option<String>| m.clone().unwrap_or_else(|| "the ambient default".into());
        eprintln!(
            "\ncontainment-probe: the record was probed by {} and this run by {} — the OS \
             boundary is model-independent, but which tools a child TRIED is not.",
            name(&recorded.model),
            name(&fresh.model)
        );
    }
    let drift = compare_results(&recorded, fresh);
    if !drift.is_empty() {
        eprintln!("\nBASELINE DRIFT ({}):", drift.len());
        for d in &drift {
            eprintln!("  ~ {d}");
        }
        eprintln!(
            "\ncontainment-probe: FAIL — a probe moved. A flip in either direction fails the \
             gate until a human re-records it on purpose (--write)."
        );
        std::process::exit(1);
    }
    if !failures.is_empty() {
        eprintln!(
            "\ncontainment-probe: FAIL — {} hard gate(s) not met.",
            failures.len()
        );
        std::process::exit(1);
    }
    eprintln!("\ncontainment-probe: PASS — hard gates green, baselines match the record.");
}

/// Sign this executable with the deployment's identity, re-executing once if the signature on
/// disk was not the expected one. No identity: one line, and the run goes on ad hoc. A
/// configured identity that cannot sign or verify stops the run here, before any probe.
fn sign_self() {
    use jesse_bridge::probesign::{
        ensure_signed, plan_from_env, SelfSign, SystemCodesign, AD_HOC_NOTE, RESIGNED_ENV,
    };
    if !cfg!(target_os = "macos") {
        return;
    }
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("containment-probe: cannot find my own executable to sign it: {e}");
            std::process::exit(1);
        }
    };
    let plan = plan_from_env();
    let reexeced = std::env::var_os(RESIGNED_ENV).is_some();
    match ensure_signed(&exe, plan.as_ref(), reexeced, &SystemCodesign::default()) {
        Ok(SelfSign::AdHoc) => eprintln!("{AD_HOC_NOTE}"),
        Ok(SelfSign::AlreadySigned) => {
            if let Some(p) = &plan {
                eprintln!(
                    "containment-probe: signed as {} ({})",
                    p.identifier,
                    exe.display()
                );
            }
        }
        Ok(SelfSign::Resigned) => {
            let id = plan.as_ref().map(|p| p.identifier.as_str()).unwrap_or("");
            eprintln!(
                "containment-probe: re-signed {} as {id}; re-executing",
                exe.display()
            );
            let e = jesse_bridge::probesign::reexec(&exe);
            eprintln!("containment-probe: {e}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("containment-probe: signing failed, nothing was probed: {e}");
            std::process::exit(1);
        }
    }
}

/// Name every LAN MCP server that failed with `os error 65` during this run as a likely Local
/// Network denial for this binary.
fn report_local_network_denials() {
    let failures = jesse_bridge::gateway::take_upstream_failures();
    let exe = std::env::current_exe()
        .map(|e| e.display().to_string())
        .unwrap_or_else(|_| "containment-probe".to_string());
    let lines = jesse_bridge::probesign::local_network_lines(&failures, &exe);
    if !lines.is_empty() {
        eprintln!("\nLOCAL NETWORK DENIED ({}):", lines.len());
        for l in &lines {
            eprintln!("  ✗ {l}");
        }
    }
}

fn parse_row(s: &str) -> ContainmentRow {
    let (cap, mcp) = s.trim().split_once('/').unwrap_or_else(|| {
        eprintln!("containment-probe: a row is <capability>/<mcp set>, e.g. read/qmd (got {s})");
        std::process::exit(2)
    });
    let (Some(capability), Some(mcp)) = (jesse_bridge::parse_capability(cap), McpSet::parse(mcp))
    else {
        eprintln!("containment-probe: unknown row {s}");
        std::process::exit(2)
    };
    ContainmentRow { capability, mcp }
}

/// Print the committed record as a table, for the human who wants to know where a pair stands
/// without spending five minutes and a few dollars finding out again.
fn print_record(text: &str) {
    let r = match parse_results(text) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("containment-probe: the committed record does not parse: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "harness {} — binary {} — recorded {} — gate {}",
        r.harness, r.binary_version, r.recorded, r.gate
    );
    for row in &r.rows {
        println!("\n{} [{}]", row.label(), row.status);
        for p in &row.probes {
            // The record stores ids; what each id ATTEMPTS lives with the probe table, so
            // the human-readable form is joined here rather than duplicated 52 times in the
            // file.
            let what = jesse_bridge::PROBES
                .iter()
                .find(|probe| probe.id == p.id)
                .map(|probe| probe.summary)
                .unwrap_or("(no such probe in this build)");
            println!(
                "  {:<24} {:<10} {:<10} {}\n  {:<24} {}",
                p.id, p.verdict, p.status, what, "", p.evidence
            );
        }
    }
    let failures = r.hard_gate_failures();
    if !failures.is_empty() {
        println!("\nhard gates not met:");
        for f in failures {
            println!("  ✗ {f}");
        }
    }
    let open = r.known_open();
    if !open.is_empty() {
        println!("\nknown-open baselines:");
        for o in open {
            println!("  ! {o}");
        }
    }
}
