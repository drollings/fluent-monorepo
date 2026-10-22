use super::*;

fn scale(mode: PriceScaleMode, min: f64, max: f64) -> PriceScale {
    PriceScale::new(
        mode,
        PriceRange::new(min, max).expect("valid test range"),
        400.0,
        8.0,
        min,
        false,
    )
    .expect("valid test scale")
}

#[test]
fn price_round_trips_within_a_tenth_of_a_percent() {
    let mapping = scale(PriceScaleMode::Normal, 90.0, 110.0);
    for price in [90.0, 95.5, 100.0, 104.25, 110.0] {
        let y = mapping.price_to_y(price).expect("mappable price");
        let back = mapping.y_to_price(y).expect("invertible pixel");
        let tolerance = price.abs() * 1e-3 + 1e-6;
        assert!(
            (back - price).abs() <= tolerance,
            "price {price} -> {y} -> {back}"
        );
    }
}

#[test]
fn top_price_maps_near_top_and_bottom_near_bottom() {
    let mapping = scale(PriceScaleMode::Normal, 90.0, 110.0);
    let top = mapping.price_to_y(110.0).expect("top maps");
    let bottom = mapping.price_to_y(90.0).expect("bottom maps");
    assert!(top < 1.0);
    assert!((bottom - 400.0).abs() < 1.0);
    assert!(top < bottom);
}

#[test]
fn inverted_scale_flips_direction() {
    let range = PriceRange::new(90.0, 110.0).expect("valid test range");
    let normal = PriceScale::new(PriceScaleMode::Normal, range, 400.0, 8.0, 90.0, false)
        .expect("valid scale");
    let flipped = PriceScale::new(PriceScaleMode::Normal, range, 400.0, 8.0, 90.0, true)
        .expect("valid scale");
    let y_normal = normal.price_to_y(110.0).expect("maps");
    let y_flipped = flipped.price_to_y(110.0).expect("maps");
    assert!((y_normal + y_flipped - 400.0).abs() < 1e-3);
}

#[test]
fn log_mode_round_trips() {
    let mapping = scale(PriceScaleMode::Logarithmic, 90.0, 110.0);
    let y = mapping.price_to_y(100.0).expect("log maps positive");
    let back = mapping.y_to_price(y).expect("log inverts");
    assert!((back - 100.0).abs() <= 0.1);
}

#[test]
fn log_mode_refuses_non_positive_bounds() {
    assert!(PriceRange::new(-5.0, 10.0).is_ok(), "range itself is fine");
    let range = PriceRange::new(-5.0, 10.0).expect("range builds");
    assert!(PriceScale::new(PriceScaleMode::Logarithmic, range, 400.0, 8.0, 1.0, false).is_err());
}

#[test]
fn percentage_mode_rebases_onto_first_value() {
    let range = PriceRange::new(90.0, 110.0).expect("valid test range");
    let mapping = PriceScale::new(PriceScaleMode::Percentage, range, 400.0, 8.0, 100.0, false)
        .expect("valid scale");
    let y_base = mapping.price_to_y(100.0).expect("base maps");
    let y_high = mapping.price_to_y(110.0).expect("high maps");
    assert!(y_high < y_base, "a 10% gain sits above the baseline");
}

#[test]
fn degenerate_construction_refuses() {
    let range = PriceRange::new(90.0, 110.0).expect("valid test range");
    assert!(PriceScale::new(PriceScaleMode::Normal, range, 0.0, 8.0, 90.0, false).is_err());
    assert!(PriceScale::new(PriceScaleMode::Normal, range, 400.0, 0.0, 90.0, false).is_err());
    assert!(PriceRange::new(110.0, 90.0).is_err());
    assert!(PriceRange::new(f64::NAN, 110.0).is_err());
}

#[test]
fn non_finite_prices_do_not_map() {
    let mapping = scale(PriceScaleMode::Normal, 90.0, 110.0);
    assert!(mapping.price_to_y(f64::NAN).is_none());
    assert!(mapping.price_to_y(f64::INFINITY).is_none());
    assert!(mapping.y_to_price(f32::NAN).is_none());
}

#[test]
fn in_pane_round_trips_and_off_pane_refuses() {
    for (mode, min, max) in [
        (PriceScaleMode::Normal, 90.0, 110.0),
        (PriceScaleMode::Logarithmic, 90.0, 110.0),
        (PriceScaleMode::Percentage, 90.0, 110.0),
        (PriceScaleMode::IndexedTo100, 90.0, 110.0),
    ] {
        let mapping = scale(mode, min, max);
        for step in 0..=20 {
            let price = min + (max - min) * f64::from(step) / 20.0;
            let y = mapping.price_to_y(price).expect("in-pane price maps");
            let back = mapping.y_to_price(y).expect("in-pane pixel inverts");
            let tolerance = price.abs() * 1e-3 + 1e-6;
            assert!(
                (back - price).abs() <= tolerance,
                "{mode:?}: {price} -> {y} -> {back}"
            );
        }
        // The pane is exactly `0..=height`; both edges invert, either side refuses.
        assert!(
            mapping.y_to_price(0.0).is_some(),
            "{mode:?} top edge inverts"
        );
        assert!(
            mapping.y_to_price(400.0).is_some(),
            "{mode:?} bottom edge inverts"
        );
        assert_eq!(
            mapping.y_to_price(-0.5),
            None,
            "{mode:?} above the pane refuses"
        );
        assert_eq!(
            mapping.y_to_price(400.5),
            None,
            "{mode:?} below the pane refuses"
        );
    }
}

