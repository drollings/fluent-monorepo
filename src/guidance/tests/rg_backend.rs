//! Ripgrep arm pins: exact intents normalize through the same finding
//! shape and enter the shadow judge; broad language never scans.
use super::{ExplainContext, ExplainRequest, ReadEngine};
use crate::query::strategy::QueryIntent;

#[test]
fn exact_intent_rg_findings_normalize_and_judge() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), "fn exact_needle_fn() {}\n").expect("write");
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let engine = ReadEngine::new();
    let plan = engine.plan(&super::ReadRequest {
        query: "exact_needle_fn".to_string(),
        budget_override: None,
    });
    assert_eq!(plan.intent, QueryIntent::SingleIdentifier);
    assert!(plan.ripgrep);
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "exact_needle_fn".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
        },
        &ctx,
        None,
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.contains("exact_needle_fn"), "{rendered}");
}

#[test]
fn broad_query_does_not_scan() {
    let engine = ReadEngine::new();
    let plan = engine.plan(&super::ReadRequest {
        query: "how does sync decide what to reprocess".to_string(),
        budget_override: None,
    });
    assert!(!plan.ripgrep);
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), "fn broad_needle_fn() {}\n").expect("write");
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "how does sync decide what to reprocess".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
        },
        &ctx,
        None,
    );
    assert!(!crate::read::render::render_answer(&answer).contains("broad_needle_fn"));
}
