//! Replay tests: determinism, fault cost, high-water.

use crate::{ScriptScenario, ScriptStep};

use crate::harness::replay::run_scenario;

fn turn() -> ScriptScenario {
    ScriptScenario::clean(vec![
        ScriptStep { stage: "endpoint_hold".to_string(), latency_ms: 300, peak_bytes: 1024 },
        ScriptStep { stage: "stt".to_string(), latency_ms: 80, peak_bytes: 2048 },
        ScriptStep { stage: "llm_first_token".to_string(), latency_ms: 150, peak_bytes: 4096 },
        ScriptStep { stage: "first_clause".to_string(), latency_ms: 120, peak_bytes: 1024 },
        ScriptStep { stage: "tts_first_audio".to_string(), latency_ms: 100, peak_bytes: 2048 },
        ScriptStep { stage: "playout".to_string(), latency_ms: 40, peak_bytes: 512 },
    ])
}

#[test]
fn replay_is_deterministic() {
    let (first, first_bytes) = run_scenario(&turn());
    let (second, second_bytes) = run_scenario(&turn());
    assert_eq!(first, second);
    assert_eq!(first_bytes, second_bytes);
    assert_eq!(first.events.len(), 6 * 4);
}

#[test]
fn faults_cost_latency_not_data() {
    let scenario = ScriptScenario { steps: turn().steps, fault_every: 2 };
    let (log, _) = run_scenario(&scenario);
    let clean = run_scenario(&turn()).0;
    assert!(log.events.len() > clean.events.len());
    let kinds: Vec<crate::harness::events::BenchKind> =
        log.events.iter().map(|event| event.kind).collect();
    assert!(kinds.contains(&crate::harness::events::BenchKind::SendPcm));
}
