//! `jesse-housedb-mcp` — the house's history (Home Assistant's recorder), read-only, spoken as
//! MCP over stdio.
//!
//! The same server as `jesse-healthdb-mcp`, over a different database: the logic, every guard
//! and the transport live in [`jesse_bridge::healthquery`], and what is particular to the house
//! (the tool names, the guide, the schema order, `[housedb]`) in
//! [`jesse_bridge::housequery`]. This file names the server and nothing else.
//!
//! It is started with two paths in its environment, `JESSE_HOUSEDB_RO_URL_FILE` and
//! `JESSE_HOUSEDB_CA_FILE`, and reads the `house_ro` role's URL itself, with the same `0600` and
//! owner checks the bridge makes.
//!
//! One stderr line per call: the tool, the outcome, the duration and the row count. Never the
//! SQL, a value, an error message or the URL.

#[tokio::main]
async fn main() {
    jesse_bridge::serve_ro_db_stdio(&jesse_bridge::HOUSEDB).await;
}
