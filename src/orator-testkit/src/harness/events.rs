//! Compatibility re-export: the bench log and render live in
//! `orator::bench` (single home); the replay harness composes
//! them from here.

pub use orator::bench::{BenchEvent, BenchKind, BenchLog};

#[cfg(test)]
#[path = "../../tests/harness_events.rs"]
mod tests;
