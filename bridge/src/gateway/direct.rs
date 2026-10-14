//! **THE GATEWAY AS AN IN-PROCESS TOOL SET** — what the `direct` harness gets with the gateway
//! on, beside its vault tools.
//!
//! The same admitted servers a spawned child would be given for a main turn, served by the
//! same [`TurnSession`] with no socket in between: every `mcp__<server>__<tool>` name a Claude
//! Code turn would see at this capability, and no other. Two filters, both of which must pass:
//!
//!   1. **THE ALLOWLIST-DERIVED GRANT** — the turn's capability resolves to the same allowlist
//!      a Claude Code main turn gets ([`crate::row_allowed_tools`]), so a tool is never exposed
//!      at a level above the one the CLI grants it at. At `read` that grants no gateway tool
//!      (only `qmd`, which `direct` reaches behind its own store, never as `mcp__qmd__*`).
//!   2. **`Level::permits` ON THE TOOL'S CLASS** from the table
//!      ([`crate::mcpservers::McpServerRow::class_of`]), so a tool classed `write` is withheld
//!      below `write` even if an allowlist were ever to grant it.
//!
//! The set owns the turn's [`TurnGuard`]: dropping the set at turn end closes the gateway
//! turn, killing what it started.

use std::sync::Arc;
use std::time::Duration;

use jesse_agent::mcp::{exposed_class, tool_name, tool_result_from_mcp};
use jesse_agent::provider::{BoxFuture, ToolSpec};
use jesse_agent::tools::{
    ActionClass, Level, StaticToolSet, Tool, ToolContext, ToolError, ToolResult, ToolSet,
    ToolSetBuilder,
};
use jesse_agent::Scope;
use serde_json::Value;

use super::{admitted_servers, global, TurnGuard, TurnSession, TurnSpec};

/// The gateway's tools for one `direct` turn.
pub struct GatewayToolSet {
    inner: StaticToolSet,
    /// Held so the turn stays open exactly as long as the tools that call into it.
    _guard: TurnGuard,
    report: Vec<String>,
}

impl GatewayToolSet {
    /// One line per admitted server: how many tools it exposed. Operator log only.
    pub fn report(&self) -> &[String] {
        &self.report
    }
}

impl ToolSet for GatewayToolSet {
    fn manifest(&self) -> Vec<ToolSpec> {
        self.inner.manifest()
    }

    fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.inner.get(name)
    }

    fn max_level(&self) -> Level {
        self.inner.max_level()
    }
}

struct GatewayTool {
    name: String,
    remote: String,
    server: String,
    description: String,
    schema: Value,
    class: ActionClass,
    session: Arc<TurnSession>,
}

impl Tool for GatewayTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    fn action_class(&self) -> ActionClass {
        self.class
    }

    fn call<'a>(
        &'a self,
        _scope: &'a Scope,
        args: Value,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if !args.is_null() && !args.is_object() {
                return Err(ToolError::InvalidArgs(
                    "arguments must be a JSON object".into(),
                ));
            }
            let args = if args.is_null() {
                serde_json::json!({})
            } else {
                args
            };
            tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => Err(ToolError::Failed(
                    "not run: the turn was cancelled".into(),
                )),
                out = self.session.call(&self.server, &self.remote, args) => match out {
                    Ok(result) => tool_result_from_mcp(&result),
                    // A refusal here is the gateway's grant check, which the manifest already
                    // applied, so it cannot happen for a tool this set exposes.
                    Err((_, message)) => Err(ToolError::Failed(message)),
                },
            }
        })
    }
}

/// The level a capability is.
fn level_of(c: crate::Capability) -> Level {
    Level::from(c)
}

