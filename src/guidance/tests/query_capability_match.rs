//! Route-level tests for the capability matching core
//! (`query::capability_match`): SVO extraction runs on the funneled
//! vocabulary, unknown-concept queries match as raw text would, and
//! overlap ranks predicate hits above role coverage. Included via
//! `#[path]` forwarder, per repo convention. Hermetic: rule
//! pipeline, hand-built aliases, no model, no endpoint.

use super::*;
use crate::query::funnel::funnel_terms;

fn alias_table() -> search_vector::aliases::SemanticAliases {
    search_vector::aliases::SemanticAliases::from_json(r#"{"components": ["modules", "parts"]}"#)
        .expect("alias fixture parses")
}

fn rule_pipeline() -> spacy_rs::pipeline::NlpPipeline {
    spacy_rs::pipeline::NlpPipeline::en_default().expect("rule pipeline builds")
}

fn signal_lemmas(signals: &crate::graph_index::GraphQuerySignals) -> Vec<String> {
    signals
        .predicate
        .iter()
        .chain(signals.roles.iter())
        .cloned()
        .collect()
}

#[test]
fn signals_run_on_funneled_vocabulary_not_raw_text() {
    // Scaffolding ("What"/"are") never reaches extraction: the
    // signals equal a manual pipeline run over the expanded terms,
    // and differ from a run over the raw query.
    let nlp = rule_pipeline();
    let funneled = funnel_terms("What are the major components", &alias_table(), None);
    assert_eq!(
        funneled.expanded,
        vec![
            "major".to_string(),
            "components".to_string(),
            "modules".to_string(),
            "parts".to_string()
        ]
    );
    let signals = funneled_query_signals(&funneled, Some(&nlp));
    let manual = crate::graph_index::signals_from_routing(&spacy_rs::extract_routing_signals(
        &nlp.process_sync(&funneled.expanded.join(" "), None)
            .expect("manual run"),
    ));
    assert_eq!(signals, manual);
    let raw = crate::graph_index::signals_from_routing(&spacy_rs::extract_routing_signals(
        &nlp.process_sync("What are the major components", None)
            .expect("raw run"),
    ));
    assert_ne!(signals, raw);
    for lemma in signal_lemmas(&signals) {
        assert!(
            !crate::query::funnel::SCAFFOLDING_STOPWORDS.contains(&lemma.as_str()),
            "scaffolding leaked into signals: {lemma:?}"
        );
    }
}

#[test]
fn unknown_concept_query_matches_as_raw_text_would() {
    // No alias, no concept resolution: the funnel is the identity,
    // so funneled signals equal raw-text signals exactly.
    let nlp = rule_pipeline();
    let funneled = funnel_terms("sprocket wobble", &alias_table(), None);
    assert_eq!(
        funneled.expanded,
        vec!["sprocket".to_string(), "wobble".to_string()]
    );
    let signals = funneled_query_signals(&funneled, Some(&nlp));
    let raw = crate::graph_index::signals_from_routing(&spacy_rs::extract_routing_signals(
        &nlp.process_sync("sprocket wobble", None).expect("raw run"),
    ));
    assert_eq!(signals, raw);
}

#[test]
fn missing_nlp_yields_empty_signals() {
    // No pipeline: fail open to empty (the route falls back to
    // text matching), never to invented lemmas.
    let funneled = funnel_terms("major components", &alias_table(), None);
    assert_eq!(
        funneled_query_signals(&funneled, None),
        crate::graph_index::GraphQuerySignals::default()
    );
}

#[test]
fn empty_funnel_yields_empty_signals() {
    // Empty (or all-scaffolding) query: no pipeline call, no signals.
    let nlp = rule_pipeline();
    let funneled = funnel_terms("what are the", &alias_table(), None);
    assert!(funneled.is_empty());
    assert_eq!(
        funneled_query_signals(&funneled, Some(&nlp)),
        crate::graph_index::GraphQuerySignals::default()
    );
}

#[test]
fn overlap_counts_predicate_and_role_hits() {
    let query = crate::graph_index::GraphQuerySignals {
        predicate: vec!["index".to_string()],
        roles: vec!["fragment".to_string(), "lemma".to_string()],
    };
    let hit = capability_overlap(&query, &["index".to_string()], &["fragment".to_string()]);
    assert_eq!(
        hit,
        CapabilityOverlap {
            predicate: 1,
            roles: 1
        }
    );
    let miss = capability_overlap(&query, &["summarize".to_string()], &["comment".to_string()]);
    assert!(miss.is_empty());
}

#[test]
fn rank_prefers_svo_overlap_then_fts_then_id() {
    use super::{rank_briefs, BriefCandidate};
    let signals = crate::graph_index::GraphQuerySignals {
        predicate: vec!["index".to_string()],
        roles: vec!["fragment".to_string()],
    };
    let candidate = |id: &str, predicate: &str, roles: &[&str], fts: Option<f64>| BriefCandidate {
        id: id.to_string(),
        purpose: format!("Purpose {id}."),
        predicate: predicate.to_string(),
        roles: roles.iter().map(ToString::to_string).collect(),
        crate_name: "guidance-core".to_string(),
        domain: "guidance".to_string(),
        source_path: "doc/a.md".to_string(),
        anchor: id.to_string(),
        fts_score: fts,
    };
    let ranked = rank_briefs(
        &signals,
        &[
            candidate("b-role", "", &["fragment"], Some(5.0)),
            candidate("c-both", "index", &["fragment"], None),
            candidate("a-head", "index", &[], Some(1.0)),
        ],
    );
    assert_eq!(ranked.len(), 3);
    assert_eq!(ranked[0].id, "c-both");
    assert_eq!(ranked[0].overlap.rank_key(), (1, 1));
    assert_eq!(ranked[0].rank, 1);
    assert_eq!(ranked[1].id, "a-head");
    assert_eq!(ranked[1].overlap.rank_key(), (1, 0));
    assert_eq!(ranked[2].id, "b-role");
    assert_eq!(ranked[2].overlap.rank_key(), (0, 1));
    assert!((ranked[2].fts_score - 5.0).abs() < f64::EPSILON);
}

#[test]
fn rank_caps_at_top_briefs_with_id_tiebreak() {
    use super::{rank_briefs, BriefCandidate, CAPABILITY_TOP_BRIEFS};
    let signals = crate::graph_index::GraphQuerySignals::default();
    let candidate = |id: &str| BriefCandidate {
        id: id.to_string(),
        purpose: format!("Purpose {id}."),
        predicate: String::new(),
        roles: Vec::new(),
        crate_name: "guidance-core".to_string(),
        domain: "guidance".to_string(),
        source_path: "doc/a.md".to_string(),
        anchor: id.to_string(),
        fts_score: Some(1.0),
    };
    let ranked = rank_briefs(
        &signals,
        &[
            candidate("d"),
            candidate("c"),
            candidate("b"),
            candidate("a"),
        ],
    );
    assert_eq!(ranked.len(), CAPABILITY_TOP_BRIEFS);
    assert_eq!(ranked[0].id, "a");
    assert_eq!(ranked[1].id, "b");
    assert_eq!(ranked[2].id, "c");
    assert_eq!(ranked[2].rank, 3);
}

#[test]
fn rank_empty_predicate_never_head_matches() {
    use super::{rank_briefs, BriefCandidate};
    let signals = crate::graph_index::GraphQuerySignals {
        predicate: vec!["index".to_string()],
        roles: vec![],
    };
    let ranked = rank_briefs(
        &signals,
        &[BriefCandidate {
            id: "a".to_string(),
            purpose: "Purpose a.".to_string(),
            predicate: String::new(),
            roles: Vec::new(),
            crate_name: "guidance-core".to_string(),
            domain: "guidance".to_string(),
            source_path: "doc/a.md".to_string(),
            anchor: "a".to_string(),
            fts_score: None,
        }],
    );
    assert_eq!(ranked.len(), 1);
    assert!(ranked[0].overlap.is_empty());
    assert!(ranked[0].fts_score.abs() < f64::EPSILON);
    assert_eq!(ranked[0].rank, 1);
}
#[test]
fn overlap_rank_prefers_predicate_over_role_coverage() {
    // Deterministic rank key: a predicate hit outranks any number
    // of role-only hits, so head-to-head SVO agreement sorts first.
    let head = CapabilityOverlap {
        predicate: 1,
        roles: 0,
    };
    let coverage = CapabilityOverlap {
        predicate: 0,
        roles: 9,
    };
    assert!(head > coverage);
    assert_eq!(head.rank_key(), (1, 0));
}

#[test]
fn judge_labels_do_not_reorder_or_filter_before_calibration() {
    // M7.7/M10 contract: the native judge returns labels only. Rank
    // order comes from SVO+FTS alone; nothing is dropped and nothing
    // moves, however the labels read — promotion waits on the
    // calibrated capability point.
    use super::{rank_briefs, BriefCandidate};
    let signals = crate::graph_index::GraphQuerySignals {
        predicate: vec!["route".to_string()],
        roles: vec!["request".to_string()],
    };
    let candidate = |id: &str, predicate: &str, fts: Option<f64>| BriefCandidate {
        id: id.to_string(),
        purpose: format!("Purpose {id}."),
        predicate: predicate.to_string(),
        roles: vec!["request".to_string()],
        crate_name: "c".to_string(),
        domain: "d".to_string(),
        source_path: "doc/a.md".to_string(),
        anchor: id.to_string(),
        fts_score: fts,
    };
    let ranked = rank_briefs(
        &signals,
        &[
            candidate("b", "", Some(1.0)),
            candidate("a", "route", Some(0.5)),
        ],
    );
    // SVO head match outranks the FTS leader regardless of any label.
    assert_eq!(
        ranked
            .iter()
            .map(|brief| brief.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    let labels = ["strong", "considered"];
    for label in labels {
        let _ = label;
        let reranked = rank_briefs(
            &signals,
            &[
                candidate("b", "", Some(1.0)),
                candidate("a", "route", Some(0.5)),
            ],
        );
        assert_eq!(
            reranked
                .iter()
                .map(|brief| brief.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"],
            "label {label} must not reorder"
        );
    }
    // No filtering: every candidate ranks, even with empty overlap.
    let signals = crate::graph_index::GraphQuerySignals::default();
    let ranked = rank_briefs(
        &signals,
        &[candidate("a", "", None), candidate("b", "", None)],
    );
    assert_eq!(ranked.len(), 2);
}

/// M2.1 characterization: NaN FTS falls into the `Equal` arm, so id
/// order decides; empty input ranks empty (C7).
#[test]
fn rank_nan_fts_falls_back_to_id_order() {
    use super::{rank_briefs, BriefCandidate};
    let signals = crate::graph_index::GraphQuerySignals::default();
    let candidate = |id: &str, fts: Option<f64>| BriefCandidate {
        id: id.to_string(),
        purpose: format!("Purpose {id}."),
        predicate: String::new(),
        roles: Vec::new(),
        crate_name: "guidance-core".to_string(),
        domain: "guidance".to_string(),
        source_path: "doc/a.md".to_string(),
        anchor: id.to_string(),
        fts_score: fts,
    };
    // NaN poisons its FTS comparisons into the `Equal` arm, so id
    // order decides among NaNs — and against the scored candidate
    // too, since every NaN-vs-score comparison is also `Equal`.
    let ranked = rank_briefs(
        &signals,
        &[
            candidate("b", Some(f64::NAN)),
            candidate("a", Some(f64::NAN)),
            candidate("c", Some(1.0)),
        ],
    );
    assert_eq!(
        ranked.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
    assert!(rank_briefs(&signals, &[]).is_empty());
}
