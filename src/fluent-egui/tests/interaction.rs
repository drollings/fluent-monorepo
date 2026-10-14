use super::*;
use protopyne_chart_scale::PriceScaleMode;

/// Price pane `[90, 110]` on 400 px: 20 px per price unit.
fn pane() -> Viewport {
    Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, 8.0, 90.0, false)
        .expect("valid price pane")
}

/// Wide elastic bounds `[0, 200]`: the `[90, 110]` pane roams freely.
fn free_state() -> InteractionState {
    InteractionState::new(0.0, 200.0).expect("valid bounds")
}

fn range_of(viewport: &Viewport) -> (f64, f64) {
    let range = viewport.scale().range();
    (range.min, range.max)
}

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() <= 1e-9 * expected.abs().max(1.0)
}

#[test]
fn bounds_refuse_degenerate() {
    assert!(InteractionState::new(10.0, 10.0).is_err(), "empty extent refuses");
    assert!(InteractionState::new(20.0, 10.0).is_err(), "inverted extent refuses");
    assert!(InteractionState::new(f64::NAN, 10.0).is_err(), "non-finite extent refuses");
    let err = InteractionState::new(0.0, 0.0).expect_err("refusal carries a reason");
    assert!(err.to_string().contains("bounds"), "refusal names bounds: {err}");
}

#[test]
fn same_inputs_give_same_viewport() {
    fn script() -> (Vec<(f64, f64)>, Vec<f64>) {
        let mut state = free_state();
        let script = vec![
            (1.0 / 60.0, vec![PointerInput::Drag { dy_px: 40.0 }]),
            (1.0 / 60.0, vec![PointerInput::Wheel { delta_px: 50.0, anchor_y: 200.0 }]),
            (1.0 / 60.0, vec![]),
            (1.0 / 30.0, vec![]),
            (1.0 / 60.0, vec![PointerInput::Pinch { scale: 1.5, anchor_y: 100.0 }]),
        ];
        let mut ranges = Vec::new();
        let mut velocities = Vec::new();
        let mut current = pane();
        for (dt, events) in &script {
            current = state.step(*dt, &current, events);
            ranges.push(range_of(&current));
            velocities.push(state.kinetic_velocity());
        }
        (ranges, velocities)
    }
    let (first_ranges, first_velocities) = script();
    let (second_ranges, second_velocities) = script();
    assert_eq!(first_ranges, second_ranges, "replay is bit-identical");
    assert_eq!(first_velocities, second_velocities, "velocity replay is bit-identical");
}

#[test]
fn drag_pans_price_window_grab_style() {
    let mut state = free_state();
    let start = pane();
    // Dragging down 40 px pulls content down: the window rises 2 price units.
    let moved = state.step(1.0 / 60.0, &start, &[PointerInput::Drag { dy_px: 40.0 }]);
    assert_eq!(range_of(&moved), (92.0, 112.0), "grab drag shifts the window by dy-mapped prices");
    // Dragging back up restores the window exactly.
    let back = state.step(1.0 / 60.0, &moved, &[PointerInput::Drag { dy_px: -40.0 }]);
    assert_eq!(range_of(&back), (90.0, 110.0), "opposite drags cancel bit-for-bit");
}

#[test]
fn drag_seeds_kinetic_velocity() {
    let mut state = free_state();
    assert!(state.is_settled(), "fresh state is at rest");
    let _dragged = state.step(1.0 / 60.0, &pane(), &[PointerInput::Drag { dy_px: 40.0 }]);
    // 2 price units in 1/60 s, half-blended into rest: ~60 price/s.
    assert!(close(state.kinetic_velocity(), 60.0), "velocity seeds from the drag: {}", state.kinetic_velocity());
    assert!(!state.is_settled(), "a drag leaves motion to coast");
}

#[test]
fn wheel_zooms_around_anchor() {
    let mut state = free_state();
    let start = pane();
    let zoomed = state.step(
        1.0 / 60.0,
        &start,
        &[PointerInput::Wheel { delta_px: 50.0, anchor_y: 200.0 }],
    );
    let (min, max) = range_of(&zoomed);
    // Anchor price (100 at mid-pane) is preserved; the span shrinks by 1.15x.
    let anchor = zoomed.y_to_price(200.0).expect("anchor stays mapped");
    assert!(close(anchor, 100.0), "zoom keeps the anchor price: {anchor}");
    assert!(close(max - min, 20.0 / 1.15), "one wheel step scales the span: {}", max - min);
    assert!(min > 90.0 && max < 110.0, "scroll up zooms in: [{min}, {max}]");
    // Scroll down zooms back out past the start span.
    let out = state.step(
        1.0 / 60.0,
        &zoomed,
        &[PointerInput::Wheel { delta_px: -50.0, anchor_y: 200.0 }],
    );
    let (omin, omax) = range_of(&out);
    assert!(omax - omin > max - min, "scroll down zooms out");
    assert!(close(omin + omax, 200.0), "zoom stays centered on the anchor: [{omin}, {omax}]");
}

