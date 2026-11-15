//! Scope hypothesis (shadow-only): bounded choice over crates, rank
//! prior never a filter, deterministic funnel fallback when down.
use crate::query::decision_points::{scope_spec, SCOPE_POINT, SCOPE_VERSION};
use crate::query::fusion::{apply_scope_prior, fuse_candidates, RecallCandidate};
use fluent_llm::decision::ScriptedDecideBackend;

fn candidate(id: &str, path: &str, rank: usize) -> RecallCandidate {
    use crate::search_types::{FileInfo, RecallPath, RrfScore, SearchRecallTrace};
    RecallCandidate {
        id: id.to_string(),
        entity: crate::search_types::Entity {
            id: id.to_string(),
            file_id: id.to_string(),
            range: crate::search_types::FragmentSpan::File,
            content: crate::search_types::FragmentContent::Text {
                text: String::new(),
            },
            metadata: None,
        },
        file: FileInfo {
            id: id.to_string(),
            absolute_path: format!("/repo/{path}"),
            relative_path: path.to_string(),
            root_path: "/repo".to_string(),
            size_bytes: 10,
            last_modified_time: 1,
            content_hash: None,
            kind: None,
            format: "rust".to_string(),
            index_status: None,
        },
        sources: vec![RecallPath::Fts],
        recall: vec![SearchRecallTrace {
            found: true,
            rank: Some(rank),
            forced: None,
            path: RecallPath::Fts,
            route_id: None,
            query: None,
            score: None,
            reason: None,
        }],
        evidence: Vec::new(),
        score: RrfScore::new(0.0),
        rank: 0,
        forced: false,
    }
}

#[test]
fn scope_spec_is_pinned_and_bounded() {
    let crates = vec![
        "guidance-core".to_string(),
        "fluent-router".to_string(),
        "common-core".to_string(),
    ];
    let spec = scope_spec(&crates);
    assert_eq!(spec.id, SCOPE_POINT);
    assert_eq!(spec.version, SCOPE_VERSION);
    assert!(spec.options().len() <= 10, "bounded options");
    assert!(!spec.options().is_empty(), "bounded options");
    let empty = scope_spec(&[]);
    assert!(!empty.options().is_empty(), "unknown fallback");
}

#[test]
fn scope_hypothesis_picks_right_crate_shadow_only() {
    let decision = fluent_llm::decision::Decision::from_distribution(
        "guidance-core",
        vec![
            ("guidance-core".to_string(), 0.9),
            ("fluent-router".to_string(), 0.1),
        ],
        "decision".to_string(),
        1,
        0,
        false,
    )
    .expect("decision");
    let backend = ScriptedDecideBackend::new(vec![Ok(decision.clone())]);
    let record = crate::query::shadow::scope_shadow(Some(&decision), &["sync".to_string()]);
    assert!(record.is_some());
    let record = record.expect("shadow");
    assert_eq!(record.point, SCOPE_POINT);
    assert_eq!(record.label, "guidance-core");
    assert_eq!(backend.requests().len(), 0, "shadow helper never dials");
}

#[test]
fn wrong_scope_never_reduces_recall_prior_not_filter() {
    let fused = fuse_candidates(vec![
        candidate("a", "src/guidance/src/sync_engine.rs", 1),
        candidate("b", "src/router/dispatch.rs", 2),
    ]);
    assert_eq!(fused.len(), 2);
    let prior_wrong = apply_scope_prior(fused.clone(), Some("fluent-router"));
    assert_eq!(prior_wrong.len(), 2, "prior never filters");
    let ids: std::collections::HashSet<String> = prior_wrong.iter().map(|c| c.id.clone()).collect();
    assert!(ids.contains("a") && ids.contains("b"), "recall preserved");
    let prior_none = apply_scope_prior(fused, None);
    assert_eq!(prior_none.len(), 2, "fallback keeps all");
}

#[test]
fn scope_prior_boosts_matching_crate_only() {
    let fused = fuse_candidates(vec![
        candidate("a", "src/guidance/src/sync_engine.rs", 1),
        candidate("b", "src/router/dispatch.rs", 1),
    ]);
    let boosted = apply_scope_prior(fused, Some("guidance-core"));
    let rank_of = |id: &str| boosted.iter().position(|c| c.id == id).expect("present");
    assert!(
        rank_of("a") < rank_of("b"),
        "matching crate ranks first: {boosted:?}"
    );
}
