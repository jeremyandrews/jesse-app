//! The gateway's unit tests: the child's config, the Codex translation, the posture that must
//! not move, the override rules, and the rule that no gateway code reads a model or a harness.

use super::*;
use crate::testutil::test_config;
use crate::{
    claude_capability_args, codex_mcp_args, render_child_mcp, Capability, ClaudeCode, Codex,
    Harness, McpSet, CODEX_ID, DEFAULT_ALLOWED_TOOLS, MAIN_CHILD_MCP_CONFIG,
};
use serde_json::json;

fn gateway_on() -> crate::Config {
    let mut cfg = test_config();
    cfg.state_dir = Some("/tmp/jesse-gw-unit".into());
    cfg.gateway = GatewaySettings {
        enabled: true,
        ..GatewaySettings::default()
    };
    cfg
}

fn servers(config: &str) -> serde_json::Map<String, Value> {
    serde_json::from_str::<Value>(config).unwrap()["mcpServers"]
        .as_object()
        .unwrap()
        .clone()
}

#[test]
fn the_child_config_names_a_stub_for_every_gateway_server_under_its_own_name() {
    let cfg = gateway_on();
    let rendered = render_child_mcp(&cfg, MAIN_CHILD_MCP_CONFIG.as_str());
    let stub = Path::new("/opt/bin/jesse-gateway-mcp");
    let out = child_mcp_config(
        &cfg.gateway,
        &rendered,
        Path::new("/tmp/jesse-gw-unit"),
        "job-1",
        stub,
    )
    .unwrap();
    let before = servers(&rendered);
    let after = servers(&out);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "every server keeps its name, so every tool keeps its mcp__<server>__<tool> name"
    );
    for (name, spec) in &after {
        if name == "qmd" {
            assert_eq!(
                spec, &before["qmd"],
                "qmd is started by the child, as before"
            );
            continue;
        }
        assert_eq!(spec["type"], "stdio", "{name}");
        assert_eq!(spec["command"], "/opt/bin/jesse-gateway-mcp", "{name}");
        assert_eq!(
            spec["args"],
            json!([
                "--upstream",
                name,
                "--socket",
                format!("/tmp/jesse-gw-unit/gw/job-1/{name}.sock")
            ]),
            "{name}"
        );
        assert!(spec.get("env").is_none() && spec.get("headers").is_none());
    }
    // NO CREDENTIAL NAME survives in what the child is given.
    for name in [
        "HA_MCP_TOKEN",
        "RYBBIT_API_KEY",
        "JESSE_HOUSE_TOKEN",
        "SLACK_MCP_XOXP_TOKEN",
    ] {
        assert!(!out.contains(name), "{name} is in the child's config");
    }
}

#[test]
fn the_url_shape_hands_the_child_a_token_file_not_a_token() {
    let mut cfg = gateway_on();
    cfg.gateway.child_url = Some("http://bridge.jesse:7443".into());
    let out = child_mcp_config(
        &cfg.gateway,
        r#"{"mcpServers":{"slack":{"type":"stdio","command":"npx","args":[]}}}"#,
        Path::new("/s"),
        "j",
        Path::new("/b/jesse-gateway-mcp"),
    )
    .unwrap();
    assert_eq!(
        servers(&out)["slack"]["args"],
        json!([
            "--upstream",
            "slack",
            "--url",
            "http://bridge.jesse:7443",
            "--token-file",
            "/s/gw/j/token"
        ])
    );
}

