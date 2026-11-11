//! LIVE: real model calls through the model proxy, one per route the Studio uses. Ignored:
//! it spends real calls (the subscription, Fireworks, the local gateway, the ChatGPT login)
//! and needs the bridge's launchd plist for the registry's keys.
//!
//! ```text
//! cargo test --test proxy_live -- --ignored --nocapture --test-threads 1
//! ```
//!
//! For each route it opens a gateway turn whose token is signed for that route, runs the real
//! CLI pointed at the proxy with the turn token as its only credential, and compares the
//! provider's own usage (the CLI's JSON result) with the `model_call` lines the proxy wrote.
//! Nothing it prints is a credential: the token is a turn token, and the proxy's credentials
//! never leave it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jesse_bridge::gateway::{self, ProxyClaims, TurnSpec};
use jesse_bridge::proxy::{self, Egress};

fn plist_env() {
    let home = std::env::var("HOME").unwrap();
    let la = Path::new(&home).join("Library/LaunchAgents");
    let plist = jesse_bridge::plistenv::find_bridge_plist(&la, None).expect("the bridge plist");
    jesse_bridge::plistenv::load_plist_env(plist.to_str().unwrap()).expect("plist env");
}

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("k04-proxy-live-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Live {
    cfg: jesse_bridge::Config,
    gw: Arc<gateway::Gateway>,
    base: String,
}

async fn start() -> Live {
    plist_env();
    let state = scratch();
    // The deployed TOML, READ ONLY, so the registry has its `[[models]]` (qwen-local); the
    // state directory is this run's scratch one.
    let home = std::env::var("HOME").unwrap();
    std::env::set_var(
        "JESSE_CONFIG",
        format!("{home}/.jesse-bridge/jesse.local.toml"),
    );
    std::env::set_var("JESSE_STATE_DIR", &state);
    let mut cfg = jesse_bridge::Config::from_env();
    cfg.state_dir = Some(state.display().to_string());
    cfg.gateway.enabled = true;
    cfg.proxy = proxy::ProxySettings::from_toml(
        Some(proxy::ProxyToml {
            model: true,
            model_listen: Some("127.0.0.1:18867".into()),
            ..Default::default()
        }),
        None,
        None,
    );
    let gw = gateway::global(&cfg).expect("the gateway");
    let bound = proxy::wiring::start_model_proxy(&cfg, gw.clone())
        .await
        .expect("the model proxy starts")
        .expect("it is on");
    Live {
        base: format!("http://{bound}"),
        cfg,
        gw,
    }
}

async fn open(live: &Live, job: &str, route: &str, model: &str) -> gateway::TurnGuard {
    live.gw
        .open_turn(TurnSpec {
            proxy: ProxyClaims {
                tier: Some(Egress::Closed),
                route: route.into(),
                metered: model.into(),
                filter_all_egress: true,
                required: true,
            },
            job_id: job.into(),
            capability: jesse_bridge::Capability::Basic,
            servers: Vec::new(),
            harness_env_value: None,
            deadline: Duration::from_secs(300),
            bind_sockets: false,
            env_overlay: BTreeMap::new(),
        })
        .await
        .expect("a turn opens")
}

/// The proxy's `model_call` lines for `job`: (lines, input, output, cache write, cache read,
/// cost micros).
fn ledger_totals(live: &Live, job: &str) -> (usize, u64, u64, u64, u64, u64) {
    let dir = live.gw.ledger().expect("a ledger").dir().to_path_buf();
    let now = jesse_bridge::activity::now_ms();
    let (lines, _) =
        jesse_bridge::activity_query::read_range(&dir, now - 3_600_000, now + 60_000, 10_000);
    let mine: Vec<_> = lines
        .iter()
        .filter(|l| l["kind"] == "model_call" && l["job_id"] == job)
        .collect();
    let sum = |k: &str| mine.iter().map(|l| l[k].as_u64().unwrap_or(0)).sum::<u64>();
    for l in &mine {
        eprintln!("  ledger: {l}");
    }
    (
        mine.len(),
        sum("input_tokens"),
        sum("output_tokens"),
        sum("cache_write_tokens"),
        sum("cache_read_tokens"),
        sum("cost_micros"),
    )
}

