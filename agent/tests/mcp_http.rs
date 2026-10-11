//! **The Streamable HTTP MCP transport, against an in-process mock server.**
//!
//! The mock speaks HTTP/1.1 by hand on a loopback socket, the way the conformance suite stands
//! up its provider mocks, and records every request it saw: method, JSON-RPC method, session
//! header, whether an `Authorization` header arrived. The assertions are about what crossed the
//! wire, read off that record, rather than about what the client says it did.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use jesse_agent::mcp::{Bearer, McpClient, McpError, McpToolSet, ServerGrant};
use jesse_agent::tools::{ActionClass, Level, ResultBlock, SystemClock, ToolContext, ToolSet};
use jesse_agent::Scope;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "mock-bearer-value-7f3a";
const SESSION: &str = "sess-abc123";
/// One pixel, as a PNG, base64.
const PNG_1PX: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Every reply as one `application/json` body.
    Json,
    /// Every reply as an SSE stream, with a notification and a comment ahead of the reply.
    Sse,
    /// Every request answered 401.
    Unauthorized,
    /// `tools/call` never answers.
    HangOnCall,
}

#[derive(Debug, Clone)]
struct Seen {
    http_method: String,
    rpc_method: Option<String>,
    session: Option<String>,
    authorized: bool,
    protocol_header: Option<String>,
}

struct Mock {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

async fn mock(mode: Mode) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let _ = serve(sock, mode, log).await;
            });
        }
    });
    Mock { url, seen }
}

/// One connection, any number of requests on it (reqwest keeps connections alive).
async fn serve(mut sock: TcpStream, mode: Mode, log: Arc<Mutex<Vec<Seen>>>) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            let mut chunk = [0u8; 4096];
            let n = sock.read(&mut chunk).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.lines();
        let request_line = lines.next().unwrap_or_default().to_string();
        let mut headers = Vec::new();
        for l in lines {
            if let Some((k, v)) = l.split_once(':') {
                headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
        }
        let header = |k: &str| headers.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let len: usize = header("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        while buf.len() < head_end + len {
            let mut chunk = [0u8; 4096];
            let n = sock.read(&mut chunk).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let body: Vec<u8> = buf[head_end..head_end + len].to_vec();
        buf.drain(..head_end + len);

        let http_method = request_line
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        let msg: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let rpc_method = msg
            .get("method")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        log.lock().unwrap().push(Seen {
            http_method: http_method.clone(),
            rpc_method: rpc_method.clone(),
            session: header("mcp-session-id"),
            authorized: header("authorization").as_deref() == Some(&format!("Bearer {TOKEN}")),
            protocol_header: header("mcp-protocol-version"),
        });

        if mode == Mode::Unauthorized {
            write(&mut sock, "401 Unauthorized", &[], b"").await?;
            continue;
        }
        if http_method == "DELETE" {
            // The transport lets a server refuse this, and the client must not mind.
            write(&mut sock, "405 Method Not Allowed", &[], b"").await?;
            continue;
        }
        let id = msg.get("id").cloned();
        let Some(id) = id else {
            // A notification.
            write(&mut sock, "202 Accepted", &[], b"").await?;
            continue;
        };
        let result = match rpc_method.as_deref() {
            Some("initialize") => json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "mock-house", "version": "0.0.1"},
            }),
            Some("tools/list") => json!({"tools": [
                {"name": "list_sites", "description": "sites", "inputSchema": {"type": "object"}},
                {"name": "render_plan", "description": "plan", "inputSchema": {"type": "object"}},
            ]}),
            Some("tools/call") => {
                if mode == Mode::HangOnCall {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                }
                match msg["params"]["name"].as_str() {
                    Some("render_plan") => json!({"content": [
                        {"type": "text", "text": "plan of floor 0"},
                        {"type": "image", "mimeType": "image/png", "data": PNG_1PX},
                    ]}),
                    _ => json!({"content": [{"type": "text", "text": "[\"selftest\"]"}]}),
                }
            }
            _ => json!({}),
        };
        let reply = json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
        let mut extra: Vec<(&str, &str)> = Vec::new();
        if rpc_method.as_deref() == Some("initialize") {
            extra.push(("Mcp-Session-Id", SESSION));
        }
        match mode {
            Mode::Sse => {
                extra.push(("Content-Type", "text/event-stream"));
                let stream = format!(
                    ": keep-alive\n\nevent: message\ndata: {}\n\nid: 7\ndata: {reply}\n\n",
                    json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {}})
                );
                write(&mut sock, "200 OK", &extra, stream.as_bytes()).await?;
            }
            _ => {
                extra.push(("Content-Type", "application/json"));
                write(&mut sock, "200 OK", &extra, reply.as_bytes()).await?;
            }
        }
    }
}

