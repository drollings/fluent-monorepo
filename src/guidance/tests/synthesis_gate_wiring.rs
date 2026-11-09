use super::{QueryEngine, QueryEngineError};
use crate::grounding::can_synthesize;
use crate::tests::common::make_test_doc;

// The grounded-or-silent gate at the synthesis entry points: names that
// resolve to no current member (unknown symbols, empty mirrors, stale
// index names) take the pinned gap — never prose without excerpts.
// Grounded dispatch renders byte-identically (pinned golden below).

#[test]
fn unknown_symbol_dispatch_reports_gap() {
    let engine = QueryEngine::new();
    let result = engine.explain("zzzzNotHere", &make_test_doc());
    assert!(matches!(result, Err(QueryEngineError::NoResults)));
}

#[test]
fn empty_mirror_dispatch_reports_gap() {
    let engine = QueryEngine::new();
    let empty = fluent_types::GuidanceDoc::default();
    let result = engine.explain("hello", &empty);
    assert!(matches!(result, Err(QueryEngineError::NoResults)));
}

#[test]
fn stale_names_with_file_comment_report_gap_not_prose() {
    // Fragment names that resolve to no current member, on a doc that
    // carries a file comment: synthesis from no excerpts is blocked.
    let engine = QueryEngine::new();
    let db = search_vector::GuidanceDb::open_in_memory().expect("db");
    db.insert_node(
        "stale_name",
        "src/stale.rs",
        Some("fn stale_name()"),
        None,
        "stale",
        "rust",
        None,
    )
    .expect("insert");
    let result = engine.vector_explain("stale_name", &[0.1, 0.2, 0.3], &db, &make_test_doc(), 5);
    assert!(
        matches!(result, Err(QueryEngineError::NoResults)),
        "ungrounded synthesis must take the gap path, got {result:?}"
    );
}

#[test]
fn grounded_dispatch_output_matches_pre_change_golden() {
    // Captured before the gate landed; the gate must not move it.
    let engine = QueryEngine::new();
    let stages = engine
        .explain("helloWorld", &make_test_doc())
        .expect("grounded");
    assert!(can_synthesize(&stages));
    let json = serde_json::to_value(&stages).expect("serializes");
    let golden: serde_json::Value =
        serde_json::from_str(GOLDEN_HELLO_WORLD).expect("golden parses");
    assert_eq!(json, golden);
}

const GOLDEN_HELLO_WORLD: &str = r#"[{"content":"Test module for query engine.","kind":"prose","line":null,"source":"src/test.zig"},{"content":"fn helloWorld() void","kind":"code","line":1,"source":"src/test.zig"},{"content":"Prints hello world.","kind":"prose","line":1,"source":"src/test.zig"}]"#;
