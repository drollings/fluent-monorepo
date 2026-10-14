use super::*;
use protopyne_chart_model::ChartRow;
use protopyne_chart_scale::PriceScaleMode;

/// Price pane `[90, 110]` on 400 px with 8 px bar spacing.
fn pane() -> Viewport {
    Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, 8.0, 90.0, false)
        .expect("valid price pane")
}

/// Ten valid bars (`low <= open, close <= high`) around 100.
fn rows() -> Vec<ChartRow> {
    (0..10_i64)
        .map(|index| ChartRow {
            time: index,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.0,
            volume: 10,
        })
        .collect()
}

fn close_enough(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() <= 1e-3
}

#[test]
fn bar_centers_hit_their_own_bar() {
    let pane = pane();
    let rows = rows();
    for index in 0..rows.len() {
        let x = pane.index_to_x(index);
        let y = pane.price_to_y(100.0).expect("close maps");
        let hit = hit_test((x, y), &pane, &rows).expect("bar center hits");
        assert_eq!(hit.index, index, "pointer on bar {index} names bar {index}");
        assert_eq!(hit.x.to_bits(), x.to_bits(), "hit x is the bar center");
    }
}

#[test]
fn ties_break_up_deterministically() {
    let pane = pane();
    let rows = rows();
    // Midpoint between bar 3 (x = 24) and bar 4 (x = 32) is 4 px from each
    // wick — inside the epsilon — so the rounding rule decides, every time.
    let y = pane.price_to_y(100.0).expect("close maps");
    let first = hit_test((28.0, y), &pane, &rows).expect("midpoint hits");
    let second = hit_test((28.0, y), &pane, &rows).expect("midpoint hits again");
    assert_eq!(first.index, second.index, "ties decide the same way twice");
    assert_eq!(first.index, 4, "half-way slots round up to the higher bar");
}

#[test]
fn hit_round_trips_through_data_and_back_to_pixel() {
    let pane = pane();
    let rows = rows();
    let pointer = (pane.index_to_x(5), pane.price_to_y(100.5).expect("maps"));
    let hit = hit_test(pointer, &pane, &rows).expect("near-wick pointer hits");
    // Hit -> data coord -> pixel is stable: the stored pixel is exactly what
    // the viewport maps the stored data back to.
    let pixel = crosshair_pixel(&Crosshair { index: hit.index, price: hit.price }, &pane)
        .expect("hit data maps");
    assert_eq!(pixel.0.to_bits(), hit.x.to_bits(), "x round-trips bit-for-bit");
    assert_eq!(pixel.1.to_bits(), hit.y.to_bits(), "y round-trips bit-for-bit");
    assert!(close_enough(hit.x, pointer.0), "hit x stays near the pointer");
    assert!(close_enough(hit.y, pointer.1), "hit y stays near the pointer");
    // And the data leg round-trips through the crosshair reader.
    let data = crosshair_data(pixel, &pane, rows.len()).expect("pixel reads back");
    assert_eq!(data.index, hit.index, "bar index survives the round trip");
    // Price tolerance is pixel-quantization scale (f32 y ulps map back to
    // ~4e-7 price units here); the pixel leg above is the bit-exact one.
    assert!((data.price - hit.price).abs() < 1e-6, "price survives the round trip");
}

#[test]
fn far_pointers_miss_never_grab() {
    let pane = pane();
    let rows = rows();
    // Same bar column, but 180 px above the wick: the x column matches while
    // the segment distance does not — a column-only test would grab here.
    let pointer = (pane.index_to_x(5), 0.0);
    assert!(hit_test(pointer, &pane, &rows).is_none(), "far pointer misses");
    // Sideways out of reach of every wick.
    let mid_y = pane.price_to_y(100.0).expect("close maps");
    assert!(
        hit_test((500.0, mid_y), &pane, &rows).is_none(),
        "pointer past the last bar misses"
    );
    assert!(hit_test((-100.0, mid_y), &pane, &rows).is_none(), "pointer left of bar zero misses");
}

