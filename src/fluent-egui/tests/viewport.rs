use super::*;
use protopyne_chart_model::ChartRow;
use protopyne_chart_scale::PriceScaleMode;

fn viewport(mode: PriceScaleMode, min: f64, max: f64) -> Viewport {
    Viewport::new(mode, min, max, 400.0, 8.0, min, false).expect("valid test viewport")
}

fn row_at(time: i64, price: f64, volume: u64) -> ChartRow {
    ChartRow { time, open: price, high: price, low: price, close: price, volume }
}

#[test]
fn price_round_trip_stays_within_pixel_quantum() {
    for mode in [PriceScaleMode::Normal, PriceScaleMode::Logarithmic] {
        let pane = viewport(mode, 90.0, 110.0);
        // One pixel quantum in price units, with headroom for float rounding.
        let quantum = (110.0 - 90.0) / 400.0 * 8.0;
        for price in [90.0, 95.5, 100.0, 104.25, 110.0] {
            let y = pane.price_to_y(price).expect("mappable price");
            let back = pane.y_to_price(y).expect("invertible pixel");
            assert!(
                (back - price).abs() <= quantum,
                "{mode:?}: {price} -> {y} -> {back} exceeds pixel quantum {quantum}"
            );
        }
    }
}

#[test]
fn index_round_trip_inverts_tightly() {
    let pane = viewport(PriceScaleMode::Normal, 90.0, 110.0);
    for index in [0_usize, 1, 7, 100, 4096] {
        let x = pane.index_to_x(index);
        let back = pane.x_to_index(x).expect("finite x inverts");
        assert!((back - index as f64).abs() <= 1e-9, "index {index} -> {x} -> {back}");
    }
}

#[test]
fn non_finite_inputs_do_not_map() {
    let pane = viewport(PriceScaleMode::Normal, 90.0, 110.0);
    assert_eq!(pane.price_to_y(f64::NAN), None);
    assert_eq!(pane.price_to_y(f64::INFINITY), None);
    assert_eq!(pane.y_to_price(f32::NAN), None);
    assert_eq!(pane.x_to_index(f32::NAN), None);
}

#[test]
fn degenerate_geometry_refuses() {
    // Zero height.
    assert!(Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 0.0, 8.0, 90.0, false).is_err());
    // Negative bar spacing.
    assert!(Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, -1.0, 90.0, false).is_err());
    // Inverted range.
    assert!(Viewport::new(PriceScaleMode::Normal, 110.0, 90.0, 400.0, 8.0, 90.0, false).is_err());
    // Non-finite bounds.
    assert!(Viewport::new(PriceScaleMode::Normal, f64::NAN, 110.0, 400.0, 8.0, 90.0, false).is_err());
    // Log mode with non-positive bounds.
    assert!(
        Viewport::new(PriceScaleMode::Logarithmic, -5.0, 10.0, 400.0, 8.0, 1.0, false).is_err()
    );
    // Percentage mode rebased onto zero.
    assert!(
        Viewport::new(PriceScaleMode::Percentage, 90.0, 110.0, 400.0, 8.0, 0.0, false).is_err()
    );
    // Non-finite base value.
    assert!(
        Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, 8.0, f64::NAN, false).is_err()
    );
}

#[test]
fn error_names_the_refusing_parameter() {
    let err = Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 0.0, 8.0, 90.0, false)
        .expect_err("zero height refuses");
    assert!(err.to_string().contains("height"), "refusal names height: {err}");
}

#[test]
fn visible_rows_windows_without_copying_or_panicking() {
    let rows: Vec<ChartRow> = (0..10).map(|i| row_at(i, 100.0, 1)).collect();
    assert_eq!(visible_rows(&rows, 2, 3).len(), 3);
    assert_eq!(visible_rows(&rows, 2, 3)[0].time, 2);
    assert_eq!(visible_rows(&rows, 8, 100).len(), 2, "count saturates at the data edge");
    assert!(visible_rows(&rows, 50, 10).is_empty(), "start past the end shows empty");
    assert!(visible_rows(&[] as &[ChartRow], 0, 10).is_empty());
}

#[test]
fn thinning_keeps_sparse_grids_whole_and_caps_dense_ones() {
    use protopyne_chart_scale::nice_ticks;
    // Sparse range: everything the generator emits must survive.
    let sparse = nice_ticks(0.0, 1.0, 4);
    assert!(!sparse.is_empty());
    assert_eq!(thin_ticks(sparse.clone(), 8), sparse, "sparse grids are never thinned");
    // Dense range: the cap binds, legibility survives, order survives.
    let dense = nice_ticks(0.0, 1000.0, 64);
    assert!(dense.len() > 8, "fixture really is dense: {}", dense.len());
    let thinned = thin_ticks(dense.clone(), 8);
    assert!(thinned.len() <= 8, "cap binds: {}", thinned.len());
    assert!(thinned.len() >= 2, "legibility survives: {}", thinned.len());
    let mut ordered = thinned.clone();
    ordered.sort_by(|a, b| a.value.partial_cmp(&b.value).expect("finite ticks"));
    assert_eq!(
        ordered.iter().map(|t| t.value.to_bits()).collect::<Vec<_>>(),
        thinned.iter().map(|t| t.value.to_bits()).collect::<Vec<_>>(),
        "thinning preserves order"
    );
    for tick in &thinned {
        assert!(dense.iter().any(|d| d.value.to_bits() == tick.value.to_bits()));
        assert!(tick.value >= 0.0 && tick.value <= 1000.0);
    }
    // Zero cap hides the axis without refusing.
    assert!(thin_ticks(dense, 0).is_empty());
}

#[test]
fn axis_ticks_match_grid_then_cap() {
    use protopyne_chart_scale::nice_ticks;
    let (min, max, budget, cap) = (0.0, 1000.0, 64, 8);
    assert_eq!(axis_ticks(min, max, budget, cap), thin_ticks(nice_ticks(min, max, budget), cap));
    assert!(axis_ticks(f64::NAN, 10.0, 6, 8).is_empty(), "degenerate windows stay empty");
}
