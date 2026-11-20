//! `jesse-healthdb-mcp` — the health archive, read-only, spoken as MCP over stdio.
//!
//! The logic, every guard and the transport live in [`jesse_bridge::healthquery`], shared with
//! `jesse-housedb-mcp`; this file names the server and nothing else, for the reason
//! `jesse-places-mcp` is a transport: a `"command": "jesse-healthdb-mcp"` entry is a BARE NAME
//! resolved from the child's `PATH` and reads identically everywhere.
//!
//! It is started with two paths in its environment, `JESSE_HEALTHDB_RO_URL_FILE` and
//! `JESSE_HEALTHDB_CA_FILE`, and reads the read-only role's URL itself, with the same `0600`
//! and owner checks the bridge makes. It never sees the owner's credential.
//!
//! One stderr line per call: the tool, the outcome, the duration and the row count. Never the
//! SQL, a value, an error message or the URL.

#[tokio::main]
async fn main() {
    jesse_bridge::serve_ro_db_stdio(&jesse_bridge::HEALTHDB).await;
}
