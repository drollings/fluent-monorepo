//! Fake tests: clock math, fault cadence, slow scaling.

use crate::fakes::{FakeClock, ScriptScenario, ScriptStep, SlowFake};

#[test]
fn clock_advances_saturatingly() {
    let mut clock = FakeClock::new();
    assert_eq!(clock.now_ms(), 0);
    clock.advance(150);
    assert_eq!(clock.now_ms(), 150);
}

#[test]
fn faults_fire_on_cadence() {
    let scenario = ScriptScenario {
        steps: Vec::new(),
        fault_every: 4,
    };
    assert!(!scenario.faults_at(0));
    assert!(!scenario.faults_at(3));
    assert!(scenario.faults_at(4));
    assert!(scenario.faults_at(8));
    assert!(!ScriptScenario::clean(Vec::new()).faults_at(4));
}

#[test]
fn slow_fake_scales_latency_not_bytes() {
    let scenario = ScriptScenario::clean(vec![ScriptStep {
        stage: "stt".to_string(),
        latency_ms: 80,
        peak_bytes: 1024,
    }]);
    let scaled = SlowFake { factor: 10 }.scale(&scenario);
    assert_eq!(scaled.steps[0].latency_ms, 800);
    assert_eq!(scaled.steps[0].peak_bytes, 1024);
}
