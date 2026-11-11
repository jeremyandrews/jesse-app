//! **THE EGRESS PROXY** — the open web, one inspected request at a time.
//!
//! A small HTTP/1.1 forward proxy on hyper. Every child's `HTTPS_PROXY`/`HTTP_PROXY` names it
//! with the turn's credentials (`http://<job id>:<turn token>@<egress_listen>`), and it serves
//! both shapes a client uses: `CONNECT host:port` for HTTPS, and absolute-form requests
//! (`GET http://host/path`) for plain HTTP.
//!
//! ---- THE ORDER OF THE CHECKS, AND WHY ---------------------------------------------
//!
//!  1. **Auth.** `Proxy-Authorization: Basic base64(job:token)`, validated by the
//!     [`TurnAuthority`]. MISSING is a bare `407` with a `Proxy-Authenticate` challenge and no
//!     ledger line, because that is the normal first round trip: git (measured) sends its
//!     credentials only after a challenge. WRONG is a `407` and an `auth` line.
//!  2. **Tier admits the host.** `closed` admits nothing; an allowlist only its hosts. A
//!     refusal here happens before anything is resolved, terminated or forwarded.
//!  3. **Resolve, and refuse private addresses** ([`is_private`]). On EVERY tier, open-logged
//!     included: a turn on the open web must not reach the LAN, the Studio's loopback
//!     services or a cloud metadata endpoint by naming a host that resolves to one. The
//!     proxy resolves the host ITSELF and connects to the address it checked, never
//!     re-resolving, so a DNS answer that changes between the check and the connect (DNS
//!     rebinding) cannot slip a private address past it.
//!  4. **`CONNECT` only: the inner stream must be TLS, and the client must accept our
//!     certificate.** The proxy answers `200`, peeks the first byte (a TLS handshake starts
//!     `0x16`), and terminates TLS with a leaf from the per-install CA ([`super::ca`]). Bytes
//!     that are not TLS, or a client that refuses the leaf (a pinned client), are `opaque`:
//!     the tunnel closes, a ledger line says so, and a stderr line names the client, so a
//!     pinned tool fails LOUDLY rather than mysteriously.
//!  5. **Per request**, on the terminated stream or the plain one: the whole body is read
//!     (capped at [`MAX_BODY`]), the request is classed `read` or `send` ([`classify`]), a
//!     `send` on an open-logged turn must go to a host on the send list, and the content
//!     check ([`super::content`]) reads the URL, headers and body for canaries and vault
//!     fingerprints. Only then does it leave, over a fresh connection to the checked address.
//!
//! ---- WHAT IS WRITTEN DOWN ------------------------------------------------------------
//!
//! One `egress` ledger line per request (and one per refused `CONNECT`): job, tier, HOST,
//! port, class, byte counts, verdict, reason. Never the URL, a header or a body; never the
//! token. The full exchange goes to the [`ExchangeRecorder`] hook, which today records
//! nothing.

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use base64::Engine;
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full, Limited};
use hyper::body::{Body, Frame, Incoming};
use hyper::header::{HeaderName, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};

use super::ca::ProxyCa;
use super::content::{ContentCheck, ContentVerdict};
use super::{
    EgressPolicy, EgressTurn, ExchangeRecorder, InspectedExchange, RefusalNotifier, TurnAuthority,
};
use crate::activity::{
    now_ms, ActivityLedger, ActivityLine, EgressLine, EgressReason, EgressVerdict, MethodClass,
    Token,
};
use crate::proxy::tier::{host_matches, Egress};

/// The largest request body the proxy reads (and so the largest a turn can send).
pub const MAX_BODY: usize = 16 * 1024 * 1024;

/// The bridge-owned send list under the state directory (a JSON array of host patterns).
pub const SEND_HOSTS_FILE: &str = "egress-send-hosts.json";

/// How long a client has after `200 Connection established` to start its TLS handshake,
/// and the proxy to reach an origin.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

type ProxyBody = BoxBody<Bytes, hyper::Error>;

// ---- RESOLUTION --------------------------------------------------------------------------

/// A boxed future, so [`Resolver`] needs no `async_trait`.
pub type ResolveFuture<'a> = Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>>;

/// How the proxy turns a host into addresses. The system's resolver in the bridge; a map in
/// tests, so a test origin on loopback can be named like a public host.
pub trait Resolver: Send + Sync {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a>;
}

/// `tokio::net::lookup_host`.
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a> {
        Box::pin(async move { Ok(tokio::net::lookup_host((host, port)).await?.collect()) })
    }
}

/// Whether `ip` is an address the open web must never reach through this proxy: private,
/// loopback, link-local, CGNAT, unspecified, broadcast, multicast or reserved, in IPv4, in
/// IPv6, and in the IPv6 forms that carry an IPv4 address (`::ffff:a.b.c.d`, NAT64
/// `64:ff9b::a.b.c.d`).
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => private_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return private_v4(v4);
            }
            let s = v6.segments();
            if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
                let v4 =
                    Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
                return private_v4(v4);
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || (s[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
                || (s[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
                || (s[0] & 0xff00) == 0xff00 // ff00::/8 multicast
                // ::a.b.c.d, the deprecated IPv4-compatible form, carries a v4 address too.
                || (s[0..6] == [0, 0, 0, 0, 0, 0] && private_v4(Ipv4Addr::new(
                    (s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8)))
        }
    }
}

fn private_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    o[0] == 10
        || (o[0] == 172 && (o[1] & 0xf0) == 16)
        || (o[0] == 192 && o[1] == 168)
        || o[0] == 127
        || (o[0] == 169 && o[1] == 254)
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        || o[0] == 0
        || v4 == Ipv4Addr::BROADCAST
        || (o[0] & 0xf0) == 224 // 224/4 multicast
        || (o[0] & 0xf0) == 240 // 240/4 reserved
}

// ---- CLASSIFICATION ------------------------------------------------------------------------

/// `read` iff the method is GET, HEAD or OPTIONS, there is no body, the path-plus-query is at
/// most `read_url_max` bytes and the query at most `read_query_max`; otherwise `send`.
///
/// The URL limits are what make the split mean something: a GET whose query is a paragraph
/// is a send in a read's clothes.
pub fn classify(
    method: &str,
    path_and_query: &str,
    body_len: usize,
    policy: &EgressPolicy,
) -> MethodClass {
    let reading = matches!(
        method.to_ascii_uppercase().as_str(),
        "GET" | "HEAD" | "OPTIONS"
    );
    let query = path_and_query.split_once('?').map(|(_, q)| q).unwrap_or("");
    if reading
        && body_len == 0
        && path_and_query.len() <= policy.read_url_max
        && query.len() <= policy.read_query_max
    {
        MethodClass::Read
    } else {
        MethodClass::Send
    }
}

// ---- THE PROXY -------------------------------------------------------------------------

/// What the proxy is built from.
pub struct EgressDeps {
    pub authority: Arc<dyn TurnAuthority>,
    pub ledger: Option<Arc<ActivityLedger>>,
    pub notifier: Arc<dyn RefusalNotifier>,
    pub recorder: Arc<dyn ExchangeRecorder>,
    pub ca: Arc<ProxyCa>,
    pub content: Arc<ContentCheck>,
    pub policy: EgressPolicy,
    /// Where [`SEND_HOSTS_FILE`] lives.
    pub state_dir: PathBuf,
    pub resolver: Arc<dyn Resolver>,
    /// The TLS client the proxy reaches origins with. [`default_upstream_tls`] in the bridge.
    pub upstream_tls: Arc<rustls::ClientConfig>,
    /// Whose approval a refused send needs, as the refusal tells the agent
    /// (`cfg.persona.owner_name`).
    pub owner_name: String,
}

/// The origin-side TLS client: the Mozilla roots (webpki-roots), HTTP/1.1.
pub fn default_upstream_tls() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut c = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("ring supports the default protocol versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    c.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(c)
}

/// The egress proxy.
pub struct EgressProxy {
    deps: EgressDeps,
    /// Loopback (only loopback) is reachable. FALSE in every non-test build: there is no
    /// way to set it outside `cfg(test)`.
    allow_loopback: bool,
}

/// Where a request goes: the host it named, and the address that host was checked at.
#[derive(Clone)]
struct Target {
    turn: EgressTurn,
    host: String,
    port: u16,
    addr: SocketAddr,
    tls: bool,
}

/// What one request leaves on the ledger.
struct LineFacts<'a> {
    job: &'a str,
    tier: &'a str,
    host: &'a str,
    port: u16,
    class: MethodClass,
    bytes_out: u64,
    bytes_in: u64,
    verdict: EgressVerdict,
    reason: EgressReason,
}

