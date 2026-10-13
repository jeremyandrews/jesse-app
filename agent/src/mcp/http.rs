//! **The Streamable HTTP transport** — the same four messages as stdio, POSTed to one URL.
//!
//! ---- WHAT THIS SPEAKS ------------------------------------------------------
//!
//! Every client message is one JSON-RPC object POSTed to the server's endpoint with
//! `Accept: application/json, text/event-stream`. A request's reply comes back either as one
//! `application/json` body or as an SSE stream whose events carry JSON-RPC messages, the reply
//! among them; this client reads that stream only until the reply arrives and drops it. A
//! notification is answered `202 Accepted` with no body. The `Mcp-Session-Id` the server hands
//! out on `initialize` is carried on every later message, and [`HttpConnection::close`] ends
//! the session with a `DELETE`, ignoring a `405` (a server is allowed to refuse that).
//!
//! **NO GET STREAM.** The transport lets a client open a standing `GET` for server-initiated
//! messages; this client never does. A server-initiated message is a request towards a host
//! that declared no capability to answer one, so there is nothing such a stream could carry
//! that this client would accept. A request arriving inside a reply's SSE stream is refused
//! exactly as stdio refuses one on stdout.
//!
//! ---- WHAT A FAILURE COSTS --------------------------------------------------
//!
//! The same rule as stdio: **every failure except a well-formed JSON-RPC error kills the
//! connection for the rest of the turn.** A non 2xx status, a body that is not JSON-RPC, a
//! reply carrying the wrong id, a stream that ends before its reply, and a timeout. HTTP does
//! not share stdio's reason for the timeout rule (a late reply here cannot be misread as the
//! answer to a later request, because each request has its own response), but it shares the
//! more important one: a server that has stopped answering in time costs one tool call and is
//! then left alone, rather than costing thirty seconds on every call the model tries next.
//!
//! ---- THE CREDENTIAL ---------------------------------------------------------
//!
//! An optional bearer, held in [`Bearer`], whose `Debug` prints a fixed marker and which has no
//! `Display` at all. It goes on the wire as a header marked sensitive and nowhere else: not in
//! an error (errors name the server and the status, never the header, and never the response
//! body, which a broken server could use to echo the header back), not in a log line.
//!
//! **A CREDENTIAL CAN ALSO BE NAMED RATHER THAN HELD** ([`HttpAuth`], 0.14.0): a bearer, or any
//! header, each read from a NAMED process variable at the moment a message is sent, so a
//! caller that brokers many servers holds names in its config and the value lives only in its
//! environment and on the wire. An unset variable fails the message with an error that names
//! the variable and the server, never a value.
//!
//! **TRUST** is the bundled webpki roots plus, when the grant carries one, ONE extra PEM root
//! added to THIS client only. Certificate verification is never switched off, and redirects are
//! not followed: a redirect is a server asking for the credential to be sent somewhere else.
//!
//! **MUTUAL TLS** ([`MutualTls`], 0.14.0) presents a client certificate and key read from named
//! files, and PINS the server to one CA read from a named file: the bundled roots are switched
//! off for that client, so only a server certificate issued by that CA is accepted.

use std::path::PathBuf;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use tokio::time::{timeout_at, Instant};

use crate::mcp::client::{interpret, McpError};

/// The session header the server assigns on `initialize`.
const SESSION_HEADER: &str = "mcp-session-id";
/// The negotiated version, sent on every message after `initialize` as the transport requires.
const PROTOCOL_HEADER: &str = "mcp-protocol-version";

/// The most a single reply may carry before the server is called out of spec.
///
/// Sized for the largest honest reply this client expects, a rendered floor plan as a base64
/// PNG inside a JSON string, with a wide margin; it exists so a server that streams forever
/// costs a bounded amount of memory rather than all of it.
pub const MAX_REPLY_BYTES: usize = 32 * 1024 * 1024;

/// How long the `DELETE` that ends a session may take. Best effort: the turn is over.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// A bearer token for one HTTP server.
///
/// **`Debug` PRINTS A FIXED MARKER AND THERE IS NO `Display`**, so no `{:?}` of a grant, a
/// client or a config that holds one can put the value in a log, and no `{}` can compile.
#[derive(Clone, PartialEq, Eq)]
pub struct Bearer(String);

