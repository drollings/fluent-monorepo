//! Envelope calibration: held-out queries, homonym traps, no-caller
//! traps; zero wrongly-confident no-caller answers; citation floors.

use super::{citation_precision, no_caller_trap_corpus, trap_control_fires, trap_gate_passes};
use crate::graph_index::{GraphIndex, GraphQuerySignals};
use crate::query::envelope::{AnswerEnvelope, ConfidenceAxis};
use crate::query::fusion::RecallCandidate;
use crate::query::strategy::FsmEngine;
use crate::query_engine::QueryEngine;
use crate::search_types::{Entity, EntityMetadata, FileInfo, FragmentContent, FragmentSpan};
use crate::tests::common::make_test_doc_hello;

fn candidate(id: &str, file: &str, score: f64) -> RecallCandidate {
    RecallCandidate {
        id: id.to_string(),
        entity: Entity {
            id: id.to_string(),
            file_id: file.to_string(),
            range: FragmentSpan::File,
            content: FragmentContent::Text { text: format!("fn {id}()") },
            metadata: None,
        },
        file: FileInfo {
            id: file.to_string(),
            absolute_path: format!("/repo/{file}"),
            relative_path: file.to_string(),
            root_path: "/repo".to_string(),
            ..FileInfo::default()
        },
        sources: Vec::new(),
        recall: Vec::new(),
        evidence: Vec::new(),
        score: search_vector::fusion::RrfScore::new(score),
        rank: 0,
        forced: false,
    }
}

#[test]
fn zero_wrongly_confident_no_caller_answers() {
    let engine = QueryEngine::new();
    let doc = make_test_doc_hello();
    for query in ["zzzAbsentSymbol", "qqqNoSuchFn", "xyz_missing_thing"] {
        let envelope = engine.answer(query, &doc);
        assert!(!envelope.is_answerable(), "{query}");
        assert_eq!(envelope.confidence.axis, ConfidenceAxis::ProducerOrderOnly, "{query}");
        assert!(envelope.citations.is_empty(), "{query}");
    }
}

#[test]
fn homonym_trap_stays_ambiguous_never_unique() {
    let class = FsmEngine::new().run("render");
    let stages = vec![
        crate::query::synthesize::Stage {
            kind: fluent_types::StageKind::Code,
            content: "fn render()".to_string(),
            source: "src/a.rs".to_string(),
            line: Some(3),
            end_line: None,
            member_name: Some("render".to_string()),
            member_type: None,
            trace: None,
        },
        crate::query::synthesize::Stage {
            kind: fluent_types::StageKind::Code,
            content: "fn render()".to_string(),
            source: "src/b.rs".to_string(),
            line: Some(9),
            end_line: None,
            member_name: Some("render".to_string()),
            member_type: None,
            trace: None,
        },
    ];
    let envelope = AnswerEnvelope::from_stages(&class, "word_index", stages);
    assert!(envelope.gaps.iter().any(|gap| gap.contains("ambiguous: 2 candidates")));
    assert!(!envelope.resolution.contains("unique"));
}

#[test]
fn citation_validity_floor_is_exact() {
    let engine = QueryEngine::new();
    let doc = make_test_doc_hello();
    let envelope = engine.answer("hello", &doc);
    assert!(envelope.is_answerable());
    let total = envelope.citations.len();
    assert!(total > 0);
    assert!((citation_precision(total, total) - 1.0).abs() < 1e-9);
    assert!((citation_precision(0, 3) - 0.0).abs() < 1e-9);
    assert!((citation_precision(0, 0) - 1.0).abs() < 1e-9);
}

#[test]
fn trap_controls_block_promotion_at_operating_threshold() {
    assert_eq!(trap_control_fires(0.9), 2);
    assert!(!trap_gate_passes(0.9));
    assert!(!no_caller_trap_corpus().is_empty());
}

#[test]
fn rerank_skip_discipline_preserves_order_without_graph_touch() {
    let graph = GraphIndex::build(&[], &["/repo".to_string()]).expect("empty graph");
    let mut candidates = vec![candidate("b", "b.rs", 0.5), candidate("a", "a.rs", 0.9)];
    let before: Vec<String> = candidates.iter().map(|candidate| candidate.id.clone()).collect();
    let signals = GraphQuerySignals { predicate: Vec::new(), roles: Vec::new() };
    graph.rerank(&mut candidates, &signals);
    let after: Vec<String> = candidates.iter().map(|candidate| candidate.id.clone()).collect();
    assert_eq!(before, after);
    let _ = EntityMetadata::Code {
        symbol_type: crate::search_types::CodeSymbolType::Function,
        symbol_name: None,
        scope: None,
        node_type: None,
        signature: None,
        doc: None,
        modifiers: Vec::new(),
    };
}
