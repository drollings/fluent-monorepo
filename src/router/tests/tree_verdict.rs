use super::*;

// M6.1 characterization: the three-axis verdict's tolerant parse — malformed
// fields fall back to pass-friendly defaults (coherence/safety 1.0,
// complexity 5), never to a rejection.

#[test]
fn pristine_verdict_parses() {
    let v = parse_tree_verdict(
        r#"{"route": "code", "coherence": 0.9, "safety": 0.8, "complexity": 7, "reason": "r"}"#,
    )
    .expect("pristine");
    assert_eq!(v.route.as_deref(), Some("code"));
    assert!((v.coherence - 0.9).abs() < f64::EPSILON);
    assert_eq!(v.complexity, 7);
}

#[test]
fn fenced_verdict_parses() {
    let v = parse_tree_verdict("```json\n{\"route\": \"chat\"}\n```").expect("fenced");
    assert_eq!(v.route.as_deref(), Some("chat"));
    // Missing axes default pass-friendly.
    assert!((v.coherence - 1.0).abs() < f64::EPSILON);
    assert!((v.safety - 1.0).abs() < f64::EPSILON);
    assert_eq!(v.complexity, 5);
}

#[test]
fn missing_fields_default_pass_friendly() {
    let v = parse_tree_verdict(r#"{"route": "x"}"#).expect("defaults");
    assert!((v.coherence - 1.0).abs() < f64::EPSILON);
    assert!((v.safety - 1.0).abs() < f64::EPSILON);
    assert_eq!(v.complexity, 5);
    assert!(v.reason.is_empty());
}

#[test]
fn string_numbers_coerce() {
    let v = parse_tree_verdict(r#"{"coherence": "0.4", "complexity": "9"}"#).expect("coerce");
    assert!((v.coherence - 0.4).abs() < f64::EPSILON);
    assert_eq!(v.complexity, 9);
}

#[test]
fn unparseable_is_error_not_default() {
    assert!(parse_tree_verdict("not json at all {{{").is_err());
    assert!(parse_tree_verdict("").is_err());
}