impl EgressProxy {
    /// The production proxy: private and loopback addresses are refused, unconditionally.
    pub fn new(deps: EgressDeps) -> EgressProxy {
        EgressProxy {
            deps,
            allow_loopback: false,
        }
    }

    /// A proxy that may reach loopback test origins. Every other private range is still
    /// refused, so the rule stays exercised.
    #[cfg(test)]
    pub(crate) fn with_loopback_for_tests(deps: EgressDeps) -> EgressProxy {
        EgressProxy {
            deps,
            allow_loopback: true,
        }
    }

    /// Accept connections on `listener`, forever. Each connection is served on its own task;
    /// a failed accept is logged and the loop goes on.
    pub async fn serve(self: Arc<Self>, listener: tokio::net::TcpListener) {
        loop {
            let (stream, _peer) = match listener.accept().await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("egress proxy: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let me = self.clone();
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(move |req| {
                    let me = me.clone();
                    async move { Ok::<_, Infallible>(me.outer(req).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .with_upgrades()
                    .await;
            });
        }
    }

    fn record(&self, f: LineFacts<'_>) {
        if let Some(ledger) = &self.deps.ledger {
            ledger.record(&ActivityLine::egress(EgressLine {
                job_id: Token::new(f.job),
                tier: Token::new(f.tier),
                host: Token::new(f.host),
                port: f.port as u64,
                method_class: f.class,
                at_ms: now_ms(),
                bytes_out: f.bytes_out,
                bytes_in: f.bytes_in,
                verdict: f.verdict,
                reason: f.reason,
            }));
        }
    }

    /// A refusal's ledger line (no bytes moved).
    fn refuse_line(
        &self,
        t: &EgressTurn,
        host: &str,
        port: u16,
        class: MethodClass,
        why: EgressReason,
    ) {
        self.record(LineFacts {
            job: &t.job_id,
            tier: t.tier.label(),
            host,
            port,
            class,
            bytes_out: 0,
            bytes_in: 0,
            verdict: EgressVerdict::Refused,
            reason: why,
        });
    }

    /// The outer connection's requests: `CONNECT`, or an absolute-form plain HTTP request.
    async fn outer(self: Arc<Self>, req: Request<Incoming>) -> Response<ProxyBody> {
        if req.method() == Method::CONNECT {
            return self.connect(req).await;
        }
        if req.uri().scheme().is_some() && req.uri().authority().is_some() {
            return self.plain(req).await;
        }
        text(
            StatusCode::BAD_REQUEST,
            "this is a forward proxy: send absolute-form requests or CONNECT\n",
        )
    }

    /// Step 1: the turn, or the `407` to answer with.
    fn authenticate(
        &self,
        headers: &hyper::HeaderMap,
        host: &str,
        port: u16,
        class: MethodClass,
    ) -> Result<EgressTurn, Box<Response<ProxyBody>>> {
        let challenge = || {
            let mut r = text(
                StatusCode::PROXY_AUTHENTICATION_REQUIRED,
                "proxy authentication required\n",
            );
            r.headers_mut().insert(
                hyper::header::PROXY_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"jesse-egress\""),
            );
            r
        };
        let Some(value) = headers.get(hyper::header::PROXY_AUTHORIZATION) else {
            return Err(Box::new(challenge()));
        };
        let creds = value
            .to_str()
            .ok()
            .and_then(|v| {
                let (scheme, rest) = v.trim().split_once(' ')?;
                scheme.eq_ignore_ascii_case("basic").then_some(rest.trim())
            })
            .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
            .and_then(|b| String::from_utf8(b).ok());
        let (job, token) = match creds.as_deref().and_then(|c| c.split_once(':')) {
            Some((j, t)) => (j.to_string(), t.to_string()),
            None => (String::new(), String::new()),
        };
        match (!job.is_empty() && !token.is_empty())
            .then(|| self.deps.authority.egress_turn(&job, &token))
            .flatten()
        {
            Some(turn) => Ok(turn),
            None => {
                // The job id only if it is identifier-shaped (Token::new); never the token.
                self.record(LineFacts {
                    job: &job,
                    tier: "unknown",
                    host,
                    port,
                    class,
                    bytes_out: 0,
                    bytes_in: 0,
                    verdict: EgressVerdict::Refused,
                    reason: EgressReason::Auth,
                });
                Err(Box::new(challenge()))
            }
        }
    }

    /// Steps 2 and 3: the tier admits the host, and the host resolves to public addresses
    /// only. The address to connect to, or the refusal to answer with.
    async fn admit(
        &self,
        turn: &EgressTurn,
        host: &str,
        port: u16,
        class: MethodClass,
    ) -> Result<SocketAddr, Box<Response<ProxyBody>>> {
        if !turn.tier.admits_host(host) {
            self.refuse_line(turn, host, port, class, EgressReason::Tier);
            return Err(Box::new(text(
                StatusCode::FORBIDDEN,
                &format!(
                    "this turn's egress tier ({}) does not admit {host}\n",
                    turn.tier.label()
                ),
            )));
        }
        let addrs = match host.parse::<IpAddr>() {
            Ok(ip) => Ok(vec![SocketAddr::new(ip, port)]),
            Err(_) => self.deps.resolver.resolve(host, port).await,
        };
        let addrs = match addrs {
            Ok(a) if !a.is_empty() => a,
            _ => {
                return Err(Box::new(text(
                    StatusCode::BAD_GATEWAY,
                    &format!("{host} does not resolve\n"),
                )))
            }
        };
        let blocked =
            |a: &SocketAddr| is_private(a.ip()) && !(self.allow_loopback && a.ip().is_loopback());
        if addrs.iter().any(blocked) {
            self.refuse_line(turn, host, port, class, EgressReason::PrivateAddress);
            return Err(Box::new(text(
                StatusCode::FORBIDDEN,
                &format!(
                    "{host} resolves to a private address; the egress proxy never reaches one\n"
                ),
            )));
        }
        Ok(addrs[0])
    }

    /// `CONNECT host:port`: authenticate, admit, answer `200`, and terminate TLS on the
    /// upgraded stream.
    async fn connect(self: Arc<Self>, req: Request<Incoming>) -> Response<ProxyBody> {
        let Some(authority) = req.uri().authority().cloned() else {
            return text(StatusCode::BAD_REQUEST, "CONNECT needs host:port\n");
        };
        let host = bare_host(authority.host());
        let port = authority.port_u16().unwrap_or(443);
        let turn = match self.authenticate(req.headers(), &host, port, MethodClass::None) {
            Ok(t) => t,
            Err(r) => return *r,
        };
        let addr = match self.admit(&turn, &host, port, MethodClass::None).await {
            Ok(a) => a,
            Err(r) => return *r,
        };
        let ua = user_agent(req.headers());
        let target = Target {
            turn,
            host,
            port,
            addr,
            tls: true,
        };
        tokio::spawn(async move {
            match hyper::upgrade::on(req).await {
                Ok(up) => self.tunnel(up, target, ua).await,
                Err(e) => eprintln!("egress proxy: CONNECT upgrade failed: {e}"),
            }
        });
        Response::new(empty())
    }

    /// Step 4: the inner stream must be TLS the client lets us terminate.
    async fn tunnel(self: Arc<Self>, up: hyper::upgrade::Upgraded, target: Target, ua: String) {
        let mut io = TokioIo::new(up);
        let mut first = [0u8; 1];
        match tokio::time::timeout(STEP_TIMEOUT, io.read(&mut first)).await {
            Ok(Ok(1)) => {}
            // Closed or idle before a byte: nothing was sent, nothing to record.
            _ => return,
        }
        let job = target.turn.job_id.clone();
        if first[0] != 0x16 {
            self.refuse_line(
                &target.turn,
                &target.host,
                target.port,
                MethodClass::None,
                EgressReason::Opaque,
            );
            eprintln!(
                "egress proxy: job {job}: {ua} opened a CONNECT tunnel to {}:{} and sent \
                 something that is not TLS; closed (an uninspectable stream never leaves)",
                target.host, target.port
            );
            return;
        }
        let config = match self.deps.ca.server_config_for(&target.host) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("egress proxy: no certificate for {}: {e}", target.host);
                self.refuse_line(
                    &target.turn,
                    &target.host,
                    target.port,
                    MethodClass::None,
                    EgressReason::Opaque,
                );
                return;
            }
        };
        let stream = Prefixed {
            first: Some(first[0]),
            inner: io,
        };
        let tls = match tokio::time::timeout(
            STEP_TIMEOUT,
            tokio_rustls::TlsAcceptor::from(config).accept(stream),
        )
        .await
        {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                self.refuse_line(
                    &target.turn,
                    &target.host,
                    target.port,
                    MethodClass::None,
                    EgressReason::Opaque,
                );
                eprintln!(
                    "egress proxy: job {job}: {ua} refused the inspecting proxy's certificate \
                     for {} ({e}). A client that pins its certificates, or does not trust the \
                     bridge's egress CA, cannot reach the web through this proxy.",
                    target.host
                );
                return;
            }
            Err(_) => return,
        };
        let target = Arc::new(target);
        let svc = hyper::service::service_fn(move |req| {
            let me = self.clone();
            let target = target.clone();
            async move { Ok::<_, Infallible>(me.request(req, &target).await) }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(tls), svc)
            .await;
    }

