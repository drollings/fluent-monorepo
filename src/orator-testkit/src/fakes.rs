//! Deterministic fakes: scripted clocks, scenarios, and faults.
//!
//! Every fake runs on logical time: the clock advances only when the
//! scenario says so, so replays are bit-identical across machines and
//! CI gates measure the script, never the scheduler.

/// Deterministic millisecond clock.
#[derive(Debug, Clone, Default)]
pub struct FakeClock {
    now_ms: u64,
}

impl FakeClock {
    /// Clock starting at zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current logical time in ms.
    #[must_use]
    pub const fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// Advance by `ms`.
    pub fn advance(&mut self, ms: u64) {
        self.now_ms = self.now_ms.saturating_add(ms);
    }
}

/// One scripted pipeline step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptStep {
    /// Stage name (`vad`, `stt`, `llm`, `tts`, `playout`).
    pub stage: String,
    /// Scripted compute latency in ms.
    pub latency_ms: u64,
    /// Simulated peak bytes held during this step.
    pub peak_bytes: u64,
}

/// A scripted scenario: ordered steps plus periodic faults.
#[derive(Debug, Clone, Default)]
pub struct ScriptScenario {
    /// Steps in run order.
    pub steps: Vec<ScriptStep>,
    /// Every `fault_every` steps (0 = no faults), the step drops once
    /// and retries (costs one extra latency pass, never loses data).
    pub fault_every: usize,
}

impl ScriptScenario {
    /// Scenario with no faults.
    #[must_use]
    pub fn clean(steps: Vec<ScriptStep>) -> Self {
        Self { steps, fault_every: 0 }
    }

    /// Whether step `index` faults.
    #[must_use]
    pub fn faults_at(&self, index: usize) -> bool {
        self.fault_every > 0 && index > 0 && index % self.fault_every == 0
    }
}

/// Programmable latency multiplier for anti-tautology probes.
#[derive(Debug, Clone)]
pub struct SlowFake {
    /// Latency multiplier (1 = real-time script).
    pub factor: u64,
}

impl SlowFake {
    /// Scale `scenario` latencies by `factor`.
    #[must_use]
    pub fn scale(&self, scenario: &ScriptScenario) -> ScriptScenario {
        ScriptScenario {
            steps: scenario
                .steps
                .iter()
                .map(|step| ScriptStep {
                    stage: step.stage.clone(),
                    latency_ms: step.latency_ms.saturating_mul(self.factor),
                    peak_bytes: step.peak_bytes,
                })
                .collect(),
            fault_every: scenario.fault_every,
        }
    }
}

#[cfg(test)]
#[path = "../tests/fakes.rs"]
mod tests;
