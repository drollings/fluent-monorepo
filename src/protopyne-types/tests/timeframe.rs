use super::*;
use crate::error::TypesError;

/// Every fixed-ms variant with its canonical code. Calendar timeframes
/// (`MO1`/`MO3`/`Y`) have no row here by design: they stay refused.
fn all_variants() -> [(Timeframe, &'static str); 15] {
    [
        (Timeframe::S5, "S5"),
        (Timeframe::S10, "S10"),
        (Timeframe::S15, "S15"),
        (Timeframe::S30, "S30"),
        (Timeframe::M1, "M1"),
        (Timeframe::M3, "M3"),
        (Timeframe::M5, "M5"),
        (Timeframe::M10, "M10"),
        (Timeframe::M15, "M15"),
        (Timeframe::M30, "M30"),
        (Timeframe::H1, "H1"),
        (Timeframe::H2, "H2"),
        (Timeframe::H4, "H4"),
        (Timeframe::D1, "D1"),
        (Timeframe::W1, "W1"),
    ]
}

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
        (Timeframe::S5, "S5"),
        (Timeframe::S10, "S10"),
        (Timeframe::S15, "S15"),
        (Timeframe::S30, "S30"),
        (Timeframe::M3, "M3"),
        (Timeframe::M10, "M10"),
        (Timeframe::H2, "H2"),
        (Timeframe::W1, "W1"),
    ];
    assert_eq!(cases.len(), all_variants().len());
    for (variant, code) in cases {
        assert_eq!(variant.code(), code);
        assert_eq!(variant.to_string(), code);
        assert_eq!(code.parse::<Timeframe>().expect("known code"), variant);
    }
}

#[test]
fn rejects_unknown_codes() {
    for bad in [
        "", "m1", "M2", "M15 ", "  M1", "1m", "1M", "3M", "12M", "MO1", "MO3", "Y", "C", "-",
        "Chart",
    ] {
        assert!(bad.parse::<Timeframe>().is_err(), "must reject {bad:?}");
    }
}

#[test]
fn duration_ms_monotonic_with_variant_order() {
    let ordered = [
        Timeframe::S5,
        Timeframe::S10,
        Timeframe::S15,
        Timeframe::S30,
        Timeframe::M1,
        Timeframe::M3,
        Timeframe::M5,
        Timeframe::M10,
        Timeframe::M15,
        Timeframe::M30,
        Timeframe::H1,
        Timeframe::H2,
        Timeframe::H4,
        Timeframe::D1,
        Timeframe::W1,
    ];
    let mut prev = 0_i64;
    for tf in ordered {
        let ms = tf.duration_ms();
        assert!(ms > prev, "{tf:?} duration {ms} not greater than {prev}");
        prev = ms;
    }
}

#[test]
fn minutes_keep_legacy_values() {
    // Whole minutes, truncating: sub-minute spans read 0 by construction.
    let cases = [
        (Timeframe::S5, 0),
        (Timeframe::S10, 0),
        (Timeframe::S15, 0),
        (Timeframe::S30, 0),
        (Timeframe::M1, 1),
        (Timeframe::M3, 3),
        (Timeframe::M5, 5),
        (Timeframe::M10, 10),
        (Timeframe::M15, 15),
        (Timeframe::M30, 30),
        (Timeframe::H1, 60),
        (Timeframe::H2, 120),
        (Timeframe::H4, 240),
        (Timeframe::D1, 1440),
        (Timeframe::W1, 10_080),
    ];
    for (variant, minutes) in cases {
        assert_eq!(variant.minutes(), minutes, "{variant:?} whole minutes");
    }
}

#[test]
fn serde_round_trip() {
    for (variant, _) in all_variants().iter().take(15) {
        let json = serde_json::to_string(variant).expect("serialize");
        let back: Timeframe = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&back, variant);
    }
}