async fn write(
    sock: &mut TcpStream,
    status: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut out = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    sock.write_all(out.as_bytes()).await?;
    sock.write_all(body).await?;
    sock.flush().await
}

const CALL: Duration = Duration::from_secs(5);
const CONNECT: Duration = Duration::from_secs(5);

async fn connect(m: &Mock, per_call: Duration) -> Result<McpClient, McpError> {
    McpClient::connect_http(
        "house",
        &m.url,
        Some(&Bearer::new(TOKEN)),
        None,
        per_call,
        CONNECT,
    )
    .await
}

/// The whole conversation on one transport mode: initialize, the notification, a list, a call,
/// and the session ended. Asserted from the server's own record.
async fn a_full_session(mode: Mode) {
    let m = mock(mode).await;
    let c = connect(&m, CALL).await.expect("connects");
    assert_eq!(c.server_info(), "mock-house 0.0.1");
    let tools = c.list_tools().await.expect("lists");
    assert_eq!(tools.len(), 2);
    let out = c.call_tool("list_sites", json!({})).await.expect("calls");
    assert_eq!(out.content[0]["text"], "[\"selftest\"]");
    c.shutdown().await;

    let seen = m.seen.lock().unwrap().clone();
    let methods: Vec<String> = seen
        .iter()
        .map(|s| {
            format!(
                "{} {}",
                s.http_method,
                s.rpc_method.clone().unwrap_or_default()
            )
        })
        .collect();
    assert_eq!(
        methods,
        [
            "POST initialize",
            "POST notifications/initialized",
            "POST tools/list",
            "POST tools/call",
            "DELETE ",
        ],
        "the four messages, then the session ended"
    );
    assert!(
        seen.iter().all(|s| s.authorized),
        "every message carries the bearer"
    );
    assert_eq!(
        seen[0].session, None,
        "initialize is sent without a session"
    );
    assert_eq!(seen[0].protocol_header, None);
    for s in &seen[1..] {
        assert_eq!(
            s.session.as_deref(),
            Some(SESSION),
            "the session id is carried"
        );
        assert_eq!(s.protocol_header.as_deref(), Some("2025-06-18"));
    }
}

#[tokio::test]
async fn a_json_reply_completes_a_session_and_carries_the_session_id() {
    a_full_session(Mode::Json).await;
}

#[tokio::test]
async fn an_sse_reply_completes_a_session_past_comments_and_notifications() {
    a_full_session(Mode::Sse).await;
}

#[tokio::test]
async fn a_401_is_surfaced_by_server_and_status_and_never_names_the_token() {
    let m = mock(Mode::Unauthorized).await;
    let err = connect(&m, CALL).await.expect_err("refused");
    assert_eq!(
        err,
        McpError::Http {
            server: "house".into(),
            status: 401
        }
    );
    let text = err.to_string();
    assert!(text.contains("house") && text.contains("401"), "{text}");
    assert!(!text.contains(TOKEN), "{text}");
    assert!(!format!("{err:?}").contains(TOKEN));
}

#[tokio::test]
async fn a_server_that_is_down_at_connect_is_unreachable_by_name() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    drop(listener);
    let err = McpClient::connect_http(
        "house",
        &url,
        Some(&Bearer::new(TOKEN)),
        None,
        CALL,
        CONNECT,
    )
    .await
    .expect_err("nothing is listening");
    assert!(matches!(err, McpError::Unreachable(_)), "{err:?}");
    let text = err.to_string();
    assert!(text.contains("'house'"), "{text}");
    assert!(!text.contains(TOKEN), "{text}");
}

