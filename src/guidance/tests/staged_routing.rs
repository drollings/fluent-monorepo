//! Cheapest sufficient rung wins; exact verdicts short-circuit; gaps honest.

use super::should_short_circuit;
use crate::query::search_backend::{
    ConceptBackend, FilePathBackend, GeneralBackend, IdentifierBackend, KeywordBackend,
    SearchBackend,
};
use crate::query::strategy::{FsmEngine, QueryIntent};
use crate::query_engine::QueryEngine;
use crate::tests::common::make_test_doc_hello;

#[test]
fn route_maps_intent_to_cheapest_rung() {
    let fsm = FsmEngine::new();
    let single = FsmEngine::new().run("hello");
    assert_eq!(single.intent, QueryIntent::SingleIdentifier);
    assert_eq!(fsm.route(&single), "word_index");
    let file = FsmEngine::new().run("src/main.rs");
    assert_eq!(file.intent, QueryIntent::FilePath);
    assert_eq!(fsm.route(&file), "anchor_lookup");
    let concept = FsmEngine::new()
        .run("distributed caching architecture patterns for edge deployments today");
    assert_eq!(concept.intent, QueryIntent::Conceptual);
    assert_eq!(fsm.route(&concept), "vector");
}

#[test]
fn exact_verdicts_short_circuit_ambiguous_ones_do_not() {
    assert!(should_short_circuit(&FsmEngine::new().run("hello")));
    assert!(should_short_circuit(&FsmEngine::new().run("src/main.rs")));
    assert!(!should_short_circuit(&FsmEngine::new().run(
        "distributed caching architecture patterns for edge deployments today"
    )));
    assert!(!should_short_circuit(&FsmEngine::new().run("")));
}

#[test]
fn model_backed_rungs_opt_out_of_exact_short_circuits() {
    assert!(ConceptBackend.uses_model());
    assert!(!IdentifierBackend.uses_model());
    assert!(!KeywordBackend.uses_model());
    assert!(!FilePathBackend.uses_model());
    assert!(!GeneralBackend.uses_model());
}

#[test]
fn short_circuited_exact_query_still_resolves() {
    let engine = QueryEngine::new();
    let doc = make_test_doc_hello();
    let envelope = engine.answer("hello", &doc);
    assert_eq!(envelope.intent, QueryIntent::SingleIdentifier);
    assert!(envelope.resolution.contains("deterministic precheck"));
    assert!(envelope.stages.iter().any(|stage| stage
        .member_name
        .as_deref()
        .is_some_and(|name| name.contains("hello"))));
}

#[test]
fn missing_query_reports_an_honest_gap() {
    let engine = QueryEngine::new();
    let doc = make_test_doc_hello();
    let envelope = engine.answer("noSuchThingAnywhere", &doc);
    assert!(!envelope.is_answerable());
    assert!(!envelope.gaps.is_empty());
}