/// The CLI's own usage from `--output-format json`: summed over every model it reports.
fn cli_usage(stdout: &str) -> (u64, u64, u64, u64) {
    let v: serde_json::Value = serde_json::from_str(
        stdout
            .lines()
            .rev()
            .find(|l| l.trim_start().starts_with('{'))
            .unwrap_or("{}"),
    )
    .unwrap_or_default();
    let mut t = (0, 0, 0, 0);
    if let Some(m) = v["modelUsage"].as_object() {
        for u in m.values() {
            t.0 += u["inputTokens"].as_u64().unwrap_or(0);
            t.1 += u["outputTokens"].as_u64().unwrap_or(0);
            t.2 += u["cacheCreationInputTokens"].as_u64().unwrap_or(0);
            t.3 += u["cacheReadInputTokens"].as_u64().unwrap_or(0);
        }
    }
    eprintln!(
        "  cli result: is_error={} result={:?}",
        v["is_error"],
        v["result"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect::<String>()
    );
    t
}

async fn run_claude(live: &Live, route: &str, token: &str, model: Option<&str>) -> String {
    let mut cmd = tokio::process::Command::new(&live.cfg.claude_bin);
    cmd.env_clear();
    for k in ["PATH", "HOME", "TMPDIR", "LANG", "USER", "LOGNAME", "SHELL"] {
        if let Ok(v) = std::env::var(k) {
            cmd.env(k, v);
        }
    }
    cmd.env(
        "ANTHROPIC_BASE_URL",
        proxy::wiring::proxied_base_url(&live.cfg, route, None)
            .replace(&live.cfg.proxy.model_child_url, &live.base),
    )
    .env("ANTHROPIC_AUTH_TOKEN", token)
    .env("CLAUDE_CODE_MAX_RETRIES", "1");
    if let Some(m) = model {
        for k in [
            "ANTHROPIC_MODEL",
            "CLAUDE_CODE_SUBAGENT_MODEL",
            "ANTHROPIC_SMALL_FAST_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        ] {
            cmd.env(k, m);
        }
    }
    cmd.args([
        "-p",
        "Reply with exactly the word: proxied",
        "--output-format",
        "json",
        "--max-turns",
        "1",
        "--tools",
        "",
    ])
    .current_dir(std::env::temp_dir());
    let out = tokio::time::timeout(Duration::from_secs(240), cmd.output())
        .await
        .expect("the CLI finishes")
        .expect("the CLI runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn registry_model(cfg: &jesse_bridge::Config, id: &str) -> Option<(String, String)> {
    cfg.model_registry
        .models
        .iter()
        .find(|m| m.id == id)
        .and_then(|m| m.backend.as_ref())
        .map(|(u, _, model)| (u.clone(), model.clone()))
}

fn report(name: &str, live: &Live, job: &str, cli: (u64, u64, u64, u64)) -> bool {
    let (n, i, o, cw, cr, cost) = ledger_totals(live, job);
    let ok = n > 0 && (i, o, cw, cr) == cli;
    eprintln!(
        "{name}: {n} model_call line(s); ledger in/out/cw/cr = {i}/{o}/{cw}/{cr} \
         (${:.6}); CLI reports {}/{}/{}/{} -> {}",
        cost as f64 / 1e6,
        cli.0,
        cli.1,
        cli.2,
        cli.3,
        if ok { "MATCH" } else { "DIFFERENT" }
    );
    ok
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_model_proxy_routes() {
    let live = start().await;
    let mut results = Vec::new();

    // 1. The subscription: Opus through `anthropic-subscription`.
    let opus = std::env::var("JESSE_MODEL_OPUS_MODEL").unwrap_or("claude-opus-5-5".into());
    let g = open(&live, "live-sub-1", "anthropic-subscription", "opus").await;
    let out = run_claude(&live, "anthropic-subscription", g.token(), Some(&opus)).await;
    results.push((
        "anthropic-subscription (opus)",
        report(
            "anthropic-subscription",
            &live,
            "live-sub-1",
            cli_usage(&out),
        ),
    ));
    drop(g);

    // 2. Fireworks: GLM through `fireworks`.
    if let Some((_, slug)) = registry_model(&live.cfg, "glm") {
        let g = open(&live, "live-fw-1", "fireworks", "glm").await;
        let out = run_claude(&live, "fireworks", g.token(), Some(&slug)).await;
        results.push((
            "fireworks (glm)",
            report("fireworks", &live, "live-fw-1", cli_usage(&out)),
        ));
        drop(g);
    }

    // 3. The local gateway: qwen-local through `local`.
    if let Some((_, slug)) = registry_model(&live.cfg, "qwen-local") {
        let g = open(&live, "live-local-1", "local", "qwen-local").await;
        let out = run_claude(&live, "local", g.token(), Some(&slug)).await;
        results.push((
            "local (qwen-local)",
            report("local", &live, "live-local-1", cli_usage(&out)),
        ));
        drop(g);
    }

    // 4. A wrong-route token is refused by the proxy itself.
    let g = open(&live, "live-scope-1", "local", "qwen-local").await;
    let r = reqwest::Client::new()
        .post(format!("{}/model/fireworks/v1/messages", live.base))
        .bearer_auth(g.token())
        .body("{}")
        .send()
        .await
        .unwrap();
    eprintln!("wrong-route token on fireworks: HTTP {}", r.status());
    results.push(("route scoping", r.status() == 401));
    drop(g);

    for (name, ok) in &results {
        eprintln!("{:<34} {}", name, if *ok { "ok" } else { "NOT OK" });
    }
}

/// Codex on the ChatGPT login through `openai-chatgpt`: no `auth.json` in the child's home,
/// the turn token as its provider key.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_codex_through_the_model_proxy() {
    let live = start().await;
    let model = live
        .cfg
        .model_registry
        .models
        .iter()
        .find(|m| m.harness == "codex" && !matches!(m.kind, jesse_bridge::ModelKind::OpenAi))
        .map(|m| m.id.clone())
        .expect("a codex model");
    let g = open(&live, "live-codex-1", "openai-chatgpt", &model).await;
    let home = scratch().join("codex-home");
    std::fs::create_dir_all(&home).unwrap();
    let base = format!("{}/model/openai-chatgpt", live.base);
    let mut cmd = tokio::process::Command::new(&live.cfg.codex_bin);
    cmd.env_clear();
    for k in ["PATH", "HOME", "TMPDIR", "LANG", "USER", "LOGNAME", "SHELL"] {
        if let Ok(v) = std::env::var(k) {
            cmd.env(k, v);
        }
    }
    cmd.env("CODEX_HOME", &home)
        .env(jesse_bridge::CODEX_PROVIDER_KEY_ENV, g.token())
        .arg("exec")
        .arg("--skip-git-repo-check");
    for a in jesse_bridge::codex_provider_args_for(&base, None) {
        cmd.arg(a);
    }
    cmd.arg("-c")
        .arg("model_providers.jesse.request_max_retries=0")
        .arg("Reply with exactly the word: proxied")
        .current_dir(std::env::temp_dir());
    let out = tokio::time::timeout(Duration::from_secs(240), cmd.output())
        .await
        .expect("codex finishes")
        .expect("codex runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    eprintln!(
        "codex exit {:?}; last line: {:?}",
        out.status.code(),
        stdout.lines().last().unwrap_or("")
    );
    assert!(
        !home.join("auth.json").exists(),
        "the child's home holds no login"
    );
    let (n, i, o, cw, cr, cost) = ledger_totals(&live, "live-codex-1");
    eprintln!(
        "openai-chatgpt: {n} model_call line(s); in/out/cw/cr = {i}/{o}/{cw}/{cr} (${:.6})",
        cost as f64 / 1e6
    );
}