#[test]
fn codex_gets_the_same_stubs_with_no_environment_and_the_same_grant() {
    let cfg = gateway_on();
    let rendered = render_child_mcp(&cfg, MAIN_CHILD_MCP_CONFIG.as_str());
    let stubbed = child_mcp_config(
        &cfg.gateway,
        &rendered,
        Path::new("/tmp/jesse-gw-unit"),
        "job-1",
        Path::new("/opt/bin/jesse-gateway-mcp"),
    )
    .unwrap();
    let off = codex_mcp_args(CODEX_ID, &rendered, DEFAULT_ALLOWED_TOOLS).unwrap();
    let on = codex_mcp_args(CODEX_ID, &stubbed, DEFAULT_ALLOWED_TOOLS).unwrap();
    for a in &on {
        assert!(
            !a.contains(".env_vars=")
                && !a.contains(".env=")
                && !a.contains("bearer_token_env_var")
                && !a.contains("env_http_headers"),
            "a stub entry carries no environment: {a}"
        );
    }
    // The grant is the same string either way, server by server.
    let grants = |args: &[String]| -> Vec<String> {
        args.iter()
            .filter(|a| a.contains(".enabled_tools="))
            .cloned()
            .collect()
    };
    assert_eq!(grants(&off), grants(&on));
    assert!(on.iter().any(|a| a == r#"mcp_servers.qmd.command="qmd""#));
}

/// THE POSTURE DOES NOT MOVE: `capability_args` for both CLI harnesses are byte-identical with
/// the gateway on, at every shipped row.
#[test]
fn capability_args_are_byte_identical_with_the_gateway_on() {
    let off = test_config();
    let on = gateway_on();
    for h in [&ClaudeCode as &dyn Harness, &Codex as &dyn Harness] {
        for row in h.shipped_rows() {
            assert_eq!(
                h.capability_args(&off, row.capability, row.mcp),
                h.capability_args(&on, row.capability, row.mcp),
                "{}",
                h.id()
            );
        }
    }
    assert_eq!(
        claude_capability_args(&off, Capability::Write, McpSet::None),
        claude_capability_args(&on, Capability::Write, McpSet::None)
    );
}

#[test]
fn basic_admits_nothing_and_qmd_is_never_admitted() {
    let cfg = gateway_on();
    let config = MAIN_CHILD_MCP_CONFIG.as_str();
    assert!(admitted_servers(
        &cfg.gateway,
        config,
        Capability::Basic,
        DEFAULT_ALLOWED_TOOLS
    )
    .unwrap()
    .is_empty());
    let w = admitted_servers(
        &cfg.gateway,
        config,
        Capability::Write,
        DEFAULT_ALLOWED_TOOLS,
    )
    .unwrap();
    assert!(!w.iter().any(|a| a.server.name == "qmd"));
    let slack = w.iter().find(|a| a.server.name == "slack").unwrap();
    assert_eq!(slack.tools.len(), 6);
    let r = admitted_servers(
        &cfg.gateway,
        config,
        Capability::Read,
        crate::READ_ALLOWED_TOOLS,
    )
    .unwrap();
    assert!(
        r.iter().all(|a| a.tools.is_empty()),
        "the read allowlist grants no gateway tool"
    );
}

#[test]
fn an_override_can_move_a_row_to_http_with_any_credential_shape() {
    let mut s = GatewaySettings {
        enabled: true,
        ..GatewaySettings::default()
    };
    s.overrides.insert(
        "slack".into(),
        McpOverrideToml {
            transport: Some("http".into()),
            url: Some("http://slack.mcp:8080/mcp".into()),
            bearer_env: Some("SLACK_ADAPTER_TOKEN".into()),
            ..McpOverrideToml::default()
        },
    );
    s.overrides.insert(
        "qmd".into(),
        McpOverrideToml {
            transport: Some("http".into()),
            placement: Some("gateway".into()),
            url: Some("http://qmd:8181/mcp".into()),
            ..McpOverrideToml::default()
        },
    );
    s.overrides.insert(
        "tag1".into(),
        McpOverrideToml {
            header_env: [("x-api-key".to_string(), "TAG1_KEY".to_string())].into(),
            ..McpOverrideToml::default()
        },
    );
    s.client_certs.insert(
        "mac".into(),
        ClientCertToml {
            cert_file: "/c".into(),
            key_file: "/k".into(),
            ca_file: "/ca".into(),
        },
    );
    s.overrides.insert(
        "imcp".into(),
        McpOverrideToml {
            transport: Some("http".into()),
            url: Some("https://mac.svc:8443/mcp/imcp".into()),
            client_cert: Some("mac".into()),
            ..McpOverrideToml::default()
        },
    );
    assert!(s.problems().is_empty(), "{:?}", s.problems());
    let slack = resolve_row("slack", &s).unwrap();
    assert_eq!(slack.transport, Transport::Http);
    assert_eq!(
        slack.auth,
        ResolvedAuth::BearerEnv("SLACK_ADAPTER_TOKEN".into())
    );
    assert_eq!(
        resolve_row("qmd", &s).unwrap().placement,
        Placement::Gateway
    );
    assert_eq!(
        resolve_row("tag1", &s).unwrap().auth,
        ResolvedAuth::HeadersEnv(vec![("x-api-key".into(), "TAG1_KEY".into())])
    );
    assert_eq!(
        resolve_row("imcp", &s).unwrap().auth,
        ResolvedAuth::ClientCert("mac".into())
    );
}

#[test]
fn a_bad_override_is_named_at_startup() {
    let bad = |o: McpOverrideToml, name: &str| {
        let mut s = GatewaySettings {
            enabled: true,
            ..GatewaySettings::default()
        };
        s.overrides.insert(name.into(), o);
        s.problems()
    };
    assert!(!bad(
        McpOverrideToml {
            transport: Some("sse".into()),
            ..Default::default()
        },
        "slack"
    )
    .is_empty());
    assert!(
        !bad(
            McpOverrideToml {
                placement: Some("child".into()),
                ..Default::default()
            },
            "slack"
        )
        .is_empty(),
        "only qmd may be started by the child"
    );
    assert!(!bad(
        McpOverrideToml {
            client_cert: Some("nope".into()),
            ..Default::default()
        },
        "tag1"
    )
    .is_empty());
    assert!(!bad(McpOverrideToml::default(), "no-such-server").is_empty());
    assert!(!bad(
        McpOverrideToml {
            bearer_env: Some("A".into()),
            client_cert: Some("b".into()),
            ..Default::default()
        },
        "tag1"
    )
    .is_empty());
}

/// NOTHING A CHILD INHERITS NAMES AN MCP CREDENTIAL: no row's environment column or auth
/// variable is on the passthrough list.
#[test]
fn the_child_passthrough_names_no_mcp_credential() {
    for r in crate::mcpservers::MCP_SERVERS {
        for n in r.env {
            assert!(!CHILD_ENV_PASSTHROUGH.contains(n), "{}: {n}", r.name);
        }
        let auth_vars: Vec<&str> = match r.auth {
            crate::mcpservers::HttpAuthSpec::Bearer(v) => vec![v],
            crate::mcpservers::HttpAuthSpec::Headers(p) => p.iter().map(|(_, v)| *v).collect(),
            _ => vec![],
        };
        for v in auth_vars {
            assert!(!CHILD_ENV_PASSTHROUGH.contains(&v), "{}: {v}", r.name);
        }
    }
}

#[test]
fn the_gateway_is_off_by_default_and_then_changes_nothing() {
    let cfg = test_config();
    assert!(!cfg.gateway.enabled);
    assert!(global(&cfg).is_none());
    let rendered = render_child_mcp(&cfg, MAIN_CHILD_MCP_CONFIG.as_str());
    assert_eq!(child_config_for_turn(&cfg, &rendered, "j"), rendered);
    assert!(validate_gateway(&cfg).is_empty());
}

#[test]
fn a_missing_stub_is_fatal_when_enabled() {
    let mut cfg = gateway_on();
    cfg.gateway.stub = Some("/nonexistent/jesse-gateway-mcp".into());
    let errs = validate_gateway(&cfg);
    assert!(
        errs.iter().any(|e| e.message.contains(STUB_BIN)),
        "{:?}",
        errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn a_job_id_that_is_not_an_identifier_cannot_open_a_turn() {
    for bad in ["", "../x", "a/b", ".hidden", "with space"] {
        assert!(!usable_job_id(bad), "{bad:?}");
    }
    assert!(usable_job_id("4f1c2d3e-aa00-4b11-9c22-0123456789ab"));
    assert!(usable_job_id("title-0123"));
}

/// NO GATEWAY CODE PATH READS THE MODEL OR THE HARNESS. Every source file of the module is
/// scanned, comments and tests aside, for the types and fields a model or a harness is reached
/// through. The one per-harness value a server needs arrives as an opaque string.
#[test]
fn no_gateway_source_reads_the_model_or_the_harness() {
    let sources = [
        ("mod.rs", include_str!("mod.rs")),
        ("session.rs", include_str!("session.rs")),
        ("listen.rs", include_str!("listen.rs")),
        ("direct.rs", include_str!("direct.rs")),
        ("token.rs", include_str!("token.rs")),
    ];
    let forbidden = [
        "ActiveModel",
        "dyn Harness",
        "SpawnedHarness",
        "ClaudeCode",
        "Codex",
        "Direct",
        "CLAUDE_CODE_ID",
        "CODEX_ID",
        "DIRECT_ID",
        ".active",
        ".harnesses",
        "model_registry",
        "harness.id",
        ".model",
    ];
    for (file, src) in sources {
        let code = src.split("#[cfg(test)]").next().unwrap();
        for (i, line) in code.lines().enumerate() {
            let l = line.trim_start();
            if l.starts_with("//") {
                continue;
            }
            let l = l.split("//").next().unwrap();
            for f in forbidden {
                assert!(
                    !l.contains(f),
                    "{file}:{}: `{f}` in gateway code: {line}",
                    i + 1
                );
            }
        }
    }
}

// ---- `in_process` rows reach a spawned child (0.186.0) ------------------------------------

/// An in-process server that answers nothing, standing in for `devjob`.
struct Silent;

impl InProcessServer for Silent {
    fn tools(&self) -> Vec<Value> {
        crate::devjob::tool_descriptors()
    }
    fn call<'a>(
        &'a self,
        _tool: &'a str,
        _arguments: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send + 'a>>
    {
        Box::pin(async { Ok(json!({ "content": [] })) })
    }
}

fn with_devjob(mut cfg: crate::Config) -> crate::Config {
    cfg.gateway
        .in_process
        .0
        .insert(crate::devjob::DEVJOB_SERVER.to_string(), Arc::new(Silent));
    cfg
}

/// THE GAP 0.177.0 LEFT: an `in_process` row named in a set rendered as nothing, so no spawned
/// child ever saw it and `admitted_servers` never admitted it. With the gateway on and the
/// server registered, the shipped main set now carries it, admitted, and the child is handed the
/// stub for it under its own name, on both harnesses.
#[test]
fn an_in_process_row_in_a_set_reaches_a_spawned_child_through_the_stub_with_the_gateway_on() {
    let cfg = with_devjob(gateway_on());
    let rendered = render_child_mcp(&cfg, MAIN_CHILD_MCP_CONFIG.as_str());
    let s = servers(&rendered);
    assert!(
        rendered.ends_with(r#","devjob":{"type":"in_process"}}}"#),
        "in the set's order (last): {rendered}"
    );
    assert_eq!(
        s["devjob"],
        json!({ "type": crate::mcpservers::IN_PROCESS_ENTRY_TYPE })
    );
    // Byte for byte: the gateway-off string with one entry added at the set's place for it.
    assert_eq!(
        rendered.replace(r#","devjob":{"type":"in_process"}"#, ""),
        render_child_mcp(&gateway_on(), MAIN_CHILD_MCP_CONFIG.as_str())
    );

    let admitted = admitted_servers(
        &cfg.gateway,
        &rendered,
        Capability::Write,
        DEFAULT_ALLOWED_TOOLS,
    )
    .unwrap();
    let devjob = admitted
        .iter()
        .find(|a| a.server.name == "devjob")
        .expect("admitted");
    assert_eq!(
        devjob.server.transport,
        crate::mcpservers::Transport::InProcess
    );
    let mut tools = devjob.tools.clone();
    tools.sort();
    assert_eq!(tools, ["devjob_list", "devjob_start", "devjob_status"]);
    // A read turn's grant names none of them: the server is listed with no tools.
    let read = admitted_servers(
        &cfg.gateway,
        &rendered,
        Capability::Read,
        crate::READ_ALLOWED_TOOLS,
    )
    .unwrap();
    assert!(read
        .iter()
        .find(|a| a.server.name == "devjob")
        .is_some_and(|a| a.tools.is_empty()));

    let stub = Path::new("/opt/bin/jesse-gateway-mcp");
    let child = child_mcp_config(
        &cfg.gateway,
        &rendered,
        Path::new("/tmp/jesse-gw-unit"),
        "job-1",
        stub,
    )
    .unwrap();
    let c = servers(&child);
    assert_eq!(
        c["devjob"],
        json!({
            "type": "stdio",
            "command": "/opt/bin/jesse-gateway-mcp",
            "args": ["--upstream", "devjob", "--socket", "/tmp/jesse-gw-unit/gw/job-1/devjob.sock"],
        })
    );
    // Codex takes the stub too, with no environment and exactly the three grants.
    let codex = codex_mcp_args(CODEX_ID, &child, DEFAULT_ALLOWED_TOOLS).unwrap();
    assert!(codex
        .iter()
        .any(|a| a == r#"mcp_servers.devjob.command="/opt/bin/jesse-gateway-mcp""#));
    assert!(codex.iter().any(|a| a
        == r#"mcp_servers.devjob.enabled_tools=["devjob_start", "devjob_status", "devjob_list"]"#));
    assert!(!codex
        .iter()
        .any(|a| a.starts_with("mcp_servers.devjob.env")));
}

/// …AND WITH THE GATEWAY OFF IT IS ABSENT, byte for byte, registered or not: every gateway-off
/// config, argv fixture and containment record is what it was before. With the gateway on but
/// the server not registered (`[devjob]` off) it is absent too: no child is handed a stub for a
/// server nothing in the bridge answers.
#[test]
fn an_in_process_row_is_absent_with_the_gateway_off_or_unregistered() {
    let golden = crate::mcpservers::golden_main_config();
    let off = with_devjob(test_config());
    let rendered = render_child_mcp(&off, MAIN_CHILD_MCP_CONFIG.as_str());
    assert!(!servers(&rendered).contains_key("devjob"));
    assert_eq!(child_config_for_turn(&off, &rendered, "j"), rendered);
    assert_eq!(MAIN_CHILD_MCP_CONFIG.as_str(), golden);

    let unregistered = gateway_on();
    let rendered = render_child_mcp(&unregistered, MAIN_CHILD_MCP_CONFIG.as_str());
    assert!(!servers(&rendered).contains_key("devjob"));
    assert!(admitted_servers(
        &unregistered.gateway,
        &rendered,
        Capability::Write,
        DEFAULT_ALLOWED_TOOLS
    )
    .unwrap()
    .iter()
    .all(|a| a.server.name != "devjob"));

    // A set that names no `in_process` row is never re-rendered.
    for set in McpSet::ALL {
        let on = with_devjob(gateway_on());
        if set.config() != MAIN_CHILD_MCP_CONFIG.as_str() {
            assert_eq!(
                crate::gateway::render_in_process_mcp(&on, set.config()),
                set.config(),
                "{}",
                set.label()
            );
        }
    }
}

/// THE GOLDEN MCP STRINGS: the shipped main config with the gateway off is the twenty-six-server
/// golden; with it on and `devjob` registered it is that golden with exactly one entry appended.
#[test]
fn the_main_config_golden_with_the_gateway_off_and_on() {
    let golden = crate::mcpservers::golden_main_config();
    assert_eq!(
        crate::gateway::render_in_process_mcp(&test_config(), MAIN_CHILD_MCP_CONFIG.as_str()),
        golden
    );
    let on = crate::gateway::render_in_process_mcp(
        &with_devjob(gateway_on()),
        MAIN_CHILD_MCP_CONFIG.as_str(),
    );
    let want = format!(
        "{},\"devjob\":{{\"type\":\"in_process\"}}}}}}",
        golden.strip_suffix("}}").unwrap()
    );
    assert_eq!(on, want);
}
