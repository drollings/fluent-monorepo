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

const DECIMAL_REJECTED: [&str; 8] = [
    "",
    "   ",
    "+",
    "-",
    ".",
    "abc",
    "1,000",
    "١٢٣",
];

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
