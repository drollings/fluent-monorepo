use super::*;

/// Characterization of the three identical clamp spellings
/// (`paint_ribbon` :466, `paint_ribbon_fill` :629, `paint_stroked_oscillator`
/// :871): `clamp(0,1) * 255 → round → clamp(0,255) as u8`.
///
/// The local helper below deliberately restates the call-site expression so
/// the table pins its CURRENT output bytes; once the shared primitive lands,
/// this table becomes that primitive's contract.
fn current_alpha_spelling(alpha: f32) -> u8 {
    let clamped = alpha.clamp(0.0, 1.0);
    (clamped * 255.0).round().clamp(0.0, 255.0) as u8
}

#[test]
fn alpha_spelling_reference_table() {
    let cases: &[(f32, u8)] = &[
        (0.0, 0),
        (1.0, 255),
        (0.5, 128),
        (0.5 / 128.0, 1),
        (-0.5, 0),
        (1.5, 255),
        (f32::INFINITY, 255),
        (f32::NEG_INFINITY, 0),
    ];
    for (alpha, want) in cases {
        assert_eq!(current_alpha_spelling(*alpha), *want, "alpha {alpha}");
    }
}

#[test]
fn alpha_spelling_nan_saturates_to_zero() {
    assert_eq!(
        current_alpha_spelling(f32::NAN),
        0,
        "NaN saturates, never wraps"
    );
}

#[test]
fn alpha_spelling_rounds_half_away() {
    assert_eq!(current_alpha_spelling(0.5), 128, "127.5 rounds to 128");
    assert_eq!(
        current_alpha_spelling(1.0 / 255.0),
        1,
        "single step reads 1"
    );
    assert_eq!(
        current_alpha_spelling(254.5 / 255.0),
        255,
        "254.5 rounds to 255"
    );
}

#[test]
fn transparency_variant_reference_table() {
    let cases: &[(f64, u8)] = &[
        (0.0, 255),
        (50.0, 128),
        (100.0, 0),
        (-10.0, 255),
        (150.0, 0),
    ];
    for (transparency, want) in cases {
        assert_eq!(
            opacity_from_transparency(*transparency),
            *want,
            "transparency {transparency}"
        );
    }
}

#[test]
fn transparency_variant_nan_paints_opaque() {
    assert_eq!(
        opacity_from_transparency(f64::NAN),
        255,
        "non-finite stays visible"
    );
    assert_eq!(
        opacity_from_transparency(f64::INFINITY),
        255,
        "positive infinite stays visible"
    );
    assert_eq!(
        opacity_from_transparency(f64::NEG_INFINITY),
        255,
        "negative infinite stays visible"
    );
}

#[test]
fn alpha_u8_reproduces_the_pinned_spelling_bit_exact() {
    // The primitive is the spelling: dense sweep plus every special,
    // compared bit-for-bit against the restated call-site expression.
    let mut sweep: Vec<f32> = (-400..=500)
        .map(|i| f64::from(i) / 255.0)
        .map(|v| v as f32)
        .collect();
    sweep.extend([
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -0.0,
        f32::MIN_POSITIVE,
        f32::EPSILON,
        1.0 - f32::EPSILON,
    ]);
    for alpha in sweep {
        assert_eq!(
            alpha_u8(alpha),
            current_alpha_spelling(alpha),
            "primitive agrees on alpha {alpha}"
        );
    }
}

#[test]
fn alpha_u8_reference_table() {
    let cases: &[(f32, u8)] = &[
        (0.0, 0),
        (1.0, 255),
        (0.5, 128),
        (0.5 / 128.0, 1),
        (-0.5, 0),
        (1.5, 255),
        (f32::INFINITY, 255),
        (f32::NEG_INFINITY, 0),
        (f32::NAN, 0),
    ];
    for (alpha, want) in cases {
        assert_eq!(alpha_u8(*alpha), *want, "alpha {alpha}");
    }
}
