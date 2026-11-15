//! Code plane: FTS + vector + lemma recall within code only, no
//! global symbol preference, rg normalization, budgets. Included via
//! `#[path]` forwarders from `read/planes`, per repo convention.
//! Hermetic: hand-built rows in an in-memory index, no endpoint.

use super::code::{code_plan, rg_hits_to_findings, search_hits_to_findings};
use crate::query::rg_backend::RgHit;
use crate::search_types::SearchPlanRouteMode;

fn seed_db() -> search_vector::db::GuidanceDb {
    use search_vector::db::{FileRecord, FragmentRecord};
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let file = |id: &str, rel: &str| FileRecord {
        id: id.to_string(),
        absolute_path: format!("/repo/{rel}"),
        relative_path: rel.to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 100,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    };
    let frag = |id: &str, file_id: &str, symbol: &str, text: &str| FragmentRecord {
        id: id.to_string(),
        group: None,
        file_id: file_id.to_string(),
        range_json: "{\"kind\":\"item\"}".to_string(),
        content_kind: "text".to_string(),
        content_text: Some(text.to_string()),
        cjk_text: String::new(),
        symbol_type: Some("function".to_string()),
        symbol_name: Some(symbol.to_string()),
        scope: None,
        signature: Some(format!("fn {symbol}()")),
        doc: None,
        modifiers: String::new(),
        heading: None,
        heading_level: None,
        embedding: None,
    };
    db.upsert_fragments(
        &file("file-a", "src/router/dispatch.rs"),
        &[frag(
            "a-fn",
            "file-a",
            "dispatch_routes",
            "pub fn dispatch_routes() {}",
        )],
        &[],
    )
    .expect("seed a");
    db
}

#[test]
fn code_plan_combines_routes_without_symbol_routes() {
    use crate::query::strategy::QueryIntent;
    let plan = code_plan("how does the router rank code hits", 5, false);
    assert_eq!(plan.routes.len(), 2);
    assert!(plan.routes.iter().all(|route| matches!(
        route.mode,
        SearchPlanRouteMode::Fts | SearchPlanRouteMode::Vector
    )));
    assert_eq!(plan.prefer_symbol, Some(false));
    // Symbol/Graph intents earn the symbol-filtered routes; mechanism
    // queries never do, however generic their verbs.
    let intent = crate::read::intent::classify("how does the router rank code hits");
    assert_eq!(intent, QueryIntent::HowTo);
    let symbol = code_plan("dispatch_routes", 5, true);
    assert_eq!(symbol.prefer_symbol, Some(true));
}

#[test]
fn fused_code_hits_normalize_to_findings() {
    use crate::query::db_storage::GuidanceDbStorage;
    use crate::query::recall::run_recall;
    let db = seed_db();
    let storage = GuidanceDbStorage::new(&db);
    let plan = code_plan("dispatch_routes", 5, false);
    let output = run_recall(&plan, &storage, None, None).expect("recall");
    assert!(!output.hits.is_empty(), "{output:?}");
    let findings = search_hits_to_findings(&output.hits);
    assert_eq!(findings.len(), output.hits.len());
    for finding in &findings {
        assert_eq!(
            finding.producer,
            crate::read::engine::FindingProducer::Database
        );
        assert_eq!(finding.kind, crate::read::engine::FindingKind::Code);
        assert_eq!(finding.source.display(), "src/router/dispatch.rs");
        assert!(!finding.provenance.is_empty());
    }
}

#[test]
fn rg_hits_normalize_without_a_second_path() {
    let hit = RgHit {
        path: "src/router/dispatch.rs".to_string(),
        line: 7,
        column: 3,
        text: "pub fn dispatch_routes() {}".to_string(),
        context_before: vec!["// Routes onward.".to_string()],
        context_after: vec![],
    };
    let findings = rg_hits_to_findings(
        std::path::Path::new("/workspace"),
        std::slice::from_ref(&hit),
    );
    assert_eq!(findings.len(), 1);
    let finding = &findings[0];
    assert_eq!(
        finding.producer,
        crate::read::engine::FindingProducer::Ripgrep
    );
    assert_eq!(finding.kind, crate::read::engine::FindingKind::Code);
    assert_eq!(finding.source.display(), "src/router/dispatch.rs");
    assert_eq!(finding.start_line, Some(7));
    assert!(
        finding.producer_score.abs() < f64::EPSILON,
        "exhaustion has no score"
    );
    assert!(finding.text.contains("dispatch_routes"), "{}", finding.text);
    assert_eq!(finding.provenance, "ripgrep");
}

#[test]
fn live_resolution_moves_ranges_with_edits() {
    use super::code::resolve_live_findings;
    use crate::read::engine::{Finding, FindingKind, FindingProducer};
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("moved.rs"),
        "// header\n// more\n/// Adds one.\npub fn alpha() -> u64 {\n    1\n}\n",
    )
    .expect("write");
    let finding = |text: &str, start: u32, end: u32| Finding {
        id: "f".to_string(),
        producer: FindingProducer::Database,
        kind: FindingKind::Code,
        source: crate::source_ref::SourceRef::from_workspace(
            dir.path(),
            std::path::Path::new("moved.rs"),
        ),
        start_line: Some(start),
        end_line: Some(end),
        text: text.to_string(),
        producer_score: 1.0,
        provenance: "code".to_string(),
    };
    // Stored range still holds the text: kept byte-identically.
    let (kept, gaps) = resolve_live_findings(
        dir.path(),
        vec![finding("/// Adds one.\npub fn alpha() -> u64 {", 3, 4)],
    );
    assert!(gaps.is_empty(), "{gaps:?}");
    assert_eq!(kept[0].start_line, Some(3));
    assert!(kept[0].text.contains("pub fn alpha"), "{}", kept[0].text);
    // Stored range is stale (pre-edit lines): the anchor re-resolves
    // to live lines 3-4.
    let (moved, gaps) = resolve_live_findings(
        dir.path(),
        vec![finding("/// Adds one.\npub fn alpha() -> u64 {", 1, 2)],
    );
    assert!(gaps.is_empty(), "{gaps:?}");
    assert_eq!((moved[0].start_line, moved[0].end_line), (Some(3), Some(4)));
    assert!(moved[0].text.contains("pub fn alpha"), "{}", moved[0].text);
    // Unresolvable text keeps its stored slice and logs a gap.
    let (kept, gaps) =
        resolve_live_findings(dir.path(), vec![finding("no such line anywhere", 1, 1)]);
    assert_eq!(
        gaps,
        vec!["source changed since index: moved.rs".to_string()]
    );
    assert_eq!(kept[0].start_line, Some(1));
    // Findings without line ranges pass through untouched.
    let mut edge = finding("calls x", 1, 1);
    edge.start_line = None;
    edge.end_line = None;
    let (kept, gaps) = resolve_live_findings(dir.path(), vec![edge]);
    assert!(gaps.is_empty());
    assert!(kept[0].start_line.is_none());
}