impl Bearer {
    pub fn new(token: impl Into<String>) -> Bearer {
        Bearer(token.into())
    }
}

impl std::fmt::Debug for Bearer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Bearer(<redacted>)")
    }
}

/// Mutual TLS for one server: a client certificate and its key, and the ONE CA the server's
/// certificate must chain to. Paths, never contents; each file is read once, at connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutualTls {
    /// The client certificate (chain), PEM.
    pub cert_file: PathBuf,
    /// The client certificate's private key, PEM.
    pub key_file: PathBuf,
    /// The pinned CA, PEM. The bundled roots are OFF for a client that carries this.
    pub ca_file: PathBuf,
}

/// Everything one HTTP server's client presents, in one place.
///
/// **NAMES, NOT VALUES**, except for [`Bearer`], which is kept for the caller that has already
/// read its token (the `direct` harness's house grant). `bearer_env` and `header_env` name
/// process variables that are read when each message is built, so a value is never stored in
/// this struct, in a config or in a `Debug` line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpAuth {
    /// A bearer VALUE, already read by the caller.
    pub bearer: Option<Bearer>,
    /// The NAME of a variable whose value is sent as `Authorization: Bearer <value>`.
    pub bearer_env: Option<String>,
    /// `(header name, variable NAME)` pairs: each header carries that variable's value. This is
    /// the shape a server that takes `x-api-key` rather than a bearer needs.
    pub header_env: Vec<(String, String)>,
    /// One extra trust root, beside the bundled ones, as PEM bytes.
    pub ca_pem: Option<Vec<u8>>,
    /// A client certificate, and a CA pinned in place of the bundled roots.
    pub mtls: Option<MutualTls>,
}

/// One live HTTP session with one server.
pub(crate) struct HttpConnection {
    /// The configured server name, for errors.
    server: String,
    client: reqwest::Client,
    url: String,
    /// The `Authorization` header, prebuilt and marked sensitive, when the grant has a bearer.
    auth: Option<HeaderValue>,
    /// A bearer read from this variable on every message, when the grant names one.
    bearer_env: Option<String>,
    /// Headers read from variables on every message.
    header_env: Vec<(HeaderName, String)>,
    /// Assigned by the server on `initialize`, then carried on every message.
    session: Option<String>,
    /// The negotiated protocol version, once `initialize` has answered.
    protocol: Option<String>,
}

