//! Compatibility re-export: the bench log and render live in
//! `orator-core::bench` (single home); the replay harness composes
//! them from here.

pub use orator_core::bench::{BenchEvent, BenchKind, BenchLog};

#[cfg(test)]
#[path = "../tests/events.rs"]
mod tests;
