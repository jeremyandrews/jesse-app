//! **THE RUNNER** — `jesse-runner <run dir>`, started by launchd as the run's own job.
//!
//! It runs one round of the agent: `claude -p` in the run's worktree, its own process group,
//! the prompt (round 1) or the reply (later rounds) on stdin, every stream event appended to
//! `stream.jsonl`. It has **no timeout**. When the agent exits it writes `exit.json` and boots its
//! own launchd label out. The environment is an allowlist ([`super::ENV_ALLOWLIST`]), so no
//! bridge secret reaches the agent, plus the repository's `env_file` when the run records one:
//! the runner reads it at each round and hands its variables to the agent and nothing else.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::Path;
use std::process::{Command, Stdio};

use super::{
    now_ms, read_env_file, read_run, rfc, update_run, write_exit, ExitInfo, RunRecord,
    ENV_ALLOWLIST, PROMPT_MD, REPORT_MD, STREAM, TARGET_DIR, WORKTREE_DIR,
};

/// `last_event_at` is written at most this often.
const EVENT_WRITE_MS: u64 = 1000;

/// The runner's own variable (never the agent's) naming `<state_dir>/attribution`, set by the
/// bridge in the run's launchd job.
pub const ATTRIBUTION_ENV: &str = "JESSE_ATTRIBUTION_DIR";

/// The argv the agent runs with, after the program.
pub fn claude_args(rec: &RunRecord, first_round: bool, session: &str) -> Vec<String> {
    claude_args_with(rec, first_round, session, None)
}

/// [`claude_args`], with the bridge's attribution settings file (`--settings`) when there is
/// one: Claude Code's own attribution off and the hook-skipping git spellings denied.
pub fn claude_args_with(
    rec: &RunRecord,
    first_round: bool,
    session: &str,
    settings: Option<&Path>,
) -> Vec<String> {
    let mut a = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
    ];
    if first_round {
        a.push("--session-id".into());
    } else {
        a.push("--resume".into());
    }
    a.push(session.to_string());
    a.push("--permission-mode".into());
    a.push(rec.permission_mode.clone());
    if let Some(m) = &rec.model {
        a.push("--model".into());
        a.push(m.clone());
    }
    if let Some(s) = settings {
        a.push("--settings".into());
        a.push(s.to_string_lossy().into_owned());
    }
    a
}

/// `env` with the variables that point its `git` at the bridge's hooks appended, after any
/// `GIT_CONFIG_*` it already carries (from the repository's env file, say). Unchanged without
/// a hooks directory.
pub fn with_attribution(
    mut env: Vec<(String, String)>,
    hooks: Option<&Path>,
) -> Vec<(String, String)> {
    if let Some(h) = hooks {
        let add = crate::attribution::git_env(
            |k| {
                env.iter()
                    .rev()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| v.clone())
            },
            h,
        );
        env.extend(add);
    }
    env
}

/// The environment the agent gets: the allowlist from the runner's own environment, plus the
/// run's three.
pub fn agent_env(
    dir: &Path,
    run_id: &str,
    from: impl Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    agent_env_with(dir, run_id, from, Vec::new())
}

/// [`agent_env`] with the repository's env file variables after the allowlist, so they may
/// override it; the run's own three come last and are never overridden.
pub fn agent_env_with(
    dir: &Path,
    run_id: &str,
    from: impl Fn(&str) -> Option<String>,
    env_file: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = ENV_ALLOWLIST
        .iter()
        .filter_map(|k| from(k).map(|v| (k.to_string(), v)))
        .collect();
    env.extend(env_file);
    env.push(("JESSE_RUN_ID".into(), run_id.to_string()));
    env.push((
        "JESSE_RUN_REPORT".into(),
        dir.join(REPORT_MD).to_string_lossy().into_owned(),
    ));
    env.push((
        "CARGO_TARGET_DIR".into(),
        dir.join(TARGET_DIR).to_string_lossy().into_owned(),
    ));
    env
}