    /// An absolute-form plain HTTP request: authenticated and admitted per request, then the
    /// same per-request path as a terminated tunnel's.
    async fn plain(self: Arc<Self>, req: Request<Incoming>) -> Response<ProxyBody> {
        let uri = req.uri();
        if uri.scheme_str() != Some("http") {
            return text(
                StatusCode::BAD_REQUEST,
                "only http:// is proxied in absolute form; use CONNECT for https\n",
            );
        }
        let host = bare_host(uri.host().unwrap_or(""));
        let port = uri.port_u16().unwrap_or(80);
        let head_class = classify(
            req.method().as_str(),
            uri.path_and_query().map(|p| p.as_str()).unwrap_or("/"),
            usize::from(has_body_hint(req.headers())),
            &self.deps.policy,
        );
        let turn = match self.authenticate(req.headers(), &host, port, head_class) {
            Ok(t) => t,
            Err(r) => return *r,
        };
        let addr = match self.admit(&turn, &host, port, head_class).await {
            Ok(a) => a,
            Err(r) => return *r,
        };
        let target = Target {
            turn,
            host,
            port,
            addr,
            tls: false,
        };
        self.request(req, &target).await
    }

    /// Whether a `send` may go to `host`: the configured send list, or the bridge-owned one
    /// under the state directory (read on each send: it is small and changes at runtime).
    fn send_allowed(&self, host: &str) -> bool {
        if self
            .deps
            .policy
            .send_hosts
            .iter()
            .any(|p| host_matches(p, host))
        {
            return true;
        }
        let path = self.deps.state_dir.join(SEND_HOSTS_FILE);
        match std::fs::read(&path) {
            Ok(b) => match serde_json::from_slice::<Vec<String>>(&b) {
                Ok(list) => list.iter().any(|p| host_matches(p.trim(), host)),
                Err(_) => {
                    eprintln!("egress proxy: {SEND_HOSTS_FILE} is not a JSON array of hosts");
                    false
                }
            },
            Err(_) => false,
        }
    }

