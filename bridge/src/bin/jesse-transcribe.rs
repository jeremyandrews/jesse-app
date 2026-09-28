//! `jesse-transcribe`: transcribe one recording on this machine, without the app and without
//! the running bridge.
//!
//! It runs the bridge's own pipeline in process (the same intake gate, custody directory,
//! decoder, conditioning, engines, chunking and reconciliation `POST /jesse/transcriptions`
//! runs) and prints the transcript to stdout, then the disagreement list if there is one.
//! Progress and notes go to stderr, so `> transcript.txt` keeps only the words.
//!
//! It reads the same configuration the bridge reads: the environment (`JESSE_SPEECH_*`,
//! `JESSE_MODEL_*`, `JESSE_STATE_DIR`) and the `[[models]]` file. The bridge's launch
//! environment lives in its launchd plist, which an interactive shell does not have, so
//! `--env-plist <plist>` fills every variable the shell has not set from that plist's
//! `EnvironmentVariables`. Nothing is written back and no secret is printed.
//!
//! Usage:
//!     jesse-transcribe <file> [--engine local|hosted:<id>|local,hosted:<id>]
//!                             [--second-engine local|hosted:<id>] [--no-second-reading]
//!                             [--language <code>] [--conditioning auto|on|off]
//!                             [--env-plist <path>] [--list-engines]
//!
//! `--engine hosted:<id>` SENDS THE AUDIO OFF THIS MACHINE to that provider. Without it (and
//! with `JESSE_SPEECH_ENGINE` unset) nothing leaves the machine.
//!
//! Exit status: 0 with a transcript, 1 when the run failed, 2 on bad usage, 130 on Ctrl-C.

use jesse_bridge::speech::http::{engine_rows, resolve_plan};
use jesse_bridge::speech::intake::{sniff_audio, AudioCustody, UploadGate, SNIFF_BYTES};
use jesse_bridge::speech::service::JobOptions;
use jesse_bridge::speech::SpeechService;
use jesse_bridge::Config;
use serde_json::Value;
use std::io::Read;
use std::time::Duration;

const USAGE: &str =
    "usage: jesse-transcribe <file> [--engine local|hosted:<id>|local,hosted:<id>] \
[--second-engine local|hosted:<id>] [--no-second-reading] [--language <code>] \
[--conditioning auto|on|off] [--env-plist <path>] [--list-engines]";

#[derive(Default)]
struct Args {
    file: Option<String>,
    engine: Option<String>,
    second_engine: Option<String>,
    second_reading: Option<String>,
    language: Option<String>,
    conditioning: Option<String>,
    env_plist: Option<String>,
    list: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--engine" => a.engine = Some(value("--engine")?),
            "--second-engine" => a.second_engine = Some(value("--second-engine")?),
            "--no-second-reading" => a.second_reading = Some("off".to_string()),
            "--language" => a.language = Some(value("--language")?),
            "--conditioning" => a.conditioning = Some(value("--conditioning")?),
            "--env-plist" => a.env_plist = Some(value("--env-plist")?),
            "--list-engines" => a.list = true,
            "-h" | "--help" => return Err(String::new()),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s if a.file.is_none() => a.file = Some(s.to_string()),
            s => return Err(format!("one file at a time (got {s:?} as well)")),
        }
    }
    if a.file.is_none() && !a.list {
        return Err("name the recording to transcribe".to_string());
    }
    Ok(a)
}

