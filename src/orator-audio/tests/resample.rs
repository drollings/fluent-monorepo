//! Resample tests: exact ratios, length math, streaming chunks.

use crate::resample::{resample_linear, RubatoResampler};

#[test]
fn identical_rates_pass_through() {
    let input = vec![0.5; 100];
    assert_eq!(resample_linear(&input, 16_000, 16_000), input);
}

#[test]
fn downsample_halves_length() {
    let input: Vec<f32> = (0..100).map(|index| index as f32).collect();
    let output = resample_linear(&input, 48_000, 16_000);
    assert_eq!(output.len(), 33);
    assert!((output[0] - 0.0).abs() < f32::EPSILON);
}

#[test]
fn upsample_interpolates_linearly() {
    let output = resample_linear(&[0.0, 10.0], 1, 4);
    assert_eq!(output.len(), 8);
    assert!((output[0]).abs() < f32::EPSILON);
    assert!((output[2] - 5.0).abs() < 1e-5);
    assert!((output[7] - 10.0).abs() < 1e-5);
}

#[test]
fn empty_input_yields_empty_output() {
    assert!(resample_linear(&[], 48_000, 16_000).is_empty());
}

#[test]
fn rubato_chunk_round_trip_keeps_length_class() {
    let mut resampler = RubatoResampler::new(48_000, 16_000, 1024).expect("setup");
    let block = vec![0.25; 1024];
    let output = resampler.process(&block).expect("process");
    assert!(!output.is_empty());
    assert!(output.len() < 1024);
}
