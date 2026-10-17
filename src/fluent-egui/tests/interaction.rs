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

/// A generous horizontal envelope so coasting is never clamped mid-test.
fn horizontal_state() -> InteractionState {
    free_state().with_horizontal_bounds(1000, 320.0).expect("valid horizontal bounds")
}

#[test]
fn pan_2d_shifts_price_and_time_axes_and_coasts_in_closed_form() {
    let mut state = horizontal_state();
    let start = pane(); // spacing 8, offset 0
    let moved = state.step(
        1.0 / 60.0,
        &start,
        &[PointerInput::Pan2D { dx_px: 40.0, dy_px: 40.0 }],
    );
    // The vertical leg matches a plain drag; the horizontal leg shifts the
    // time-axis offset by exactly dx.
    assert_eq!(range_of(&moved), (92.0, 112.0), "vertical pan matches drag semantics");
    assert_eq!(moved.x_offset_px().to_bits(), 40.0f32.to_bits(), "horizontal pan shifts x by dx");
    assert!(state.kinetic_velocity() > 0.0, "vertical coast seeded");
    assert!(state.kinetic_velocity_x() > 0.0, "horizontal coast seeded");
    assert!(!state.is_settled(), "a 2D pan leaves both axes coasting");

    // Closed-form split: same seed, same total coast, different splits agree
    // on both axes.
    let seed = |s: &mut InteractionState| {
        s.step(1.0 / 60.0, &start, &[PointerInput::Pan2D { dx_px: 40.0, dy_px: 40.0 }])
    };
    let mut whole = horizontal_state();
    let mut current = seed(&mut whole);
    current = whole.step(0.2, &current, &[]);
    let (wmin, wmax) = range_of(&current);
    let wx = f64::from(current.x_offset_px());
    let wvx = whole.kinetic_velocity_x();

    let mut split = horizontal_state();
    current = seed(&mut split);
    for _ in 0..4 {
        current = split.step(0.05, &current, &[]);
    }
    let (smin, smax) = range_of(&current);
    assert!(close(wmin, smin) && close(wmax, smax), "vertical split coast matches");
    assert!(
        (wx - f64::from(current.x_offset_px())).abs() <= 1e-6,
        "horizontal split coast matches: {wx} vs {}",
        current.x_offset_px()
    );
    assert!(close(wvx, split.kinetic_velocity_x()), "horizontal decay matches");
}

#[test]
fn wheel_time_zooms_bar_spacing_anchored_at_cursor_x() {
    let mut state = horizontal_state();
    let start = pane(); // spacing 8, offset 0
    let anchor_x = 160.0f32;
    let slot_before = start.x_to_index(anchor_x).expect("finite"); // 20.0
    let zoomed = state.step(
        1.0 / 60.0,
        &start,
        &[PointerInput::WheelTime { delta_px: 50.0, anchor_x }],
    );
    let spacing = zoomed.scale().bar_spacing();
    assert!(spacing > 8.0, "scroll up widens the bars: {spacing}");
    assert!(
        (f64::from(spacing) - 8.0 * 1.15).abs() <= 1e-4,
        "one step scales spacing by 1.15: {spacing}"
    );
    // The bar slot under the cursor is invariant, so the bar stays put.
    let slot_after = zoomed.x_to_index(anchor_x).expect("finite");
    assert!(
        (slot_after - slot_before).abs() <= 1e-6,
        "slot under the cursor is invariant: {slot_before} vs {slot_after}"
    );
    assert!(
        (f64::from(zoomed.slot_to_x(slot_before)) - f64::from(anchor_x)).abs()
            <= f64::from(ANCHOR_DRIFT_EPS_PX),
        "the bar stays under the cursor pixel"
    );
    // Scroll down zooms back out around the same anchor.
    let out = state.step(
        1.0 / 60.0,
        &zoomed,
        &[PointerInput::WheelTime { delta_px: -50.0, anchor_x }],
    );
    assert!(out.scale().bar_spacing() < spacing, "scroll down narrows the bars");
    let out_slot = out.x_to_index(anchor_x).expect("finite");
    assert!((out_slot - slot_before).abs() <= 1e-6, "anchor holds across zoom-out");
}

