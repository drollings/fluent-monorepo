//! Contract tests for the scalar kernel (moved with the code from its
//! previous home): byte-identical behavior on aligned, degenerate, and
//! NaN inputs.
//!
//! Exact `0.0` assertions are intentional: the degenerate paths return the
//! literal `0.0` (never a rounded computation), so `float_cmp` does not
//! apply.
#![allow(clippy::float_cmp)]

use super::cosine_similarity_f32;

#[test]
fn equal_vectors_score_one() {
    assert!((cosine_similarity_f32(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    assert!((cosine_similarity_f32(&[1.0, 2.0], &[2.0, 4.0]) - 1.0).abs() < 1e-6);
}

#[test]
fn orthogonal_and_opposite() {
    assert_eq!(cosine_similarity_f32(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
    assert!((cosine_similarity_f32(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
}

#[test]
fn degenerate_inputs_yield_zero() {
    assert_eq!(cosine_similarity_f32(&[0.0, 0.0], &[0.0, 0.0]), 0.0);
    assert_eq!(cosine_similarity_f32(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    assert_eq!(cosine_similarity_f32(&[], &[]), 0.0);
    assert_eq!(cosine_similarity_f32(&[], &[1.0]), 0.0);
    assert_eq!(cosine_similarity_f32(&[1.0], &[1.0, 2.0]), 0.0);
}

#[test]
fn nan_propagates() {
    assert!(cosine_similarity_f32(&[f32::NAN], &[1.0]).is_nan());
}

#[test]
fn longer_vectors_agree_with_reference() {
    // Reference value computed independently (dot / (|a|·|b|)).
    let a = [0.5f32, -1.25, 3.0, 0.0, 2.5];
    let b = [1.0f32, 0.5, -2.0, 4.0, 1.5];
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag = a.iter().map(|x| x * x).sum::<f32>().sqrt()
        * b.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((cosine_similarity_f32(&a, &b) - dot / mag).abs() < 1e-6);
}