/// Fill every variable the shell has not set from a launchd plist's `EnvironmentVariables`.
fn load_plist_env(path: &str) -> Result<usize, String> {
    let out = std::process::Command::new("/usr/bin/plutil")
        .args(["-extract", "EnvironmentVariables", "json", "-o", "-", path])
        .output()
        .map_err(|e| format!("could not run plutil: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{path} has no readable EnvironmentVariables: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let map: serde_json::Map<String, Value> = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("{path}: EnvironmentVariables is not a dictionary ({e})"))?;
    let mut filled = 0;
    for (k, v) in map {
        if std::env::var_os(&k).is_none() {
            if let Some(s) = v.as_str() {
                std::env::set_var(&k, s);
                filled += 1;
            }
        }
    }
    Ok(filled)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("jesse-transcribe: {e}");
            }
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    // Before the runtime starts, so no other thread can be reading the environment.
    if let Some(p) = &args.env_plist {
        match load_plist_env(p) {
            Ok(n) => eprintln!("jesse-transcribe: {n} variable(s) taken from {p}"),
            Err(e) => {
                eprintln!("jesse-transcribe: {e}");
                std::process::exit(2);
            }
        }
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    std::process::exit(rt.block_on(run(args)));
}

async fn run(args: Args) -> i32 {
    let cfg = Config::from_env();
    if args.list {
        for row in engine_rows(&cfg) {
            println!(
                "{:<28} {}",
                row["id"].as_str().unwrap_or(""),
                row["label"].as_str().unwrap_or("")
            );
        }
        println!("default: {}", cfg.speech.engine.label());
        if args.file.is_none() {
            return 0;
        }
    }
    let speech = SpeechService::from_config(cfg.speech.clone());
    if let Err(why) = speech.availability() {
        eprintln!("jesse-transcribe: {why}");
        return 1;
    }
    let mut opts = match JobOptions::parse(
        args.language.as_deref(),
        args.conditioning.as_deref(),
        args.second_reading.as_deref(),
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("jesse-transcribe: {e}");
            return 2;
        }
    };
    opts.plan = match resolve_plan(&cfg, args.engine.as_deref(), args.second_engine.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("jesse-transcribe: {e}");
            return 2;
        }
    };
    if opts.plan.leaves_the_studio() {
        eprintln!(
            "jesse-transcribe: engine {}: this recording may be sent off this machine",
            opts.plan.choice.label()
        );
    }

    let file = args.file.as_deref().unwrap_or_default();
    let Some(root) = speech.intake_dir() else {
        eprintln!("jesse-transcribe: no intake directory (set JESSE_STATE_DIR)");
        return 1;
    };
    let custody = match AudioCustody::open(&root) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("jesse-transcribe: could not open a private intake directory: {e}");
            return 1;
        }
    };
    // The same gate the upload route streams through: type from the magic bytes, the cap.
    let (upload, sniffed) = match take_custody(file, &custody, speech.config.max_audio_bytes) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("jesse-transcribe: {e}");
            return 1;
        }
    };
    let first = speech.start(custody, upload, sniffed, opts, None);
    let Some(id) = first["id"].as_str().map(str::to_string) else {
        eprintln!("jesse-transcribe: the run did not start");
        return 1;
    };

    let mut last_line = String::new();
    let mut interrupted = false;
    let status = loop {
        let v = speech.status(&id).unwrap_or(Value::Null);
        if v["state"] != "running" {
            break v;
        }
        let line = format!(
            "{} {}{}",
            v["phase"].as_str().unwrap_or(""),
            v["engine"].as_str().unwrap_or(""),
            match v["fraction"].as_f64() {
                Some(f) if f > 0.0 => format!(" {:.0}%", f * 100.0),
                _ => String::new(),
            }
        );
        if line != last_line {
            eprintln!("jesse-transcribe: {line}");
            last_line = line;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            _ = tokio::signal::ctrl_c(), if !interrupted => {
                interrupted = true;
                eprintln!("jesse-transcribe: cancelling; the audio is deleted as the run stops");
                speech.cancel(&id);
            }
        }
    };

    for note in status["notes"].as_array().into_iter().flatten() {
        eprintln!("jesse-transcribe: note: {}", note.as_str().unwrap_or(""));
    }
    match status["state"].as_str() {
        Some("done") => {
            let engines: Vec<String> = status["engines"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| match e["host"].as_str() {
                    Some(h) => format!("{} (hosted at {h})", e["label"].as_str().unwrap_or("")),
                    None => format!("{} (on this machine)", e["label"].as_str().unwrap_or("")),
                })
                .collect();
            eprintln!("jesse-transcribe: read by {}", engines.join(" and "));
            println!("{}", status["transcript"].as_str().unwrap_or(""));
            let disagreements = status["disagreements"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if !disagreements.is_empty() {
                println!("\n--- where the two readings disagree ---");
                for d in disagreements {
                    let at = d["start_ms"].as_u64().unwrap_or(0) / 1_000;
                    println!(
                        "{:02}:{:02}  {}  |  {}",
                        at / 60,
                        at % 60,
                        d["primary"].as_str().unwrap_or(""),
                        d["alternative"].as_str().unwrap_or("")
                    );
                }
            }
            0
        }
        Some("cancelled") => 130,
        _ => {
            eprintln!(
                "jesse-transcribe: failed ({}): {}",
                status["error"]["kind"].as_str().unwrap_or("unknown"),
                status["error"]["message"].as_str().unwrap_or("")
            );
            1
        }
    }
}

/// Copy the recording into custody through the upload gate. The copy is what the pipeline
/// reads and what custody deletes; the owner's file is never touched.
fn take_custody(
    file: &str,
    custody: &AudioCustody,
    cap: u64,
) -> Result<
    (
        std::path::PathBuf,
        jesse_bridge::speech::intake::SniffedUpload,
    ),
    String,
> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut src = std::fs::File::open(file).map_err(|e| format!("{file}: {e}"))?;
    let mut head = [0u8; SNIFF_BYTES];
    let n = src.read(&mut head).map_err(|e| format!("{file}: {e}"))?;
    let (mime, _) = sniff_audio(&head[..n])
        .ok_or_else(|| format!("{file} is not a recording this pipeline reads"))?;
    let mut gate = UploadGate::new(mime, cap);
    let partial = custody.file("upload.part");
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&partial)
        .map_err(|e| format!("could not stage the recording: {e}"))?;
    let mut feed = |bytes: &[u8]| -> Result<(), String> {
        gate.accept(bytes).map_err(|(_, e)| e)?;
        out.write_all(bytes)
            .map_err(|e| format!("could not stage the recording: {e}"))
    };
    feed(&head[..n])?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = src.read(&mut buf).map_err(|e| format!("{file}: {e}"))?;
        if n == 0 {
            break;
        }
        feed(&buf[..n])?;
    }
    out.sync_all()
        .map_err(|e| format!("could not stage the recording: {e}"))?;
    let sniffed = gate.finish().map_err(|(_, e)| e)?;
    let upload = custody.file(&format!("upload.{}", sniffed.ext));
    std::fs::rename(&partial, &upload).map_err(|e| format!("could not keep the upload: {e}"))?;
    Ok((upload, sniffed))
}
