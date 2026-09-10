use super::*;
use crate::query::db_storage::GuidanceDbStorage;
use crate::query::ingest::ingest_text_file;
use crate::query::recall::run_recall;
use crate::query::strategy::QueryIntent;
use crate::zg_types::FileInfo;

// Search-only slice of `cli.test.mjs:602`: an indexed workspace answers a
// symbol query with the right file. Full CLI parity arrives in P5.

fn file_info(id: &str, relative: &str) -> FileInfo {
    FileInfo {
        id: id.to_string(),
        absolute_path: format!("/repo/{relative}"),
        relative_path: relative.to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 64,
        last_modified_time: 100,
        content_hash: None,
        kind: Some(crate::zg_types::FileKind::Code),
        format: "typescript".to_string(),
        index_status: None,
    }
}

fn seed_workflow_repo(db: &search_vector::db::GuidanceDb) {
    ingest_text_file(
        db,
        &file_info("example", "src/example.ts"),
        "export const FirstWorkflowSymbol = 41;\n",
        None,
        None,
    )
    .expect("ingest example");
    ingest_text_file(
        db,
        &file_info("other", "src/other.ts"),
        "export const UnrelatedHelper = 7;\n",
        None,
        None,
    )
    .expect("ingest other");
}

#[test]
fn indexed_workspace_answers_symbol_search() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    seed_workflow_repo(&db);
    let storage = GuidanceDbStorage::new(&db);
    let engine = crate::query_engine::QueryEngine::new();
    let stages = engine
        .hybrid_explain("FirstWorkflowSymbol", &storage, None, None, 5)
        .expect("hybrid");
    assert!(!stages.is_empty());
    assert!(stages.iter().any(|stage| stage.source == "src/example.ts"));
    // Trace provenance rides on every hybrid stage.
    assert!(stages.iter().all(|stage| stage.trace.is_some()));
    // A nonsense query finds nothing.
    let result = engine.hybrid_explain("zzz-no-such-symbol", &storage, None, None, 5);
    assert!(matches!(
        result,
        Err(crate::query_engine::QueryEngineError::NoResults)
    ));
}

#[test]
fn uncitable_code_hits_never_reach_render() {
    use crate::query::hybrid::stages_from_hits;
    use crate::zg_types::{Entity, FileInfo, SearchHit, SearchMatchedBy, ZgContent, ZgRange};
    let hit = SearchHit {
        entity: Entity {
            id: "e".to_string(),
            file_id: "f".to_string(),
            range: ZgRange::File,
            content: ZgContent::Text {
                text: "x".to_string(),
            },
            metadata: Some(crate::zg_types::EntityMetadata::Code {
                symbol_type: crate::zg_types::CodeSymbolType::Function,
                symbol_name: Some("f".to_string()),
                scope: None,
                node_type: None,
                signature: Some("fn f()".to_string()),
                doc: None,
                modifiers: vec![],
            }),
        },
        file: FileInfo {
            id: "f".to_string(),
            ..Default::default()
        },
        evidence: vec![],
        rank: 1,
        score: 1.0,
        matched_by: SearchMatchedBy::Fts,
        trace: None,
    };
    // File range → no line → Code stage dropped (G-1.ground).
    assert!(stages_from_hits(std::slice::from_ref(&hit)).is_empty());
}

#[test]
fn plan_from_query_marks_identifier_intent_for_symbols() {
    let plan = plan_from_query("AlphaSymbol", QueryIntent::SingleIdentifier, 7);
    assert_eq!(plan.routes.len(), 2);
    assert_eq!(plan.prefer_symbol, Some(true));
    let plan = plan_from_query("how do things work", QueryIntent::HowTo, 7);
    assert_eq!(plan.prefer_symbol, Some(false));
}

#[test]
fn stages_carry_zvec_provenance_vocabulary() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    seed_workflow_repo(&db);
    let storage = GuidanceDbStorage::new(&db);
    let plan = plan_from_query("FirstWorkflowSymbol", QueryIntent::SingleIdentifier, 5);
    let output = run_recall(&plan, &storage, None, None).expect("recall");
    assert!(!output.hits.is_empty());
    let stages = stages_from_hits(&output.hits);
    let trace = stages[0].trace.as_ref().expect("trace");
    assert_eq!(trace.matched_by, "fts");
    assert_eq!(trace.rank, 1);
    assert!(!trace.recall.is_empty());
    assert!(output
        .timings
        .iter()
        .any(|entry| entry.name == "search_total"));
}