#[test]
fn epsilon_boundary_decides_hits() {
    // Calibration control for the hit radius (task value: feel). A wide
    // 20 px bar spacing keeps the candidate bar fixed while the pointer
    // moves horizontally off the wick, so the segment distance is exactly
    // the pixel offset.
    let wide = Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, 20.0, 90.0, false)
        .expect("valid wide pane");
    let rows = rows();
    let x = wide.index_to_x(5);
    let mid_y = wide.price_to_y(100.0).expect("close maps");
    let inside = hit_test((x + HIT_EPSILON_PX - 1.0, mid_y), &wide, &rows);
    assert!(inside.is_some(), "one pixel inside the radius still hits");
    assert_eq!(inside.expect("hits").index, 5, "inside hit names the wick bar");
    let boundary = hit_test((x + HIT_EPSILON_PX, mid_y), &wide, &rows);
    assert!(boundary.is_some(), "the radius is inclusive: exactly-epsilon hits");
    assert!(
        hit_test((x + HIT_EPSILON_PX + 1.0, mid_y), &wide, &rows).is_none(),
        "one pixel past the radius misses"
    );
}

#[test]
fn calibration_controls_that_must_not_hit() {
    let pane = pane();
    let rows = rows();
    // 1. Empty pane: no bar exists, so no pointer can be on a candle. The
    // crosshair readout is silent too — there is no window to index into.
    let center = (pane.index_to_x(0), pane.price_to_y(100.0).expect("close maps"));
    assert!(hit_test(center, &pane, &[]).is_none(), "empty pane never hits");
    assert!(
        crosshair_data(center, &pane, 0).is_none(),
        "empty window names no crosshair either"
    );
    // 2. Horizontal gutter between two candles: a wide spacing puts the
    // midpoint 10 px from each wick — past the radius — while the readout
    // still answers (the crosshair takes no epsilon).
    let wide = Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, 20.0, 90.0, false)
        .expect("valid wide pane");
    let mid_y = wide.price_to_y(100.0).expect("close maps");
    let gutter = (wide.index_to_x(5) + 10.0, mid_y);
    assert!(hit_test(gutter, &wide, &rows).is_none(), "gutter between wicks misses");
    assert!(
        crosshair_data(gutter, &wide, rows.len()).is_some(),
        "the readout still names the slot the hit refuses"
    );
    // 3. Twenty pixels above the highest wick: the x column matches while
    // the segment distance does not.
    let x = pane.index_to_x(5);
    let top_y = pane.price_to_y(101.0).expect("high maps");
    assert!(
        hit_test((x, top_y - 20.0), &pane, &rows).is_none(),
        "a pointer 20 px above the wick misses"
    );
    // 4. Degenerate segment: a flat bar (high == low) collapses the wick to
    // a point, so the hit reads the point distance. On the point hits;
    // 20 px above it misses.
    let flat: Vec<ChartRow> = (0..10_i64)
        .map(|index| ChartRow { time: index, open: 100.0, high: 100.0, low: 100.0, close: 100.0, volume: 10 })
        .collect();
    let dot_y = pane.price_to_y(100.0).expect("flat price maps");
    assert!(
        hit_test((x, dot_y), &pane, &flat).is_some(),
        "the degenerate point still hits on contact"
    );
    assert!(
        hit_test((x, dot_y - 20.0), &pane, &flat).is_none(),
        "the degenerate point misses 20 px away"
    );
}

