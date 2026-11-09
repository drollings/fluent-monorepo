//! Canary fingerprint tests (tests-first).

use fluent_llm::decision::{canary_signature, canary_suite, verify_canary, Decision};

fn measured(top: &str, mass: f64) -> Decision {
    let other = 1.0 - mass;
    let other_label = if top == "A" { "B" } else { "A" };
    Decision::from_distribution(
        top,
        vec![(top.to_string(), mass), (other_label.to_string(), other)],
        "m".to_string(),
        1,
        0,
        false,
    )
    .expect("measured")
}

#[test]
fn suite_shape_pinned() {
    let suite = canary_suite();
    assert_eq!(suite.len(), 2);
    for case in &suite {
        assert!(!case.prompt.is_empty());
        assert!(case.options.len() >= 2);
        assert!(!case.expected.is_empty());
    }
}

#[test]
fn verify_passes_within_tolerance() {
    let suite = canary_suite();
    let d = measured("A", 0.97);
    assert!(verify_canary(&d, &suite[0]).is_ok());
}

#[test]
fn verify_flags_drift_beyond_tolerance() {
    let suite = canary_suite();
    let d = measured("B", 0.9);
    let drift = verify_canary(&d, &suite[0]).expect_err("drift");
    assert_eq!(drift.case_id, suite[0].id);
}

#[test]
fn verify_rejects_degraded_canary() {
    let suite = canary_suite();
    let mut d = measured("A", 0.5);
    d.degraded = true;
    assert!(verify_canary(&d, &suite[0]).is_err());
}

#[test]
fn signature_deterministic_and_sensitive() {
    let a = measured("A", 0.97);
    let b = measured("A", 0.97);
    let c = measured("A", 0.60);
    assert_eq!(
        canary_signature(std::slice::from_ref(&a)),
        canary_signature(&[b])
    );
    assert_ne!(canary_signature(&[a]), canary_signature(&[c]));
}
