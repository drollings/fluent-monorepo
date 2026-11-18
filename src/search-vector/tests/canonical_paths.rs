//! Canonical-path pins for the vector consolidation.
//!
//! Each `use` below names the single home of a shared primitive. If a
//! primitive moves to its target home, this file moves with it — `grep`
//! for the old path must come back empty outside history. Target homes:
//! scalar kernels in `vector-math`, scored top-K in `common-core::score`,
//! distance mapping, byte codecs, int8 codec, brute-force core, and RRF in
//! `search-vector`, `HnswIndex`/`AdaptiveHnsw`/`hnsw_lookup`/`make_hnsw` in
//! `search-vector`, storage taxonomy solely in `fluent_db::error::DbError`.
//! Raw `hnsw_rs` types stay in the index home; `HnswParams` and the
//! threshold/bound constants live in `search-vector::hnsw` with a
//! value-identical `common_core::constants` shim. No second threshold,
//! error, top-K, or distance mapping may appear anywhere else
//! (`bin/vector-check.sh` enforces the same).

#![allow(clippy::float_cmp)]

use crate::hnsw::{hnsw_lookup, make_hnsw, AdaptiveHnsw, HnswIndex, HnswParams};
use crate::math::{
    bytes_to_vec, distance_to_similarity, distance_to_similarity_clamped, knn_brute_force,
    knn_brute_force_q8, quantized_from_bytes, quantized_to_bytes, rrf_merge, rrf_merge_n, rrf_term,
    scored_hits, try_bytes_to_vec, vec_to_bytes, PlainBruteForceIndex, QuantizedEmbedding,
};
use common_core::score::top_k_by_score;
use fluent_db::error::DbError;
use vector_math::cosine_similarity_f32;

#[test]
fn scalar_kernel_and_topk_have_single_homes() {
    assert!((cosine_similarity_f32(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    let top = top_k_by_score(vec![1_u64, 2_u64], 1, |v| *v as f32, true);
    assert_eq!(top, vec![2_u64]);
    let _ = HnswParams::default();
}

#[test]
fn distance_mapping_has_a_single_home() {
    assert!((distance_to_similarity(0.25) - 0.75).abs() < 1e-6);
    assert_eq!(distance_to_similarity_clamped(2.0), 0.0);
    let mapped: Vec<(i64, f32)> = scored_hits(vec![(1_i64, 0.5_f32)], distance_to_similarity);
    assert!((mapped[0].1 - 0.5).abs() < 1e-6);
}

#[test]
fn codecs_and_brute_force_have_single_homes() {
    let v = vec![1.0_f32, 2.0];
    assert_eq!(bytes_to_vec(&vec_to_bytes(&v)), v);
    assert!(try_bytes_to_vec(&[0_u8; 3]).is_none());
    let q = QuantizedEmbedding::from_f32(&v);
    assert!(quantized_from_bytes(&quantized_to_bytes(&q)).is_some());
    let cands = [(7_i64, vec![1.0_f32, 0.0])];
    let got = knn_brute_force(
        &[1.0_f32, 0.0],
        cands.iter().map(|(id, e)| (*id, e.as_slice())),
        1,
    );
    assert_eq!(got.len(), 1);
    let q8 = QuantizedEmbedding::from_f32(&[1.0_f32, 0.0]);
    let got8 = knn_brute_force_q8(&q8, [(9_i64, &q8)].into_iter(), 1);
    assert_eq!(got8.len(), 1);
    let idx = PlainBruteForceIndex::new(vec![(1_i64, vec![1.0_f32], "one".to_string())]);
    let _ = idx.knn(&[1.0_f32], 1);
    let _ = rrf_term(60.0, 0);
    let _ = rrf_merge(vec![(1_i64, "a")], vec![(1_i64, "b")], 60.0);
    let _ = rrf_merge_n(vec![(1_i64, 0_usize, "a")], 60.0);
}

#[test]
fn hnsw_index_and_dispatch_have_single_homes() {
    let index = HnswIndex::new();
    assert!(!index.is_built());
    let _ = make_hnsw(&HnswParams::default(), 8);
    let policy = AdaptiveHnsw::new(1024);
    assert!(!policy.dispatch(false, 0));
    assert!(hnsw_lookup(&index, &[1.0_f32], 1).is_none());
    let err: Result<(), DbError> = Err(DbError::NotFound("pin".to_string()));
    assert!(err.is_err());
}