#[tokio::test]
async fn a_call_that_times_out_kills_the_connection_for_the_turn() {
    let m = mock(Mode::HangOnCall).await;
    let c = connect(&m, Duration::from_millis(300))
        .await
        .expect("connects");
    let err = c
        .call_tool("list_sites", json!({}))
        .await
        .expect_err("hangs");
    assert_eq!(err, McpError::Timeout(Duration::from_millis(300)));
    assert!(
        matches!(
            c.call_tool("list_sites", json!({})).await,
            Err(McpError::Dead(_))
        ),
        "a timed-out server is left alone for the rest of the turn"
    );
}

#[tokio::test]
async fn image_content_from_a_tool_result_is_kept_as_an_image_block() {
    let m = mock(Mode::Sse).await;
    let grant = ServerGrant::http(
        "house",
        &m.url,
        ["list_sites", "render_plan"],
        ActionClass::Egress,
    )
    .with_bearer(Bearer::new(TOKEN));
    let set = McpToolSet::connect(&[grant], Level::Read, CALL, CONNECT)
        .await
        .expect("builds");
    assert!(set.report().warnings.is_empty(), "{:?}", set.report());
    let tool = set.get("mcp__house__render_plan").expect("exposed");
    let ctx = ToolContext {
        turn_id: "t".into(),
        conversation_id: "c".into(),
        call_id: "1".into(),
        cancel: CancellationToken::new(),
        clock: Arc::new(SystemClock::new()),
        artifact_dir: None,
    };
    let out = tool
        .call(&Scope::new("local", "owner", "vault"), json!({}), &ctx)
        .await
        .expect("answers");
    assert_eq!(out.content[0], ResultBlock::Text("plan of floor 0".into()));
    assert_eq!(
        out.content[1],
        ResultBlock::Image {
            media_type: "image/png".into(),
            data_base64: PNG_1PX.into(),
        }
    );
    set.shutdown().await;
}

/// The grant's `Debug` names the transport and the URL and never the bearer.
#[test]
fn a_grant_formatted_for_a_log_does_not_carry_its_token() {
    let g = ServerGrant::http(
        "house",
        "https://house.example/mcp",
        ["find"],
        ActionClass::Read,
    )
    .with_bearer(Bearer::new(TOKEN))
    .with_ca_pem(b"-----BEGIN CERTIFICATE-----".to_vec());
    let text = format!("{g:?} {g:#?}");
    assert!(text.contains("https://house.example/mcp"), "{text}");
    assert!(!text.contains(TOKEN), "{text}");
}

#[test]
fn an_http_grant_must_be_https_or_loopback() {
    use jesse_agent::mcp::{validate_grants, McpSetError};
    for ok in [
        "https://house.pozza/mcp",
        "http://127.0.0.1:8080/mcp",
        "http://localhost/mcp",
        "http://[::1]:9/mcp",
    ] {
        let g = ServerGrant::http("house", ok, ["find"], ActionClass::Read);
        assert!(validate_grants(&[g]).is_ok(), "{ok}");
    }
    for bad in [
        "http://house.pozza/mcp",
        "http://127.0.0.1.evil.example/mcp",
        "ftp://x",
        "https://",
    ] {
        let g = ServerGrant::http("house", bad, ["find"], ActionClass::Read);
        assert!(
            matches!(validate_grants(&[g]), Err(McpSetError::InsecureUrl { .. })),
            "{bad}"
        );
    }
}

/// A write-class tool is withheld at `read`, and the report counts what the level let through.
#[tokio::test]
async fn a_write_class_tool_is_withheld_at_read_and_the_report_says_so() {
    let m = mock(Mode::Json).await;
    let mut grant = ServerGrant::http(
        "house",
        &m.url,
        ["list_sites", "render_plan"],
        ActionClass::Egress,
    )
    .with_bearer(Bearer::new(TOKEN));
    grant
        .per_tool_class
        .insert("list_sites".into(), ActionClass::VaultWrite);
    let set = McpToolSet::connect(&[grant], Level::Read, CALL, CONNECT)
        .await
        .expect("builds");
    let names: Vec<String> = set.manifest().into_iter().map(|t| t.name).collect();
    assert_eq!(names, ["mcp__house__render_plan"]);
    assert!(
        set.report().servers[0].contains("1 of 2 granted tool(s) exposed at read"),
        "{:?}",
        set.report()
    );
    set.shutdown().await;
}
