//! Crate-internal test helpers shared by the per-module `#[cfg(test)]`
//! suites. Not compiled into the library proper.
#![cfg(test)]
use crate::*;

mod executable;
pub(crate) use executable::write_executable;

pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());
pub(crate) fn test_config() -> Config {
    Config {
        offload_order: Vec::new(),
        token: "test-token".to_string(),
        // Captured HOME for session-path lookups; tests that exercise session
        // paths override `home`/`vault` explicitly (no global-env mutation).
        home: std::env::var("HOME").unwrap_or_default(),
        // Any existing directory works — most tests never reach run_claude.
        vault: std::env::temp_dir().to_string_lossy().into_owned(),
        bind: "127.0.0.1".to_string(),
        port: 8765,
        claude_bin: "claude".to_string(),
        codex_bin: "codex".to_string(),
        timeout_secs: 1800,
        partial_blocks: DEFAULT_PARTIAL_BLOCKS,
        partial_bytes: DEFAULT_PARTIAL_BYTES,
        allowed_tools: DEFAULT_ALLOWED_TOOLS.to_string(),
        disallowed_tools: DEFAULT_DISALLOWED_TOOLS.to_string(),
        concurrency: ConcurrencySettings::uniform(2, &["opus"]),
        max_queued: DEFAULT_MAX_QUEUED,
        rate_per_min: 30,
        job_ttl_secs: 600,
        retrieval_grace_secs: 600,
        session_ttl_days: DEFAULT_SESSION_TTL_DAYS,
        // No on-disk persistence in tests by default — keeps cargo test off
        // the real $HOME. The persistence tests build a store with a temp dir.
        state_dir: None,
        // The shared instruction bundle is OFF in the fixture, which is the shipped default:
        // a test that wants it on points this at a scratch root of its own.
        rules_root: None,
        max_attachments: DEFAULT_MAX_ATTACHMENTS,
        max_attachment_bytes: DEFAULT_MAX_ATTACHMENT_BYTES,
        max_attachments_total_bytes: DEFAULT_MAX_ATTACHMENTS_TOTAL_BYTES,
        document_text_chars: DEFAULT_DOCUMENT_TEXT_CHARS,
        document_text_total_chars: DEFAULT_DOCUMENT_TEXT_TOTAL_CHARS,
        // Shipped artifact caps in the fixture: the cap tests override the one they
        // exercise, so a default that differs from production would only hide drift.
        max_artifacts: DEFAULT_MAX_ARTIFACTS,
        max_artifact_bytes: DEFAULT_MAX_ARTIFACT_BYTES,
        max_artifacts_total_bytes: DEFAULT_MAX_ARTIFACTS_TOTAL_BYTES,
        artifact_ttl_days: DEFAULT_ARTIFACT_TTL_DAYS,
        artifact_store_max_bytes: DEFAULT_ARTIFACT_STORE_MAX_BYTES,
        scratch_dir: None,
        // No title-backend override by default — tests that need one set it
        // explicitly, mirroring an unconfigured (ambient-backend) deploy.
        // No diet-extract backend override by default — the pipeline is dormant
        // (kill switch), so tests exercise today's hosted path unless they set it.
        // Probation on by default, matching from_env's default.
        diet_micro_complete: true,
        // No vault-QA backend override by default — the route is inert (kill
        // switch), so tests exercise today's hosted Ask path unless they set it.
        vaultqa_mcp_config: None,
        // No brief-child MCP override in the fixture — the shipped default, and the
        // only value that boots at all (setting it is a startup refusal).
        today_brief_mcp_config: None,
        // No own-identity table in the fixture: the tests that exercise the message-citation
        // check build their own, and a default one here would make every other test's briefs
        // silently able to close on message evidence.
        own_identities: std::collections::HashMap::new(),
        // Message-evidence auto-close OFF, which is the shipped default. The tests that cover
        // it set it explicitly, in both directions.
        today_brief_message_closes: false,
        // No main-path MCP override in the fixture — the main turn falls back to the
        // qmd-only inline const, matching from_env's default.
        main_mcp_config: None,
        // Badge OFF in the fixture so a turn's stored reply is byte-for-byte the
        // model text — the many exact-`response` assertions predate the badge and
        // must not have to account for it. Badge behavior is covered by dedicated
        // tests that enable it explicitly (the shipped `from_env` default is ON).
        model_badge: false,
        // No metrics log and emergency OFF in the fixture — both dormant, matching
        // an unconfigured deploy. The both-unset safety property depends on this
        // default: every existing path is byte-for-byte unchanged. Tests that
        // exercise metrics/emergency set these explicitly.
        metrics_log: None,
        emergency_local: false,
        // Context carry OFF in the fixture (like the badge/emergency defaults): the
        // many exact-`response`/`session_id` assertions predate it and must be
        // byte-for-byte unaffected. Carry behavior is covered by dedicated tests that
        // enable it explicitly (the shipped `from_env` default is ON).
        context_carry: false,
        // Shadow comparison DISARMED in the fixture (kill switch): no backend triple,
        // so no ask turn is ever mirrored and every path is byte-for-byte today's.
        // Tests that exercise shadow set `shadow_backend`/`shadow_log` explicitly.
        shadow_backend: None,
        shadow_sample_pct: 100,
        shadow_log: std::env::temp_dir()
            .join("jesse-shadow-test.jsonl")
            .to_string_lossy()
            .into_owned(),
        shadow_timeout_secs: 120,
        // No scheduled jobs in the fixture: the tick task is never started and the
        // scheduler is inert, so every existing test is byte-for-byte unaffected. Tests
        // that exercise the scheduler build their own `Schedule`.
        schedule: Arc::new(Schedule::default()),
        // Generic default persona (owner "the user"): the fresh-clone identity, so
        // every existing prompt/gate assertion is byte-for-byte the generic form.
        // Tests that exercise a named owner or extra diet vocab set this explicitly.
        persona: Persona::default(),
        // Opus-only registry in the fixture: the single always-available ambient
        // default, so a turn that doesn't opt into the switch is byte-for-byte today's
        // behavior. Tests that exercise a hosted/local model build their own registry.
        model_registry: ModelRegistry::opus_only(),
        vision: VisionConfig::default(),
        // The shipped harness registry: `claude-code` only, exactly as `from_env` builds
        // it. A test that needs a second harness constructs its own `HarnessRegistry`.
        harnesses: Arc::new(HarnessRegistry::claude_code_only()),
        // No `[direct]` table in the fixture: the direct harness is not registered here, so
        // every setting is inert and every existing turn assertion is unaffected.
        direct: DirectSettings::default(),
        // No Fireworks account and the per-scope TTLs: the quota store is only ever read by
        // tests that install their own fetcher.
        quota: QuotaSettings::default(),
        // No `[house]` table in the fixture: the shipped default, and the server is not
        // registered. The house tests set it explicitly.
        house: HouseConfig::Absent,
        gateway: crate::gateway::GatewaySettings::default(),
        devjob: crate::devjob::DevJobSettings::default(),
        hostrun: crate::hostrun::HostRunSettings::default(),
        // No `[healthdb]` table: the shipped default. No pool, no task, `not_configured`.
        healthdb: HealthDbConfig::Absent,
        // Speech transcription OFF in the fixture: no intake or models directory is ever
        // touched. The speech tests build their own service over a scratch root and fakes.
        speech: crate::speech::SpeechConfig::disabled(),
    }
}
pub(crate) fn test_state() -> AppState {
    AppState::new(test_config())
}
pub(crate) fn temp_jobs_dir() -> PathBuf {
    std::env::temp_dir().join(format!("jesse-jobs-{}", random_hex()))
}

