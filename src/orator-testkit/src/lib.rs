#![forbid(unsafe_code)]

//! orator-testkit: deterministic fakes and the replay harness.
//!
//! `fakes` is the zero-dependency core: scripted audio scenarios,
//! programmable latency, fault injection. `harness` is the
//! deterministic replay runner and regression gates for latency,
//! throughput, memory, and drops, composed over the fakes and the
//! bench vocabulary in `orator`.

pub mod fakes;
pub mod harness;

pub use fakes::{FakeClock, ScriptScenario, ScriptStep, SlowFake};
pub use harness::{gate, run_scenario, BenchEvent, BenchKind, BenchLog, Profile};
