use super::*;

#[test]
fn parse_display_round_trip_all_variants() {
    let cases = [
        (Timeframe::M1, "M1"),
        (Timeframe::M5, "M5"),
        (Timeframe::M15, "M15"),
        (Timeframe::M30, "M30"),
        (Timeframe::H1, "H1"),
        (Timeframe::H4, "H4"),
        (Timeframe::D1, "D1"),
    ];
    for (variant, code) in cases {
        assert_eq!(variant.to_string(), code);
        assert_eq!(code.parse::<Timeframe>().expect("known code"), variant);
    }
}

#[test]
fn rejects_unknown_codes() {
    for bad in ["", "m1", "M2", "H2", "W1", "M15 ", "  M1", "1m"] {
        assert!(bad.parse::<Timeframe>().is_err(), "must reject {bad:?}");
    }
}

#[test]
fn minutes_monotonic_with_variant_order() {
    let ordered = [
        Timeframe::M1,
        Timeframe::M5,
        Timeframe::M15,
        Timeframe::M30,
        Timeframe::H1,
        Timeframe::H4,
        Timeframe::D1,
    ];
    let mut prev = 0_u32;
    for tf in ordered {
        let m = tf.minutes();
        assert!(m > prev, "{tf:?} minutes {m} not greater than {prev}");
        prev = m;
    }
    assert_eq!(Timeframe::M1.minutes(), 1);
    assert_eq!(Timeframe::M5.minutes(), 5);
    assert_eq!(Timeframe::M15.minutes(), 15);
    assert_eq!(Timeframe::M30.minutes(), 30);
    assert_eq!(Timeframe::H1.minutes(), 60);
    assert_eq!(Timeframe::H4.minutes(), 240);
    assert_eq!(Timeframe::D1.minutes(), 1440);
}

#[test]
fn serde_round_trip() {
    for tf in [
        Timeframe::M1,
        Timeframe::M5,
        Timeframe::M15,
        Timeframe::M30,
        Timeframe::H1,
        Timeframe::H4,
        Timeframe::D1,
    ] {
        let json = serde_json::to_string(&tf).expect("serialize");
        let back: Timeframe = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, tf);
    }
}
