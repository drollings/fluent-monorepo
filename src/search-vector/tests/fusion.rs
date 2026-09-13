//! M6.2 unit pins for `search_vector::fusion`: RRF goldens (K=60),
//! tie→id order, empty inputs, per-list dedupe, boost additivity, and the
//! `RrfScore` ordinal contract. Pure — no guidance types.

use super::*;

#[test]
fn rrf_empty_inputs_yield_empty() {
    assert!(rrf_fuse(&[], 60.0).is_empty());
    assert!(rrf_fuse(&[Vec::new()], 60.0).is_empty());
    assert!(rrf_fuse(&[Vec::new(), Vec::new()], 60.0).is_empty());
}

#[test]
fn rrf_sums_across_lists_and_orders_by_score() {
    // 1-based storage ranks: a: 1/61 · b: 1/62+1/61 · c: 1/62.
    let fused = rrf_fuse(
        &[
            vec!["a".to_string(), "b".to_string()],
            vec!["b".to_string(), "c".to_string()],
        ],
        60.0,
    );
    let ids: Vec<&str> = fused.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, vec!["b", "a", "c"]);
    assert_eq!(fused[0].rank, 1);
    assert_eq!(fused[1].rank, 2);
    assert_eq!(fused[2].rank, 3);
    assert!((fused[0].score - (1.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-12);
}

#[test]
fn rrf_breaks_score_ties_by_lexicographic_id() {
    let fused = rrf_fuse(&[vec!["b".to_string()], vec!["a".to_string()]], 60.0);
    let ids: Vec<&str> = fused.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "b"]);
    assert_eq!(fused[0].score.to_bits(), fused[1].score.to_bits());
}

#[test]
fn rrf_generalizes_to_n_routes() {
    let fused = rrf_fuse(
        &[
            vec!["x".to_string()],
            vec!["x".to_string(), "y".to_string()],
            vec!["x".to_string()],
        ],
        60.0,
    );
    assert_eq!(fused[0].id, "x");
    assert!((fused[0].score - 3.0 / 61.0).abs() < 1e-12);
    assert_eq!(fused[1].id, "y");
}

#[test]
fn rrf_dedupes_within_a_list() {
    let fused = rrf_fuse(&[vec!["a".to_string(), "a".to_string()]], 60.0);
    assert_eq!(fused.len(), 1);
    assert_eq!(fused[0].id, "a");
    assert!((fused[0].score - 1.0 / 61.0).abs() < 1e-12);
}

#[test]
fn score_postings_sums_duplicates_per_id() {
    let scores = score_postings(
        vec![("a".to_string(), 1), ("a".to_string(), 1)],
        60.0,
    );
    assert!((scores["a"] - 2.0 / 61.0).abs() < 1e-12);
}

#[test]
fn score_postings_empty_is_empty() {
    assert!(score_postings(Vec::new(), 60.0).is_empty());
}

#[test]
fn apply_boosts_stacks_additively() {
    let base = 1.0 / 61.0;
    assert_eq!(
        apply_boosts(base, [SAME_FILE_BOOST]).to_bits(),
        (base + 0.2).to_bits()
    );
    assert_eq!(
        apply_boosts(base, [SAME_FILE_BOOST, CALL_BOOST, SCOPE_BOOST]).to_bits(),
        (base + 0.2 + 0.5 + 0.1).to_bits()
    );
    // No boosts is the identity; empty and populated agree on order-independence.
    assert_eq!(apply_boosts(base, []).to_bits(), base.to_bits());
    assert_eq!(
        apply_boosts(base, [CALL_BOOST, SAME_FILE_BOOST]).to_bits(),
        apply_boosts(base, [SAME_FILE_BOOST, CALL_BOOST]).to_bits()
    );
}

#[test]
fn boost_constants_pinned() {
    assert_eq!(CALL_BOOST.to_bits(), 0.5f64.to_bits());
    assert_eq!(DEPENDENT_BOOST.to_bits(), 0.3f64.to_bits());
    assert_eq!(SAME_FILE_BOOST.to_bits(), 0.2f64.to_bits());
    assert_eq!(SCOPE_BOOST.to_bits(), 0.1f64.to_bits());
    assert_eq!(ROLE_EPSILON.to_bits(), 1e-4f64.to_bits());
}

#[test]
fn rrf_score_orders_like_magnitude_and_reads_through_value() {
    use std::cmp::Ordering;
    let low = RrfScore::new(1.0 / 61.0);
    let high = RrfScore::new(2.0 / 65.0);
    assert_eq!(low.partial_cmp(&high), Some(Ordering::Less));
    assert_eq!(high.partial_cmp(&low), Some(Ordering::Greater));
    assert_eq!(low.partial_cmp(&low), Some(Ordering::Equal));
    assert!((high.value() - 2.0 / 65.0).abs() < f64::EPSILON);
}

#[test]
fn rrf_score_serializes_as_a_bare_number() {
    let score = RrfScore::new(0.03278688524590164);
    assert_eq!(
        serde_json::to_string(&score).expect("serialize"),
        "0.03278688524590164"
    );
    let back: RrfScore = serde_json::from_str("0.03278688524590164").expect("parse");
    assert_eq!(back.value().to_bits(), 0.03278688524590164f64.to_bits());
}
