#![allow(clippy::float_cmp)]

use crate::math::{
    bytes_to_vec, distance_to_similarity, distance_to_similarity_clamped, knn_brute_force,
    quantized_from_bytes, quantized_to_bytes, rrf_merge, rrf_merge_n, scored_hits,
    try_bytes_to_vec, vec_to_bytes, QuantizedEmbedding,
};

#[test]
fn distance_mapping_pins_identity() {
    assert_eq!(distance_to_similarity(0.0), 1.0);
    assert_eq!(distance_to_similarity(0.25), 0.75);
    assert_eq!(distance_to_similarity_clamped(1.5), 0.0);
    let resolved = vec![(7_i64, 0.2_f32), (3_i64, 0.9_f32)];
    let mapped = scored_hits(resolved.clone(), distance_to_similarity);
    assert_eq!(mapped.len(), 2);
    assert_eq!(mapped[0].0, 7_i64);
    assert!((mapped[0].1 - 0.8_f32).abs() < 1e-6);
    assert!((mapped[1].1 - 0.1_f32).abs() < 1e-6);
}

#[test]
fn byte_codec_pins_round_trip() {
    let v = vec![1.0_f32, -2.5, 0.0];
    assert_eq!(bytes_to_vec(&vec_to_bytes(&v)), v);
    assert!(try_bytes_to_vec(&[1, 2, 3]).is_none());
    assert_eq!(try_bytes_to_vec(&vec_to_bytes(&v)), Some(v));
    let q = QuantizedEmbedding::from_f32(&[1.0, -1.0, 0.5]);
    let enc = quantized_to_bytes(&q);
    let dec = quantized_from_bytes(&enc).expect("decode");
    assert_eq!(dec.dimensions, q.dimensions);
    assert!(quantized_from_bytes(&enc[..3]).is_none());
}

#[test]
fn brute_force_pins_skip_and_k() {
    let query = vec![1.0_f32, 0.0];
    let cands = [
        (1_i64, vec![1.0_f32, 0.0]),
        (2_i64, vec![0.0_f32, 1.0]),
        (3_i64, vec![1.0_f32]),
    ];
    let got = knn_brute_force(&query, cands.iter().map(|(id, e)| (*id, e.as_slice())), 10);
    assert_eq!(got.len(), 2, "dimension mismatch is skipped");
    assert_eq!(got[0].0, 1);
    assert!(knn_brute_force(&query, cands.iter().map(|(id, e)| (*id, e.as_slice())), 0).is_empty());
    assert!(knn_brute_force(&[], cands.iter().map(|(id, e)| (*id, e.as_slice())), 2).is_empty());
}

#[test]
fn rrf_pins_first_wins_and_tie_break() {
    let kw = vec![(1_i64, "k1"), (2_i64, "k2")];
    let vs = vec![(2_i64, "v2"), (1_i64, "v1")];
    let fused = rrf_merge(kw, vs, 60.0);
    assert_eq!(fused.len(), 2);
    assert_eq!(fused[0].1, "k1", "first-seen item wins on id collision");
    let postings = vec![(2_i64, 0_usize, "b"), (1_i64, 0_usize, "a")];
    let merged = rrf_merge_n(postings, 60.0);
    assert_eq!(merged[0].1, 1, "ties break by ascending id");
}