impl HttpConnection {
    /// Build the client. Nothing is sent: the first request is `initialize`.
    pub(crate) fn new(
        server: &str,
        url: &str,
        auth: &HttpAuth,
        connect_timeout: Duration,
    ) -> Result<HttpConnection, McpError> {
        let creds = auth;
        let bearer = creds.bearer.as_ref();
        let ca_pem = creds.ca_pem.as_deref();
        let mut builder = reqwest::Client::builder()
            .use_rustls_tls()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(connect_timeout);
        if let Some(m) = &creds.mtls {
            let read = |what: &str, p: &PathBuf| {
                std::fs::read(p).map_err(|e| {
                    McpError::Unreachable(format!(
                        "could not read the {what} for '{server}' ({}): {e}",
                        p.display()
                    ))
                })
            };
            let mut identity = read("client certificate", &m.cert_file)?;
            identity.push(b'\n');
            identity.extend_from_slice(&read("client key", &m.key_file)?);
            let identity = reqwest::Identity::from_pem(&identity).map_err(|e| {
                McpError::Unreachable(format!(
                    "the client certificate and key for '{server}' are not usable PEM: {e}"
                ))
            })?;
            let ca =
                reqwest::Certificate::from_pem(&read("pinned CA", &m.ca_file)?).map_err(|e| {
                    McpError::Unreachable(format!(
                        "the pinned CA for '{server}' is not a PEM certificate: {e}"
                    ))
                })?;
            builder = builder
                .identity(identity)
                .tls_built_in_root_certs(false)
                .add_root_certificate(ca);
        }
        if let Some(pem) = ca_pem {
            let cert = reqwest::Certificate::from_pem(pem).map_err(|e| {
                McpError::Unreachable(format!(
                    "the CA certificate for '{server}' is not a PEM certificate: {e}"
                ))
            })?;
            builder = builder.add_root_certificate(cert);
        }
        let client = builder.build().map_err(|e| {
            McpError::Unreachable(format!(
                "could not build an HTTP client for '{server}': {e}"
            ))
        })?;
        let auth_header = match bearer {
            None => None,
            Some(b) => {
                let mut v = HeaderValue::from_str(&format!("Bearer {}", b.0)).map_err(|_| {
                    // The value is NOT in this message, by construction.
                    McpError::Unreachable(format!(
                        "the bearer token for '{server}' is not a valid header value"
                    ))
                })?;
                v.set_sensitive(true);
                Some(v)
            }
        };
        let mut header_env = Vec::new();
        for (name, var) in &creds.header_env {
            let h = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                McpError::Unreachable(format!(
                    "the header name '{name}' configured for '{server}' is not a valid header name"
                ))
            })?;
            header_env.push((h, var.clone()));
        }
        Ok(HttpConnection {
            server: server.to_string(),
            client,
            url: url.to_string(),
            auth: auth_header,
            bearer_env: creds.bearer_env.clone(),
            header_env,
            session: None,
            protocol: None,
        })
    }

    /// Record the version `initialize` negotiated, for the header every later message carries.
    pub(crate) fn set_protocol(&mut self, version: &str) {
        self.protocol = Some(version.to_string());
    }

    fn headers(&self) -> Result<HeaderMap, McpError> {
        let mut h = HeaderMap::new();
        h.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        if let Some(a) = &self.auth {
            h.insert(AUTHORIZATION, a.clone());
        }
        // NAMED credentials are read NOW, per message, so a rotated value is picked up by the
        // next call and nothing here holds one between calls.
        if let Some(var) = &self.bearer_env {
            let v = self.read_var(var)?;
            h.insert(AUTHORIZATION, self.sensitive(&format!("Bearer {v}"), var)?);
        }
        for (name, var) in &self.header_env {
            let v = self.read_var(var)?;
            h.insert(name.clone(), self.sensitive(&v, var)?);
        }
        // Both were checked to be visible ASCII when they arrived, so neither can fail here.
        if let Some(s) = self
            .session
            .as_deref()
            .and_then(|s| HeaderValue::from_str(s).ok())
        {
            h.insert(SESSION_HEADER, s);
        }
        if let Some(p) = self
            .protocol
            .as_deref()
            .and_then(|p| HeaderValue::from_str(p).ok())
        {
            h.insert(PROTOCOL_HEADER, p);
        }
        Ok(h)
    }

    /// One named variable's value. The error names the variable and the server, never a value.
    fn read_var(&self, var: &str) -> Result<String, McpError> {
        std::env::var(var)
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                McpError::Unreachable(format!(
                    "the credential variable {var} for '{}' is unset",
                    self.server
                ))
            })
    }

    fn sensitive(&self, value: &str, var: &str) -> Result<HeaderValue, McpError> {
        let mut v = HeaderValue::from_str(value).map_err(|_| {
            McpError::Unreachable(format!(
                "the value of {var} for '{}' is not a valid header value",
                self.server
            ))
        })?;
        v.set_sensitive(true);
        Ok(v)
    }

    fn post(&self, body: &str) -> Result<reqwest::RequestBuilder, McpError> {
        Ok(self
            .client
            .post(&self.url)
            .headers(self.headers()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string()))
    }

    /// One request and its reply, under one deadline that covers the whole exchange.
    pub(crate) async fn exchange(
        &mut self,
        id: u64,
        body: &str,
        budget: Duration,
        method: &str,
    ) -> Result<Value, McpError> {
        let deadline = Instant::now() + budget;
        match timeout_at(deadline, self.exchange_inner(id, body, method)).await {
            Err(_) => Err(McpError::Timeout(budget)),
            Ok(r) => r,
        }
    }

    async fn exchange_inner(
        &mut self,
        id: u64,
        body: &str,
        method: &str,
    ) -> Result<Value, McpError> {
        let resp = self
            .post(body)?
            .send()
            .await
            .map_err(|e| self.unreachable(&e))?;
        self.check_status(&resp)?;
        if resp.status() == reqwest::StatusCode::ACCEPTED {
            return Err(
                self.protocol_error(format!("answered {method} with 202 Accepted and no reply"))
            );
        }
        self.capture_session(&resp)?;
        match content_type(&resp).as_str() {
            "application/json" => {
                let bytes = self.read_capped(resp, method).await?;
                let msg: Value = serde_json::from_slice(&bytes).map_err(|e| {
                    self.protocol_error(format!("sent a reply to {method} that is not JSON: {e}"))
                })?;
                match interpret(&msg, id, method) {
                    Some(r) => r,
                    None => Err(self.protocol_error(format!(
                        "answered {method} with a notification instead of a reply"
                    ))),
                }
            }
            "text/event-stream" => self.read_event_stream(resp, id, method).await,
            other => Err(self.protocol_error(format!(
                "answered {method} with content type {other:?}, which is neither JSON nor an \
                 event stream"
            ))),
        }
    }

    /// Send a notification. The server answers `202 Accepted`; any 2xx is taken.
    pub(crate) async fn notify(&mut self, body: &str, deadline: Instant) -> Result<(), McpError> {
        let send = async {
            let resp = self
                .post(body)?
                .send()
                .await
                .map_err(|e| self.unreachable(&e))?;
            self.check_status(&resp)
        };
        match timeout_at(deadline, send).await {
            Err(_) => Err(McpError::Timeout(
                deadline.saturating_duration_since(Instant::now()),
            )),
            Ok(r) => r,
        }
    }

    /// End the session with a `DELETE`, best effort. A `405` (the server does not let clients
    /// end sessions) and every other outcome are ignored: the turn is over either way.
    pub(crate) async fn close(&mut self) {
        if self.session.is_none() {
            return;
        }
        let Ok(headers) = self.headers() else {
            self.session = None;
            return;
        };
        let req = self.client.delete(&self.url).headers(headers);
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, req.send()).await;
        self.session = None;
    }

    /// A `DELETE` for a session whose client is being dropped without an `await`. Returns the
    /// request to send, or `None` when there is no session to end.
    pub(crate) fn close_request(&self) -> Option<reqwest::RequestBuilder> {
        self.session.as_ref()?;
        Some(self.client.delete(&self.url).headers(self.headers().ok()?))
    }

    fn check_status(&self, resp: &reqwest::Response) -> Result<(), McpError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        // THE STATUS AND NOTHING ELSE. The body is not read: a broken server could echo the
        // request's headers back in it, and the request's headers carry the token.
        Err(McpError::Http {
            server: self.server.clone(),
            status: status.as_u16(),
        })
    }

    fn capture_session(&mut self, resp: &reqwest::Response) -> Result<(), McpError> {
        let Some(v) = resp.headers().get(SESSION_HEADER) else {
            return Ok(());
        };
        let s = v
            .to_str()
            .ok()
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_graphic()))
            .ok_or_else(|| {
                self.protocol_error("assigned a session id that is not visible ASCII".into())
            })?;
        match &self.session {
            None => self.session = Some(s.to_string()),
            // A server may not swap the session under a client mid-conversation; one that does
            // has lost track of this client, and every later reply is suspect.
            Some(existing) if existing != s => {
                return Err(self.protocol_error("changed the session id mid-session".into()))
            }
            Some(_) => {}
        }
        Ok(())
    }

    async fn read_capped(
        &self,
        mut resp: reqwest::Response,
        method: &str,
    ) -> Result<Vec<u8>, McpError> {
        let mut out = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(c)) => {
                    if out.len() + c.len() > MAX_REPLY_BYTES {
                        return Err(self.too_large(method));
                    }
                    out.extend_from_slice(&c);
                }
                Ok(None) => return Ok(out),
                Err(e) => {
                    return Err(self.protocol_error(format!(
                        "broke off its reply to {method}: {}",
                        error_chain(&e)
                    )))
                }
            }
        }
    }

    /// Read SSE events until the one carrying the reply to `id`. Notifications are skipped; a
    /// request from the server is the same violation it is on stdio.
    async fn read_event_stream(
        &self,
        mut resp: reqwest::Response,
        id: u64,
        method: &str,
    ) -> Result<Value, McpError> {
        let mut buf: Vec<u8> = Vec::new();
        let mut parser = SseParser::default();
        let mut total = 0usize;
        loop {
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let mut line: Vec<u8> = buf.drain(..=pos).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                let line = String::from_utf8(line).map_err(|_| {
                    self.protocol_error(format!(
                        "sent an event stream for {method} that is not UTF-8"
                    ))
                })?;
                if let Some(data) = parser.line(&line) {
                    let msg: Value = serde_json::from_str(&data).map_err(|e| {
                        self.protocol_error(format!(
                            "sent an event for {method} whose data is not JSON: {e}"
                        ))
                    })?;
                    if let Some(r) = interpret(&msg, id, method) {
                        return r;
                    }
                }
            }
            match resp.chunk().await {
                Ok(Some(c)) => {
                    total += c.len();
                    if total > MAX_REPLY_BYTES {
                        return Err(self.too_large(method));
                    }
                    buf.extend_from_slice(&c);
                }
                Ok(None) => {
                    return Err(self.protocol_error(format!(
                        "ended the event stream before replying to {method}"
                    )))
                }
                Err(e) => {
                    return Err(self.protocol_error(format!(
                        "broke off the event stream for {method}: {}",
                        error_chain(&e)
                    )))
                }
            }
        }
    }

    fn unreachable(&self, e: &reqwest::Error) -> McpError {
        McpError::Unreachable(format!(
            "could not reach '{}': {}",
            self.server,
            error_chain(e)
        ))
    }

    fn too_large(&self, method: &str) -> McpError {
        self.protocol_error(format!(
            "sent a reply to {method} larger than {} MB",
            MAX_REPLY_BYTES / (1024 * 1024)
        ))
    }

    fn protocol_error(&self, what: String) -> McpError {
        McpError::Protocol(format!("'{}' {what}", self.server))
    }
}

