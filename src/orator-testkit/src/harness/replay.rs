//! Deterministic replay: scripts in, timestamped logs out.
//!
//! The fake clock advances by scripted latencies only — wall time
//! never enters the log, so CI compares numbers, not schedulers.
//! Faulted steps pay one extra latency pass (drop + retry) without
//! losing data, which is exactly what the soak asserts stays flat.

use crate::{FakeClock, ScriptScenario};

use crate::harness::events::{BenchKind, BenchLog};

/// Replay `scenario` onto a fresh clock. Returns the log plus the
/// simulated memory high-water mark in bytes.
pub fn run_scenario(scenario: &ScriptScenario) -> (BenchLog, u64) {
    let mut clock = FakeClock::new();
    let mut log = BenchLog::new();
    let mut high_water = 0u64;
    for (index, step) in scenario.steps.iter().enumerate() {
        log.push(BenchKind::InputPcm, &step.stage, clock.now_ms());
        clock.advance(step.latency_ms);
        log.push(BenchKind::Step, &step.stage, clock.now_ms());
        log.push(BenchKind::StepPostSampling, &step.stage, clock.now_ms());
        if scenario.faults_at(index) {
            // Drop + retry: one more latency pass, data intact.
            clock.advance(step.latency_ms);
            log.push(BenchKind::Step, &step.stage, clock.now_ms());
            log.push(BenchKind::StepPostSampling, &step.stage, clock.now_ms());
        }
        log.push(BenchKind::SendPcm, &step.stage, clock.now_ms());
        high_water = high_water.max(step.peak_bytes);
    }
    (log, high_water)
}

#[cfg(test)]
#[path = "../../tests/harness_replay.rs"]
mod tests;