#[test]
fn horizontal_elastic_and_spacing_rails_hold() {
    let start = pane(); // spacing 8
    // Slam the time axis right: hi_lim = margin_slots * spacing = 6 * 8 = 48.
    let mut state = free_state().with_horizontal_bounds(40, 320.0).expect("bounds");
    let slammed = state.step(
        1.0 / 60.0,
        &start,
        &[PointerInput::Pan2D { dx_px: 4000.0, dy_px: 0.0 }],
    );
    assert!(
        (f64::from(slammed.x_offset_px()) - 48.0).abs() <= 1e-3,
        "horizontal wall holds at hi_lim: {}",
        slammed.x_offset_px()
    );
    assert!(state.is_settled(), "wall contact absorbs the horizontal coast");
    // Slam the time axis left: lo_lim = 320 - (40 + 6) * 8 = -48.
    let mut state = free_state().with_horizontal_bounds(40, 320.0).expect("bounds");
    let slammed = state.step(
        1.0 / 60.0,
        &start,
        &[PointerInput::Pan2D { dx_px: -4000.0, dy_px: 0.0 }],
    );
    assert!(
        (f64::from(slammed.x_offset_px()) + 48.0).abs() <= 1e-3,
        "horizontal wall holds at lo_lim: {}",
        slammed.x_offset_px()
    );
    // Spacing rails: sustained zoom-in clamps at MAX_BAR_SPACING.
    let mut state = free_state().with_horizontal_bounds(40, 320.0).expect("bounds");
    let mut current = start;
    for _ in 0..40 {
        current = state.step(
            1.0 / 60.0,
            &current,
            &[PointerInput::WheelTime { delta_px: 50.0, anchor_x: 160.0 }],
        );
    }
    assert!(
        (current.scale().bar_spacing() - MAX_BAR_SPACING).abs() <= 1e-3,
        "spacing clamps at MAX_BAR_SPACING: {}",
        current.scale().bar_spacing()
    );
    // Sustained zoom-out clamps at MIN_BAR_SPACING.
    let mut state = free_state().with_horizontal_bounds(40, 320.0).expect("bounds");
    let mut current = start;
    for _ in 0..80 {
        current = state.step(
            1.0 / 60.0,
            &current,
            &[PointerInput::WheelTime { delta_px: -50.0, anchor_x: 160.0 }],
        );
    }
    assert!(
        (current.scale().bar_spacing() - MIN_BAR_SPACING).abs() <= 1e-3,
        "spacing clamps at MIN_BAR_SPACING: {}",
        current.scale().bar_spacing()
    );
    // Bad horizontal bounds refuse with the parameter named.
    assert!(free_state().with_horizontal_bounds(0, 320.0).is_err(), "zero bars refuses");
    assert!(free_state().with_horizontal_bounds(40, 0.0).is_err(), "zero width refuses");
}

/// Calibration controls for the 2D pan and cursor-anchored wheel-zoom
/// heuristics: at rest nothing drifts, and across a full zoom sweep the bar
/// slot under the cursor stays pinned to its anchor pixel while the spacing
/// rails hold.
#[test]
fn cursor_anchored_wheel_zoom_and_2d_pan_calibration_controls() {
    // Control group (at rest / zero delta): 120 idle frames move neither the
    // horizontal offset nor the bar spacing.
    let mut state = free_state().with_horizontal_bounds(5_000, 320.0).expect("valid bounds");
    let start = pane(); // spacing 8, offset 0
    let mut current = start;
    for frame in 0..120 {
        let events = if frame % 2 == 0 {
            vec![PointerInput::Pan2D { dx_px: 0.0, dy_px: 0.0 }]
        } else {
            vec![PointerInput::WheelTime { delta_px: 0.0, anchor_x: 160.0 }]
        };
        current = state.step(1.0 / 60.0, &current, &events);
    }
    assert_eq!(
        current.x_offset_px().to_bits(),
        start.x_offset_px().to_bits(),
        "idle frames hold the time offset bit-for-bit"
    );
    assert_eq!(
        current.scale().bar_spacing().to_bits(),
        start.scale().bar_spacing().to_bits(),
        "idle frames hold the bar spacing bit-for-bit"
    );
    assert!(state.is_settled(), "zero-delta gestures seed no coast");

    // Precision: across 20 zoom-in and 20 zoom-out steps around three in-pane
    // anchors, the bar slot under the cursor never drifts past the tolerance.
    // The data is far wider than the pane, so the horizontal envelope never
    // clamps the offset the anchor math sets.
    for anchor_x in [40.0_f32, 160.0, 310.0] {
        let slot = start.x_to_index(anchor_x).expect("finite anchor slot");
        for delta in [50.0_f32, -50.0] {
            let mut state = free_state().with_horizontal_bounds(5_000, 320.0).expect("valid bounds");
            let mut current = start;
            for step in 0..20 {
                current = state.step(
                    1.0 / 60.0,
                    &current,
                    &[PointerInput::WheelTime { delta_px: delta, anchor_x }],
                );
                let drift = (f64::from(current.slot_to_x(slot)) - f64::from(anchor_x)).abs();
                assert!(
                    drift <= f64::from(ANCHOR_DRIFT_EPS_PX),
                    "anchor {anchor_x} delta {delta} step {step}: bar drifted {drift} px"
                );
            }
        }
    }

    // The spacing rails hold across a sustained zoom sweep.
    let mut state = free_state().with_horizontal_bounds(5_000, 320.0).expect("valid bounds");
    let mut zoomed_in = start;
    for _ in 0..40 {
        zoomed_in = state.step(
            1.0 / 60.0,
            &zoomed_in,
            &[PointerInput::WheelTime { delta_px: 50.0, anchor_x: 160.0 }],
        );
    }
    assert!(
        (zoomed_in.scale().bar_spacing() - MAX_BAR_SPACING).abs() <= 1e-3,
        "zoom-in clamps at the max rail: {}",
        zoomed_in.scale().bar_spacing()
    );
    let mut state = free_state().with_horizontal_bounds(5_000, 320.0).expect("valid bounds");
    let mut zoomed_out = start;
    for _ in 0..40 {
        zoomed_out = state.step(
            1.0 / 60.0,
            &zoomed_out,
            &[PointerInput::WheelTime { delta_px: -50.0, anchor_x: 160.0 }],
        );
    }
    assert!(
        (zoomed_out.scale().bar_spacing() - MIN_BAR_SPACING).abs() <= 1e-3,
        "zoom-out clamps at the min rail: {}",
        zoomed_out.scale().bar_spacing()
    );
}

