#![forbid(unsafe_code)]

//! orator-bench: Deterministic replay harness and regression gates for latency, throughput, memory, and drops.

pub mod events;
pub mod gates;
pub mod replay;
pub mod scenarios;
pub mod stats;
pub use events::{BenchEvent, BenchKind, BenchLog};
pub use gates::{gate, Profile, END_TO_END_CPU_MS, END_TO_END_MS, MEMORY_BUDGET_BYTES, STAGE_CEILINGS};
pub use replay::run_scenario;
pub use stats::{histograms, render_stats, Stats};

#[cfg(test)]
#[path = "../tests/soak.rs"]
mod soak_tests;