/// The gateway's tools for a `direct` turn at `capability`, or `None` with the gateway off or
/// nothing to admit. `harness_env_value` is opaque here (see [`TurnSpec`]).
pub async fn direct_tool_set(
    cfg: &crate::Config,
    job_id: &str,
    capability: crate::Capability,
    harness_env_value: &str,
    deadline: Duration,
) -> Result<Option<GatewayToolSet>, String> {
    let Some(gw) = global(cfg) else {
        return Ok(None);
    };
    let level = level_of(capability);
    if level == Level::Basic {
        return Ok(None);
    }
    // The MAIN set, rendered for this deployment, and the allowlist a main turn at this
    // capability gets: the same two inputs a spawned child's grant is made of.
    // The deployment's main set: its override when it has one, else the shipped one. Spelled
    // out rather than asked of a harness, because nothing here may depend on which harness or
    // model the turn runs on.
    let config = cfg
        .main_mcp_config
        .as_deref()
        .unwrap_or(crate::MAIN_CHILD_MCP_CONFIG.as_str());
    let rendered = crate::render_child_mcp(cfg, config);
    let set = crate::McpSet::from_config(config).unwrap_or(crate::McpSet::None);
    let allowed = crate::row_allowed_tools(cfg, capability, set);
    let servers: Vec<_> = admitted_servers(&cfg.gateway, &rendered, capability, allowed)?
        .into_iter()
        .filter(|a| !a.tools.is_empty())
        .collect();
    if servers.is_empty() {
        return Ok(None);
    }
    let guard = gw
        .open_turn(TurnSpec {
            job_id: job_id.to_string(),
            capability,
            servers,
            harness_env_value: Some(harness_env_value.to_string()),
            deadline,
            bind_sockets: false,
            env_overlay: Default::default(),
        })
        .await?;
    tool_set_for_turn(guard, level).await.map(Some)
}

/// The tool set over an already-open gateway turn, at `level`: every admitted server listed,
/// its granted intersection exposed where the level permits the tool's class.
pub async fn tool_set_for_turn(guard: TurnGuard, level: Level) -> Result<GatewayToolSet, String> {
    let session = guard.session().clone();
    // Every admitted server listed CONCURRENTLY: a cold start of twenty servers one after
    // another would cost the turn their sum.
    let names = session.server_names();
    let lists = futures_join(names.iter().map(|n| {
        let session = session.clone();
        let n = n.clone();
        async move { (n.clone(), session.list(&n).await) }
    }))
    .await;
    let mut builder = ToolSetBuilder::new(level);
    let mut report = Vec::new();
    for (server, listed) in lists {
        let Some(admitted) = session.admitted(&server) else {
            continue;
        };
        let tools = match listed {
            Ok(t) => t,
            Err(e) => {
                report.push(format!("{server}: unavailable ({e})"));
                continue;
            }
        };
        let mut exposed = 0usize;
        let mut withheld = Vec::new();
        for t in tools {
            let Some(remote) = t.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let class = admitted.server.row.class_of(remote).agent_class();
            if !level.permits(class) {
                continue;
            }
            let Some(exposed_as) = exposed_class(class) else {
                continue;
            };
            let tool = Arc::new(GatewayTool {
                name: tool_name(&server, remote),
                remote: remote.to_string(),
                server: server.clone(),
                description: t
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                schema: t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type": "object"})),
                class,
                session: session.clone(),
            }) as Arc<dyn Tool>;
            // EACH TOOL VETTED ALONE by the builder's own checks (a usable name, an object
            // schema, a class that agrees, no scope-shaped argument). One upstream advertising
            // a tool the agent refuses (plex's `user_get_watch_history` declares `user_id`)
            // costs that tool, never the whole set: it is withheld, and the report says why.
            if let Err(e) = ToolSetBuilder::new(level)
                .add(exposed_as, tool.clone())
                .build()
            {
                withheld.push(format!("{remote} ({e})"));
                continue;
            }
            builder = builder.add(exposed_as, tool);
            exposed += 1;
        }
        report.push(format!(
            "{server}: {exposed} of {} granted tool(s) exposed at {level}",
            admitted.tools.len()
        ));
        for w in withheld {
            report.push(format!("{server}: withheld {w}"));
        }
    }
    let inner = builder
        .build()
        .map_err(|e| format!("gateway tool set: {e}"))?;
    Ok(GatewayToolSet {
        inner,
        _guard: guard,
        report,
    })
}

/// Await every future, in order. A small local join so the bridge does not take a combinator
/// crate for one call.
async fn futures_join<F, T>(futs: impl Iterator<Item = F>) -> Vec<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let handles: Vec<_> = futs.map(tokio::spawn).collect();
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        if let Ok(v) = h.await {
            out.push(v);
        }
    }
    out
}
