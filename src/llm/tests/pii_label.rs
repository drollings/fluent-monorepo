use super::*;
use crate::backend::{PiiSpanDetector, RegexPiiDetector};

#[test]
fn pii_label_strings() {
    for (label, text) in [
        (PiiLabel::Ssn, "ssn"),
        (PiiLabel::CardNumber, "card_number"),
        (PiiLabel::Email, "email"),
        (PiiLabel::Phone, "phone"),
        (PiiLabel::ApiKey, "api_key"),
        (PiiLabel::Ipv4, "ipv4"),
        (PiiLabel::Ipv6, "ipv6"),
        (PiiLabel::Bearer, "bearer"),
        (PiiLabel::AwsKey, "aws_key"),
        (PiiLabel::GenericApiKey, "generic_api_key"),
        (PiiLabel::NinoUk, "nino_uk"),
        (PiiLabel::SinCa, "sin_ca"),
    ] {
        assert_eq!(label.as_str(), text);
        assert_eq!(label.to_string(), text);
        let parsed: PiiLabel = text.parse().expect("known label parses");
        assert_eq!(parsed, label);
    }
    assert!("credential.password".parse::<PiiLabel>().is_err());
}

#[test]
fn detector_firing_set_pinned() {
    let detector = RegexPiiDetector;
    let text = "mail me at daniel@example.com now ssn 123-45-6789 \
        pay 4111 1111 1111 1111 today call 555-867-5309 \
        api_key = abcdef1234567890abcdef1234";
    let spans = detector.detect(text).expect("detect");
    let mut labels: Vec<&str> = spans.iter().map(|s| s.label.as_str()).collect();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(
        labels,
        ["api_key", "card_number", "email", "phone", "ssn"],
        "detector fires exactly the 5 pre-existing kinds: {labels:?}"
    );
    for span in &spans {
        span.label_enum()
            .expect("detector label parses as PiiLabel");
    }
}

#[test]
fn anonymize_firing_set_pinned() {
    let before = "contact daniel@example.com ssn 123-45-6789 \
        card 4111 1111 1111 1111 phone 555-867-5309 \
        api_key = abcdef1234567890abcdef1234 \
        host 192.168.1.1 bearer Bearer abcdefgh key AKIAIOSFODNN7EXAMPLE";
    let after = crate::anonymize::anonymize(before);
    assert_ne!(after, before);
    for leaked in [
        "daniel@example.com",
        "123-45-6789",
        "4111 1111 1111 1111",
        "192.168.1.1",
    ] {
        assert!(
            !after.contains(leaked),
            "matched bytes scrubbed: {leaked:?} -> {after:?}"
        );
    }
}
