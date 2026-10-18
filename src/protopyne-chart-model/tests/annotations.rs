use super::*;
use rust_decimal::Decimal;
use std::str::FromStr as _;

fn key() -> AnnotationKey {
    AnnotationKey {
        bar_timestamp: 1_700_000_000,
        price: Decimal::from_str("100.05").expect("price"),
    }
}

#[test]
fn key_round_trips_through_serde_exactly() {
    let key = key();
    let json = serde_json::to_string(&key).expect("key serializes");
    let back: AnnotationKey = serde_json::from_str(&json).expect("key deserializes");
    assert_eq!(back, key, "domain key survives serialization");
    assert_eq!(
        back.price,
        Decimal::from_str("100.05").expect("price"),
        "Decimal stays exact, never float-rounded"
    );
}

#[test]
fn annotation_round_trips_with_label_and_style() {
    let annotation = Annotation {
        key: key(),
        label: "breakout retest".to_owned(),
        style: AnnotationStyle::Bull,
    };
    let json = serde_json::to_string(&annotation).expect("annotation serializes");
    let back: Annotation = serde_json::from_str(&json).expect("annotation deserializes");
    assert_eq!(back.key, annotation.key);
    assert_eq!(back.label, "breakout retest");
    assert_eq!(back.style, AnnotationStyle::Bull);
}

#[test]
fn keys_compare_by_domain_identity() {
    let base = key();
    assert_eq!(base, key(), "same timestamp and price is the same key");
    assert_ne!(
        base,
        AnnotationKey {
            bar_timestamp: base.bar_timestamp + 60,
            price: base.price
        },
        "a different bar is a different key"
    );
    assert_ne!(
        base,
        AnnotationKey {
            bar_timestamp: base.bar_timestamp,
            price: base.price + Decimal::from_str("0.01").expect("cent"),
        },
        "a different price is a different key"
    );
}

#[test]
fn price_converts_to_display_float_without_panicking() {
    assert_eq!(
        key().price_float(),
        Some(100.05),
        "ordinary prices map to floats"
    );
    let huge = AnnotationKey {
        bar_timestamp: 0,
        price: Decimal::from_str("7922816251426433759354395033.5").expect("huge"),
    };
    assert!(
        huge.price_float().is_some(),
        "large-but-finite prices still map"
    );
}

#[test]
fn display_floats_become_keys_exactly_or_not_at_all() {
    // The sanctioned display→key crossing: ordinary floats key exactly and
    // round-trip through `price_float`, while non-finite floats refuse
    // instead of pinning a substituted decimal.
    let keyed = AnnotationKey::from_display(1_700_000_000, 100.5).expect("100.5 keys");
    assert_eq!(keyed.bar_timestamp, 1_700_000_000);
    assert_eq!(
        keyed.price_float(),
        Some(100.5),
        "the key maps back to its float"
    );
    for vapor in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(
            AnnotationKey::from_display(1_700_000_000, vapor).is_none(),
            "{vapor} keys nothing"
        );
    }
}
