//! Capability-gated I/O primitive engines (fs, net, db).
//!
//! The capability-gating primitives (`CapabilityError`, `check_capability`,
//! `CURRENT_CAPS`) are owned by `fluent-wvr` — the canonical home of the
//! capability model — and re-exported here so the existing
//! `fluent_concurrency::io::*` paths keep working. The pooled database
//! capability (`DbCapability`) lives in `fluent-db` and is re-exported from
//! the `db` submodule.

pub use fluent_wvr::capability::{check_capability, CapabilityError};

#[cfg(feature = "db")]
pub mod db;
pub mod fs;
pub mod net;

#[cfg(test)]
#[path = "../../tests/io_mod.rs"]
mod tests;