    /// Step 5: one request, read whole, classed, checked, forwarded.
    async fn request(self: Arc<Self>, req: Request<Incoming>, t: &Target) -> Response<ProxyBody> {
        let (parts, body) = req.into_parts();
        let method = parts.method.as_str().to_string();
        let pq = parts
            .uri
            .path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "/".into());
        let chunked = has_body_hint(&parts.headers);
        let body = match Limited::new(body, MAX_BODY).collect().await {
            Ok(b) => b.to_bytes(),
            Err(e) => {
                if e.downcast_ref::<http_body_util::LengthLimitError>()
                    .is_some()
                {
                    // Too big to inspect is uninspectable: `opaque`.
                    self.refuse_line(
                        &t.turn,
                        &t.host,
                        t.port,
                        MethodClass::Send,
                        EgressReason::Opaque,
                    );
                    return text(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "the egress proxy reads at most 16 MiB of a request body\n",
                    );
                }
                return text(
                    StatusCode::BAD_REQUEST,
                    "the request body could not be read\n",
                );
            }
        };
        let class = classify(
            &method,
            &pq,
            body.len().max(usize::from(chunked)),
            &self.deps.policy,
        );
        // The request as the origin and the content check see it: no hop-by-hop headers and
        // none of the proxy's own (its credential is the turn token).
        let headers = forwardable(&parts.headers);
        let job = t.turn.job_id.as_str();

        if class == MethodClass::Send
            && t.turn.tier == Egress::OpenLogged
            && !self.send_allowed(&t.host)
        {
            self.refuse_line(&t.turn, &t.host, t.port, class, EgressReason::SendHost);
            self.deps.recorder.record(&InspectedExchange {
                job_id: job,
                host: &t.host,
                port: t.port,
                method: &method,
                target: &pq,
                request_headers: &headers,
                request_body: &body,
                response_status: None,
                response_headers: &[],
            });
            let owner = &self.deps.owner_name;
            return text(
                StatusCode::FORBIDDEN,
                &format!(
                    "Refused: sending data to {} needs {owner}'s approval from the phone. Do \
                     not retry this request or try another route to the same place. Tell \
                     {owner} what you wanted to send to {} and why.\n",
                    t.host, t.host
                ),
            );
        }

        let scheme = if t.tls { "https" } else { "http" };
        let url = format!("{scheme}://{}{pq}", t.host);
        let verdict = {
            let content = self.deps.content.clone();
            let (h, b) = (headers.clone(), body.clone());
            tokio::task::spawn_blocking(move || content.check(&url, &h, &b))
                .await
                .unwrap_or(ContentVerdict::ContentMatch)
        };
        let refused = match verdict {
            ContentVerdict::Clean => None,
            ContentVerdict::Canary => Some(EgressReason::Canary),
            ContentVerdict::ContentMatch => Some(EgressReason::ContentMatch),
        };
        if let Some(reason) = refused {
            self.refuse_line(&t.turn, &t.host, t.port, class, reason);
            self.deps.notifier.refused(job, &t.host, reason);
            self.deps.recorder.record(&InspectedExchange {
                job_id: job,
                host: &t.host,
                port: t.port,
                method: &method,
                target: &pq,
                request_headers: &headers,
                request_body: &body,
                response_status: None,
                response_headers: &[],
            });
            return text(
                StatusCode::FORBIDDEN,
                "Refused: this request carries content from the private notes. It was not \
                 sent, and the refusal has been reported. Do not retry it or re-encode it.\n",
            );
        }

        let bytes_out = body.len() as u64;
        let resp = match self
            .forward(t, &parts.method, &pq, &headers, body.clone())
            .await
        {
            Ok(r) => r,
            Err(e) => {
                self.record(LineFacts {
                    job,
                    tier: t.turn.tier.label(),
                    host: &t.host,
                    port: t.port,
                    class,
                    bytes_out,
                    bytes_in: 0,
                    verdict: EgressVerdict::Allowed,
                    reason: EgressReason::None,
                });
                return text(
                    StatusCode::BAD_GATEWAY,
                    &format!("the egress proxy could not reach {}: {e}\n", t.host),
                );
            }
        };
        let (mut rparts, rbody) = resp.into_parts();
        strip_hop_by_hop(&mut rparts.headers);
        let rheaders: Vec<(String, Vec<u8>)> = rparts
            .headers
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.as_bytes().to_vec()))
            .collect();
        self.deps.recorder.record(&InspectedExchange {
            job_id: job,
            host: &t.host,
            port: t.port,
            method: &method,
            target: &pq,
            request_headers: &headers,
            request_body: &body,
            response_status: Some(rparts.status.as_u16()),
            response_headers: &rheaders,
        });
        // The line is written when the response body has been relayed (or dropped), with
        // the bytes that came in.
        let me = self.clone();
        let (j, tier, host, port) = (job.to_string(), t.turn.tier.label(), t.host.clone(), t.port);
        let done: Box<dyn FnOnce(u64) + Send + Sync> = Box::new(move |bytes_in| {
            me.record(LineFacts {
                job: &j,
                tier,
                host: &host,
                port,
                class,
                bytes_out,
                bytes_in,
                verdict: EgressVerdict::Allowed,
                reason: EgressReason::None,
            })
        });
        let counted = Counting {
            inner: rbody,
            bytes: 0,
            done: Some(done),
        };
        Response::from_parts(rparts, counted.boxed())
    }

    /// Send one request to the checked address over a fresh connection.
    async fn forward(
        &self,
        t: &Target,
        method: &Method,
        pq: &str,
        headers: &[(String, Vec<u8>)],
        body: Bytes,
    ) -> Result<Response<Incoming>, String> {
        let tcp = tokio::time::timeout(STEP_TIMEOUT, tokio::net::TcpStream::connect(t.addr))
            .await
            .map_err(|_| "connect timed out".to_string())?
            .map_err(|e| e.to_string())?;
        let io: Box<dyn Io> = if t.tls {
            let name = rustls::pki_types::ServerName::try_from(t.host.clone())
                .map_err(|_| "not a TLS server name".to_string())?;
            let tls = tokio::time::timeout(
                STEP_TIMEOUT,
                tokio_rustls::TlsConnector::from(self.deps.upstream_tls.clone()).connect(name, tcp),
            )
            .await
            .map_err(|_| "TLS handshake timed out".to_string())?
            .map_err(|e| format!("TLS: {e}"))?;
            Box::new(tls)
        } else {
            Box::new(tcp)
        };
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io))
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let mut req = Request::builder()
            .method(method.clone())
            .uri(pq)
            .body(Full::new(body))
            .map_err(|e| e.to_string())?;
        for (k, v) in headers {
            if k == "host" {
                continue;
            }
            if let (Ok(n), Ok(v)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_bytes(v),
            ) {
                req.headers_mut().append(n, v);
            }
        }
        let default_port = if t.tls { 443 } else { 80 };
        let host_header = match (t.port == default_port, t.host.contains(':')) {
            (true, false) => t.host.clone(),
            (true, true) => format!("[{}]", t.host),
            (false, false) => format!("{}:{}", t.host, t.port),
            (false, true) => format!("[{}]:{}", t.host, t.port),
        };
        req.headers_mut().insert(
            hyper::header::HOST,
            HeaderValue::from_str(&host_header).map_err(|e| e.to_string())?,
        );
        sender.send_request(req).await.map_err(|e| e.to_string())
    }
}

