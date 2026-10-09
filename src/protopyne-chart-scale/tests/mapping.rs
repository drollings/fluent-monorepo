use super::*;

fn scale(mode: PriceScaleMode, min: f64, max: f64) -> PriceScale {
    PriceScale::new(mode, PriceRange::new(min, max).expect("valid test range"), 400.0, 8.0, min, false)
        .expect("valid test scale")
}

#[test]
fn price_round_trips_within_a_tenth_of_a_percent() {
    let mapping = scale(PriceScaleMode::Normal, 90.0, 110.0);
    for price in [90.0, 95.5, 100.0, 104.25, 110.0] {
        let y = mapping.price_to_y(price).expect("mappable price");
        let back = mapping.y_to_price(y).expect("invertible pixel");
        let tolerance = price.abs() * 1e-3 + 1e-6;
        assert!((back - price).abs() <= tolerance, "price {price} -> {y} -> {back}");
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
    let normal =
        PriceScale::new(PriceScaleMode::Normal, range, 400.0, 8.0, 90.0, false).expect("valid scale");
    let flipped =
        PriceScale::new(PriceScaleMode::Normal, range, 400.0, 8.0, 90.0, true).expect("valid scale");
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
    let mapping =
        PriceScale::new(PriceScaleMode::Percentage, range, 400.0, 8.0, 100.0, false)
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
fn index_moves_by_bar_spacing() {
    let mapping = scale(PriceScaleMode::Normal, 90.0, 110.0);
    assert_eq!(mapping.index_to_x(0).to_bits(), 0.0_f32.to_bits());
    assert_eq!(mapping.index_to_x(10).to_bits(), 80.0_f32.to_bits());
    assert_eq!(mapping.x_to_index(80.0), Some(10.0));
    let half = mapping.x_to_index(4.0).expect("finite x inverts");
    assert!((half - 0.5).abs() < 1e-9);
}

#[test]
fn mode_parses_common_spellings() {
    use std::str::FromStr as _;
    assert_eq!(PriceScaleMode::from_str("log").expect("parses"), PriceScaleMode::Logarithmic);
    assert_eq!(PriceScaleMode::from_str("%").expect("parses"), PriceScaleMode::Percentage);
    assert!(PriceScaleMode::from_str("exponential").is_err());
}
