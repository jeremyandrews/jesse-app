//! The gateway's two listeners.
//!
//! **UNIX** — one socket per admitted server under the turn's `0700` directory, newline-delimited
//! JSON-RPC exactly as stdio carries it, so the stub is a byte pump. The directory's mode is the
//! access control: only the bridge's own user can reach a socket, and the path names one job.
//!
//! **TCP** (`[gateway] listen`, off by default) — `POST /mcp/<server>` speaking the Streamable
//! HTTP transport's JSON-response form. EVERY request carries the turn token as
//! `Authorization: Bearer <token>`, and every request is checked: the signature, the expiry, the
//! server against the token's granted set, and that the token's turn is still open and issued
//! this exact token (a token whose turn has ended is refused, which is what revocation is).
//! Every refusal is a bare 401: the reason is for the log, not for the caller.

use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::task::JoinHandle;

use super::session::{Handled, TurnSession};
use super::token::now_secs;
use super::Gateway;

/// The largest message a child may send through the gateway in one line or one request body.
/// Tool arguments are small; this bounds memory, not anything a turn needs.
pub const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

/// Bind `path` and serve `server` for this turn on every connection made to it.
pub fn serve_unix(
    session: Arc<TurnSession>,
    server: String,
    path: &Path,
) -> std::io::Result<JoinHandle<()>> {
    use std::os::unix::fs::PermissionsExt;
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let session = session.clone();
            let server = server.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut lines = BufReader::new(r).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.len() > MAX_REQUEST_BYTES {
                        return;
                    }
                    if line.trim().is_empty() {
                        continue;
                    }
                    let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                        let e = serde_json::json!({"jsonrpc": "2.0", "id": null,
                            "error": {"code": -32700, "message": "parse error"}});
                        let _ = w.write_all(format!("{e}\n").as_bytes()).await;
                        return;
                    };
                    match session.handle(&server, &msg).await {
                        Handled::NoReply => {}
                        Handled::Reply(v) => {
                            if w.write_all(format!("{v}\n").as_bytes()).await.is_err() {
                                return;
                            }
                        }
                        Handled::Close(v) => {
                            let _ = w.write_all(format!("{v}\n").as_bytes()).await;
                            let _ = w.shutdown().await;
                            return;
                        }
                    }
                }
            });
        }
    }))
}

/// Bind the TCP listener at `addr` and serve it until the process ends. Returns the bound
/// address (so a test can bind port 0).
pub async fn serve_tcp(
    gateway: Arc<Gateway>,
    addr: &str,
) -> std::io::Result<(std::net::SocketAddr, JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let app = router(gateway);
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((local, task))
}

/// The TCP listener's routes.
pub fn router(gateway: Arc<Gateway>) -> Router {
    Router::new()
        .route("/mcp/:server", post(mcp_post).delete(mcp_delete))
        .with_state(gateway)
}

/// The bearer from `Authorization`, verified for `server`, and its open session.
fn admit(gw: &Gateway, headers: &HeaderMap, server: &str) -> Result<Arc<TurnSession>, String> {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or("no bearer")?;
    let claims = gw
        .key
        .verify(presented, server, now_secs())
        .map_err(|e| format!("{e:?}"))?;
    let session = gw.session(&claims.job).ok_or("the turn is closed")?;
    // THE TOKEN THIS TURN WAS ISSUED, byte for byte, compared in constant time: a validly
    // signed token from an earlier turn with a reused job id is not this turn's.
    use subtle::ConstantTimeEq;
    if !bool::from(presented.as_bytes().ct_eq(session.token().as_bytes())) {
        return Err("not this turn's token".into());
    }
    Ok(session)
}

const SESSION_HEADER: &str = "mcp-session-id";

async fn mcp_post(
    State(gw): State<Arc<Gateway>>,
    UrlPath(server): UrlPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let session = match admit(&gw, &headers, &server) {
        Ok(s) => s,
        Err(why) => {
            eprintln!("gateway: refused a request for '{server}': {why}");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    };
    if body.len() > MAX_REQUEST_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let is_initialize = msg.get("method").and_then(|m| m.as_str()) == Some("initialize");
    let sid = headers
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if !is_initialize {
        match &sid {
            Some(id) if session.http_session_is(id, &server) => {}
            _ => return StatusCode::NOT_FOUND.into_response(),
        }
    }
    match session.handle(&server, &msg).await {
        Handled::NoReply => StatusCode::ACCEPTED.into_response(),
        Handled::Reply(v) => {
            let mut resp = (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                v.to_string(),
            )
                .into_response();
            if is_initialize {
                let id = session.open_http_session(&server);
                if let Ok(h) = id.parse() {
                    resp.headers_mut().insert(SESSION_HEADER, h);
                }
            }
            resp
        }
        Handled::Close(v) => {
            if let Some(id) = sid {
                session.close_http_session(&id);
            }
            (
                StatusCode::BAD_REQUEST,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                v.to_string(),
            )
                .into_response()
        }
    }
}

async fn mcp_delete(
    State(gw): State<Arc<Gateway>>,
    UrlPath(server): UrlPath<String>,
    headers: HeaderMap,
) -> Response {
    let Ok(session) = admit(&gw, &headers, &server) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if let Some(id) = headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok()) {
        session.close_http_session(id);
    }
    StatusCode::OK.into_response()
}