/// Millisecond spans match the Pine `getTimeframeMS` table entry for entry.
#[test]
fn duration_ms_matches_pine_table() {
    let cases = [
        (Timeframe::S5, 5_000),
        (Timeframe::S10, 10_000),
        (Timeframe::S15, 15_000),
        (Timeframe::S30, 30_000),
        (Timeframe::M1, 60_000),
        (Timeframe::M3, 180_000),
        (Timeframe::M5, 300_000),
        (Timeframe::M10, 600_000),
        (Timeframe::M15, 900_000),
        (Timeframe::M30, 1_800_000),
        (Timeframe::H1, 3_600_000),
        (Timeframe::H2, 7_200_000),
        (Timeframe::H4, 14_400_000),
        (Timeframe::D1, 86_400_000),
        (Timeframe::W1, 604_800_000),
    ];
    for (variant, ms) in cases {
        assert_eq!(variant.duration_ms(), ms, "{variant:?} ms");
        assert_eq!(
            i64::try_from(variant.total_seconds()).expect("fits"),
            ms / 1_000,
            "{variant:?} seconds"
        );
    }
}

/// The single Pine↔Rust map covers every abbrev the Pine `getTimeframeAbbrev`
/// emits for the fixed-ms set; calendar abbrevs refuse fail-closed.
#[test]
fn from_pine_abbrev_covers_pine_table() {
    let cases = [
        ("5S", Timeframe::S5),
        ("10S", Timeframe::S10),
        ("15S", Timeframe::S15),
        ("30S", Timeframe::S30),
        ("1", Timeframe::M1),
        ("3", Timeframe::M3),
        ("5", Timeframe::M5),
        ("10", Timeframe::M10),
        ("15", Timeframe::M15),
        ("30", Timeframe::M30),
        ("60", Timeframe::H1),
        ("120", Timeframe::H2),
        ("240", Timeframe::H4),
        ("D", Timeframe::D1),
        ("W", Timeframe::W1),
    ];
    for (abbrev, variant) in cases {
        assert_eq!(
            Timeframe::from_pine_abbrev(abbrev).expect("known abbrev"),
            variant,
            "abbrev {abbrev:?}"
        );
    }
    for bad in ["1M", "3M", "12M", "C", "-", "Chart", "", "m1", "H2"] {
        match Timeframe::from_pine_abbrev(bad) {
            Err(TypesError::UnknownTimeframe(_)) => {}
            other => panic!("abbrev {bad:?} must refuse as unknown, got {other:?}"),
        }
    }
}

#[test]
fn from_resolution_covers_chart_codes() {
    let cases = [
        ("5S", Timeframe::S5),
        ("10S", Timeframe::S10),
        ("15S", Timeframe::S15),
        ("30S", Timeframe::S30),
        ("1", Timeframe::M1),
        ("3", Timeframe::M3),
        ("5", Timeframe::M5),
        ("10", Timeframe::M10),
        ("15", Timeframe::M15),
        ("30", Timeframe::M30),
        ("60", Timeframe::H1),
        ("1H", Timeframe::H1),
        ("120", Timeframe::H2),
        ("2H", Timeframe::H2),
        ("240", Timeframe::H4),
        ("4H", Timeframe::H4),
        ("1D", Timeframe::D1),
        ("D", Timeframe::D1),
        ("W", Timeframe::W1),
        ("1W", Timeframe::W1),
    ];
    for (resolution, variant) in cases {
        assert_eq!(
            Timeframe::from_resolution(resolution),
            Some(variant),
            "resolution {resolution:?}"
        );
    }
    for bad in ["", "nope", "1M", "3M", "12M", "M1", "m1"] {
        assert_eq!(
            Timeframe::from_resolution(bad),
            None,
            "resolution {bad:?} stays unknown"
        );
    }
}

#[test]
fn every_variant_is_fixed_ms() {
    for (variant, _) in all_variants().iter().take(15) {
        assert!(variant.is_fixed_ms(), "{variant:?} is fixed-ms");
    }
}

/// Bucket math over whole-second spans is idempotent:
/// `floor(ts/ms)*ms` floors once and stays put.
#[test]
fn bucket_math_is_idempotent() {
    for (variant, _) in all_variants().iter().take(15) {
        let ms = variant.duration_ms();
        assert_eq!(ms % 1_000, 0, "{variant:?} spans whole seconds");
        for ts in [0_i64, 1, 999, 1_699_999_980_000, 1_700_000_040_123] {
            let floored = ts.div_euclid(ms) * ms;
            assert_eq!(
                floored.div_euclid(ms) * ms,
                floored,
                "{variant:?} bucket floor idempotent at {ts}"
            );
        }
    }
}