/// `write_executable`'s own tests. They live here rather than in `testutil/executable.rs`
/// because that file is also compiled into every integration test target that writes a fake
/// binary, and a test inside it would run once per target.
#[cfg(test)]
mod write_executable_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jesse-write-exec-{tag}-{}", random_hex()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The shape that failed in CI, many times over: a script written and spawned at once by
    /// each of several threads, every one of which is also forking children. Writing in this
    /// process lost this race on Linux with ETXTBSY; every spawn here must succeed.
    #[test]
    fn a_script_spawned_the_moment_it_is_written_never_finds_its_file_busy() {
        let dir = scratch("race");
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for i in 0..40 {
                        let p = dir.join(format!("fake-{t}-{i}"));
                        write_executable(&p, format!("#!/bin/sh\nexit {}\n", (t + i) % 2));
                        let status = std::process::Command::new(&p)
                            .status()
                            .unwrap_or_else(|e| panic!("spawn {}: {e:?}", p.display()));
                        assert_eq!(status.code(), Some((t + i) % 2), "{}", p.display());
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("a spawning thread panicked");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// It replaces a script that is already there (and may be running), leaves it 0755, and
    /// leaves nothing beside it: no staging body, no half placed copy.
    #[test]
    fn an_existing_script_is_replaced_whole_and_nothing_is_left_beside_it() {
        let dir = scratch("replace");
        let p = dir.join("tool");
        write_executable(&p, "#!/bin/sh\necho one\n");
        let running = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("'{}'; sleep 1", p.display()))
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        assert_eq!(write_executable(&p, "#!/bin/sh\necho two\n"), p);
        let out = std::process::Command::new(&p).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "two\n");
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["tool".to_string()]);
        let mut running = running;
        let _ = running.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
