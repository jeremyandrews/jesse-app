//! `jesse_eval` — the offline eval harness for the Jesse assistant, as a library.
//!
//! One suite definition ([`Suite`], [`Task`]), one runner ([`run_suite`]), one set of
//! assertions ([`assertions`]), and a [`Driver`] seam an out of tree runner can implement.
//! The `jesse-eval` binary is a thin CLI over this crate: it parses flags, picks one of the
//! drivers in [`driver`], builds a [`RunConfig`] and calls [`run_suite`]. Anything that
//! executes a task some other way implements [`Driver`] and goes through the same runner,
//! so its results are scored by the same assertions and reported in the same scorecard.

pub mod assertions;
pub mod compare;
pub mod driver;
pub mod judge;
pub mod mapping;
pub mod mock;
pub mod runner;
pub mod state;
pub mod suite;
#[cfg(test)]
mod suite_checks;
pub mod transcript;
pub mod workspace;

// ---- The seam, at the crate root -----------------------------------------------------
//
// Exactly what an out of tree crate names to implement a driver and run a suite through
// it; `eval/tests/out_of_tree_driver.rs` is written against these paths and nothing else.

pub use driver::{BoxFuture, Driver, PreparedWorkspace, TaskRun};
pub use runner::{run_suite, RunConfig, RunReport};
pub use suite::{Suite, Task};

/// Re-exported because [`RunConfig::prices`] has this type: a caller that builds a
/// [`RunConfig`] must name it, and taking it from here keeps that caller on the same
/// `jesse-agent` this crate was built against.
pub use jesse_agent::PriceDeck;

/// Re-exported because [`Driver::run_task`] takes one: an implementation must name it in
/// its signature, and taking it from here keeps the implementation on the same
/// `tokio-util` this crate was built against.
pub use tokio_util::sync::CancellationToken;
