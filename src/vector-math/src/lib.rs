//! Scalar embedding-vector math — the canonical home of the workspace's
//! zero-dependency vector kernels.
//!
//! This crate has **no dependencies** (not even workspace-internal ones) by
//! design: it sits below every consumer (`fluent-db`, `search-vector`,
//! `fluent-onnx`, `fluent-router`, `common-core`'s deprecated shim), so all
//! of them — including weight-sensitive ones like the deterministic
//! tokenization spine — compose the same kernel without inheriting sqlite,
//! HNSW, or async-runtime weight. Keep it that way: the `leaf` test pins
//! the dependency-free manifest.
//!
//! Byte-identical contract: `0.0` on length mismatch, empty input, or zero
//! magnitude; NaN propagates per IEEE-754.

#![forbid(unsafe_code)]

/// Cosine similarity between two equal-length vectors. Returns `0.0` when the
/// vectors have mismatched lengths or are empty.
pub fn cosine_similarity_f32(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let mag = na.sqrt() * nb.sqrt();
    if mag == 0.0 {
        0.0
    } else {
        dot / mag
    }
}

#[cfg(test)]
#[path = "tests/math.rs"]
mod math_tests;

#[cfg(test)]
#[path = "tests/leaf.rs"]
mod leaf_tests;
