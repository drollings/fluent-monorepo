use super::*;

const ACCEPTED_SCALE: [(&str, u32); 11] = [
    ("0", 0),
    ("100", 0),
    ("100.05", 2),
    ("100.0500", 4),
    ("+100.05", 2),
    ("-5", 0),
    (".5", 1),
    ("5.", 0),
    ("  100.05  ", 2),
    ("0.1111111111111111111111111111", 28),
    ("-0.0000000000000000000000000001", 28),
];

const TOO_LARGE: [&str; 2] = [
    "0.00000000000000000000000000001",
    "1.00000000000000000000000000000",
];

const MALFORMED: [(&str, DecimalScaleError); 10] = [
    ("", DecimalScaleError::Empty),
    ("   ", DecimalScaleError::Empty),
    ("+", DecimalScaleError::SignWithoutDigits),
    ("-", DecimalScaleError::SignWithoutDigits),
    (".", DecimalScaleError::NoDigits),
    ("abc", DecimalScaleError::InvalidCharacter),
    ("1,000", DecimalScaleError::InvalidCharacter),
    ("١٢٣", DecimalScaleError::InvalidCharacter),
    ("1e3", DecimalScaleError::InvalidCharacter),
    ("0x10", DecimalScaleError::InvalidCharacter),
];

const DECIMAL_REJECTED: [&str; 8] = ["", "   ", "+", "-", ".", "abc", "1,000", "١٢٣"];

#[test]
fn scale_guard_counts_ascii_fraction_digits() {
    for (text, scale) in ACCEPTED_SCALE {
        assert_eq!(
            parse_scale_guarded(text.trim()),
            Ok(scale),
            "shared scale decision for {text:?}"
        );
    }
    for text in TOO_LARGE {
        assert_eq!(
            parse_scale_guarded(text),
            Err(DecimalScaleError::ScaleTooLarge(29)),
            "shared scale decision for {text:?}"
        );
    }
    for (text, refusal) in MALFORMED {
        assert_eq!(
            parse_scale_guarded(text.trim()),
            Err(refusal),
            "shared shape decision for {text:?}"
        );
    }
}

#[test]
fn money_and_quantity_follow_the_shared_guard() {
    for (text, _) in ACCEPTED_SCALE {
        Money::from_str(text).expect("money accepts the shared shape");
        Qty::from_str(text).expect("quantity accepts the shared shape");
    }
    for text in TOO_LARGE {
        let money = Money::from_str(text).expect_err("money refuses excess scale");
        assert_eq!(
            money.to_string(),
            format!("invalid money: invalid money value: {text}")
        );
        let quantity = Qty::from_str(text).expect_err("quantity refuses excess scale");
        assert_eq!(
            quantity.to_string(),
            format!("invalid quantity: invalid quantity value: {text}")
        );
    }
    for text in DECIMAL_REJECTED {
        assert!(
            Money::from_str(text).is_err(),
            "money refuses the malformed shape: {text:?}"
        );
        assert!(
            Qty::from_str(text).is_err(),
            "quantity refuses the malformed shape: {text:?}"
        );
    }
}

#[test]
fn decimal_fallback_stays_outside_the_scale_guard() {
    // The shared guard answers only the plain-ASCII scale question. Shapes it
    // rejects can still reach `Decimal` through `Money`/`Qty`; the display
    // fence remains stricter by design.
    assert_eq!(
        parse_scale_guarded("1e3"),
        Err(DecimalScaleError::InvalidCharacter)
    );
    Money::from_str("1e3").expect("decimal fallback accepts scientific form");
    Qty::from_str("1e3").expect("decimal fallback accepts scientific form");
}

#[test]
fn money_min_max_select_the_ceiling_and_floor() {
    let (lo, hi) = (
        Money::new(Decimal::new(100, 2)),
        Money::new(Decimal::new(200, 2)),
    );
    assert_eq!(lo.min(hi), lo);
    assert_eq!(hi.min(lo), lo);
    assert_eq!(lo.max(hi), hi);
    assert_eq!(hi.max(lo), hi);
    assert_eq!(lo.min(lo), lo);
}

#[test]
fn money_saturating_add_matches_plain_add_and_saturates_at_max() {
    let (a, b) = (
        Money::new(Decimal::new(150, 2)),
        Money::new(Decimal::new(250, 2)),
    );
    assert_eq!(a.saturating_add(b), Money::new(Decimal::new(400, 2)));
    assert_eq!(Money::zero().saturating_add(Money::zero()), Money::zero());
    assert_eq!(
        Money::new(Decimal::MAX).saturating_add(Money::new(Decimal::ONE)),
        Money::new(Decimal::MAX)
    );
}

#[test]
fn money_saturating_sub_floors_at_zero() {
    let (a, b) = (
        Money::new(Decimal::new(500, 2)),
        Money::new(Decimal::new(200, 2)),
    );
    assert_eq!(a.saturating_sub(b), Money::new(Decimal::new(300, 2)));
    assert_eq!(b.saturating_sub(a), Money::zero());
    assert_eq!(a.saturating_sub(a), Money::zero());
}

#[test]
fn as_finite_f64_reads_finite_and_refuses_the_rest() {
    assert_eq!(
        Money::new(Decimal::new(12345, 2)).as_finite_f64(),
        Some(123.45)
    );
    assert_eq!(Money::zero().as_finite_f64(), Some(0.0));
    // Decimal's range sits inside f64's, so only non-finite readings
    // refuse — never a representable amount.
    assert!(Money::new(Decimal::MAX).as_finite_f64().is_some());
}

#[test]
fn from_f64_or_zero_retains_and_floors() {
    assert_eq!(
        Money::from_f64_or_zero(19.99),
        Money::new(Decimal::from_f64_retain(19.99).expect("finite"))
    );
    assert_eq!(Money::from_f64_or_zero(0.0), Money::zero());
    assert_eq!(Money::from_f64_or_zero(f64::NAN), Money::zero());
    assert_eq!(Money::from_f64_or_zero(f64::INFINITY), Money::zero());
}
