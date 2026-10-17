use super::*;

#[test]
fn wrap_and_reveal_round_trips_bit_exact() {
    for value in [0.0_f64, -0.0, 100.05, f64::MIN_POSITIVE, 1.0e300] {
        let fenced = DisplayOnly::new(value);
        assert_eq!(fenced.reveal().to_bits(), value.to_bits(), "the fence stores, never converts");
    }
}

#[test]
fn copies_and_clones_stay_fenced() {
    let fenced = DisplayOnly::new(100.05_f64);
    let copied = fenced;
    assert_eq!(copied.reveal().to_bits(), 100.05_f64.to_bits());
    assert_eq!(copied, fenced, "fenced values compare as fenced values");
    let owned = DisplayOnly::new("label".to_owned());
    let cloned = owned.clone();
    assert_eq!(cloned.reveal(), "label", "non-Copy payloads clone wrapped");
}

#[test]
fn money_crossing_refuses_unconditionally() {
    // Refusal is unconditional, not validation: even a clean integral
    // display value never becomes a decision denomination.
    for value in [0.0_f64, 100.0, 100.05] {
        let error = DisplayOnly::new(value).try_into_money().expect_err("display never mints money");
        assert_eq!(
            error,
            DisplayOnlyError::DecisionRefused,
            "the refusal names itself: {error}"
        );
    }
}

#[test]
fn operator_decimal_controls_all_refuse() {
    // One-sided gate: every control below must refuse — any single false
    // accept fails. Each refusal names its reason.
    let controls = [
        "",
        "   ",
        "١٢٣",
        "1,000",
        "0.000000000000000000000000000001",
        "NaN",
        "inf",
        "-inf",
        "Infinity",
        "1 000",
        "5 0",
        "+-5",
        "--5",
        "5.5.5",
        ".",
        "+",
        "-",
        "abc",
        "12a34",
        "1e3",
        "0x10",
    ];
    for text in controls {
        let error = DisplayOnly::new(text.to_owned())
            .try_into_money()
            .expect_err("control refuses: {text}");
        assert!(
            matches!(error, DisplayOnlyError::InvalidDecimal(_)),
            "control refusal is a validation reason, not the blanket ban, got: {error}"
        );
    }
}

#[test]
fn operator_decimals_become_exact_money() {
    // The validated crossing: exact text in, exact money out — including
    // scale-carrying and signed text. Negativity is accepted here: whether
    // a field requires positive is the field's context, not money's.
    for (text, amount) in [
        ("0", rust_decimal::Decimal::ZERO),
        ("100", rust_decimal::Decimal::new(100, 0)),
        ("100.05", rust_decimal::Decimal::new(10005, 2)),
        ("100.0500", rust_decimal::Decimal::new(1_000_500, 4)),
        ("+100.05", rust_decimal::Decimal::new(10005, 2)),
        ("-5", rust_decimal::Decimal::new(-5, 0)),
        (".5", rust_decimal::Decimal::new(5, 1)),
        ("5.", rust_decimal::Decimal::new(5, 0)),
        ("  100.05  ", rust_decimal::Decimal::new(10005, 2)),
    ] {
        let money =
            DisplayOnly::new(text.to_owned()).try_into_money().expect("exact text mints: {text}");
        assert_eq!(money, Money::new(amount), "text {text:?} denotes exactly {amount}");
    }
}

#[test]
fn money_display_fromstr_try_into_money_round_trips_exactly() {
    // Property: Display then the crossing's own FromStr then money — zero
    // false refuses on the round-trip set. Each amount formats by Display,
    // re-enters as operator text, and must come back identical.
    let mut amounts = vec![
        rust_decimal::Decimal::ZERO,
        rust_decimal::Decimal::ONE,
        rust_decimal::Decimal::new(-5, 0),
        rust_decimal::Decimal::new(10005, 2),
        rust_decimal::Decimal::new(1_000_500, 4),
        rust_decimal::Decimal::new(1, 28),
        rust_decimal::Decimal::MAX,
    ];
    for mantissa in [0_i64, 1, 7, 99, 500, 123_456_789, 9_223_372_036_854_775_807] {
        for scale in [0_u32, 1, 2, 8, 18, 28] {
            amounts.push(rust_decimal::Decimal::new(mantissa, scale));
            if mantissa != 0 {
                amounts.push(rust_decimal::Decimal::new(-mantissa, scale));
            }
        }
    }
    for amount in amounts {
        let money = Money::new(amount);
        let text = money.to_string();
        let fenced: DisplayOnly<String> =
            text.parse().expect("operator text enters the fence: {text}");
        let back = fenced.try_into_money().expect("round trip holds: {text}");
        assert_eq!(back, money, "Display → FromStr → crossing is exact for {text}");
    }
}

#[test]
fn operator_text_agrees_with_the_shared_scale_guard() {
    use protopyne_types::{DecimalScaleError, MAX_DECIMAL_SCALE, parse_scale_guarded};
    use std::str::FromStr as _;

    for (text, scale) in [
        ("0", 0),
        ("100.05", 2),
        ("100.0500", 4),
        ("+100.05", 2),
        ("-5", 0),
        (".5", 1),
        ("5.", 0),
        ("  100.05  ", 2),
    ] {
        assert_eq!(
            parse_scale_guarded(text.trim()),
            Ok(scale),
            "shared scale decision for {text:?}"
        );
        let crossed = DisplayOnly::new(text.to_owned()).try_into_money().expect("shape accepted");
        assert_eq!(
            crossed,
            Money::from_str(text).expect("money accepts the same shape"),
            "both callers accept {text:?}"
        );
    }

    let limit_text = format!("0.{}", "1".repeat(MAX_DECIMAL_SCALE as usize));
    let over_text = format!("0.{}1", "0".repeat(MAX_DECIMAL_SCALE as usize));
    assert_eq!(
        parse_scale_guarded(&limit_text),
        Ok(MAX_DECIMAL_SCALE),
        "scale {MAX_DECIMAL_SCALE} is the shared acceptance edge"
    );
    assert_eq!(
        parse_scale_guarded(&over_text),
        Err(DecimalScaleError::ScaleTooLarge(MAX_DECIMAL_SCALE + 1)),
        "scale above {MAX_DECIMAL_SCALE} is the shared refusal edge"
    );
    assert_eq!(
        DisplayOnly::new(over_text).try_into_money(),
        Err(DisplayOnlyError::InvalidDecimal("decimal scale exceeds 28 places"))
    );

    for (text, refusal, reason) in [
        ("", DecimalScaleError::Empty, "empty decimal text"),
        ("   ", DecimalScaleError::Empty, "empty decimal text"),
        ("+", DecimalScaleError::SignWithoutDigits, "sign without digits"),
        ("-", DecimalScaleError::SignWithoutDigits, "sign without digits"),
        (".", DecimalScaleError::NoDigits, "no digits"),
        ("abc", DecimalScaleError::InvalidCharacter, "not an ASCII decimal number"),
        ("1,000", DecimalScaleError::InvalidCharacter, "not an ASCII decimal number"),
        ("١٢٣", DecimalScaleError::InvalidCharacter, "not an ASCII decimal number"),
        ("1e3", DecimalScaleError::InvalidCharacter, "not an ASCII decimal number"),
    ] {
        assert_eq!(
            parse_scale_guarded(text.trim()),
            Err(refusal),
            "shared shape decision for {text:?}"
        );
        assert_eq!(
            DisplayOnly::new(text.to_owned()).try_into_money(),
            Err(DisplayOnlyError::InvalidDecimal(reason)),
            "the fence keeps its reason for {text:?}"
        );
    }
}
