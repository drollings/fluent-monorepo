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
    let scores = score_postings(vec![("a".to_string(), 1), ("a".to_string(), 1)], 60.0);
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

// Moved with `parse_rerank_output` from the router's chart selector (M7.4) —
// the parse characterization lives with the parser.

#[test]
fn parse_rerank_output_accepts_array_and_ranking_object() {
    assert_eq!(
        parse_rerank_output(r#"["draft_doc", "bug_triage"]"#).unwrap(),
        vec!["draft_doc".to_string(), "bug_triage".to_string()]
    );
    assert_eq!(
        parse_rerank_output(r#"{"ranking": ["bug_triage"]}"#).unwrap(),
        vec!["bug_triage".to_string()]
    );
    // Fences tolerated; the noise before the JSON array is dropped.
    let fenced = "Sure!\n```json\n[\"draft_doc\", \"bug_triage\"]\n```";
    assert_eq!(
        parse_rerank_output(fenced).unwrap(),
        vec!["draft_doc".to_string(), "bug_triage".to_string()]
    );
}

#[test]
fn parse_rerank_output_rejects_garbage() {
    assert!(parse_rerank_output("not json at all").is_none());
    assert!(parse_rerank_output(r#"{"chart": "bug_triage"}"#).is_none());
    assert!(
        parse_rerank_output("[]").is_none(),
        "empty ranking is unusable"
    );
}

// ── Cross-crate RRF calibration ──
//
// `rrf_fuse` composes the shared kernel (`score_postings` →
// `fluent_db::vector::rrf_merge_n` → `rrf_term`): there is no second RRF
// formula. These pin the composition so a future re-fork fails loudly.
// Documented difference (not a fork): storage recall ranks are 1-based
// (`pos + 1`, matching `fuseCandidates`), while `fluent_db::rrf_merge_n`
// treats ranks opaquely (its two-list `rrf_merge` enumerates 0-based
// in-memory positions) — absolute scores differ by that convention, so the
// cross-fn check compares ranked ORDER, and the kernel/delegation checks
// compare scores exactly.

#[test]
fn rrf_score_postings_delegates_to_shared_merge_n() {
    let postings = vec![
        ("a".to_string(), 1usize),
        ("b".to_string(), 2),
        ("a".to_string(), 1),
    ];
    let via_seam = score_postings(postings.clone(), 60.0);
    let via_shared: std::collections::HashMap<String, f64> = fluent_db::vector::rrf_merge_n(
        postings
            .into_iter()
            .map(|(id, rank)| (id, rank, ()))
            .collect(),
        60.0,
    )
    .into_iter()
    .map(|(score, id, ())| (id, score))
    .collect();
    assert_eq!(
        via_seam, via_shared,
        "score_postings must stay a delegation over rrf_merge_n"
    );
}

#[test]
fn rrf_fused_scores_are_shared_kernel_terms() {
    // One recall per id: each fused score IS one `rrf_term` — bitwise.
    let fused = rrf_fuse(
        &[vec!["a".to_string(), "b".to_string(), "c".to_string()]],
        60.0,
    );
    let by_id: std::collections::HashMap<&str, f64> =
        fused.iter().map(|h| (h.id.as_str(), h.score)).collect();
    for (id, rank) in [("a", 1usize), ("b", 2), ("c", 3)] {
        assert_eq!(
            by_id[id].to_bits(),
            fluent_db::vector::rrf_term(60.0, rank).to_bits(),
            "{id} must score exactly one shared kernel term"
        );
    }
}

#[test]
fn rrf_fuse_order_agrees_with_shared_merge_on_corpus() {
    // Same candidate sets through both rank conventions (1-based storage
    // vs 0-based in-memory): ranked order must agree on this corpus, which
    // spans single-list, overlap, k sweep, tied ranks, disjoint lists,
    // n-route, and reversed lists. No intra-list dupes (fuse dedupes per
    // list; merge_n sums — different contracts, not compared here).
    let corpus: Vec<(Vec<Vec<&str>>, f64)> = vec![
        (vec![vec!["a", "b", "c"]], 60.0),
        (vec![vec!["a", "b"], vec!["b", "c"]], 60.0),
        (vec![vec!["a", "b"], vec!["b", "c"]], 1.0),
        (vec![vec!["b"], vec!["a"]], 60.0),
        (vec![vec!["a"], vec!["b"]], 60.0),
        (vec![vec!["x"], vec!["x", "y"], vec!["x"]], 60.0),
        (vec![vec!["a", "b", "c"], vec!["c", "b", "a"]], 60.0),
        (vec![vec!["m", "n"], vec!["n", "o"], vec!["o", "m"]], 60.0),
    ];
    for (lists, k) in corpus {
        let owned: Vec<Vec<String>> = lists
            .iter()
            .map(|l| l.iter().map(std::string::ToString::to_string).collect())
            .collect();
        let fuse_order: Vec<String> = rrf_fuse(&owned, k).into_iter().map(|h| h.id).collect();
        // Shared side at its own 0-based convention; dedupe per list to
        // match fuse's contract (corpus has none, belt and braces).
        let mut postings = Vec::new();
        for list in &owned {
            let mut seen = std::collections::HashSet::new();
            for (pos, id) in list.iter().enumerate() {
                if seen.insert(id.as_str()) {
                    postings.push((id.clone(), pos, ()));
                }
            }
        }
        let merge_order: Vec<String> = fluent_db::vector::rrf_merge_n(postings, k)
            .into_iter()
            .map(|(_, id, ())| id)
            .collect();
        assert_eq!(
            fuse_order, merge_order,
            "order disagreement at k={k} on {owned:?}: investigate the rank convention before touching either side"
        );
    }
}
