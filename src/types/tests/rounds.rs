use super::*;

#[test]
fn bounded_rounds_rejects_zero_and_over_max() {
    assert!(BoundedRounds::new(0).is_err());
    assert!(BoundedRounds::new(BoundedRounds::MAX).is_ok());
    assert!(BoundedRounds::new(BoundedRounds::MAX + 1).is_err());
    assert_eq!(BoundedRounds::default().get(), 2);
}

#[test]
fn bounded_rounds_advance_terminates() {
    let r = BoundedRounds::new(3).expect("valid");
    assert_eq!(r.rounds(), 3);
    let r = r.advance().expect("advance");
    assert_eq!(r.get(), 2);
    let r = r.advance().expect("advance");
    assert!(r.advance().is_none());
}

#[test]
fn bounded_rounds_deserialize_validates() {
    assert!(serde_json::from_str::<BoundedRounds>("3").is_ok());
    assert!(serde_json::from_str::<BoundedRounds>("0").is_err());
    assert!(serde_json::from_str::<BoundedRounds>("9").is_err());
}

#[test]
fn thresholds_clamp_to_unit_interval() {
    assert!(SeverityThreshold::new(0.7).is_ok());
    assert!(SeverityThreshold::new(-0.1).is_err());
    assert!(SeverityThreshold::new(1.1).is_err());
    assert!(EscalationConfidence::new(0.4).is_ok());
    assert!(EscalationConfidence::new(2.0).is_err());
    assert!((SeverityThreshold::default().get() - 0.7).abs() < f64::EPSILON);
    assert!((EscalationConfidence::default().get() - 0.4).abs() < f64::EPSILON);
    assert!(serde_json::from_str::<SeverityThreshold>("1.5").is_err());
}

#[test]
fn unit_threshold_aliases_share_range_validation() {
    // Inclusive bounds accept, out-of-range (incl. NaN) rejects — all four.
    for v in [0.0, 1.0] {
        assert!(SeverityThreshold::new(v).is_ok());
        assert!(EscalationConfidence::new(v).is_ok());
        assert!(TaskRelevanceThreshold::new(v).is_ok());
        assert!(ProducerConfidenceThreshold::new(v).is_ok());
    }
    for v in [-0.1, 1.1, f64::NAN] {
        assert!(SeverityThreshold::new(v).is_err());
        assert!(EscalationConfidence::new(v).is_err());
        assert!(TaskRelevanceThreshold::new(v).is_err());
        assert!(ProducerConfidenceThreshold::new(v).is_err());
    }
    assert!((TaskRelevanceThreshold::new(0.15).unwrap().get() - 0.15).abs() < f64::EPSILON);
}

#[test]
fn unit_threshold_error_text_preserved_per_tag() {
    assert_eq!(
        SeverityThreshold::new(2.0).unwrap_err(),
        "SeverityThreshold 2 out of 0.0..=1.0"
    );
    assert_eq!(
        EscalationConfidence::new(2.0).unwrap_err(),
        "EscalationConfidence 2 out of 0.0..=1.0"
    );
    assert_eq!(
        TaskRelevanceThreshold::new(2.0).unwrap_err(),
        "TaskRelevanceThreshold 2 out of range"
    );
    assert_eq!(
        ProducerConfidenceThreshold::new(2.0).unwrap_err(),
        "ProducerConfidenceThreshold 2 out of range"
    );
}

#[test]
fn unit_threshold_serde_parity_all_aliases() {
    for payload in ["0.5", "0", "1"] {
        assert!(serde_json::from_str::<SeverityThreshold>(payload).is_ok());
        assert!(serde_json::from_str::<EscalationConfidence>(payload).is_ok());
        assert!(serde_json::from_str::<TaskRelevanceThreshold>(payload).is_ok());
        assert!(serde_json::from_str::<ProducerConfidenceThreshold>(payload).is_ok());
    }
    for payload in ["1.5", "-0.1"] {
        assert!(serde_json::from_str::<SeverityThreshold>(payload).is_err());
        assert!(serde_json::from_str::<EscalationConfidence>(payload).is_err());
        assert!(serde_json::from_str::<TaskRelevanceThreshold>(payload).is_err());
        assert!(serde_json::from_str::<ProducerConfidenceThreshold>(payload).is_err());
    }
    // Wire shape stays a bare number.
    assert_eq!(
        serde_json::to_string(&SeverityThreshold::new(0.5).unwrap()).unwrap(),
        "0.5"
    );
    // Defaults unchanged.
    assert!((SeverityThreshold::default().get() - 0.7).abs() < f64::EPSILON);
    assert!((EscalationConfidence::default().get() - 0.4).abs() < f64::EPSILON);
}

#[test]
fn unit_threshold_tags_stay_distinct_types() {
    use std::any::TypeId;
    assert_ne!(
        TypeId::of::<SeverityThreshold>(),
        TypeId::of::<EscalationConfidence>()
    );
    assert_ne!(
        TypeId::of::<TaskRelevanceThreshold>(),
        TypeId::of::<ProducerConfidenceThreshold>()
    );
    assert_ne!(
        TypeId::of::<SeverityThreshold>(),
        TypeId::of::<TaskRelevanceThreshold>()
    );
}