/// The media type of a response, lowercased, without parameters.
fn content_type(resp: &reqwest::Response) -> String {
    resp.headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

/// An error and its sources, joined. reqwest's top-level message is "error sending request";
/// the cause an operator needs ("connection refused", "certificate verify failed") is below
/// it. None of these carries a header: reqwest never prints request headers in an error.
fn error_chain(e: &reqwest::Error) -> String {
    let mut out = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(s) = src {
        out.push_str(": ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

/// The SSE field grammar, one line at a time. Returns an event's data when a blank line ends an
/// event of type `message` (the default) that carried any.
#[derive(Default)]
struct SseParser {
    data: String,
    has_data: bool,
    event: Option<String>,
}

impl SseParser {
    fn line(&mut self, line: &str) -> Option<String> {
        if line.is_empty() {
            let kind_ok = self.event.as_deref().is_none_or(|k| k == "message");
            let out = (self.has_data && kind_ok).then(|| std::mem::take(&mut self.data));
            self.data.clear();
            self.has_data = false;
            self.event = None;
            return out;
        }
        if line.starts_with(':') {
            return None; // a comment, which servers use as a keep-alive
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "event" => self.event = Some(value.to_string()),
            // `id` and `retry` are for resuming a dropped stream, which this client never does.
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bearer_never_prints() {
        let b = Bearer::new("s3cret-value");
        assert_eq!(format!("{b:?}"), "Bearer(<redacted>)");
    }

    #[test]
    fn the_sse_grammar_joins_data_lines_and_skips_other_events() {
        let mut p = SseParser::default();
        assert_eq!(p.line(": keep-alive"), None);
        assert_eq!(p.line("event: message"), None);
        assert_eq!(p.line("data: {\"a\":"), None);
        assert_eq!(p.line("data:1}"), None);
        assert_eq!(p.line(""), Some("{\"a\":\n1}".to_string()));
        assert_eq!(p.line("event: ping"), None);
        assert_eq!(p.line("data: x"), None);
        assert_eq!(p.line(""), None, "a non-message event is skipped");
        assert_eq!(
            p.line(""),
            None,
            "a blank line with no data dispatches nothing"
        );
    }
}
