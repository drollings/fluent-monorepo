//! Clustering tests: two voices separate, one voice unites, DER gate.

use crate::cluster::{cluster, diarization_error, smooth, DiarConfig};

fn voice_a() -> Vec<f32> {
    vec![1.0, 0.1, 0.0, 0.0, 0.2, 0.0, 0.1, 0.0]
}

fn voice_b() -> Vec<f32> {
    vec![0.0, 0.1, 1.0, 0.0, 0.0, 0.2, 0.0, 0.1]
}

#[test]
fn two_voices_separate_and_one_unites() {
    let config = DiarConfig::default();
    let mixed = vec![voice_a(), voice_a(), voice_b(), voice_b(), voice_a()];
    let labels = cluster(&mixed, &config);
    assert_eq!(labels[0], labels[1]);
    assert_eq!(labels[2], labels[3]);
    assert_ne!(labels[0], labels[2]);
    let single = vec![voice_a(), voice_a(), voice_a()];
    assert_eq!(cluster(&single, &config), vec![0, 0, 0]);
    assert!(cluster(&[], &config).is_empty());
}

#[test]
fn smoothing_removes_single_segment_flicker() {
    assert_eq!(smooth(&[0, 0, 1, 0, 0], 1), vec![0, 0, 0, 0, 0]);
    assert_eq!(smooth(&[0, 1, 1, 1, 0], 1), vec![0, 1, 1, 1, 0]);
}

#[test]
fn der_scores_held_out_attribution() {
    // Control: single-speaker audio must score near zero.
    let single = vec![0, 0, 0, 0];
    assert!(diarization_error(&single, &[0, 0, 0, 0]) < 0.01);
    // Task: two-speaker alternation attributes exactly.
    let got = vec![0, 0, 1, 1, 0];
    assert!(diarization_error(&got, &[0, 0, 1, 1, 0]) < 0.01);
    // Label ids are arbitrary: swapped ids still score zero.
    assert!(diarization_error(&[1, 1, 0, 0], &[0, 0, 1, 1]) < 0.01);
    // Thresholds ride config, never hidden constants.
    assert!((DiarConfig::default().cluster_threshold - 0.35).abs() < f32::EPSILON);
}