// ---- HELPERS ---------------------------------------------------------------------------

trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

fn empty() -> ProxyBody {
    Full::new(Bytes::new()).map_err(|n| match n {}).boxed()
}

fn text(status: StatusCode, body: &str) -> Response<ProxyBody> {
    let mut r = Response::new(
        Full::new(Bytes::from(body.to_string()))
            .map_err(|n| match n {})
            .boxed(),
    );
    *r.status_mut() = status;
    r.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    r
}

/// `[::1]` → `::1`, lowercase, no trailing dot.
fn bare_host(h: &str) -> String {
    h.trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// The client's `User-Agent`, printable and short, for a stderr line. Not a credential.
fn user_agent(h: &hyper::HeaderMap) -> String {
    let ua: String = h
        .get(hyper::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(120)
        .collect();
    if ua.is_empty() {
        "a client with no User-Agent".into()
    } else {
        format!("client {ua:?}")
    }
}

/// Whether the head promises a body: a non-zero `Content-Length`, or chunked framing.
fn has_body_hint(h: &hyper::HeaderMap) -> bool {
    let cl = h
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let chunked = h.get_all(hyper::header::TRANSFER_ENCODING).iter().any(|v| {
        v.to_str()
            .map(|s| s.to_ascii_lowercase().contains("chunked"))
            .unwrap_or(false)
    });
    cl > 0 || chunked
}

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

fn connection_named(h: &hyper::HeaderMap) -> Vec<String> {
    h.get_all(hyper::header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn strip_hop_by_hop(h: &mut hyper::HeaderMap) {
    for name in connection_named(h) {
        h.remove(name.as_str());
    }
    for name in HOP_BY_HOP {
        h.remove(*name);
    }
}

/// The request headers that leave: lowercase names, no hop-by-hop or proxy headers, and no
/// `content-length` (the body is re-framed).
fn forwardable(h: &hyper::HeaderMap) -> Vec<(String, Vec<u8>)> {
    let named = connection_named(h);
    h.iter()
        .filter(|(k, _)| {
            let k = k.as_str();
            !HOP_BY_HOP.contains(&k) && k != "content-length" && !named.iter().any(|n| n == k)
        })
        .map(|(k, v)| (k.as_str().to_string(), v.as_bytes().to_vec()))
        .collect()
}

/// A stream that yields one already-read byte before the rest: the byte the proxy peeked to
/// tell TLS from anything else.
struct Prefixed<S> {
    first: Option<u8>,
    inner: S,
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if let Some(b) = self.first {
            if buf.remaining() > 0 {
                buf.put_slice(&[b]);
                self.first = None;
                return Poll::Ready(Ok(()));
            }
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// A response body that counts the bytes it relays and reports the count once, when the
/// body ends or is dropped (a client that hangs up mid-body still gets its line).
struct Counting {
    inner: Incoming,
    bytes: u64,
    done: Option<Box<dyn FnOnce(u64) + Send + Sync>>,
}

impl Counting {
    fn finish(&mut self) {
        if let Some(f) = self.done.take() {
            f(self.bytes);
        }
    }
}

impl Body for Counting {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        let r = Pin::new(&mut self.inner).poll_frame(cx);
        match &r {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(d) = frame.data_ref() {
                    self.bytes += d.len() as u64;
                }
                if self.inner.is_end_stream() {
                    self.finish();
                }
            }
            Poll::Ready(None) | Poll::Ready(Some(Err(_))) => self.finish(),
            Poll::Pending => {}
        }
        r
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for Counting {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::ca::tests::tmp;
    use crate::proxy::content::tests::{fixture_vault, PARAGRAPH};
    use crate::proxy::content::{canary_new, FingerprintIndex};
    use std::collections::HashMap;
    use std::net::Ipv6Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tokio::io::AsyncWriteExt;

    // ---- fakes ----

    struct FakeAuthority(HashMap<(String, String), Egress>);

    impl TurnAuthority for FakeAuthority {
        fn egress_turn(&self, job: &str, token: &str) -> Option<EgressTurn> {
            self.0
                .get(&(job.to_string(), token.to_string()))
                .map(|tier| EgressTurn {
                    job_id: job.to_string(),
                    tier: tier.clone(),
                })
        }
        fn model_turn(&self, _: &str, _: &str) -> Option<super::super::ModelTurn> {
            None
        }
    }

    #[derive(Default)]
    struct Notes(Mutex<Vec<(String, String, EgressReason)>>);

    impl RefusalNotifier for Notes {
        fn refused(&self, job: &str, host: &str, reason: EgressReason) {
            self.0
                .lock()
                .unwrap()
                .push((job.to_string(), host.to_string(), reason));
        }
    }

    struct MapResolver(HashMap<String, SocketAddr>);

    impl Resolver for MapResolver {
        fn resolve<'a>(&'a self, host: &'a str, _port: u16) -> ResolveFuture<'a> {
            let r = self
                .0
                .get(host)
                .map(|a| vec![*a])
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such test host"));
            Box::pin(async move { r })
        }
    }

    // ---- origins ----

    /// Echoes what arrived: method, target, body length, whether a proxy credential leaked
    /// through, and the Host header.
    async fn echo(
        req: Request<Incoming>,
        hits: Arc<AtomicUsize>,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        hits.fetch_add(1, Ordering::SeqCst);
        let method = req.method().clone();
        let uri = req.uri().clone();
        let pa = req.headers().contains_key("proxy-authorization");
        let host = req
            .headers()
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = req
            .into_body()
            .collect()
            .await
            .map(|b| b.to_bytes())
            .unwrap_or_default();
        Ok(Response::new(Full::new(Bytes::from(format!(
            "{method} {uri} body={} pa={pa} host={host}",
            body.len()
        )))))
    }

    struct Origins {
        http: SocketAddr,
        https: SocketAddr,
        trust: Arc<rustls::ClientConfig>,
        hits: Arc<AtomicUsize>,
    }

    const TLS_HOSTS: &[&str] = &["secure.test", "send.test", "json.test", "other.test"];

    async fn origins() -> Origins {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, PKCS_ECDSA_P256_SHA256};
        let hits = Arc::new(AtomicUsize::new(0));
        let serve =
            |hits: Arc<AtomicUsize>| hyper::service::service_fn(move |req| echo(req, hits.clone()));

        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http = l.local_addr().unwrap();
        let h = hits.clone();
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                let svc = serve(h.clone());
                tokio::spawn(
                    hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(s), svc),
                );
            }
        });

        let ca_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let mut cap = CertificateParams::new(Vec::<String>::new()).unwrap();
        cap.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = cap.self_signed(&ca_key).unwrap();
        let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let leaf =
            CertificateParams::new(TLS_HOSTS.iter().map(|h| h.to_string()).collect::<Vec<_>>())
                .unwrap()
                .signed_by(&leaf_key, &ca, &ca_key)
                .unwrap();
        let provider = || Arc::new(rustls::crypto::ring::default_provider());
        let mut sc = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![leaf.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
            )
            .unwrap();
        sc.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(sc));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let https = l.local_addr().unwrap();
        let h = hits.clone();
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                let (acceptor, svc) = (acceptor.clone(), serve(h.clone()));
                tokio::spawn(async move {
                    if let Ok(tls) = acceptor.accept(s).await {
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(tls), svc)
                            .await;
                    }
                });
            }
        });

        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.der().clone()).unwrap();
        let mut trust = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        trust.alpn_protocols = vec![b"http/1.1".to_vec()];
        Origins {
            http,
            https,
            trust: Arc::new(trust),
            hits,
        }
    }

    // ---- the harness ----

    struct Harness {
        port: u16,
        state: PathBuf,
        ca: Arc<ProxyCa>,
        notes: Arc<Notes>,
        origins: Origins,
    }

    const OPEN: &str = "j-open:t-open";
    const ALLOW: &str = "j-allow:t-allow";
    const CLOSED: &str = "j-closed:t-closed";

    /// An allowlist that names the private test addresses too, so the private-address rule
    /// (not the tier) is what refuses them.
    fn allowlist() -> Egress {
        Egress::Allowlist(
            [
                "secure.test",
                "origin.test",
                "private.test",
                "10.20.0.1",
                "127.0.0.1",
                "100.64.1.1",
                "169.254.1.1",
                "::1",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        )
    }

    async fn harness(
        loopback: bool,
        policy: EgressPolicy,
        index: Option<FingerprintIndex>,
    ) -> Harness {
        let origins = origins().await;
        let state = tmp("egress");
        let ca = Arc::new(ProxyCa::load_or_create(&state).unwrap());
        let content = Arc::new(ContentCheck::new(&state, policy.fingerprint_threshold));
        if let Some(i) = index {
            content.set_index(i);
        }
        let mut map = HashMap::new();
        map.insert("origin.test".to_string(), origins.http);
        for h in TLS_HOSTS {
            map.insert(h.to_string(), origins.https);
        }
        map.insert("private.test".into(), "10.20.0.1:443".parse().unwrap());
        let mut turns = HashMap::new();
        for (creds, tier) in [
            (OPEN, Egress::OpenLogged),
            (ALLOW, allowlist()),
            (CLOSED, Egress::Closed),
        ] {
            let (j, t) = creds.split_once(':').unwrap();
            turns.insert((j.to_string(), t.to_string()), tier);
        }
        let notes = Arc::new(Notes::default());
        let deps = EgressDeps {
            authority: Arc::new(FakeAuthority(turns)),
            ledger: Some(Arc::new(ActivityLedger::new(&state))),
            notifier: notes.clone(),
            recorder: Arc::new(super::super::NoRecorder),
            ca: ca.clone(),
            content,
            policy,
            state_dir: state.clone(),
            resolver: Arc::new(MapResolver(map)),
            upstream_tls: origins.trust.clone(),
            owner_name: "Jeremy".into(),
        };
        let proxy = Arc::new(if loopback {
            EgressProxy::with_loopback_for_tests(deps)
        } else {
            EgressProxy::new(deps)
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(proxy.serve(l));
        Harness {
            port,
            state,
            ca,
            notes,
            origins,
        }
    }

    impl Harness {
        fn client(&self, creds: Option<&str>, trust_proxy_ca: bool) -> reqwest::Client {
            let url = match creds {
                Some(c) => format!("http://{c}@127.0.0.1:{}", self.port),
                None => format!("http://127.0.0.1:{}", self.port),
            };
            let mut b = reqwest::Client::builder()
                .proxy(reqwest::Proxy::all(url).unwrap())
                .user_agent("egress-test/1")
                .timeout(Duration::from_secs(10));
            if trust_proxy_ca {
                b = b.add_root_certificate(
                    reqwest::Certificate::from_pem(self.ca.cert_pem().as_bytes()).unwrap(),
                );
            }
            b.build().unwrap()
        }

        fn ledger_text(&self) -> String {
            let day = chrono::Local::now().format("%Y-%m-%d").to_string();
            std::fs::read_to_string(self.state.join("activity").join(format!("{day}.jsonl")))
                .unwrap_or_default()
        }

        /// The ledger's lines, once there are at least `n` (a response's line is written
        /// when its body has been relayed, which can trail the client by a moment).
        async fn lines(&self, n: usize) -> Vec<serde_json::Value> {
            for _ in 0..300 {
                let t = self.ledger_text();
                if t.lines().count() >= n {
                    return t
                        .lines()
                        .map(|l| serde_json::from_str(l).unwrap())
                        .collect();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("expected {n} ledger lines, have:\n{}", self.ledger_text());
        }

        fn hits(&self) -> usize {
            self.origins.hits.load(Ordering::SeqCst)
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.state);
        }
    }

    fn reasons(lines: &[serde_json::Value]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l["reason"].as_str().unwrap().to_string())
            .collect()
    }

    // ---- pure rules ----

    #[test]
    fn private_addresses_cover_every_range() {
        for a in [
            "10.20.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "127.0.0.1",
            "127.8.8.8",
            "169.254.169.254",
            "100.64.1.1",
            "100.127.255.255",
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "febf::1",
            "ff02::1",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::a00:1",
            "64:ff9b::7f00:1",
            "::127.0.0.1",
        ] {
            assert!(is_private(a.parse().unwrap()), "{a} is private");
        }
        for a in [
            "8.8.8.8",
            "1.1.1.1",
            "172.32.0.1",
            "172.15.0.1",
            "100.128.0.1",
            "100.63.0.1",
            "192.169.0.1",
            "223.255.255.255",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
        ] {
            assert!(!is_private(a.parse().unwrap()), "{a} is public");
        }
        assert!(is_private(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    }

    #[test]
    fn classification_at_each_boundary() {
        let p = EgressPolicy::default();
        let url = |n: usize| format!("/{}", "a".repeat(n - 1));
        assert_eq!(classify("GET", &url(512), 0, &p), MethodClass::Read);
        assert_eq!(classify("GET", &url(513), 0, &p), MethodClass::Send);
        let q = |n: usize| format!("/?{}", "q".repeat(n));
        assert_eq!(classify("GET", &q(256), 0, &p), MethodClass::Read);
        assert_eq!(classify("GET", &q(257), 0, &p), MethodClass::Send);
        assert_eq!(classify("HEAD", "/", 0, &p), MethodClass::Read);
        assert_eq!(classify("OPTIONS", "/", 0, &p), MethodClass::Read);
        assert_eq!(classify("get", "/", 0, &p), MethodClass::Read);
        assert_eq!(classify("GET", "/", 1, &p), MethodClass::Send);
        assert_eq!(classify("POST", "/", 0, &p), MethodClass::Send);
        assert_eq!(classify("PUT", "/", 0, &p), MethodClass::Send);
        assert_eq!(classify("DELETE", "/", 0, &p), MethodClass::Send);
    }

    // ---- end to end ----

    #[tokio::test]
    async fn no_credentials_get_a_challenge_and_bad_ones_a_ledger_line() {
        let h = harness(true, EgressPolicy::default(), None).await;
        let r = h
            .client(None, true)
            .get("http://origin.test/")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 407);
        assert!(r.headers()["proxy-authenticate"]
            .to_str()
            .unwrap()
            .contains("realm=\"jesse-egress\""));
        assert_eq!(
            h.ledger_text(),
            "",
            "a missing credential is the normal first round trip"
        );

        let r = h
            .client(Some("j-open:wrong-token"), true)
            .get("http://origin.test/")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 407);
        assert!(h
            .client(Some("j-open:wrong-token"), true)
            .get("https://secure.test/")
            .send()
            .await
            .is_err());
        let lines = h.lines(2).await;
        assert_eq!(reasons(&lines), ["auth", "auth"]);
        assert_eq!(lines[0]["job_id"], "j-open");
        assert_eq!(lines[1]["method_class"], "none");
        assert!(!h.ledger_text().contains("wrong-token"));
        assert_eq!(h.hits(), 0);
    }

    #[tokio::test]
    async fn a_closed_turn_refuses_connect_and_terminates_nothing() {
        let h = harness(true, EgressPolicy::default(), None).await;
        assert!(h
            .client(Some(CLOSED), true)
            .get("https://secure.test/x")
            .send()
            .await
            .is_err());
        let r = h
            .client(Some(CLOSED), true)
            .get("http://origin.test/x")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        let lines = h.lines(2).await;
        assert_eq!(reasons(&lines), ["tier", "tier"]);
        assert_eq!(lines[0]["tier"], "closed");
        assert_eq!(lines[0]["method_class"], "none");
        assert_eq!(lines[0]["verdict"], "refused");
        assert!(!h.ca.has_leaf("secure.test"), "no certificate was minted");
        assert_eq!(h.hits(), 0);
    }

    #[tokio::test]
    async fn an_open_turn_reads_and_the_ledger_has_the_host_only() {
        let h = harness(true, EgressPolicy::default(), None).await;
        let c = h.client(Some(OPEN), true);
        let body = c
            .get("https://secure.test/secret-path?q=private-words")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(
            body,
            "GET /secret-path?q=private-words body=0 pa=false host=secure.test"
        );
        // The same tunnel serves the next request (keep-alive), each with its own line.
        let again = c.get("https://secure.test/second").send().await.unwrap();
        assert_eq!(again.status(), 200);
        let plain = c
            .get("http://origin.test/plain")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(plain.starts_with("GET /plain body=0 pa=false"), "{plain}");
        let lines = h.lines(3).await;
        assert_eq!(lines.len(), 3);
        let l = &lines[0];
        assert_eq!(l["kind"], "egress");
        assert_eq!(l["host"], "secure.test");
        assert_eq!(l["port"], 443);
        assert_eq!(l["tier"], "open-logged");
        assert_eq!(l["method_class"], "read");
        assert_eq!(l["verdict"], "allowed");
        assert_eq!(l["reason"], "none");
        assert_eq!(l["bytes_in"], body.len() as u64);
        assert_eq!(
            (lines[2]["host"].as_str(), lines[2]["port"].as_u64()),
            (Some("origin.test"), Some(80))
        );
        let text = h.ledger_text();
        for leak in ["secret-path", "private-words", "t-open", "/plain"] {
            assert!(!text.contains(leak), "the ledger carries {leak}");
        }
    }

    #[tokio::test]
    async fn private_addresses_are_refused_on_every_tier() {
        let h = harness(false, EgressPolicy::default(), None).await;
        let local = format!("http://127.0.0.1:{}/", h.origins.http.port());
        let urls = [
            "http://10.20.0.1/",
            local.as_str(),
            "http://100.64.1.1/",
            "http://169.254.1.1/",
            "http://[::1]/",
            "http://private.test/",
        ];
        for creds in [OPEN, ALLOW] {
            let c = h.client(Some(creds), true);
            for u in urls {
                let r = c.get(u).send().await.unwrap();
                assert_eq!(r.status(), 403, "{creds} {u}");
            }
            assert!(c.get("https://private.test/").send().await.is_err());
            assert!(c.get("https://127.0.0.1/").send().await.is_err());
        }
        let lines = h.lines(16).await;
        assert!(
            reasons(&lines).iter().all(|r| r == "private_address"),
            "{lines:?}"
        );
        assert!(lines.iter().any(|l| l["host"] == "::1"));
        assert!(lines.iter().any(|l| l["tier"] == "allowlist"));
        // Closed refuses before it resolves anything.
        let r = h
            .client(Some(CLOSED), true)
            .get("http://10.20.0.1/")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        assert_eq!(reasons(&h.lines(17).await)[16], "tier");
        assert_eq!(h.hits(), 0);
    }

    #[tokio::test]
    async fn an_allowlist_admits_its_hosts_only() {
        let h = harness(true, EgressPolicy::default(), None).await;
        let c = h.client(Some(ALLOW), true);
        assert_eq!(
            c.get("https://secure.test/").send().await.unwrap().status(),
            200
        );
        assert_eq!(
            c.get("http://origin.test/").send().await.unwrap().status(),
            200
        );
        // An allowlist admits sends to its hosts, without a send list.
        let r = c
            .post("https://secure.test/up")
            .body("payload")
            .send()
            .await
            .unwrap();
        assert_eq!(
            r.text().await.unwrap(),
            "POST /up body=7 pa=false host=secure.test"
        );
        assert!(c.get("https://other.test/").send().await.is_err());
        let lines = h.lines(4).await;
        assert_eq!(reasons(&lines), ["none", "none", "none", "tier"]);
        assert_eq!(lines[2]["method_class"], "send");
        assert_eq!(lines[2]["bytes_out"], 7);
        assert_eq!(h.hits(), 3);
    }

    #[tokio::test]
    async fn sends_need_the_send_list_and_reads_do_not() {
        let policy = EgressPolicy {
            send_hosts: vec!["send.test".into()],
            ..EgressPolicy::default()
        };
        let h = harness(true, policy, None).await;
        std::fs::write(h.state.join(SEND_HOSTS_FILE), r#"["json.test"]"#).unwrap();
        let c = h.client(Some(OPEN), true);

        let r = c
            .post("https://secure.test/x")
            .body("hi")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        let msg = r.text().await.unwrap();
        assert!(
            msg.contains("sending data to secure.test needs Jeremy's approval from the phone"),
            "{msg}"
        );
        assert!(msg.contains("Do not retry"));
        assert_eq!(
            c.get("https://secure.test/x")
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        // A GET with a body is a send.
        assert_eq!(
            c.get("https://secure.test/x")
                .body("b")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        // The query limit, both sides of it.
        let q = |n: usize| format!("https://secure.test/?{}", "q".repeat(n));
        assert_eq!(c.get(q(256)).send().await.unwrap().status(), 200);
        assert_eq!(c.get(q(257)).send().await.unwrap().status(), 403);
        // The URL limit, both sides of it (path-plus-query 512 vs 513).
        let u = |n: usize| format!("https://secure.test/{}", "a".repeat(n - 1));
        assert_eq!(c.get(u(512)).send().await.unwrap().status(), 200);
        assert_eq!(c.get(u(513)).send().await.unwrap().status(), 403);
        assert_eq!(
            c.head("https://secure.test/h")
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let r = c
            .request(Method::OPTIONS, "https://secure.test/o")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        // The configured send list and the bridge-owned one.
        let r = c
            .post("https://send.test/x")
            .body("hi")
            .send()
            .await
            .unwrap();
        assert_eq!(
            r.text().await.unwrap(),
            "POST /x body=2 pa=false host=send.test"
        );
        let r = c
            .post("https://json.test/x")
            .body("hi")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        let lines = h.lines(10).await;
        let got: Vec<(String, String)> = lines
            .iter()
            .map(|l| {
                (
                    l["method_class"].as_str().unwrap().to_string(),
                    l["reason"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let want = [
            ("send", "send_host"),
            ("read", "none"),
            ("send", "send_host"),
            ("read", "none"),
            ("send", "send_host"),
            ("read", "none"),
            ("send", "send_host"),
            ("read", "none"),
            ("read", "none"),
            ("send", "none"),
        ];
        let want: Vec<(String, String)> = want
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        assert_eq!(got[..10], want[..]);
        assert_eq!(h.hits(), 7);
        assert!(
            h.notes.0.lock().unwrap().is_empty(),
            "a send-host refusal is not content"
        );
    }

    #[tokio::test]
    async fn a_canary_in_a_base64_query_is_refused_and_reported() {
        let h = harness(true, EgressPolicy::default(), None).await;
        let vault = tmp("egress-canary-vault");
        let canary = canary_new(&h.state, &vault, "codes.md").unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(canary.marker.as_bytes());
        let c = h.client(Some(OPEN), true);
        let r = c
            .get(format!("https://secure.test/search?d={b64}"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        assert!(!r.text().await.unwrap().contains(&canary.marker));
        let lines = h.lines(1).await;
        assert_eq!(reasons(&lines), ["canary"]);
        assert_eq!(
            *h.notes.0.lock().unwrap(),
            vec![(
                "j-open".to_string(),
                "secure.test".to_string(),
                EgressReason::Canary
            )]
        );
        assert!(!h.ledger_text().contains(&canary.marker));
        assert_eq!(h.hits(), 0);
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[tokio::test]
    async fn a_vault_paragraph_posted_to_a_send_host_is_refused() {
        let vault = fixture_vault();
        let policy = EgressPolicy {
            send_hosts: vec!["send.test".into()],
            ..EgressPolicy::default()
        };
        let h = harness(true, policy, Some(FingerprintIndex::build(&vault))).await;
        let c = h.client(Some(OPEN), true);
        let r = c
            .post("https://send.test/paste")
            .body(PARAGRAPH)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        let r = c
            .post("https://send.test/paste")
            .body("A short note about the weather being pleasant this afternoon.")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let lines = h.lines(2).await;
        assert_eq!(reasons(&lines), ["content_match", "none"]);
        assert_eq!(
            *h.notes.0.lock().unwrap(),
            vec![(
                "j-open".to_string(),
                "send.test".to_string(),
                EgressReason::ContentMatch
            )]
        );
        assert_eq!(h.hits(), 1);
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[tokio::test]
    async fn opaque_bytes_after_connect_are_refused() {
        let h = harness(true, EgressPolicy::default(), None).await;
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", h.port))
            .await
            .unwrap();
        let auth = base64::engine::general_purpose::STANDARD.encode(OPEN);
        s.write_all(
            format!(
                "CONNECT secure.test:443 HTTP/1.1\r\nHost: secure.test:443\r\n\
                 Proxy-Authorization: Basic {auth}\r\nUser-Agent: raw-test\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut b).await.unwrap();
            head.push(b[0]);
        }
        assert!(
            head.starts_with(b"HTTP/1.1 200"),
            "{}",
            String::from_utf8_lossy(&head)
        );
        s.write_all(b"GET / HTTP/1.1\r\nHost: secure.test\r\n\r\n")
            .await
            .unwrap();
        let mut rest = Vec::new();
        let n = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut rest))
            .await
            .expect("the proxy closes the tunnel")
            .unwrap_or(0);
        assert_eq!(n, 0, "nothing came back");
        let lines = h.lines(1).await;
        assert_eq!(reasons(&lines), ["opaque"]);
        assert_eq!(lines[0]["method_class"], "none");
        assert_eq!(h.hits(), 0);
    }

    #[tokio::test]
    async fn a_client_that_does_not_trust_the_proxy_ca_fails_and_is_opaque() {
        let h = harness(true, EgressPolicy::default(), None).await;
        let err = h
            .client(Some(OPEN), false)
            .get("https://secure.test/")
            .send()
            .await;
        assert!(err.is_err());
        let lines = h.lines(1).await;
        assert_eq!(reasons(&lines), ["opaque"]);
        assert_eq!(lines[0]["host"], "secure.test");
        assert_eq!(h.hits(), 0);
    }
}
