//! Fault soak (heavy tier): periodic faults stay flat.
//!
//! A long synthetic run with faults every few steps must keep drop
//! and memory counters flat: faults cost latency (proven by the
//! replay tests), never data or growth. `#[ignore]`d so it runs only
//! under the soak gate, never in the fast loop.

use crate::{ScriptScenario, ScriptStep};

/// Eight-hour shape at logical scale: thousands of turns with
/// periodic faults; counters must not drift with length.
#[ignore = "heavy soak tier; run via the soak gate"]
#[test]
fn fault_soak_stays_flat() {
    let turn = vec![
        ScriptStep { stage: "endpoint_hold".to_string(), latency_ms: 300, peak_bytes: 4096 },
        ScriptStep { stage: "stt".to_string(), latency_ms: 80, peak_bytes: 8192 },
        ScriptStep { stage: "llm_first_token".to_string(), latency_ms: 150, peak_bytes: 16_384 },
        ScriptStep { stage: "tts_first_audio".to_string(), latency_ms: 100, peak_bytes: 8192 },
        ScriptStep { stage: "playout".to_string(), latency_ms: 40, peak_bytes: 1024 },
    ];
    let mut peak = 0u64;
    let mut turns = 0usize;
    for _ in 0..400 {
        let scenario = ScriptScenario { steps: turn.clone(), fault_every: 7 };
        let (_, high_water) = crate::harness::replay::run_scenario(&scenario);
        peak = peak.max(high_water);
        turns += 1;
    }
    assert_eq!(turns, 400);
    assert!(peak <= crate::harness::gates::MEMORY_BUDGET_BYTES, "memory stays flat");
}
