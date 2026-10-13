//! **Named HTTP credentials and mutual TLS** (0.14.0), against loopback servers.
//!
//! The server records every request's headers, so each assertion is about what crossed the
//! wire: a bearer and an `x-api-key` header read from NAMED variables at call time, an unset
//! variable failing the call without a value in the error, and a client certificate presented
//! to a server that demands one and pins the client to one CA.
//!
//! The certificates are minted per run with `rcgen`; no private key is committed.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jesse_agent::mcp::{HttpAuth, McpClient, MutualTls};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

type Headers = Vec<(String, String)>;

/// One connection, any number of requests on it. Answers the four messages with fixed replies.
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut sock: S,
    log: Arc<Mutex<Vec<Headers>>>,
) -> std::io::Result<()> {
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
        let headers: Headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        let len: usize = headers
            .iter()
            .find(|(k, _)| k == "content-length")
            .and_then(|(_, v)| v.parse().ok())
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
        log.lock().unwrap().push(headers);

        let (status, reply) = if request_line.starts_with("DELETE") {
            ("405 Method Not Allowed", String::new())
        } else {
            let msg: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            match msg.get("id").cloned() {
                None => ("202 Accepted", String::new()),
                Some(id) => {
                    let result = match msg["method"].as_str() {
                        Some("initialize") => json!({
                            "protocolVersion": "2025-06-18",
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "mock-auth", "version": "0.0.1"},
                        }),
                        Some("tools/list") => json!({"tools": [
                            {"name": "ping", "description": "p", "inputSchema": {"type": "object"}},
                        ]}),
                        _ => json!({"content": [{"type": "text", "text": "pong"}]}),
                    };
                    (
                        "200 OK",
                        json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
                    )
                }
            }
        };
        let mut out = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", reply.len());
        if !reply.is_empty() {
            out.push_str("Content-Type: application/json\r\nMcp-Session-Id: s-1\r\n");
        }
        out.push_str("\r\n");
        sock.write_all(out.as_bytes()).await?;
        sock.write_all(reply.as_bytes()).await?;
        sock.flush().await?;
    }
}

async fn plain_server() -> (String, Arc<Mutex<Vec<Headers>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = log.clone();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let l = l.clone();
            tokio::spawn(async move {
                let _ = serve(sock, l).await;
            });
        }
    });
    (url, log)
}

const T: Duration = Duration::from_secs(5);

fn header(h: &Headers, name: &str) -> Option<String> {
    h.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

#[tokio::test]
async fn a_named_bearer_and_a_named_header_are_read_from_their_variables_on_every_message() {
    // Unique names, so a parallel test can never see or clobber them.
    std::env::set_var("JESSE_TEST_NAMED_BEARER_81f2", "bearer-v1");
    std::env::set_var("JESSE_TEST_NAMED_APIKEY_81f2", "apikey-v1");
    let (url, log) = plain_server().await;
    let auth = HttpAuth {
        bearer_env: Some("JESSE_TEST_NAMED_BEARER_81f2".into()),
        header_env: vec![("x-api-key".into(), "JESSE_TEST_NAMED_APIKEY_81f2".into())],
        ..HttpAuth::default()
    };
    let c = McpClient::connect_http_with("named", &url, &auth, T, T)
        .await
        .expect("connects");
    // A ROTATED VALUE is picked up by the next message: read at call time, not at connect.
    std::env::set_var("JESSE_TEST_NAMED_APIKEY_81f2", "apikey-v2");
    c.call_tool("ping", json!({})).await.expect("calls");
    let seen = log.lock().unwrap().clone();
    assert!(seen.len() >= 3, "initialize, the notification and the call");
    for h in &seen[..2] {
        assert_eq!(
            header(h, "authorization").as_deref(),
            Some("Bearer bearer-v1")
        );
        assert_eq!(header(h, "x-api-key").as_deref(), Some("apikey-v1"));
    }
    let last = seen.last().unwrap();
    assert_eq!(header(last, "x-api-key").as_deref(), Some("apikey-v2"));
    // The struct names variables and holds no value, so its Debug cannot print one.
    let dbg = format!("{auth:?}");
    assert!(
        !dbg.contains("bearer-v1") && !dbg.contains("apikey-v"),
        "{dbg}"
    );
}

#[tokio::test]
async fn an_unset_credential_variable_fails_the_message_and_names_only_the_variable() {
    std::env::remove_var("JESSE_TEST_UNSET_VAR_c03e");
    let (url, log) = plain_server().await;
    let auth = HttpAuth {
        header_env: vec![("x-api-key".into(), "JESSE_TEST_UNSET_VAR_c03e".into())],
        ..HttpAuth::default()
    };
    let err = McpClient::connect_http_with("unset", &url, &auth, T, T)
        .await
        .expect_err("no value, no request");
    let text = err.to_string();
    assert!(text.contains("JESSE_TEST_UNSET_VAR_c03e"), "{text}");
    assert!(text.contains("'unset'"), "{text}");
    assert!(
        log.lock().unwrap().is_empty(),
        "nothing is sent without its credential"
    );
}

// ---- mutual TLS -----------------------------------------------------------------------

struct Pki {
    dir: PathBuf,
    ca: rcgen::Certificate,
    ca_key: rcgen::KeyPair,
}

impl Pki {
    fn new(tag: &str) -> Pki {
        let dir = std::env::temp_dir().join(format!(
            "jesse-mtls-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        p.distinguished_name
            .push(rcgen::DnType::CommonName, format!("test ca {tag}"));
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let ca = p.self_signed(&ca_key).unwrap();
        std::fs::write(dir.join("ca.pem"), ca.pem()).unwrap();
        Pki { dir, ca, ca_key }
    }

    /// Issue a leaf for `name`, write it and its key as `<file>.pem` / `<file>.key`.
    fn issue(&self, name: &str, file: &str) -> (rcgen::Certificate, rcgen::KeyPair) {
        let p = rcgen::CertificateParams::new(vec![name.to_string()]).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = p.signed_by(&key, &self.ca, &self.ca_key).unwrap();
        std::fs::write(self.dir.join(format!("{file}.pem")), cert.pem()).unwrap();
        std::fs::write(self.dir.join(format!("{file}.key")), key.serialize_pem()).unwrap();
        (cert, key)
    }

    fn path(&self, f: &str) -> PathBuf {
        self.dir.join(f)
    }
}

impl Drop for Pki {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A TLS server that DEMANDS a client certificate issued by `client_ca`.
async fn mtls_server(server_pki: &Pki, client_ca: &Path) -> (String, Arc<Mutex<Vec<Headers>>>) {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let (cert, key) = server_pki.issue("localhost", "server");
    let mut roots = rustls::RootCertStore::empty();
    let ca_pem = std::fs::read(client_ca).unwrap();
    let ca_der = pem_to_der(&ca_pem);
    roots.add(CertificateDer::from(ca_der)).unwrap();
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()
    .unwrap();
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            vec![cert.der().clone()],
            PrivateKeyDer::try_from(key.serialize_der()).unwrap(),
        )
        .unwrap();
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "https://localhost:{}/mcp",
        listener.local_addr().unwrap().port()
    );
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = log.clone();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let l = l.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(tls) = acceptor.accept(sock).await {
                    let _ = serve(tls, l).await;
                }
            });
        }
    });
    (url, log)
}

