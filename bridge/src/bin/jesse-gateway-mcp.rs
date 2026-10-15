//! `jesse-gateway-mcp` — the credential-free stub a gateway-on child starts in place of every
//! `gateway` MCP server.
//!
//! ```text
//! jesse-gateway-mcp --upstream <name> --socket <path>
//! jesse-gateway-mcp --upstream <name> --url <base> --token-file <path>
//! ```
//!
//! A stdio MCP server to the child, and nothing behind it but ONE connection to the bridge's
//! gateway: a unix socket under the turn's `0700` directory on the Studio, or `POST
//! <base>/mcp/<name>` with the turn token in the cluster. It forwards the four messages of the
//! gateway's subset (`initialize`, `notifications/initialized`, `tools/list`, `tools/call`)
//! and relays every response. NOTHING ELSE CROSSES: any other notification is dropped, and any
//! other request is answered HERE with "method not found" and goes no further. That is not a
//! courtesy. A client probes for protocol features it might use (Claude Code 2.1.296 opens
//! every server with `server/discover` and falls back to `initialize` on method-not-found),
//! and the gateway closes a session on the first message outside its subset, so a stub that
//! forwarded the probe would lose the server before it was ever initialised. The gateway's
//! close stays the rule for anything that reaches it by any other route.
//!
//! **IT READS NO ENVIRONMENT VARIABLE**, holds nothing but the token file's contents (URL
//! mode), opens nothing but its one connection, and exits when stdin closes. That is the whole
//! point of it: whatever a child can do to this process, there is no credential in it.
//!
//! **NO BRIDGE LIBRARY.** It uses `std` and `serde_json` only, so it builds without the bridge
//! crate's heavy dependencies being linked into it, and a reviewer can read all of it here.
//! URL mode speaks plain HTTP/1.1: inside the cluster, transport security is the network's.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::ExitCode;

const SUBSET: [&str; 4] = [
    "initialize",
    "notifications/initialized",
    "tools/list",
    "tools/call",
];

/// The largest response the stub will relay in URL mode. Matches the bridge's own cap on an
/// image result with headroom; it bounds memory, not anything a turn needs.
const MAX_RESPONSE_BYTES: usize = 48 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Socket(String),
    Url { base: String, token_file: String },
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    upstream: String,
    mode: Mode,
}

const USAGE: &str = "usage: jesse-gateway-mcp --upstream <name> --socket <path>\n       \
                     jesse-gateway-mcp --upstream <name> --url <base> --token-file <path>";

/// Exactly `--upstream` plus one of the two modes. Anything else, including a repeated flag,
/// is refused.
fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut upstream = None;
    let mut socket = None;
    let mut url = None;
    let mut token_file = None;
    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let slot = match flag.as_str() {
            "--upstream" => &mut upstream,
            "--socket" => &mut socket,
            "--url" => &mut url,
            "--token-file" => &mut token_file,
            other => return Err(format!("unknown argument {other:?}")),
        };
        if slot.is_some() {
            return Err(format!("{flag} given twice"));
        }
        let v = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        if v.is_empty() {
            return Err(format!("{flag} needs a value"));
        }
        *slot = Some(v.clone());
    }
    let upstream = upstream.ok_or("--upstream is required")?;
    if !upstream
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("{upstream:?} is not a server name"));
    }
    let mode = match (socket, url, token_file) {
        (Some(s), None, None) => Mode::Socket(s),
        (None, Some(base), Some(token_file)) => {
            if !base.starts_with("http://") {
                return Err("--url must be an http:// base URL".into());
            }
            Mode::Url { base, token_file }
        }
        _ => {
            return Err("give either --socket, or --url with --token-file".into());
        }
    };
    Ok(Args { upstream, mode })
}

/// What the stub does with one line from the child.
#[derive(Debug, PartialEq)]
enum Route {
    /// One of the four: send it to the gateway.
    Forward,
    /// A request outside the subset: answer it here with this "method not found" line.
    Answer(String),
    /// A notification outside the subset, a response, or not JSON: nothing.
    Drop,
}

fn route(line: &str) -> Route {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return Route::Drop;
    };
    match (v.get("method").and_then(|m| m.as_str()), v.get("id")) {
        (Some(m), _) if SUBSET.contains(&m) => Route::Forward,
        (Some(m), Some(id)) => Route::Answer(
            serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {
                "code": -32601,
                "message": format!("method not found: {m}"),
            }})
            .to_string(),
        ),
        // A notification outside the subset, or a response to a server request (the gateway
        // never sends one): nothing to do.
        _ => Route::Drop,
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("jesse-gateway-mcp: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match &args.mode {
        Mode::Socket(path) => run_socket(path),
        Mode::Url { base, token_file } => run_url(&args.upstream, base, token_file),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("jesse-gateway-mcp ({}): {e}", args.upstream);
            ExitCode::FAILURE
        }
    }
}

