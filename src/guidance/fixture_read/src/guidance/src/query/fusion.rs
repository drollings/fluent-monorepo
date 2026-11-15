//! Fusion: RRF over ranked routes (k=60).

/// Reciprocal-rank fusion rank: ordinal composite, k=60.
pub fn rrf(rank: usize) -> f64 {
    1.0 / (60.0 + rank as f64)
}
