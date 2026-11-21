//! **THE BRIDGE'S INTEGRATION TESTS, AS ONE TEST BINARY.**
//!
//! Every file under `tests/it/` is a module of this one target, so a test build links the
//! library into one integration binary rather than one per file. Each module was a top level
//! `tests/<name>.rs` until the suite was merged; its tests keep their names, with the module
//! as a prefix (`integration::a_bridge_with_no_profile_reports_home`).
//!
//! A NEW TEST GOES IN A MODULE HERE, never in a new `tests/<name>.rs`:
//! `scripts/ci-guards.sh` fails on a top level test file that is not one of the exceptions
//! below. See `docs/testing.md`.
//!
//! THE EXCEPTIONS, each its own binary because it needs a process image of its own:
//!
//!   * `tests/gateway_fds.rs` counts every descriptor the process holds, so no other test may
//!     open or close one while it runs.
//!   * `tests/hostrun_env_file.rs` has no test harness: it points the process's own stderr at a
//!     file to read back the bridge's log.
//!   * `tests/gateway.rs` sets process wide environment (`PATH` and the probe's `JESSE_GWPROBE_*`
//!     variables) that every child the gateway starts inherits; in a shared binary under plain
//!     `cargo test` those values would reach other modules' children.
//!
//! Feature gated modules are gated here with `#[cfg]`, in place of the `required-features` the
//! separate targets had.

#[path = "../common/mod.rs"]
mod common;

mod argv_split_fixture;
mod buildsvc_sandbox;
mod codex_live_turn;
mod codex_session_home;
mod codex_streaming;
mod codex_write_turn;
#[cfg(feature = "containment-probe")]
mod containment;
#[cfg(feature = "devjob-live")]
mod devjob_live;
mod direct_driver;
mod harness_registry;
mod healthdb_mcp_pg;
mod healthdb_pg;
mod hostrun_attribution;
mod hostrun_live;
mod hostrun_review_live;
mod integration;
mod places_provider;
mod platform_inventory;
mod reminders_mcp;
mod rules_scenarios;
mod scheduler;
mod sentinel;
mod shadow_audit;
mod writelock_live;