#[test]
fn hit_rate_falls_monotonically_with_no_gap_wider_than_one_pixel() {
    // Sweep straight up from the wick tip: the candidate bar never changes
    // (same x slot) and the segment projection clamps to the tip, so the
    // distance is exactly the pixel offset. The hit set must be a prefix
    // `0..=k` with `k` within one pixel of the radius — one monotone step,
    // no gaps, no second chances.
    let pane = pane();
    let rows = rows();
    let x = pane.index_to_x(5);
    let top_y = pane.price_to_y(101.0).expect("high maps");
    let hits: Vec<bool> = (0..=20).map(|offset| hit_test((x, top_y - offset as f32), &pane, &rows).is_some()).collect();
    let first_miss = hits.iter().position(|hit| !hit).expect("the sweep must miss by 20 px");
    assert!(
        hits[..first_miss].iter().all(|hit| *hit),
        "every offset below the first miss hits: {hits:?}"
    );
    assert!(
        hits[first_miss..].iter().all(|hit| !hit),
        "no offset above the first miss hits again: {hits:?}"
    );
    let last_hit = first_miss.checked_sub(1).expect("offset zero always hits");
    assert!(
        (last_hit as f32 - HIT_EPSILON_PX).abs() <= 1.0,
        "the transition sits within one pixel of the radius: last hit {last_hit}, radius {HIT_EPSILON_PX}"
    );
    assert!(
        (first_miss as f32 - HIT_EPSILON_PX).abs() <= 1.0,
        "the transition sits within one pixel of the radius: first miss {first_miss}, radius {HIT_EPSILON_PX}"
    );
}

#[test]
fn degenerate_inputs_refuse() {
    let pane = pane();
    let rows = rows();
    assert!(hit_test((16.0, 200.0), &pane, &[]).is_none(), "no rows means no hit");
    // Off-pane pixels name no price, so neither layer answers.
    assert!(hit_test((16.0, 401.0), &pane, &rows).is_none(), "pixel below the pane misses");
    assert!(hit_test((16.0, -1.0), &pane, &rows).is_none(), "pixel above the pane misses");
    assert!(
        crosshair_data((16.0, f32::NAN), &pane, rows.len()).is_none(),
        "non-finite pointer names no crosshair"
    );
    assert!(
        crosshair_data((16.0, 200.0), &pane, 0).is_none(),
        "empty window names no crosshair"
    );
    // An unmappable price (non-finite) has no pixel.
    assert!(
        crosshair_pixel(&Crosshair { index: 2, price: f64::NAN }, &pane).is_none(),
        "non-finite price has no pixel"
    );
}

#[test]
fn segment_distance_known_answers() {
    // On the segment reads zero; off the end clamps to the nearer tip.
    assert_eq!(
        point_to_segment_distance(4.0, 5.0, 0.0, 0.0, 8.0, 10.0).to_bits(),
        0.0f32.to_bits(),
        "on-segment is zero"
    );
    let past_end = point_to_segment_distance(8.0, 12.0, 0.0, 0.0, 8.0, 10.0);
    assert!((past_end - 2.0).abs() < 1e-6, "past-end clamps to the tip: {past_end}");
    // Degenerate (zero-length) segment reads the point distance.
    let point = point_to_segment_distance(3.0, 4.0, 0.0, 0.0, 0.0, 0.0);
    assert!((point - 5.0).abs() < 1e-6, "degenerate segment is a point: {point}");
    // Non-finite inputs never claim a hit distance.
    assert!(
        !point_to_segment_distance(f32::NAN, 0.0, 0.0, 0.0, 1.0, 1.0).is_finite(),
        "non-finite pointer is infinite distance"
    );
}

#[test]
fn crosshair_reads_any_in_pane_pointer() {
    let pane = pane();
    let rows = rows();
    // Between bars and off the wick: no hit, but the crosshair still reads —
    // the readout layer never takes the epsilon.
    let pointer = (27.0, pane.price_to_y(103.0).expect("maps"));
    assert!(hit_test(pointer, &pane, &rows).is_none(), "between-wick gap misses");
    let data = crosshair_data(pointer, &pane, rows.len()).expect("crosshair still reads");
    assert!((data.price - 103.0).abs() < 1e-6, "crosshair price reads: {}", data.price);
    // Slot 27/8 = 3.375 rounds up to bar 3, whose slot sits at x = 24 on
    // the 8 px spacing: literals on the documented rounding and spacing,
    // never a second mapping call.
    assert_eq!(data.index, 3, "the between-bar pointer still names a bar");
    let pixel = crosshair_pixel(&data, &pane).expect("crosshair maps back");
    assert_eq!(pixel.0.to_bits(), 24.0f32.to_bits(), "crosshair x snaps to the bar");
    assert!(close_enough(pixel.1, pointer.1), "crosshair y keeps the pointer");
}
