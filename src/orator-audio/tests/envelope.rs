//! Envelope tests: duck ramps to the floor and holds; fades zero tails.

use crate::envelope::{
    apply_fade_out, db_to_linear, duck_gain_at, duck_gain_at_with, CUT_FADE_MS, DUCK_DEPTH_DB,
};

/// The configured duck matches the default duck: file defaults equal
/// the calibrated constants, so default configs voice identically.
#[test]
fn configured_duck_matches_the_default_duck() {
    let turn = orator_core::TurnConfig::default();
    for elapsed in [0, 25, 50, 99, 100, 101, 5000] {
        assert!((duck_gain_at_with(elapsed, &turn) - duck_gain_at(elapsed)).abs() < f32::EPSILON);
    }
    let shallow = orator_core::TurnConfig {
        duck_depth_db: -6.0,
        duck_ramp_ms: 50,
        ..orator_core::TurnConfig::default()
    };
    assert!((duck_gain_at_with(0, &shallow) - 1.0).abs() < f32::EPSILON);
    assert!(duck_gain_at_with(5000, &shallow) > duck_gain_at(5000));
    assert!((duck_gain_at_with(5000, &shallow) - db_to_linear(-6.0)).abs() < f32::EPSILON);
}

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

/// M3-S1 characterization: exact scalar contract of the playout
/// envelope. The shared `common_core::envelope` primitive must
/// reproduce these vectors; the driver constants must equal these
/// values (pinned separately in `orator-turn` driver tests).
#[test]
fn orator_audio_envelope_vectors() {
    // dB scale anchors.
    assert!((db_to_linear(0.0) - 1.0).abs() < f32::EPSILON);
    assert!((db_to_linear(-12.0) - 0.251_188_65).abs() < 1e-6);
    assert!((db_to_linear(6.0206) - 2.0).abs() < 1e-3);
    // The ramp is linear: the midpoint mirrors the implementation shape.
    let floor = db_to_linear(-12.0);
    assert!((duck_gain_at(50) - (1.0 + 0.5 * (floor - 1.0))).abs() < 1e-6);
    // Just past the ramp the floor holds.
    assert!((duck_gain_at(101) - floor).abs() < f32::EPSILON);
    assert!((duck_gain_at(u64::MAX) - floor).abs() < f32::EPSILON);
    // A zero fade is a no-op, even on an empty buffer.
    let mut samples = vec![0.5; 8];
    apply_fade_out(&mut samples, 0);
    assert!(samples
        .iter()
        .all(|sample| (*sample - 0.5).abs() < f32::EPSILON));
    let mut empty: Vec<f32> = Vec::new();
    apply_fade_out(&mut empty, 480);
    assert!(empty.is_empty());
    // A single-sample fade zeroes exactly the last sample.
    let mut one = vec![0.5];
    apply_fade_out(&mut one, 1);
    assert!(one[0].abs() < f32::EPSILON);
}