/// Pump lines both ways over one unix socket until stdin or the socket closes.
fn run_socket(path: &str) -> Result<(), String> {
    let sock = std::os::unix::net::UnixStream::connect(path)
        .map_err(|e| format!("cannot reach the gateway: {e}"))?;
    let mut to_gateway = sock
        .try_clone()
        .map_err(|e| format!("cannot use the gateway socket: {e}"))?;
    // Gateway -> child, on its own thread. When the gateway closes the connection (the turn
    // ended, or it refused a message), the whole process ends.
    std::thread::spawn(move || {
        let reader = BufReader::new(sock);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            // Locked PER LINE: the main thread answers out-of-subset requests on the same
            // stdout, and a lock held for the life of this thread would starve it.
            let mut out = std::io::stdout().lock();
            if writeln!(out, "{line}").is_err() || out.flush().is_err() {
                break;
            }
        }
        std::process::exit(0);
    });
    // Child -> gateway. A request outside the subset is answered on stdout from here, under
    // the same lock the relay thread writes with, so two replies never interleave.
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        match route(&line) {
            Route::Forward => {
                if writeln!(to_gateway, "{line}").is_err() {
                    break;
                }
            }
            Route::Answer(reply) => {
                let mut out = std::io::stdout().lock();
                if writeln!(out, "{reply}").is_err() || out.flush().is_err() {
                    break;
                }
            }
            Route::Drop => {}
        }
    }
    let _ = to_gateway.shutdown(std::net::Shutdown::Both);
    Ok(())
}

/// One `POST` per message, carrying the token and, after `initialize`, the session id.
fn run_url(upstream: &str, base: &str, token_file: &str) -> Result<(), String> {
    let token = std::fs::read_to_string(token_file)
        .map_err(|e| format!("cannot read the token file: {e}"))?
        .trim()
        .to_string();
    let rest = base.trim_start_matches("http://").trim_end_matches('/');
    let (host, prefix) = match rest.split_once('/') {
        Some((h, p)) => (h.to_string(), format!("/{p}")),
        None => (rest.to_string(), String::new()),
    };
    let path = format!("{prefix}/mcp/{upstream}");
    let mut session: Option<String> = None;
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        match route(&line) {
            Route::Forward => {}
            Route::Answer(reply) => {
                if writeln!(out, "{reply}").is_err() || out.flush().is_err() {
                    break;
                }
                continue;
            }
            Route::Drop => continue,
        }
        let id = serde_json::from_str::<serde_json::Value>(&line)
            .ok()
            .and_then(|v| v.get("id").cloned());
        let resp = post(&host, &path, &token, session.as_deref(), &line);
        let reply = match resp {
            Ok(r) => {
                if let Some(s) = r.session {
                    session = Some(s);
                }
                match r.status {
                    200 if !r.body.is_empty() => Some(r.body),
                    200 | 202 => None,
                    status => id.map(|id| {
                        serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {
                            "code": -32603,
                            "message": format!("the gateway answered HTTP {status}"),
                        }})
                        .to_string()
                    }),
                }
            }
            Err(e) => id.map(|id| {
                serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {
                    "code": -32603, "message": format!("cannot reach the gateway: {e}"),
                }})
                .to_string()
            }),
        };
        if let Some(reply) = reply {
            let reply = reply.replace('\n', " ");
            if writeln!(out, "{reply}").is_err() || out.flush().is_err() {
                break;
            }
        }
    }
    Ok(())
}

struct HttpReply {
    status: u16,
    session: Option<String>,
    body: String,
}