/// The body of the first PEM block, decoded. Enough for one certificate.
fn pem_to_der(pem: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(pem);
    let b64: String = text
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .concat();
    decode_b64(&b64)
}

fn decode_b64(s: &str) -> Vec<u8> {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes().filter(|c| *c != b'=') {
        let v = A.iter().position(|a| *a == c).unwrap() as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

#[tokio::test]
async fn mutual_tls_presents_the_client_certificate_and_pins_the_server_ca() {
    let pki = Pki::new("ok");
    pki.issue("jesse-bridge", "client");
    let (url, log) = mtls_server(&pki, &pki.path("ca.pem")).await;
    let auth = HttpAuth {
        mtls: Some(MutualTls {
            cert_file: pki.path("client.pem"),
            key_file: pki.path("client.key"),
            ca_file: pki.path("ca.pem"),
        }),
        ..HttpAuth::default()
    };
    let c = McpClient::connect_http_with("mac", &url, &auth, T, T)
        .await
        .expect("an issued client certificate is accepted");
    let out = c.call_tool("ping", json!({})).await.expect("calls");
    assert_eq!(out.content[0]["text"], "pong");
    assert!(log.lock().unwrap().len() >= 3);
}

#[tokio::test]
async fn without_a_client_certificate_the_server_refuses_the_handshake() {
    let pki = Pki::new("nocert");
    let (url, log) = mtls_server(&pki, &pki.path("ca.pem")).await;
    // The server's CA is trusted, but no identity is presented.
    let ca = std::fs::read(pki.path("ca.pem")).unwrap();
    let auth = HttpAuth {
        ca_pem: Some(ca),
        ..HttpAuth::default()
    };
    McpClient::connect_http_with("mac", &url, &auth, T, T)
        .await
        .expect_err("no client certificate, no session");
    assert!(log.lock().unwrap().is_empty(), "no request got through");
}

#[tokio::test]
async fn a_server_certificate_from_another_ca_is_refused_by_the_pin() {
    let server_pki = Pki::new("server");
    let client_pki = Pki::new("client");
    client_pki.issue("jesse-bridge", "client");
    // The server accepts the client's CA, but its OWN certificate is from a different CA than
    // the one the client pins.
    let (url, log) = mtls_server(&server_pki, &client_pki.path("ca.pem")).await;
    let auth = HttpAuth {
        mtls: Some(MutualTls {
            cert_file: client_pki.path("client.pem"),
            key_file: client_pki.path("client.key"),
            ca_file: client_pki.path("ca.pem"),
        }),
        ..HttpAuth::default()
    };
    McpClient::connect_http_with("mac", &url, &auth, T, T)
        .await
        .expect_err("the pin refuses a server from another CA");
    assert!(log.lock().unwrap().is_empty());
}
