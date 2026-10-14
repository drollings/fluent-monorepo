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

/// Calibration control (task value: legibility) over the full label path:
/// narrow and wide windows keep every grid tick within budget, and thinning
/// to the label cap preserves order, keeps byte-identical survivors from the
/// grid, and never crowds denser than the capped count allows.
#[test]
fn axis_ticks_keep_budget_and_spacing_on_narrow_and_wide_windows() {
    for (name, min, max, budget, cap) in
        [("narrow", 99.5, 100.5, 8, 8), ("wide", 0.0, 1_000_000.0, 8, 8), ("dense", 0.0, 100.0, 40, 8)]
    {
        let grid = nice_ticks(min, max, budget);
        let labels = axis_ticks(min, max, budget, cap);
        assert!(labels.len() <= cap, "{name}: label count within the readability cap");
        assert!(!labels.is_empty(), "{name}: usable windows label something");
        let grid_values: Vec<f64> = grid.iter().map(|tick| tick.value).collect();
        let label_values: Vec<f64> = labels.iter().map(|tick| tick.value).collect();
        assert!(
            label_values.iter().all(|value| grid_values.contains(value)),
            "{name}: survivors come from the grid unchanged"
        );
        let mut ordered = label_values.clone();
        ordered.sort_by(|a, b| a.partial_cmp(b).expect("finite labels"));
        assert_eq!(
            ordered.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
            label_values.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
            "{name}: thinning preserves order"
        );
        if label_values.len() >= 2 {
            let floor = label_values
                .windows(2)
                .map(|window| window[1] - window[0])
                .fold(f64::INFINITY, f64::min);
            assert!(
                floor * label_values.len() as f64 + 1e-9 * (max - min) >= max - min,
                "{name}: kept labels never crowd denser than the cap allows"
            );
        }
    }
}

/// Degenerate windows label nothing instead of refusing: the axis stays
/// empty rather than aborting the frame. (The grid-then-thin composition is
/// pinned property-by-property in `axis_ticks_keep_budget_and_spacing`
/// above; restating its one-line definition here could never fail.)
#[test]
fn axis_ticks_stay_empty_on_degenerate_windows() {
    assert!(axis_ticks(f64::NAN, 10.0, 6, 8).is_empty(), "degenerate windows stay empty");
}

/// Demo-equivalent price geometry: data spanning 99–101, the 2% pad the
/// terminal seeds its viewport with, and the 420 px price pane height.
fn demo_window() -> Viewport {
    Viewport::new(PriceScaleMode::Normal, 98.96, 101.04, 420.0, 8.0, 99.0, false)
        .expect("demo-equivalent window builds")
}

fn ticks_of(window: &Viewport) -> Vec<protopyne_chart_scale::Tick> {
    let extent = window.visible_range();
    axis_ticks(extent.min, extent.max, 8, 8)
}

fn inside_fraction(window: &Viewport, ticks: &[protopyne_chart_scale::Tick], height: f32) -> (usize, usize) {
    let inside = ticks
        .iter()
        .filter(|tick| {
            window.price_to_y(tick.value).is_some_and(|y| (0.0..=height).contains(&y))
        })
        .count();
    (inside, ticks.len())
}

#[test]
fn unzoomed_window_pins_its_tick_golden() {
    // The windowed grid for the resting window, pinned value-for-value: a
    // tick-density drift fails here instead of moving the axis silently.
    let ticks = ticks_of(&demo_window());
    let values: Vec<f64> = ticks.iter().map(|tick| tick.value).collect();
    assert_eq!(values, vec![99.0, 99.5, 100.0, 100.5, 101.0], "resting grid is pinned");
}

#[test]
fn windowed_ticks_stay_inside_the_pane() {
    // Ticks are drawn from the window on screen, so every one maps inside
    // it — unzoomed, 2x zoomed about the center, and panned a full span
    // right (partly past the data edge, as a real pan goes).
    let height = 420.0;
    let cases: [(&str, Viewport); 3] = [
        ("unzoomed", demo_window()),
        (
            "zoomed",
            Viewport::new(PriceScaleMode::Normal, 99.48, 100.52, height, 8.0, 99.0, false)
                .expect("2x window builds"),
        ),
        (
            "panned",
            Viewport::new(PriceScaleMode::Normal, 100.0, 102.08, height, 8.0, 99.0, false)
                .expect("panned window builds"),
        ),
    ];
    for (name, window) in cases {
        let ticks = ticks_of(&window);
        assert!(!ticks.is_empty(), "{name}: a usable window labels something");
        let (inside, total) = inside_fraction(&window, &ticks, height);
        assert_eq!(
            inside, total,
            "{name}: every windowed tick lands inside the pane, {inside}/{total} do"
        );
    }
}

#[test]
fn zoomed_window_keeps_four_fifths_of_ticks_inside() {
    // Totality, not discrimination: after a 2x zoom at least four fifths of
    // the drawn ticks map inside the pane rect. Ticks drawn from the data
    // extent instead manage 3/5 here — that shortfall is the desync this
    // windowed grid replaces.
    let height = 420.0;
    let zoomed = Viewport::new(PriceScaleMode::Normal, 99.48, 100.52, height, 8.0, 99.0, false)
        .expect("2x window builds");
    let ticks = ticks_of(&zoomed);
    let (inside, total) = inside_fraction(&zoomed, &ticks, height);
    assert!(
        inside * 5 >= total * 4,
        "at least four fifths of ticks land inside after zoom: {inside}/{total}"
    );
}