#[test]
fn pinch_and_box_zoom_known_answers() {
    let mut state = free_state();
    // Pinch scale 2 at mid-pane halves the span around 100.
    let pinched = state.step(
        1.0 / 60.0,
        &pane(),
        &[PointerInput::Pinch { scale: 2.0, anchor_y: 200.0 }],
    );
    let (min, max) = range_of(&pinched);
    assert!(close(min, 95.0) && close(max, 105.0), "pinch halves around the anchor: [{min}, {max}]");
    // Box y=[100, 300] spans prices [95, 105] exactly.
    let boxed = state.step(
        1.0 / 60.0,
        &pane(),
        &[PointerInput::BoxZoom { top_y: 100.0, bottom_y: 300.0 }],
    );
    assert_eq!(range_of(&boxed), (95.0, 105.0), "box sets the spanned range");
    // Reversed box ends name the same window.
    let reversed = state.step(
        1.0 / 60.0,
        &pane(),
        &[PointerInput::BoxZoom { top_y: 300.0, bottom_y: 100.0 }],
    );
    assert_eq!(range_of(&reversed), (95.0, 105.0), "box ends are order-independent");
    // A zero-height box names no window and holds.
    let held = state.step(
        1.0 / 60.0,
        &pane(),
        &[PointerInput::BoxZoom { top_y: 200.0, bottom_y: 200.0 }],
    );
    assert_eq!(range_of(&held), (90.0, 110.0), "degenerate box holds the window");
}

#[test]
fn off_pane_anchors_hold_the_window() {
    let mut state = free_state();
    let start = pane();
    for events in [
        vec![PointerInput::Wheel { delta_px: 50.0, anchor_y: -10.0 }],
        vec![PointerInput::Wheel { delta_px: 50.0, anchor_y: 450.0 }],
        vec![PointerInput::Pinch { scale: 2.0, anchor_y: f32::NAN }],
        vec![PointerInput::BoxZoom { top_y: -5.0, bottom_y: 200.0 }],
    ] {
        let held = state.step(1.0 / 60.0, &start, &events);
        assert_eq!(range_of(&held), (90.0, 110.0), "off-pane anchor holds: {events:?}");
    }
}

#[test]
fn coast_decays_monotonically_and_settles() {
    let mut state = free_state();
    let dragged = state.step(1.0 / 60.0, &pane(), &[PointerInput::Drag { dy_px: 80.0 }]);
    let mut previous = state.kinetic_velocity();
    assert!(previous > 0.0, "drag seeds forward motion");
    let mut current = dragged;
    for _ in 0..600 {
        current = state.step(1.0 / 60.0, &current, &[]);
        let velocity = state.kinetic_velocity();
        assert!(velocity <= previous, "energy never grows without input: {velocity} <= {previous}");
        if previous != 0.0 {
            assert!(velocity < previous, "coast strictly decays until rest");
        }
        previous = velocity;
    }
    assert!(state.is_settled(), "coast reaches rest");
    // Bit-exact: the rest snap assigns literal zero, so replay holds it.
    assert_eq!(state.kinetic_velocity().to_bits(), 0.0f64.to_bits(), "rest is exact zero");
    let settled_range = range_of(&current);
    let again = state.step(1.0 / 60.0, &current, &[]);
    assert_eq!(range_of(&again), settled_range, "rest holds the window bit-for-bit");
}

#[test]
fn at_rest_stays_at_rest() {
    let mut state = free_state();
    let start = pane();
    let mut current = start;
    for _ in 0..10 {
        current = state.step(1.0 / 60.0, &current, &[]);
    }
    assert_eq!(range_of(&current), (90.0, 110.0), "no input means no motion");
    assert!(state.is_settled(), "rest never self-excites");
}

#[test]
fn discrete_gestures_ignore_dt() {
    let gestures =
        vec![vec![PointerInput::Drag { dy_px: 40.0 }], vec![PointerInput::Drag { dy_px: -13.0 }]];
    let run = |dt: f64| {
        let mut state = free_state();
        let mut current = pane();
        for events in &gestures {
            current = state.step(dt, &current, events);
        }
        range_of(&current)
    };
    assert_eq!(run(1.0 / 60.0), run(1.0 / 30.0), "displacements never read dt");
}

#[test]
fn coast_splitting_matches_closed_form() {
    // Same seed, same total coast time, different splits: closed-form decay
    // keeps both trajectories together.
    let seed = |state: &mut InteractionState| {
        state.step(1.0 / 60.0, &pane(), &[PointerInput::Drag { dy_px: 80.0 }])
    };
    let mut whole = free_state();
    let mut current = seed(&mut whole);
    current = whole.step(0.2, &current, &[]);
    let (wmin, wmax) = range_of(&current);
    let whole_velocity = whole.kinetic_velocity();

    let mut split = free_state();
    current = seed(&mut split);
    for _ in 0..4 {
        current = split.step(0.05, &current, &[]);
    }
    let (smin, smax) = range_of(&current);
    assert!(close(wmin, smin) && close(wmax, smax), "split coast matches: [{wmin}, {wmax}] vs [{smin}, {smax}]");
    assert!(
        close(whole_velocity, split.kinetic_velocity()),
        "split decay matches: {} vs {}",
        whole_velocity,
        split.kinetic_velocity()
    );
}

