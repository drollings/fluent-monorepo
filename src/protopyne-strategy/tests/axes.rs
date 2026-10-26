//! Unit tests for the measurement-axis vocabulary.
#![allow(clippy::float_cmp)] // exact values are the contract for a bounded score

use crate::axes::{Confidence, GateAxis, Merit};

#[test]
fn gate_axis_round_trips_through_its_schema_spelling() {
    for axis in [
        GateAxis::Correctness,
        GateAxis::TaskValue,
        GateAxis::ProducerConfidence,
    ] {
        assert_eq!(GateAxis::parse(axis.as_str()), Some(axis));
        assert_eq!(axis.to_string(), axis.as_str());
    }
    assert_eq!(GateAxis::parse("confidence"), None, "vocabulary is closed");
    assert_eq!(GateAxis::parse(""), None);
}

#[test]
fn confidence_saturation_is_bounded_and_stated() {
    assert_eq!(Confidence::saturating(0.4).value(), 0.4);
    assert_eq!(Confidence::saturating(1.2).value(), 1.0);
    assert_eq!(Confidence::saturating(-0.3).value(), 0.0);
    assert_eq!(
        Confidence::saturating(f64::NAN).value(),
        0.0,
        "unmeasurable reads as no confidence, never as a pass"
    );
    assert_eq!(Confidence::saturating(f64::INFINITY).value(), 0.0);
}

#[test]
fn confidence_strict_construction_refuses_out_of_band() {
    assert!(Confidence::try_new(0.0).is_ok());
    assert!(Confidence::try_new(1.0).is_ok());
    for bad in [-0.1, 1.1, f64::NAN, f64::INFINITY] {
        assert!(
            Confidence::try_new(bad).is_err(),
            "confidence {bad} must refuse"
        );
    }
}

#[test]
fn merit_refuses_what_it_cannot_judge() {
    assert!(Merit::new(0.5, None).is_ok());
    assert!(Merit::new(0.5, Some(0.25)).is_ok());
    for bad in [-0.1, 1.1, f64::NAN] {
        assert!(Merit::new(bad, None).is_err(), "activity {bad} must refuse");
    }
    assert!(
        Merit::new(0.5, Some(f64::NAN)).is_err(),
        "a present edge must be finite"
    );
}

/// The axis types are deliberately not interchangeable. There is no
/// `From<f64>` for `Confidence` (a bare number has no axis) and no
/// `From<Confidence>` for `Merit`: the only path from one axis to the other is
/// an explicit function that names the conversion, as here.
#[test]
fn axes_are_not_implicitly_convertible() {
    let confidence = Confidence::saturating(0.9);
    let merit = Merit::new(confidence.value(), None).expect("merit from a score");
    assert_eq!(merit.activity, 0.9);
}