fn post(
    host: &str,
    path: &str,
    token: &str,
    session: Option<&str>,
    body: &str,
) -> Result<HttpReply, String> {
    let mut s = std::net::TcpStream::connect(host).map_err(|e| e.to_string())?;
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         Content-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(sid) = session {
        req.push_str(&format!("Mcp-Session-Id: {sid}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    s.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    s.take(MAX_RESPONSE_BYTES as u64 + 64 * 1024)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    parse_reply(&raw)
}

fn parse_reply(raw: &[u8]) -> Result<HttpReply, String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("a reply with no header end")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or("a reply with no status")?;
    let mut session = None;
    let mut chunked = false;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "mcp-session-id" => session = Some(v.trim().to_string()),
                "transfer-encoding" => chunked = v.trim().eq_ignore_ascii_case("chunked"),
                _ => {}
            }
        }
    }
    let mut body = raw[split + 4..].to_vec();
    if chunked {
        body = dechunk(&body)?;
    }
    if body.len() > MAX_RESPONSE_BYTES {
        return Err("the gateway's reply is too large".into());
    }
    Ok(HttpReply {
        status,
        session,
        body: String::from_utf8_lossy(&body).trim().to_string(),
    })
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let eol = b
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("a broken chunk")?;
        let size = usize::from_str_radix(
            String::from_utf8_lossy(&b[..eol])
                .split(';')
                .next()
                .unwrap_or("")
                .trim(),
            16,
        )
        .map_err(|_| "a broken chunk size")?;
        b = &b[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size + 2 {
            return Err("a short chunk".into());
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn exactly_upstream_and_one_mode_parse() {
        assert_eq!(
            parse_args(&a(&["--upstream", "slack", "--socket", "/s/slack.sock"])),
            Ok(Args {
                upstream: "slack".into(),
                mode: Mode::Socket("/s/slack.sock".into())
            })
        );
        assert_eq!(
            parse_args(&a(&[
                "--upstream",
                "google-perseido",
                "--url",
                "http://bridge:7443",
                "--token-file",
                "/t/token",
            ])),
            Ok(Args {
                upstream: "google-perseido".into(),
                mode: Mode::Url {
                    base: "http://bridge:7443".into(),
                    token_file: "/t/token".into()
                }
            })
        );
    }

    #[test]
    fn anything_else_is_refused() {
        for bad in [
            &["--socket", "/s"][..],
            &["--upstream", "slack"],
            &[
                "--upstream",
                "slack",
                "--socket",
                "/s",
                "--url",
                "http://x",
                "--token-file",
                "/t",
            ],
            &["--upstream", "slack", "--url", "http://x"],
            &["--upstream", "slack", "--token-file", "/t"],
            &["--upstream", "slack", "--socket", "/s", "--env", "X"],
            &[
                "--upstream",
                "slack",
                "--upstream",
                "github",
                "--socket",
                "/s",
            ],
            &["--upstream", "slack", "--socket"],
            &["--upstream", "../etc", "--socket", "/s"],
            &[
                "--upstream",
                "slack",
                "--url",
                "https://x",
                "--token-file",
                "/t",
            ],
        ] {
            assert!(parse_args(&a(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn only_the_subset_is_forwarded_and_any_other_request_is_answered_here() {
        for m in SUBSET {
            let msg = if m.starts_with("notifications/") {
                format!(r#"{{"jsonrpc":"2.0","method":"{m}"}}"#)
            } else {
                format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{m}"}}"#)
            };
            assert_eq!(route(&msg), Route::Forward, "{m}");
        }
        assert_eq!(
            route(r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#),
            Route::Drop
        );
        // Claude Code's opening probe: answered method-not-found, never forwarded.
        let Route::Answer(a) =
            route(r#"{"jsonrpc":"2.0","id":"server-discover-probe-1","method":"server/discover"}"#)
        else {
            panic!("a request outside the subset is answered here");
        };
        let a: serde_json::Value = serde_json::from_str(&a).unwrap();
        assert_eq!(a["id"], "server-discover-probe-1");
        assert_eq!(a["error"]["code"], -32601);
        assert!(matches!(
            route(r#"{"jsonrpc":"2.0","id":2,"method":"resources/list"}"#),
            Route::Answer(_)
        ));
        assert_eq!(
            route(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#),
            Route::Drop
        );
        assert_eq!(route("not json"), Route::Drop);
    }

    #[test]
    fn a_reply_is_parsed_with_its_session_and_chunked_body() {
        let r = parse_reply(
            b"HTTP/1.1 200 OK\r\nMcp-Session-Id: abc\r\nTransfer-Encoding: chunked\r\n\r\n\
              5\r\nhello\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(
            (r.status, r.session.as_deref(), r.body.as_str()),
            (200, Some("abc"), "hello")
        );
        let r = parse_reply(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n").unwrap();
        assert_eq!(r.status, 401);
    }

    /// The source reads no environment variable. `std::env::args` is the one `env` call.
    #[test]
    fn the_stub_reads_no_environment_variable() {
        let src = include_str!("jesse-gateway-mcp.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let needle = ["env", "::var"].concat();
        assert!(!code.contains(&needle));
        let needle = ["env", "::vars"].concat();
        assert!(!code.contains(&needle));
        let needle = ["jesse", "_bridge"].concat();
        assert!(
            !code.contains(&needle),
            "the stub must not link the bridge library"
        );
    }
}