#[test]
fn zoom_kills_coast_velocity() {
    let mut state = free_state();
    let dragged = state.step(1.0 / 60.0, &pane(), &[PointerInput::Drag { dy_px: 80.0 }]);
    assert!(!state.is_settled(), "drag seeds motion");
    let zoomed = state.step(
        1.0 / 60.0,
        &dragged,
        &[PointerInput::Wheel { delta_px: 50.0, anchor_y: 200.0 }],
    );
    assert!(state.is_settled(), "re-anchoring stops the coast");
    let (min, max) = range_of(&zoomed);
    assert!(max - min < 20.0, "the zoom still applies: {}", max - min);
}

/// Calibration control (task value: feel): far from the elastic walls,
/// gestures behave exactly linearly — no clamp interference, no spurious
/// wall absorption. The coast then decays to rest deep inside the envelope,
/// proving the wall never fired (a wall stop would pin an envelope edge).
#[test]
fn far_from_target_motion_is_exact_and_wall_free() {
    let mut state = InteractionState::new(0.0, 1000.0).expect("valid bounds");
    let mut current = pane();
    for dy in [40.0_f32, 40.0, 10.0] {
        current = state.step(1.0 / 60.0, &current, &[PointerInput::Drag { dy_px: dy }]);
    }
    assert_eq!(range_of(&current), (94.5, 114.5), "in-bounds drags sum exactly");
    assert!(!state.is_settled(), "no spurious wall absorption");
    for _ in 0..600 {
        current = state.step(1.0 / 60.0, &current, &[]);
    }
    assert!(state.is_settled(), "decay (not the wall) settles the coast");
    let (min, max) = range_of(&current);
    assert!(min > 0.0 && max < 1000.0, "rest sits deep inside: [{min}, {max}]");
    assert!(close(max - min, 20.0), "translation preserves the span: {}", max - min);
}

/// Calibration control: non-positive or non-finite `dt` never invents or
/// destroys kinetic state — displacements still apply, rates do not.
#[test]
fn invalid_dt_holds_kinetic_state() {
    for dt in [0.0, -1.0, f64::NAN] {
        let mut state = free_state();
        let moved = state.step(dt, &pane(), &[PointerInput::Drag { dy_px: 40.0 }]);
        assert_eq!(range_of(&moved), (92.0, 112.0), "displacement never reads dt ({dt})");
        assert!(state.is_settled(), "bad dt seeds no velocity ({dt})");
        let held = state.step(1.0 / 60.0, &moved, &[]);
        assert_eq!(range_of(&held), (92.0, 112.0), "no phantom motion ({dt})");
    }
}

/// A subnormal-but-positive `dt` overflows `shift / dt` into an infinite
/// rate: the displacement still applies, but no coast velocity may seed.
/// Without the finiteness guard this step leaves an infinite velocity that
/// coasts forever.
#[test]
fn subnormal_dt_seeds_no_velocity() {
    let dt = f64::MIN_POSITIVE / 2.0;
    assert!(dt > 0.0 && dt.is_finite(), "the probe dt is subnormal-but-positive");
    let mut state = free_state();
    let moved = state.step(dt, &pane(), &[PointerInput::Drag { dy_px: 40.0 }]);
    assert_eq!(range_of(&moved), (92.0, 112.0), "displacement still applies");
    assert!(state.is_settled(), "an infinite rate seeds no velocity");
    assert!(
        state.kinetic_velocity().is_finite(),
        "velocity stays finite: {}",
        state.kinetic_velocity()
    );
    let held = state.step(1.0 / 60.0, &moved, &[]);
    assert_eq!(range_of(&held), (92.0, 112.0), "no phantom coast follows");
}

#[test]
fn elastic_bounds_never_exceeded() {
    // Data extent [90, 110]: margin 15% of 20 = 3 per side; the hard envelope
    // is [87, 113].
    let mut state = InteractionState::new(90.0, 110.0).expect("valid bounds");
    let start = pane();
    let slammed = state.step(1.0 / 60.0, &start, &[PointerInput::Drag { dy_px: 4000.0 }]);
    let (min, max) = range_of(&slammed);
    assert!(min >= 87.0 && max <= 113.0, "wall holds: [{min}, {max}]");
    assert_eq!((min, max), (93.0, 113.0), "sustained pull pins the wall, span preserved");
    assert!(min < max, "the pinned window stays valid");
    // Zooming out past the data also stops at the envelope.
    let mut zoomed = start;
    for _ in 0..40 {
        zoomed = state.step(1.0 / 60.0, &zoomed, &[PointerInput::Wheel { delta_px: -50.0, anchor_y: 200.0 }]);
    }
    let (zmin, zmax) = range_of(&zoomed);
    assert!(zmin >= 87.0 && zmax <= 113.0, "zoom-out stops at the envelope: [{zmin}, {zmax}]");
    // Wall contact absorbs the coast: release into the wall settles.
    assert!(state.is_settled(), "the wall absorbs velocity");
}
