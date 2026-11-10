//! Gate tests: green replay passes, every blown ceiling fails —
//! including the anti-tautology probe (a 10x-slowed fake must fail).

use crate::{ScriptScenario, ScriptStep, SlowFake};

use crate::harness::gates::{end_to_end_ms, gate, stage_latencies, Profile};
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
fn green_replay_passes_every_gate() {
    let (log, high_water) = run_scenario(&turn());
    assert_eq!(end_to_end_ms(&log), 790);
    assert_eq!(stage_latencies(&log).get("stt"), Some(&80));
    gate(&log, Profile::Gpu, 2000, 790, 0, high_water).expect("green passes");
    gate(&log, Profile::Cpu, 2000, 790, 0, high_water).expect("green passes on cpu");
}

#[test]
fn slow_fake_fails_the_gate() {
    let slow = SlowFake { factor: 10 }.scale(&turn());
    let (log, high_water) = run_scenario(&slow);
    let error = gate(&log, Profile::Gpu, 2000, 7900, 0, high_water).expect_err("10x must fail");
    assert!(error.to_string().contains("took 3000 ms"), "first blown ceiling reports: {error}");
}

#[test]
fn drops_realtime_and_memory_fail() {
    let (log, high_water) = run_scenario(&turn());
    assert!(gate(&log, Profile::Gpu, 2000, 790, 3, high_water).is_err());
    assert!(gate(&log, Profile::Gpu, 100, 790, 0, high_water).is_err());
    assert!(gate(&log, Profile::Gpu, 2000, 790, 0, u64::MAX).is_err());
}

#[test]
fn late_breach_reports_its_own_ceiling_first() {
    // Earliest-ceiling reporting: when only a late pipeline stage
    // blows, the gate names that stage with its numbers — never an
    // earlier stage and never a bare pass/fail.
    let mut late = turn();
    for step in &mut late.steps {
        if step.stage == "tts_first_audio" {
            step.latency_ms = 500;
        }
    }
    let (log, high_water) = run_scenario(&late);
    let error = gate(&log, Profile::Gpu, 2000, 1290, 0, high_water).expect_err("late breach fails");
    assert!(
        error.to_string().contains("tts_first_audio")
            && error.to_string().contains("500 ms"),
        "earliest blown ceiling reports itself: {error}"
    );
}