#[test]
fn inverted_scale_off_pane_refuses() {
    let range = PriceRange::new(90.0, 110.0).expect("valid range");
    let flipped = PriceScale::new(PriceScaleMode::Normal, range, 400.0, 8.0, 90.0, true)
        .expect("valid scale");
    assert!(flipped.y_to_price(200.0).is_some());
    assert_eq!(flipped.y_to_price(-1.0), None);
    assert_eq!(flipped.y_to_price(401.0), None);
}

/// Audit of each direction's doc/behavior contract, pinned so a future change
/// that silently drops extrapolation is caught here:
/// - `price_to_y` extrapolates prices outside the window on purpose — the
///   paint edge clips lines and candles that run off the pane — and its doc
///   only promises `None` for non-finite/unmappable input, so it matches.
/// - `y_to_price` promises "None off-pane"; it now refuses `y` outside
///   `0..=height` (the pane), so it matches.
/// - `index_to_x` is documented total (no refusal) and stays total.
/// - `x_to_index` promises `None` only on non-finite input and matches.
#[test]
fn mapping_promises_match_behavior() {
    let mapping = scale(PriceScaleMode::Normal, 90.0, 110.0);
    let extrapolated = mapping
        .price_to_y(200.0)
        .expect("price_to_y extrapolates by design");
    assert!(
        extrapolated < 0.0,
        "an above-window price maps above the pane"
    );
    assert_eq!(
        mapping.y_to_price(-1.0),
        None,
        "y_to_price refuses off-pane"
    );
    assert_eq!(mapping.index_to_x(0).to_bits(), 0.0_f32.to_bits());
    assert_eq!(mapping.x_to_index(f32::INFINITY), None);
}

#[test]
fn index_moves_by_bar_spacing() {
    let mapping = scale(PriceScaleMode::Normal, 90.0, 110.0);
    assert_eq!(mapping.index_to_x(0).to_bits(), 0.0_f32.to_bits());
    assert_eq!(mapping.index_to_x(10).to_bits(), 80.0_f32.to_bits());
    assert_eq!(mapping.x_to_index(80.0), Some(10.0));
    let half = mapping.x_to_index(4.0).expect("finite x inverts");
    assert!((half - 0.5).abs() < 1e-9);
}

/// Reference mapping recomputed from first principles (no cache): a
/// construction-cached `PriceScale` must equal re-deriving `to_scaled` over
/// the endpoints.
fn reference_y(
    mode: PriceScaleMode,
    base: f64,
    min: f64,
    max: f64,
    height: f64,
    inverted: bool,
    price: f64,
) -> f64 {
    let scaled = |p: f64| match mode {
        PriceScaleMode::Normal => p,
        PriceScaleMode::Logarithmic => p.ln(),
        PriceScaleMode::Percentage => (p - base) / base * 100.0,
        PriceScaleMode::IndexedTo100 => p / base * 100.0,
    };
    let low = scaled(min);
    let high = scaled(max);
    let y = (high - scaled(price)) / (high - low) * height;
    if inverted {
        height - y
    } else {
        y
    }
}

#[test]
fn cached_bounds_match_a_fresh_recompute_in_every_mode() {
    for mode in [
        PriceScaleMode::Normal,
        PriceScaleMode::Logarithmic,
        PriceScaleMode::Percentage,
        PriceScaleMode::IndexedTo100,
    ] {
        let mapping = scale(mode, 90.0, 110.0);
        for step in 0..=40 {
            let price = 90.0 + 20.0 * f64::from(step) / 40.0;
            let cached = mapping.price_to_y(price).expect("in-window price maps");
            let fresh = reference_y(mode, 90.0, 90.0, 110.0, 400.0, false, price) as f32;
            assert_eq!(
                cached.to_bits(),
                fresh.to_bits(),
                "{mode:?} at {price}: cached {cached} vs fresh {fresh}"
            );
        }
    }
}

#[test]
fn degenerate_scaled_bounds_map_to_none() {
    // A window narrower than the scaled-span guard has no image: construction
    // still succeeds (the viewport still builds) and every map refuses — the
    // same lazy refusal as before the pair was cached at construction.
    let tiny = PriceRange::new(1.0, 1.0 + 1e-13).expect("range is well-formed");
    let mapping = PriceScale::new(PriceScaleMode::Normal, tiny, 400.0, 8.0, 1.0, false)
        .expect("degenerate geometry still constructs");
    assert_eq!(mapping.price_to_y(1.0), None);
    assert_eq!(mapping.y_to_price(200.0), None);
}

#[test]
fn mode_parses_common_spellings() {
    use std::str::FromStr as _;
    assert_eq!(
        PriceScaleMode::from_str("log").expect("parses"),
        PriceScaleMode::Logarithmic
    );
    assert_eq!(
        PriceScaleMode::from_str("%").expect("parses"),
        PriceScaleMode::Percentage
    );
    assert!(PriceScaleMode::from_str("exponential").is_err());
}

#[test]
fn price_range_contains_closed_touching_nan_saturation() {
    let range = PriceRange::new(90.0, 110.0).expect("valid range");
    assert!(range.contains(90.0), "closed: min inside");
    assert!(range.contains(110.0), "closed: max inside");
    assert!(range.contains(100.0));
    assert!(!range.contains(89.999), "just below min outside");
    assert!(!range.contains(110.001), "just above max outside");
    assert!(!range.contains(f64::NAN), "NaN never inside");
    assert!(!range.contains(f64::INFINITY));
    assert!(PriceRange::new(110.0, 90.0).is_err(), "inverted refuses");
    assert!(PriceRange::new(90.0, 90.0).is_err(), "empty refuses");
    assert!(PriceRange::new(f64::NAN, 110.0).is_err());
    let tiny = PriceRange::new(1.0, 1.0 + 1e-13).expect("tiny builds");
    assert_eq!(
        tiny.length(),
        1e-12,
        "span saturates, never divides by zero"
    );
}
