#![forbid(unsafe_code)]

//! orator-testkit: Deterministic fakes for every engine trait: scripted audio scenarios, programmable latency, fault injection.

pub mod fakes;

pub use fakes::{FakeClock, ScriptScenario, ScriptStep, SlowFake};
