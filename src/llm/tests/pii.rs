use super::*;

#[test]
fn regex_detector_reports_pattern_matches_with_offsets() {
    let detector = RegexPiiDetector;
    // The canonical table covers email/ssn/phone/api_key/card_number.
    let spans = detector
        .detect("reach me at a@b.com or call 123-45-6789")
        .expect("detect");
    let labels: Vec<&str> = spans.iter().map(|s| s.label.as_str()).collect();
    assert!(labels.contains(&"email"), "email matched: {labels:?}");
    assert!(labels.contains(&"ssn"), "ssn matched: {labels:?}");
    for span in &spans {
        assert!(
            (span.score - 1.0).abs() < f64::EPSILON,
            "regex hit is exact: {}",
            span.score
        );
        assert!(span.end > span.start);
    }
}

#[test]
fn regex_detector_empty_text_yields_nothing() {
    assert!(RegexPiiDetector.detect("").unwrap().is_empty());
    assert!(RegexPiiDetector
        .detect("nothing sensitive here")
        .unwrap()
        .is_empty());
}

/// Detection parity between the two table consumers (M12): the shared
/// 5-pattern table drives `RegexPiiDetector` (spans) and — through the same
/// compiled statics — `anonymize` (rewrites). For each shared pattern, the
/// detector must report the span and `anonymize` must scrub the matched
/// bytes. One pattern table, two consumers, no drift.
#[test]
fn detector_spans_are_rewritten_by_anonymize() {
    let detector = RegexPiiDetector;
    let cases: [(&str, &str); 5] = [
        ("mail me at daniel@example.com now", "email"),
        ("ssn 123-45-6789", "ssn"),
        ("pay 4111 1111 1111 1111 today", "card_number"),
        ("call 555-867-5309", "phone"),
        ("api_key = abcdef1234567890abcdef1234", "api_key"),
    ];
    for (text, label) in cases {
        let spans = detector.detect(text).expect("detect");
        let hit = spans
            .iter()
            .find(|s| s.label == label)
            .unwrap_or_else(|| panic!("{label} span reported by the table detector: {spans:?}"));
        let matched = &text[hit.start..hit.end];
        let anonymized = crate::anonymize::anonymize(text);
        assert_ne!(
            anonymized, text,
            "{label} rewritten by anonymize: {anonymized:?}"
        );
        assert!(
            !anonymized.contains(matched),
            "{label} matched bytes scrubbed: {matched:?} -> {anonymized:?}"
        );
    }
}

/// The anonymize action table is the single mapping home (M12): its pattern
/// set is exactly the 12 compiled statics — the 5 exposed request-path
/// patterns plus the 7 anonymize-only ones the M6.2 lock pins — in the
/// historical application order.
#[test]
fn anonymize_table_covers_the_twelve_statics_in_order() {
    let actions = crate::pii_patterns::anonymize_actions();
    let names: Vec<&str> = actions.iter().map(|a| a.name).collect();
    assert_eq!(
        names,
        [
            "email",
            "credit_card",
            "ssn_us",
            "nino_uk",
            "sin_ca",
            "bearer",
            "aws_key",
            "generic_api_key",
            "ipv6",
            "ipv4",
            "phone_us",
            "api_key",
        ]
    );
    assert_eq!(
        actions[0].regex.as_str(),
        crate::pii_patterns::EMAIL_RE.as_str()
    );
    assert_eq!(actions[0].placeholder, "[EMAIL]");
    assert_eq!(
        actions[11].regex.as_str(),
        crate::pii_patterns::API_KEY_RE.as_str()
    );
    assert_eq!(actions[11].placeholder, "[REDACTED]");
}
