use super::*;

const RAW_PROBE: [f64; 7] = [-300.0, -100.0, -50.0, 0.0, 50.0, 100.0, 300.0];

/// Reference vectors computed from the `(offset, k, multiplier)` table with
/// `x / (1 + |x|) * multiplier`, `x = (raw + offset) * k`.
fn reference_vector(kind: OscillatorKind) -> [f64; 7] {
    match kind {
        OscillatorKind::Aroon => [
            -509.090909090909,
            -342.857142857143,
            -266.666666666667,
            -160.0,
            0.0,
            160.0,
            444.444444444444,
        ],
        OscillatorKind::Cci => [
            -275.280898876404,
            -172.131147540984,
            -129.62962962963,
            -74.468085106383,
            0.0,
            74.468085106383,
            233.333333333333,
        ],
        OscillatorKind::Cmo => [
            -375.0,
            -166.666666666667,
            -90.909090909091,
            0.0,
            90.909090909091,
            166.666666666667,
            375.0,
        ],
        OscillatorKind::Mfi | OscillatorKind::Rsi | OscillatorKind::Raw => [
            -552.182741116751,
            -412.566371681416,
            -337.826086956522,
            -218.87323943662,
            0.0,
            218.87323943662,
            501.290322580645,
        ],
        OscillatorKind::Srsi => [
            -1214.876033057851,
            -577.981651376147,
            -396.22641509434,
            -203.883495145631,
            0.0,
            203.883495145631,
            913.04347826087,
        ],
        OscillatorKind::Stoch => [
            -1042.553191489362,
            -488.372093023256,
            -333.333333333333,
            -170.731707317073,
            0.0,
            170.731707317073,
            777.777777777778,
        ],
        OscillatorKind::TrendSi => [
            -7448.275862068966,
            -5538.461538461539,
            -4000.0,
            0.0,
            4000.0,
            5538.461538461539,
            7448.275862068966,
        ],
        OscillatorKind::Voss => [
            -299.334811529933,
            -298.013245033113,
            -296.052631578947,
            0.0,
            296.052631578947,
            298.013245033113,
            299.334811529933,
        ],
    }
}

const ALL_KINDS: [OscillatorKind; 10] = [
    OscillatorKind::Aroon,
    OscillatorKind::Cci,
    OscillatorKind::Cmo,
    OscillatorKind::Mfi,
    OscillatorKind::Rsi,
    OscillatorKind::Srsi,
    OscillatorKind::Stoch,
    OscillatorKind::TrendSi,
    OscillatorKind::Voss,
    OscillatorKind::Raw,
];

#[test]
fn squash_constants_are_frozen() {
    let table = [
        (OscillatorKind::Aroon, -50.0, 0.005, 800.0),
        (OscillatorKind::Cci, -50.0, 0.0035, 500.0),
        (OscillatorKind::Cmo, 0.0, 0.002, 1000.0),
        (OscillatorKind::Mfi, -50.0, 0.0084, 740.0),
        (OscillatorKind::Rsi, -50.0, 0.0084, 740.0),
        (OscillatorKind::Srsi, -50.0, 0.0006, 7000.0),
        (OscillatorKind::Stoch, -50.0, 0.0005, 7000.0),
        (OscillatorKind::TrendSi, 0.0, 0.016, 9000.0),
        (OscillatorKind::Voss, 0.0, 1.5, 300.0),
        (OscillatorKind::Raw, -50.0, 0.0084, 740.0),
    ];
    for (kind, offset, k, multiplier) in table {
        assert_eq!(osc_params(kind), OscParams { offset, k, multiplier }, "{kind:?} constants moved");
    }
}

#[test]
fn normalization_matches_reference_vectors() {
    for kind in ALL_KINDS {
        let expected = reference_vector(kind);
        let mut max_diff = 0.0_f64;
        for (index, raw) in RAW_PROBE.iter().enumerate() {
            let got = normalize_oscillator(*raw, kind);
            max_diff = max_diff.max((got - expected[index]).abs());
        }
        assert!(
            max_diff <= 1e-9,
            "{kind:?} drifted from the reference table (max diff {max_diff})"
        );
    }
}

#[test]
fn normalize_series_preserves_warmup_holes() {
    let raw = vec![None, None, Some(50.0), Some(100.0), None, Some(0.0)];
    let out = normalize_series(&raw, OscillatorKind::Rsi);
    assert_eq!(out.len(), raw.len(), "output length must match input length");
    assert_eq!(&out[..2], &[None, None], "leading warmup run must survive");
    assert_eq!(out[4], None, "interior holes must survive");
    for (index, slot) in raw.iter().enumerate() {
        match slot {
            None => assert_eq!(out[index], None),
            Some(value) => {
                let expected = normalize_oscillator(*value, OscillatorKind::Rsi);
                assert_eq!(out[index], Some(expected));
            }
        }
    }
}

#[test]
fn normalize_series_turns_non_finite_into_holes() {
    let out = normalize_series(&[Some(f64::NAN)], OscillatorKind::Rsi);
    assert_eq!(out, vec![None], "NaN must become a hole, never a substitute");
    let unbounded = normalize_series(&[Some(f64::INFINITY)], OscillatorKind::Rsi);
    assert_eq!(unbounded, vec![None], "unbounded input must become a hole, never a substitute");
}

#[test]
fn oscillator_kind_names_round_trip() {
    for kind in ALL_KINDS {
        let name = oscillator_kind_name(kind);
        assert_eq!(parse_oscillator_kind(name), Some(kind), "{name} parses");
        assert_eq!(
            parse_oscillator_kind(&name.to_ascii_uppercase()),
            Some(kind),
            "names parse case-insensitively"
        );
    }
    for bad in ["", "sma", "ema", "wave", "rsi "] {
        assert_eq!(parse_oscillator_kind(bad), None, "{bad:?} refuses");
    }
}
