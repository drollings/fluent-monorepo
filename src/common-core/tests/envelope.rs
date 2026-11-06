//! `common_core::envelope` unit tests: ramp shape, dB anchors, fade
//! edges — independent of any audio call site.

use common_core::envelope::{
    apply_fade_out, db_to_linear, linear_ramp_gain, CUT_FADE_MS, DUCK_DEPTH_DB, DUCK_RAMP_MS,
};

#[test]
fn constants_pin_the_calibrated_targets() {
    assert!((DUCK_DEPTH_DB - -12.0).abs() < f32::EPSILON);
    assert_eq!(DUCK_RAMP_MS, 100);
    assert_eq!(CUT_FADE_MS, 20);
}

#[test]
fn db_anchors_hold() {
    assert!((db_to_linear(0.0) - 1.0).abs() < f32::EPSILON);
    assert!((db_to_linear(-12.0) - 0.251_188_65).abs() < 1e-6);
    assert!((db_to_linear(6.0206) - 2.0).abs() < 1e-3);
}

#[test]
fn ramp_opens_linearly_then_holds() {
    let floor = db_to_linear(DUCK_DEPTH_DB);
    assert!((linear_ramp_gain(0, DUCK_RAMP_MS, floor) - 1.0).abs() < f32::EPSILON);
    assert!((linear_ramp_gain(50, DUCK_RAMP_MS, floor) - (1.0 + 0.5 * (floor - 1.0))).abs() < 1e-6);
    assert!((linear_ramp_gain(100, DUCK_RAMP_MS, floor) - floor).abs() < f32::EPSILON);
    assert!((linear_ramp_gain(u64::MAX, DUCK_RAMP_MS, floor) - floor).abs() < f32::EPSILON);
    // Monotonic across the ramp.
    let (a, b, c) = (
        linear_ramp_gain(25, DUCK_RAMP_MS, floor),
        linear_ramp_gain(50, DUCK_RAMP_MS, floor),
        linear_ramp_gain(75, DUCK_RAMP_MS, floor),
    );
    assert!(a > b && b > c);
}

#[test]
fn fade_edges_hold() {
    let mut full = vec![0.5; 8];
    apply_fade_out(&mut full, 0);
    assert!(full
        .iter()
        .all(|sample| (*sample - 0.5).abs() < f32::EPSILON));
    let mut empty: Vec<f32> = Vec::new();
    apply_fade_out(&mut empty, 480);
    assert!(empty.is_empty());
    let mut one = vec![0.5];
    apply_fade_out(&mut one, 1);
    assert!(one[0].abs() < f32::EPSILON);
    let mut tail = vec![1.0; 10];
    apply_fade_out(&mut tail, 100);
    assert!(tail.iter().all(|sample| *sample < 1.0));
    assert!(tail[9].abs() < f32::EPSILON);
}