/// The desk command: take the session over in Terminal.
pub fn desk_command(dir: &Path, claude: &str, session: &str) -> String {
    format!(
        "cd {} && {claude} --resume {session}",
        shell_quote(&dir.join(WORKTREE_DIR).to_string_lossy())
    )
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Run the current round of the run in `dir`. Returns the process exit code for the runner.
pub fn run(dir: &Path, bootout: bool) -> i32 {
    match run_round(dir) {
        Ok(exit) => {
            eprintln!(
                "jesse-runner: round {} ended: code {:?} signal {:?}",
                exit.round, exit.code, exit.signal
            );
            if bootout {
                if let Ok(rec) = read_run(dir) {
                    boot_out_self(&rec);
                }
            }
            0
        }
        Err(e) => {
            eprintln!("jesse-runner: {e}");
            // Still leave an exit behind, so the watcher reports the round rather than calling
            // it interrupted.
            if let Ok(rec) = read_run(dir) {
                let round = rec.round().map(|r| r.n).unwrap_or(1);
                if !dir.join(REPORT_MD).exists() {
                    let _ = std::fs::write(
                        dir.join(REPORT_MD),
                        format!("The runner could not run the agent: {e}\n"),
                    );
                }
                let _ = write_exit(
                    dir,
                    &ExitInfo {
                        round,
                        code: Some(-1),
                        signal: None,
                        ended_at: rfc(now_ms()),
                    },
                );
                if bootout {
                    boot_out_self(&rec);
                }
            }
            1
        }
    }
}

/// Boot out the runner's own launchd label: the one the bridge recorded, which launchd also
/// hands the runner as `XPC_SERVICE_NAME`.
fn boot_out_self(rec: &RunRecord) {
    let Some(label) = rec
        .label
        .clone()
        .or_else(|| std::env::var("XPC_SERVICE_NAME").ok())
        .filter(|l| l.ends_with(&format!(".{}", rec.run_id)))
    else {
        return;
    };
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let _ = Command::new("/bin/launchctl")
        .arg("bootout")
        .arg(format!("gui/{uid}/{label}"))
        .status();
}

fn run_round(dir: &Path) -> Result<ExitInfo, String> {
    let rec = read_run(dir)?;
    let round = rec
        .round()
        .cloned()
        .ok_or_else(|| "run.json has no round".to_string())?;
    let first = round.n == 1 && rec.session_id.is_none();
    let session = match &rec.session_id {
        Some(s) => s.clone(),
        None => {
            let s = uuid::Uuid::new_v4().to_string();
            let desk = desk_command(dir, &rec.claude, &s);
            let s2 = s.clone();
            update_run(dir, |r| {
                r.session_id = Some(s2.clone());
                r.desk_command = Some(desk.clone());
            })?;
            s
        }
    };
    let input = match &round.text {
        Some(t) if round.n > 1 => t.clone(),
        _ => std::fs::read_to_string(dir.join(PROMPT_MD))
            .map_err(|e| format!("could not read prompt.md: {e}"))?,
    };
    let worktree = dir.join(WORKTREE_DIR);
    if !worktree.is_dir() {
        return Err(format!("the worktree {} is missing", worktree.display()));
    }
    let file_env = match &rec.env_file {
        Some(p) => read_env_file(Path::new(p))?,
        None => Vec::new(),
    };
    // NO AI ATTRIBUTION: the hooks and settings, rewritten from this binary before the round,
    // so a run never meets a copy someone edited. A runner that cannot write them does not
    // run the agent: pointed at a missing hooks directory, git would run no hook at all.
    let attribution = std::env::var(ATTRIBUTION_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from);
    if let Some(a) = &attribution {
        crate::attribution::install_root(a).map_err(|e| {
            format!(
                "could not write the attribution hooks under {}: {e}",
                a.display()
            )
        })?;
    }
    let settings = attribution
        .as_ref()
        .map(|a| a.join(crate::attribution::SETTINGS_FILE));
    let hooks = attribution
        .as_ref()
        .map(|a| a.join(crate::attribution::HOOKS_DIR));
    let args = claude_args_with(&rec, first, &session, settings.as_deref());
    eprintln!(
        "jesse-runner: {} round {}: {} {}",
        rec.run_id,
        round.n,
        rec.claude,
        args.join(" ")
    );
    let mut cmd = Command::new(&rec.claude);
    cmd.args(&args)
        .current_dir(&worktree)
        .env_clear()
        .envs(with_attribution(
            agent_env_with(dir, &rec.run_id, |k| std::env::var(k).ok(), file_env),
            hooks.as_deref(),
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", rec.claude))?;
    let pgid = child.id() as i32;
    update_run(dir, |r| r.pgid = Some(pgid))?;
    // On its own thread, so a large prompt can never deadlock against the stream.
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let mut stream = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(STREAM))
        .map_err(|e| format!("could not open stream.jsonl: {e}"))?;
    let mut last_write = 0u64;
    let mut pending = false;
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).split(b'\n') {
            let Ok(mut line) = line else {
                break;
            };
            line.push(b'\n');
            let _ = stream.write_all(&line);
            let _ = stream.flush();
            pending = true;
            let now = now_ms();
            if now.saturating_sub(last_write) >= EVENT_WRITE_MS {
                let at = rfc(now);
                let _ = update_run(dir, |r| r.last_event_at = Some(at.clone()));
                last_write = now;
                pending = false;
            }
        }
    }
    let status = child
        .wait()
        .map_err(|e| format!("could not wait for the agent: {e}"))?;
    let ended = now_ms();
    if pending {
        let at = rfc(ended);
        let _ = update_run(dir, |r| r.last_event_at = Some(at.clone()));
    }
    let exit = ExitInfo {
        round: round.n,
        code: status.code(),
        signal: status.signal(),
        ended_at: rfc(ended),
    };
    write_exit(dir, &exit)?;
    Ok(exit)
}