#[test]
fn feel_constants_are_pinned() {
    // Calibration gate: these are frozen feel heuristics (task value, never
    // producer confidence). A retune edits the constant and this pin
    // together, then re-runs the whole interaction suite — never a silent
    // feel edit.
    // Bit-exact pins (no float tolerance: a retune must move the pin).
    for (name, got, want) in [
        ("KINETIC_DAMPING_BASE", KINETIC_DAMPING_BASE, 0.92f64),
        ("WHEEL_ZOOM_BASE", WHEEL_ZOOM_BASE, 1.15),
        ("WHEEL_STEP_PX", WHEEL_STEP_PX, 50.0),
        ("REST_FRACTION_OF_SPAN", REST_FRACTION_OF_SPAN, 1e-4),
        ("ELASTIC_FRACTION", ELASTIC_FRACTION, 0.15),
        ("VELOCITY_BLEND", VELOCITY_BLEND, 0.5),
    ] {
        assert_eq!(got.to_bits(), want.to_bits(), "{name} is frozen");
    }
    assert_eq!(MIN_BAR_SPACING.to_bits(), 1.0f32.to_bits(), "zoom rail floor is frozen");
    assert_eq!(MAX_BAR_SPACING.to_bits(), 64.0f32.to_bits(), "zoom rail ceiling is frozen");
}

#[test]
fn auto_height_reanchors_through_the_machine() {
    // The menu action rides the same machine as every other range change:
    // the fitted range applies, the vertical coast stops, and the time axis
    // (spacing, offset, horizontal coast) is untouched.
    let start = pane();
    let mut state = free_state().with_horizontal_bounds(100, 400.0).expect("valid bounds");
    let _ = state.step(1.0 / 60.0, &start, &[PointerInput::Drag { dy_px: 40.0 }]);
    assert!(!state.is_settled(), "a drag seeds a coast");
    let spacing_before = start.scale().bar_spacing();
    let fitted = state.step(
        1.0 / 60.0,
        &start,
        &[PointerInput::AutoHeight { min: 95.0, max: 105.0 }],
    );
    assert_eq!(state.kinetic_velocity().to_bits(), 0.0f64.to_bits(), "re-anchoring stops the coast");
    assert!(state.is_settled(), "no horizontal coast was seeded");
    let range = fitted.scale().range();
    assert_eq!((range.min, range.max), (95.0, 105.0), "the fitted range applies");
    assert_eq!(fitted.scale().bar_spacing().to_bits(), spacing_before.to_bits(), "time axis untouched");
    assert_eq!(fitted.x_offset_px().to_bits(), start.x_offset_px().to_bits(), "pan offset untouched");
}

#[test]
fn auto_height_refusals_hold_the_window() {
    let start = pane();
    let mut state = free_state();
    for event in [
        PointerInput::AutoHeight { min: f64::NAN, max: 105.0 },
        PointerInput::AutoHeight { min: 105.0, max: 95.0 },
        PointerInput::AutoHeight { min: 95.0, max: 95.0 },
    ] {
        let held = state.step(1.0 / 60.0, &start, &[event]);
        let range = held.scale().range();
        assert_eq!((range.min, range.max), (90.0, 110.0), "a refusal holds the window");
        assert!(state.is_settled(), "a refusal seeds no coast");
    }
}

#[test]
fn scripted_auto_height_replays_deterministically() {
    // (pan, wheel, AutoHeight, wheel): the same log twice yields the same
    // viewport sequence, and the middle re-anchor composes — wheeling after
    // the fit equals wheeling the fitted viewport directly.
    let start = pane();
    let log = vec![
        PointerInput::Pan2D { dx_px: 24.0, dy_px: 30.0 },
        PointerInput::Wheel { delta_px: 50.0, anchor_y: 200.0 },
        PointerInput::AutoHeight { min: 95.0, max: 105.0 },
        PointerInput::Wheel { delta_px: -50.0, anchor_y: 200.0 },
    ];
    let run = || {
        let mut state = free_state();
        let mut view = start;
        let mut sequence = Vec::new();
        for event in &log {
            view = state.step(1.0 / 60.0, &view, std::slice::from_ref(event));
            let range = view.scale().range();
            sequence.push((range.min, range.max));
        }
        sequence
    };
    assert_eq!(run(), run(), "replay is deterministic");
    let sequence = run();
    assert_eq!(
        (sequence[2].0, sequence[2].1),
        (95.0, 105.0),
        "the re-anchor lands exactly"
    );
    assert_ne!(sequence[3], sequence[2], "the trailing wheel still acts");
}
