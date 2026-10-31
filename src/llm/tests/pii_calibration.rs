use crate::backend::{PiiSpanDetector, RegexPiiDetector};

fn must_fire_cases() -> Vec<(&'static str, &'static str)> {
    vec![
        ("reach me at a@b.com", "email"),
        ("ssn 123-45-6789", "ssn"),
        ("pay 4111 1111 1111 1111 today", "card_number"),
        ("call 555-867-5309", "phone"),
        ("api_key = abcdef1234567890abcdef1234", "api_key"),
    ]
}

fn control_group_cases() -> Vec<&'static str> {
    vec![
        "nothing sensitive here",
        "contact user@example for details",
        "order 1234-56-789 today",
        "call 555-019-999 today",
        "Bearer without tail",
        "AKIA short key",
        "meeting notes for monday review",
    ]
}

#[test]
fn pii_calibration_must_fire() {
    let detector = RegexPiiDetector;
    for (text, label) in must_fire_cases() {
        let spans = detector.detect(text).expect("detect");
        assert!(
            spans.iter().any(|s| s.label == label),
            "must-fire {label:?} in {text:?}: {spans:?}"
        );
    }
}

#[test]
fn pii_calibration_control_group() {
    let detector = RegexPiiDetector;
    for text in control_group_cases() {
        let spans = detector.detect(text).expect("detect");
        assert!(
            spans.is_empty(),
            "control group must NOT fire in {text:?}: {spans:?}"
        );
    }
}

#[test]
fn pii_precision_recall_report() {
    let detector = RegexPiiDetector;
    let mut true_positives: u32 = 0;
    let total_must_fire: u32 = must_fire_cases().len() as u32;
    for (text, label) in must_fire_cases() {
        let spans = detector.detect(text).expect("detect");
        if spans.iter().any(|s| s.label == label) {
            true_positives += 1;
        }
    }
    let mut false_positives = 0;
    for text in control_group_cases() {
        let spans = detector.detect(text).expect("detect");
        false_positives += spans.len();
    }
    let recall = f64::from(true_positives) / f64::from(total_must_fire);
    assert!(
        (recall - 1.0).abs() < f64::EPSILON,
        "recall on must-fire group"
    );
    assert_eq!(
        false_positives, 0,
        "0 false positives on control group, got {false_positives}"
    );
}
