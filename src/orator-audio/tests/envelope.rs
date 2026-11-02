//! Envelope tests: duck ramps to the floor and holds; fades zero tails.

use crate::envelope::{apply_fade_out, db_to_linear, duck_gain_at, CUT_FADE_MS, DUCK_DEPTH_DB};

#[test]
fn duck_starts_open_and_holds_the_floor() {
    assert!((duck_gain_at(0) - 1.0).abs() < f32::EPSILON);
    let floor = db_to_linear(DUCK_DEPTH_DB);
    assert!((floor - 0.2512).abs() < 0.001);
    assert!((duck_gain_at(100) - floor).abs() < f32::EPSILON);
    assert!((duck_gain_at(5000) - floor).abs() < f32::EPSILON);
}

#[test]
fn duck_ramps_monotonically() {
    let early = duck_gain_at(25);
    let mid = duck_gain_at(50);
    let late = duck_gain_at(75);
    assert!(early > mid && mid > late);
    assert!((mid - 0.6256).abs() < 0.001);
}

#[test]
fn fade_zeroes_the_tail_and_spares_the_head() {
    let mut samples = vec![1.0; 1000];
    apply_fade_out(&mut samples, 480);
    assert!((samples[0] - 1.0).abs() < f32::EPSILON);
    assert!(samples[999].abs() < f32::EPSILON);
    assert!(samples[520] < 1.0 && samples[520] > 0.0);
}

#[test]
fn fade_longer_than_buffer_fades_all() {
    let mut samples = vec![1.0; 10];
    apply_fade_out(&mut samples, 100);
    assert!(samples.iter().all(|sample| *sample < 1.0));
    assert!(samples[9].abs() < f32::EPSILON);
}

#[test]
fn cut_fade_spans_twenty_ms_at_voice_rates() {
    assert_eq!(CUT_FADE_MS, 20);
    assert_eq!(CUT_FADE_MS as usize * 24_000 / 1000, 480);
}
