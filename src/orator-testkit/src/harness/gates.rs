//! Regression gates over replayed numbers, not adjectives.
//!
//! Ceilings come from the latency budget: per-stage maxima plus the
//! end-to-end envelope (about 0.7–1.0 s GPU, 1.2–2.0 s CPU-only with
//! small models). STT must stay faster than realtime; drops must be
//! zero on a clean replay; memory must fit the high-water budget.

use std::collections::HashMap;

use crate::harness::events::{BenchKind, BenchLog};

/// Per-stage latency ceilings in ms (the budget table).
pub const STAGE_CEILINGS: [(&str, u64); 6] = [
    ("endpoint_hold", 400),
    ("stt", 80),
    ("llm_first_token", 150),
    ("first_clause", 120),
    ("tts_first_audio", 100),
    ("playout", 40),
];

/// End-to-end envelope in ms (GPU profile).
pub const END_TO_END_MS: u64 = 1_000;

/// End-to-end envelope in ms (CPU-only profile).
pub const END_TO_END_CPU_MS: u64 = 2_000;

/// Memory high-water budget in bytes (512 MiB of resident audio + weights).
pub const MEMORY_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

/// Gate failures. Each names the blown ceiling.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GateError {
    /// A stage exceeded its ceiling.
    #[error("stage {0} took {1} ms (ceiling {2} ms)")]
    Stage(String, u64, u64),
    /// End-to-end exceeded the envelope.
    #[error("end-to-end took {0} ms (envelope {1} ms)")]
    EndToEnd(u64, u64),
    /// STT ran slower than realtime.
    #[error("STT realtime factor {0} < 1.0")]
    Realtime(String),
    /// Drops observed on a clean replay.
    #[error("drops observed: {0}")]
    Drops(u64),
    /// Memory high-water exceeded the budget.
    #[error("memory high-water {0} bytes over budget {1}")]
    Memory(u64, u64),
}

/// Which hardware profile gates the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Mid-range GPU with an 8B 4-bit-quantized model.
    Gpu,
    /// CPU-only with small models.
    Cpu,
}

/// Stage latencies in ms derived from the log: `SendPcm − InputPcm`
/// per stage occurrence, maximum per stage.
#[must_use]
pub fn stage_latencies(log: &BenchLog) -> HashMap<String, u64> {
    let mut open: HashMap<String, u64> = HashMap::new();
    let mut max: HashMap<String, u64> = HashMap::new();
    for event in &log.events {
        match event.kind {
            BenchKind::InputPcm => {
                open.insert(event.stage.clone(), event.t_ms);
            }
            BenchKind::SendPcm => {
                if let Some(start) = open.remove(&event.stage) {
                    let spent = event.t_ms.saturating_sub(start);
                    max.entry(event.stage.clone())
                        .and_modify(|known| *known = (*known).max(spent))
                        .or_insert(spent);
                }
            }
            BenchKind::Step | BenchKind::StepPostSampling => {}
        }
    }
    max
}

/// End-to-end ms: first `InputPcm` to last `SendPcm`.
#[must_use]
pub fn end_to_end_ms(log: &BenchLog) -> u64 {
    let first = log.events.iter().map(|event| event.t_ms).min().unwrap_or(0);
    let last = log.events.iter().map(|event| event.t_ms).max().unwrap_or(0);
    last.saturating_sub(first)
}

/// Gate the replay: stages, envelope, realtime factor, drops, memory.
/// `audio_ms` is the scripted audio length for the realtime check;
/// `drops` counts lost units (zero on clean replays).
pub fn gate(
    log: &BenchLog,
    profile: Profile,
    audio_ms: u64,
    processing_ms: u64,
    drops: u64,
    high_water_bytes: u64,
) -> Result<(), GateError> {
    for (stage, ceiling) in STAGE_CEILINGS {
        if let Some(spent) = stage_latencies(log).get(stage) {
            if *spent > ceiling {
                return Err(GateError::Stage(stage.to_string(), *spent, ceiling));
            }
        }
    }
    let envelope = match profile {
        Profile::Gpu => END_TO_END_MS,
        Profile::Cpu => END_TO_END_CPU_MS,
    };
    let total = end_to_end_ms(log);
    if total > envelope {
        return Err(GateError::EndToEnd(total, envelope));
    }
    if processing_ms > 0 && audio_ms < processing_ms {
        return Err(GateError::Realtime(format!(
            "{audio_ms}/{processing_ms}"
        )));
    }
    if drops > 0 {
        return Err(GateError::Drops(drops));
    }
    if high_water_bytes > MEMORY_BUDGET_BYTES {
        return Err(GateError::Memory(high_water_bytes, MEMORY_BUDGET_BYTES));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/harness_gates.rs"]
mod tests;
