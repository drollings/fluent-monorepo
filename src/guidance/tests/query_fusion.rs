use super::*;
use crate::search_types::{public_entity_id, RecallPath, SearchMatchedBy};

// Fusion unit pins: score sums, lexicographic ties, forced flags,
// evidence ordering, limit/track cut, trace shape.

fn candidate(id: &str) -> RecallCandidate {
    RecallCandidate {
        id: id.to_string(),
        entity: crate::search_types::Entity {
            id: id.to_string(),
            file_id: "file-a".to_string(),
            range: crate::search_types::FragmentSpan::File,
            content: crate::search_types::FragmentContent::Text {
                text: "x".to_string(),
            },
            metadata: None,
        },
        file: crate::search_types::FileInfo {
            id: "file-a".to_string(),
            ..Default::default()
        },
        sources: Vec::new(),
        recall: Vec::new(),
        evidence: Vec::new(),
        score: crate::search_types::RrfScore::new(0.0),
        rank: 0,
        forced: false,
    }
}

fn found(path: RecallPath, route: &str, rank: usize) -> crate::search_types::SearchRecallTrace {
    crate::search_types::SearchRecallTrace {
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
    a.recall.push(crate::search_types::SearchRecallTrace {
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
    assert_eq!(fused[0].score.value().to_bits(), 0f64.to_bits());
    assert!(fused[0].forced);
}

#[test]
fn evidence_sorts_by_rank_then_path_then_id() {
    let frag = |id: &str| crate::search_types::EntityFragment {
        id: id.to_string(),
        group: None,
        file_id: "file-a".to_string(),
        range: crate::search_types::FragmentSpan::File,
        content: crate::search_types::FragmentContent::Text {
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
        fragment: crate::search_types::EntityFragment {
            id: "entity-a".to_string(),
            group: Some("entity-a".to_string()),
            file_id: "file-a".to_string(),
            range: crate::search_types::FragmentSpan::File,
            content: crate::search_types::FragmentContent::Text {
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
                candidate.score =
                    crate::search_types::RrfScore::new(1.0 / (rank as f64 + 1.0));
                candidate.sources = vec![RecallPath::Fts, RecallPath::Vector];
                candidate
            })
            .collect();
        fuse_candidates(candidates)
            .into_iter()
            .map(|hit| (hit.id, hit.score.value().to_bits()))
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

#[test]
fn lemma_route_carries_full_recall_where_fts_finds_nothing() {
    // Inflection pin: the literal query token occurs in no indexed text,
    // so the FTS route contributes no trace while the lemma route reports
    // found at rank 1 — fused recall stays 1.0 on the lemma trace alone.
    // Exact assertions, not thresholds: any regression in lemma
    // normalization or lemma-route attachment fails this pin.
    let nlp = spacy_rs::pipeline::NlpPipeline::en_default().expect("pipeline");
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let file = crate::search_types::FileInfo {
        id: "file-a".to_string(),
        absolute_path: "/repo/src/a.ts".to_string(),
        relative_path: "src/a.ts".to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 64,
        last_modified_time: 100,
        content_hash: None,
        kind: Some(crate::search_types::FileKind::Code),
        format: "typescript".to_string(),
        index_status: None,
    };
    crate::query::ingest::ingest_text_file(
        &db,
        &file,
        "the function executes every morning\n",
        None,
        Some(&nlp),
    )
    .expect("ingest");
    let storage = crate::query::db_storage::GuidanceDbStorage::new(&db);
    let plan = crate::search_types::SearchPlan {
        routes: vec![crate::search_types::SearchPlanRoute {
            mode: crate::search_types::SearchPlanRouteMode::Fts,
            query: "functions".to_string(),
        }],
        trace: true,
        ..Default::default()
    };
    let output =
        crate::query::recall::run_recall(&plan, &storage, None, Some(&nlp)).expect("recall");
    assert_eq!(output.hits.len(), 1);
    assert_eq!(output.hits[0].entity.id, "file-a");
    let trace = output.hits[0].trace.as_ref().expect("trace");
    // FTS found nothing: it contributes no recall trace at all.
    assert!(
        trace
            .recall
            .iter()
            .all(|item| item.path != RecallPath::Fts)
    );
    // The lemma route carries the full recall at rank 1.
    let lemma = trace
        .recall
        .iter()
        .find(|item| item.path == RecallPath::Lemma)
        .expect("lemma trace");
    assert!(lemma.found);
    assert_eq!(lemma.rank, Some(1));
    assert_eq!(lemma.route_id.as_deref(), Some("lemma"));
}

#[test]
fn rank_only_rrf_boundary_two_mid_ranks_outvote_one_top_rank() {
    // M5 boundary pin (landed red, resolved by decision (a) keep-rank-only):
    // X=[Fts#1] scores 1/61 ≈ 0.0164, Y=[Lemma#5, Vector#5] scores 2/65
    // ≈ 0.0308, so Y fuses first. Proven stance: RRF sums are an
    // ordinal composite over incommensurable within-route orders, NOT
    // confidence — precise-hit-wins is not guaranteed at 3 routes, and
    // no consumer may read fused scores as magnitudes. Any future
    // scoring route (L4 especially) re-opens this decision; changing
    // this pin without the M5b blast-radius re-meter is a violation.
    let mut x = candidate("precise");
    x.recall.push(found(RecallPath::Fts, "fts", 1));
    let mut y = candidate("diffuse");
    y.recall.push(found(RecallPath::Lemma, "lemma", 5));
    y.recall.push(found(RecallPath::Vector, "vector", 5));
    let fused = fuse_candidates(vec![x, y]);
    assert_eq!(fused[0].id, "diffuse");
    assert_eq!(fused[1].id, "precise");
    let one = 1.0 / 61.0;
    let two = 2.0 / 65.0;
    assert!(
        (fused[0].score.value() - two).abs() < 1e-12,
        "{}",
        fused[0].score.value()
    );
    assert!(
        (fused[1].score.value() - one).abs() < 1e-12,
        "{}",
        fused[1].score.value()
    );
}


#[test]
fn rrf_score_orders_like_its_magnitude_and_reads_through_value() {
    use crate::search_types::RrfScore;
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
    // Transparent on the wire: byte-identical JSON to the bare f64 it
    // replaces (contract outputs never see the type).
    use crate::search_types::RrfScore;
    let score = RrfScore::new(0.03278688524590164);
    assert_eq!(
        serde_json::to_string(&score).expect("serialize"),
        "0.03278688524590164"
    );
    let back: RrfScore = serde_json::from_str("0.03278688524590164").expect("parse");
    assert_eq!(back.value().to_bits(), 0.03278688524590164f64.to_bits());
}
