use super::*;
use crate::zg_types::{public_entity_id, RecallPath, SearchMatchedBy};

// Fusion unit pins: score sums, lexicographic ties, forced flags,
// evidence ordering, limit/track cut, trace shape.

fn candidate(id: &str) -> RecallCandidate {
    RecallCandidate {
        id: id.to_string(),
        entity: crate::zg_types::Entity {
            id: id.to_string(),
            file_id: "file-a".to_string(),
            range: crate::zg_types::ZgRange::File,
            content: crate::zg_types::ZgContent::Text {
                text: "x".to_string(),
            },
            metadata: None,
        },
        file: crate::zg_types::FileInfo {
            id: "file-a".to_string(),
            ..Default::default()
        },
        sources: Vec::new(),
        recall: Vec::new(),
        evidence: Vec::new(),
        score: 0.0,
        rank: 0,
        forced: false,
    }
}

fn found(path: RecallPath, route: &str, rank: usize) -> crate::zg_types::SearchRecallTrace {
    crate::zg_types::SearchRecallTrace {
        path,
        route_id: Some(route.to_string()),
        query: Some("q".to_string()),
        found: true,
        forced: None,
        rank: Some(rank),
        score: Some(1.0),
        reason: None,
    }
}

#[test]
fn fuse_sums_found_recalls_and_assigns_ranks() {
    let mut a = candidate("a");
    a.recall.push(found(RecallPath::Fts, "fts", 1));
    a.recall.push(found(RecallPath::Vector, "vector", 2));
    let mut b = candidate("b");
    b.recall.push(found(RecallPath::Fts, "fts", 2));
    let fused = fuse_candidates(vec![a, b]);
    assert_eq!(fused[0].id, "a");
    assert_eq!(fused[0].rank, 1);
    assert_eq!(fused[1].id, "b");
    assert_eq!(fused[1].rank, 2);
    // 1/61 + 1/62 vs 1/62.
    assert!(fused[0].score > fused[1].score);
}

#[test]
fn fuse_breaks_ties_lexicographically_and_marks_forced() {
    let mut a = candidate("b");
    a.recall.push(found(RecallPath::Fts, "fts", 1));
    let mut b = candidate("a");
    let mut trace = found(RecallPath::Fts, "fts", 1);
    trace.forced = Some(true);
    b.recall.push(trace);
    let fused = fuse_candidates(vec![a, b]);
    assert_eq!(fused[0].id, "a");
    assert!(fused[0].forced);
    assert!(!fused[1].forced);
}

#[test]
fn fuse_ignores_unfound_recalls() {
    let mut a = candidate("a");
    a.recall.push(crate::zg_types::SearchRecallTrace {
        path: RecallPath::Fts,
        route_id: Some("fts".to_string()),
        query: None,
        found: false,
        forced: Some(true),
        rank: None,
        score: None,
        reason: Some("No files matched the path filters".to_string()),
    });
    let fused = fuse_candidates(vec![a]);
    assert_eq!(fused[0].score.to_bits(), 0f64.to_bits());
    assert!(fused[0].forced);
}

#[test]
fn evidence_sorts_by_rank_then_path_then_id() {
    let frag = |id: &str| crate::zg_types::EntityFragment {
        id: id.to_string(),
        group: None,
        file_id: "file-a".to_string(),
        range: crate::zg_types::ZgRange::File,
        content: crate::zg_types::ZgContent::Text {
            text: "x".to_string(),
        },
        metadata: None,
    };
    let ev = |id: &str, path: RecallPath, rank: Option<usize>| RecallEvidence {
        fragment: frag(id),
        path,
        route_id: "r".to_string(),
        query: "q".to_string(),
        rank,
        score: None,
        forced: None,
    };
    let mut evidence = vec![
        ev("z", RecallPath::Fts, None),
        ev("b", RecallPath::Vector, Some(1)),
        ev("a", RecallPath::Fts, Some(1)),
        ev("c", RecallPath::Fts, Some(2)),
    ];
    sort_evidence(&mut evidence);
    let ids: Vec<&str> = evidence.iter().map(|e| e.fragment.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "b", "c", "z"]);
}

#[test]
fn limit_cut_appends_track_and_trace_marks_cutoff() {
    let mut a = candidate("a");
    a.recall.push(found(RecallPath::Fts, "fts", 1));
    let mut b = candidate("b");
    b.recall.push(found(RecallPath::Fts, "fts", 2));
    let fused = fuse_candidates(vec![a, b]);
    let visible = apply_limit_and_track(fused, 1, Some("b"));
    assert_eq!(visible.len(), 2);
    let trace_a = candidate_trace(&visible[0], 1);
    assert!(trace_a.final_trace.returned_by_limit);
    assert_eq!(trace_a.final_trace.cutoff_rank, 1);
    let trace_b = candidate_trace(&visible[1], 1);
    assert!(!trace_b.final_trace.returned_by_limit);
    // Track already visible → no duplicate append.
    let visible = apply_limit_and_track(vec![candidate("a")], 5, Some("a"));
    assert_eq!(visible.len(), 1);
}

#[test]
fn build_hit_derives_provenance_and_entity_flag() {
    let mut major = candidate("entity-a");
    major.sources = vec![RecallPath::Fts, RecallPath::Vector];
    major.evidence.push(RecallEvidence {
        fragment: crate::zg_types::EntityFragment {
            id: "entity-a".to_string(),
            group: Some("entity-a".to_string()),
            file_id: "file-a".to_string(),
            range: crate::zg_types::ZgRange::File,
            content: crate::zg_types::ZgContent::Text {
                text: "x".to_string(),
            },
            metadata: None,
        },
        path: RecallPath::Fts,
        route_id: "fts".to_string(),
        query: "q".to_string(),
        rank: Some(1),
        score: Some(1.0),
        forced: None,
    });
    let hit = build_hit(&major, 7, true);
    assert_eq!(hit.matched_by, SearchMatchedBy::FtsAndVector);
    assert!(hit.evidence[0].is_entity);
    assert_eq!(public_entity_id(&major.evidence[0].fragment), "entity-a");
    assert!(hit.trace.is_some());
    let plain = build_hit(&major, 7, false);
    assert!(plain.trace.is_none());
}

#[test]
fn fuse_candidates_is_deterministic_across_runs() {
    // P6: RRF determinism — same input orderings fuse to bit-identical
    // rankings and scores on every run (no HashMap iteration, no time).
    fn fused_ids(order: &[&str]) -> Vec<(String, u64)> {
        let candidates: Vec<RecallCandidate> = order
            .iter()
            .enumerate()
            .map(|(rank, id)| {
                let mut candidate = candidate(id);
                candidate.rank = rank;
                candidate.score = 1.0 / (rank as f64 + 1.0);
                candidate.sources = vec![RecallPath::Fts, RecallPath::Vector];
                candidate
            })
            .collect();
        fuse_candidates(candidates)
            .into_iter()
            .map(|hit| (hit.id, hit.score.to_bits()))
            .collect()
    }
    let baseline = fused_ids(&["c", "a", "b", "d"]);
    for _ in 0..25 {
        assert_eq!(fused_ids(&["c", "a", "b", "d"]), baseline);
    }
    // Input permutation changes input ranks (hence scores) but the fuse
    // itself stays a pure function of its input: same permutation twice
    // agrees exactly.
    let permuted = fused_ids(&["d", "b", "a", "c"]);
    assert_eq!(fused_ids(&["d", "b", "a", "c"]), permuted);
}
