//! Both read-only database servers, as BUILT BINARIES speaking MCP over stdio: their names and
//! tool lists, and that each refuses to start without its own two paths.
//!
//! No database is needed: `initialize` and `tools/list` never connect, and a `tools/call` with
//! no `sql` is rejected before connecting. The URL file points at port 1 on loopback.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const HEALTHDB_BIN: &str = env!("CARGO_BIN_EXE_jesse-healthdb-mcp");
const HOUSEDB_BIN: &str = env!("CARGO_BIN_EXE_jesse-housedb-mcp");

// A throwaway self signed certificate, the one `healthdb`'s tests use; its key was discarded.
const TEST_CA: &str = "-----BEGIN CERTIFICATE-----
MIIBhTCCASugAwIBAgIUf7+srFnwxd1aXzCFFKxUaJzBry4wCgYIKoZIzj0EAwIw
GDEWMBQGA1UEAwwNamVzc2UtdGVzdC1jYTAeFw0yNjEwMTIwNTUxMjFaFw0zNjEw
MDkwNTUxMjFaMBgxFjAUBgNVBAMMDWplc3NlLXRlc3QtY2EwWTATBgcqhkjOPQIB
BggqhkjOPQMBBwNCAASdD4O1d/5BFppaL8IGuf89onvHaY8nZ2PmKskMDnTaKuJx
xs5jauHhL17bA9RQNfgLIR2leo9EKdiGwoocYDSio1MwUTAdBgNVHQ4EFgQUd3J/
0Dk/160GRHuhV09rr5CQ4KIwHwYDVR0jBBgwFoAUd3J/0Dk/160GRHuhV09rr5CQ
4KIwDwYDVR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAgNIADBFAiEAnbML+lQyKoiV
FyhnU3eUVjnxaYmYvgKxZxvZ8MGUeSUCICGoxhH3xvPW0Nv7U6X6/tz83oZ0JeyD
Htby8Y5sp501
-----END CERTIFICATE-----
";

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(tag: &str) -> Dir {
    let d = std::env::temp_dir().join(format!(
        "rodb-stdio-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&d).unwrap();
    let url = d.join("ro-url");
    std::fs::write(&url, "postgres://ro:SECRETPW@127.0.0.1:1/db\n").unwrap();
    std::fs::set_permissions(&url, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(d.join("ca.crt"), TEST_CA).unwrap();
    Dir(d)
}

/// Run `bin` with `env`, feed it `requests` one per line, and return its replies and stderr.
fn session(bin: &str, env: &[(&str, &Path)], requests: &[Value]) -> (Vec<Value>, String, i32) {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn");
    {
        let mut stdin = child.stdin.take().unwrap();
        for r in requests {
            writeln!(stdin, "{r}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    let replies = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    (
        replies,
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn requests() -> Vec<Value> {
    vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ]
}

fn names(list: &Value) -> Vec<String> {
    list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn healthdb_answers_as_itself_with_its_two_tools() {
    let d = scratch("health");
    let ledger = d.0.join("ledger.log");
    let (r, _, code) = session(
        HEALTHDB_BIN,
        &[
            ("JESSE_HEALTHDB_RO_URL_FILE", &d.0.join("ro-url")),
            ("JESSE_HEALTHDB_CA_FILE", &d.0.join("ca.crt")),
            ("JESSE_HEALTHDB_MCP_LEDGER", &ledger),
        ],
        &requests(),
    );
    assert_eq!(code, 0);
    assert_eq!(
        r[0]["result"]["serverInfo"]["name"],
        json!("jesse-healthdb-mcp")
    );
    assert_eq!(r[0]["result"]["protocolVersion"], json!("2024-11-05"));
    assert_eq!(names(&r[1]), ["healthdb_schema", "healthdb_query"]);
}

#[test]
fn housedb_answers_as_itself_with_its_two_tools() {
    let d = scratch("house");
    let ledger = d.0.join("ledger.log");
    let mut reqs = requests();
    reqs.push(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": {"name": "housedb_query", "arguments": {}}}));
    reqs.push(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                     "params": {"name": "healthdb_query", "arguments": {"sql": "SELECT 1"}}}));
    let (r, stderr, code) = session(
        HOUSEDB_BIN,
        &[
            ("JESSE_HOUSEDB_RO_URL_FILE", &d.0.join("ro-url")),
            ("JESSE_HOUSEDB_CA_FILE", &d.0.join("ca.crt")),
            ("JESSE_HOUSEDB_MCP_LEDGER", &ledger),
        ],
        &reqs,
    );
    assert_eq!(code, 0);
    assert_eq!(
        r[0]["result"]["serverInfo"]["name"],
        json!("jesse-housedb-mcp")
    );
    assert_eq!(names(&r[1]), ["housedb_schema", "housedb_query"]);
    // Rejected before connecting, as a tool failure the turn can read.
    assert_eq!(r[2]["result"]["isError"], json!(true));
    assert_eq!(
        r[2]["result"]["content"][0]["text"],
        json!("housedb_query takes one argument, `sql`, a string")
    );
    // The other server's tool is not this one's.
    assert_eq!(r[3]["error"]["code"], json!(-32602));
    // One content-free stderr line and one ledger line for the call; never the URL.
    assert!(
        stderr.contains("jesse-housedb-mcp: housedb_query rejected in"),
        "{stderr}"
    );
    assert!(!stderr.contains("SECRETPW"));
    let ledger = std::fs::read_to_string(&ledger).unwrap();
    assert!(ledger.contains(r#""tool":"housedb_query""#), "{ledger}");
    assert!(!ledger.contains("SECRETPW"));
}

#[test]
fn each_server_refuses_to_start_on_the_other_servers_variables() {
    let d = scratch("cross");
    // housedb given healthdb's variables: its own are unset, so it exits before any traffic.
    let (r, stderr, code) = session(
        HOUSEDB_BIN,
        &[
            ("JESSE_HEALTHDB_RO_URL_FILE", &d.0.join("ro-url")),
            ("JESSE_HEALTHDB_CA_FILE", &d.0.join("ca.crt")),
        ],
        &requests(),
    );
    assert_eq!(code, 1);
    assert!(r.is_empty());
    assert_eq!(
        stderr.trim(),
        "jesse-housedb-mcp: JESSE_HOUSEDB_RO_URL_FILE is not set"
    );
    let (_, stderr, code) = session(
        HEALTHDB_BIN,
        &[
            ("JESSE_HOUSEDB_RO_URL_FILE", &d.0.join("ro-url")),
            ("JESSE_HOUSEDB_CA_FILE", &d.0.join("ca.crt")),
        ],
        &requests(),
    );
    assert_eq!(code, 1);
    assert_eq!(
        stderr.trim(),
        "jesse-healthdb-mcp: JESSE_HEALTHDB_RO_URL_FILE is not set"
    );
}
