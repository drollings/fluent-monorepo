use super::*;
use crate::search_types::{
    CodeSymbolType, Entity, EntityFragment, FileInfo, FragmentContent, FragmentSpan, RecallPath,
    SearchPlan, SearchPlanRoute, SearchPlanRouteMode, StorageFilter, StorageHit,
};
use std::sync::Mutex;

// Verbatim ports of `test/unit/search.test.mjs` against a fixture-faithful
// fake storage (`createFixture` role): `:160` validation, `:215` hybrid,
// `:262` short-circuit, `:302` ordered globs.

#[derive(Debug, Clone)]
struct FixtureEntity {
    id: &'static str,
    file_id: &'static str,
    symbol: &'static str,
}

#[derive(Debug, Clone)]
struct FixtureFile {
    id: &'static str,
    relative: &'static str,
    mtime: i64,
}

const FILES: [FixtureFile; 3] = [
    FixtureFile {
        id: "file-a",
        relative: "src/a.ts",
        mtime: 100,
    },
    FixtureFile {
        id: "file-b",
        relative: "src/b.test.ts",
        mtime: 200,
    },
    FixtureFile {
        id: "file-c",
        relative: "docs/c.ts",
        mtime: 300,
    },
];

const ENTITIES: [FixtureEntity; 3] = [
    FixtureEntity {
        id: "entity-a",
        file_id: "file-a",
        symbol: "AlphaSymbol",
    },
    FixtureEntity {
        id: "entity-b",
        file_id: "file-b",
        symbol: "BetaSymbol",
    },
    FixtureEntity {
        id: "entity-c",
        file_id: "file-c",
        symbol: "GammaSymbol",
    },
];

fn fixture_file(file: &FixtureFile) -> FileInfo {
    FileInfo {
        id: file.id.to_string(),
        absolute_path: format!("/repo/{}", file.relative),
        relative_path: file.relative.to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 10,
        last_modified_time: file.mtime,
        content_hash: None,
        kind: Some(crate::search_types::FileKind::Code),
        format: "typescript".to_string(),
        index_status: None,
    }
}

fn fixture_entity(entity: &FixtureEntity) -> Entity {
    Entity {
        id: entity.id.to_string(),
        file_id: entity.file_id.to_string(),
        range: FragmentSpan::Text {
            start_line: 1,
            end_line: 3,
            start_offset: 0,
            end_offset: 30,
        },
        content: FragmentContent::Text {
            text: format!("export function {}() {{}}", entity.symbol),
        },
        metadata: Some(crate::search_types::EntityMetadata::Code {
            symbol_type: CodeSymbolType::Function,
            symbol_name: Some(entity.symbol.to_string()),
            scope: None,
            node_type: Some("function_declaration".to_string()),
            signature: Some(format!("function {}()", entity.symbol)),
            doc: None,
            modifiers: vec![crate::search_types::CodeEntityModifier::Exported],
        }),
    }
}

fn fixture_fragment(entity: &FixtureEntity, path: RecallPath) -> EntityFragment {
    let (id, group) = if entity.id == "entity-a" && path == RecallPath::Vector {
        ("entity-a-fragment", Some("entity-a"))
    } else {
        (entity.id, None)
    };
    EntityFragment {
        id: id.to_string(),
        group: group.map(str::to_string),
        file_id: entity.file_id.to_string(),
        range: FragmentSpan::Text {
            start_line: 1,
            end_line: 3,
            start_offset: 0,
            end_offset: 30,
        },
        content: FragmentContent::Text {
            text: format!("export function {}() {{}}", entity.symbol),
        },
        metadata: Some(crate::search_types::EntityMetadata::Code {
            symbol_type: CodeSymbolType::Function,
            symbol_name: Some(entity.symbol.to_string()),
            scope: None,
            node_type: Some("function_declaration".to_string()),
            signature: Some(format!("function {}()", entity.symbol)),
            doc: None,
            modifiers: vec![crate::search_types::CodeEntityModifier::Exported],
        }),
    }
}

#[derive(Debug, Default)]
struct Calls {
    fts: Vec<(String, usize, StorageFilter)>,
    vector: Vec<(usize, StorageFilter)>,
}

struct FakeStorage {
    calls: Mutex<Calls>,
}

impl FakeStorage {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Calls::default()),
        }
    }

    fn hits(path: RecallPath, filter: &StorageFilter) -> Vec<StorageHit> {
        let mut out = Vec::new();
        let mut entities: Vec<&FixtureEntity> = ENTITIES.iter().collect();
        if path == RecallPath::Vector {
            entities.reverse();
        }
        for (index, entity) in entities.iter().enumerate() {
            if let Some(ids) = &filter.file_ids {
                if !ids.contains(&entity.file_id.to_string()) {
                    continue;
                }
            }
            if !filter.group_ids.is_empty() && !filter.group_ids.contains(&entity.id.to_string()) {
                continue;
            }
            if !filter.symbol_names.is_empty()
                && !filter.symbol_names.contains(&entity.symbol.to_string())
            {
                continue;
            }
            if !filter.symbol_types.is_empty()
                && !filter.symbol_types.contains(&CodeSymbolType::Function)
            {
                continue;
            }
            let file = FILES.iter().find(|f| f.id == entity.file_id).expect("file");
            out.push(StorageHit {
                fragment: fixture_fragment(entity, path),
                file: fixture_file(file),
                rank: index + 1,
                score: 1.0 - index as f64 * 0.1,
            });
        }
        out
    }
}

impl RecallStorage for FakeStorage {
    fn list_files(&self) -> Result<Vec<FileInfo>, RecallError> {
        Ok(FILES.iter().map(fixture_file).collect())
    }

    fn search_fts(
        &self,
        query: &str,
        limit: usize,
        filter: &StorageFilter,
    ) -> Result<Vec<StorageHit>, RecallError> {
        self.calls
            .lock()
            .expect("calls")
            .fts
            .push((query.to_string(), limit, filter.clone()));
        Ok(Self::hits(RecallPath::Fts, filter)
            .into_iter()
            .take(limit)
            .collect())
    }

    fn search_vector(
        &self,
        _embedding: &[f32],
        limit: usize,
        filter: &StorageFilter,
    ) -> Result<Vec<StorageHit>, RecallError> {
        self.calls
            .lock()
            .expect("calls")
            .vector
            .push((limit, filter.clone()));
        Ok(Self::hits(RecallPath::Vector, filter)
            .into_iter()
            .take(limit)
            .collect())
    }

    fn search_lemmas(
        &self,
        lemmas: &[String],
        limit: usize,
        filter: &StorageFilter,
    ) -> Result<Vec<StorageHit>, RecallError> {
        let mut out = Vec::new();
        for entity in &ENTITIES {
            if let Some(ids) = &filter.file_ids {
                if !ids.contains(&entity.file_id.to_string()) {
                    continue;
                }
            }
            if lemmas
                .iter()
                .any(|lemma| entity.symbol.to_lowercase().contains(&lemma.to_lowercase()))
            {
                let file = FILES.iter().find(|f| f.id == entity.file_id).expect("file");
                out.push(StorageHit {
                    fragment: fixture_fragment(entity, RecallPath::Fts),
                    file: fixture_file(file),
                    rank: out.len() + 1,
                    score: 1.0,
                });
            }
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    fn get_entity(&self, entity_id: &str) -> Result<Option<(Entity, FileInfo)>, RecallError> {
        let Some(entity) = ENTITIES.iter().find(|e| e.id == entity_id) else {
            return Ok(None);
        };
        let file = FILES.iter().find(|f| f.id == entity.file_id).expect("file");
        Ok(Some((fixture_entity(entity), fixture_file(file))))
    }

    fn list_entities_by_file(&self, file_id: &str) -> Result<Vec<(Entity, FileInfo)>, RecallError> {
        let mut out = Vec::new();
        for entity in ENTITIES.iter().filter(|e| e.file_id == file_id) {
            let file = FILES.iter().find(|f| f.id == entity.file_id).expect("file");
            out.push((fixture_entity(entity), fixture_file(file)));
        }
        Ok(out)
    }
}

struct FakeEmbedder;

impl fluent_llm::embeddings::EmbeddingProvider for FakeEmbedder {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn dimensions(&self) -> u32 {
        2
    }

    fn embed(&self, _text: &str) -> Result<Vec<f32>, fluent_llm::embeddings::EmbeddingError> {
        Ok(vec![1.0, 0.0])
    }

    fn embed_batch(
        &self,
        texts: &[&str],
    ) -> Result<fluent_llm::embeddings::BatchEmbedding, fluent_llm::embeddings::EmbeddingError>
    {
        Ok(fluent_llm::embeddings::BatchEmbedding {
            flat: texts.iter().flat_map(|_| [1.0, 0.0]).collect(),
            count: texts.len(),
            dims: 2,
        })
    }
}

fn route(mode: SearchPlanRouteMode, query: &str) -> SearchPlanRoute {
    SearchPlanRoute {
        mode,
        query: query.to_string(),
    }
}

// --- :160 validation ---

#[test]
fn plan_rejects_empty_routes() {
    let storage = FakeStorage::new();
    let plan = SearchPlan {
        routes: vec![],
        ..Default::default()
    };
    let err = run_recall(&plan, &storage, None, None).unwrap_err();
    assert!(matches!(err, RecallError::EmptyRoutes));
    assert_eq!(err.code(), Some(codes::EMPTY_ROUTES));
}

#[test]
fn plan_rejects_blank_queries_and_bad_time_ranges() {
    let storage = FakeStorage::new();
    let plan = SearchPlan {
        routes: vec![route(SearchPlanRouteMode::Fts, "   ")],
        ..Default::default()
    };
    let err = run_recall(&plan, &storage, None, None).unwrap_err();
    assert!(matches!(err, RecallError::EmptyRouteQuery { .. }));
    assert_eq!(err.code(), Some(codes::EMPTY_ROUTE_QUERY));

    // Unsupported modes are rejected by the closed enum at parse time.
    let bad: Result<SearchPlanRouteMode, _> = serde_json::from_str("\"unsupported\"");
    assert!(bad.is_err());

    // Non-array filters are rejected by the type system (Vec<String>).
    // Negative timestamps and inverted ranges still validate at runtime.
    let plan = SearchPlan {
        routes: vec![route(SearchPlanRouteMode::Fts, "value")],
        modified_after: Some(-1),
        ..Default::default()
    };
    assert!(matches!(
        run_recall(&plan, &storage, None, None).unwrap_err(),
        RecallError::NegativeModifiedTime { .. }
    ));
    let plan = SearchPlan {
        routes: vec![route(SearchPlanRouteMode::Fts, "value")],
        modified_after: Some(20),
        modified_before: Some(10),
        ..Default::default()
    };
    let err = run_recall(&plan, &storage, None, None).unwrap_err();
    assert!(matches!(err, RecallError::InvalidModifiedTimeRange));
    assert_eq!(err.code(), Some(codes::INVALID_MODIFIED_TIME_RANGE));
}

// --- :215 verbatim hybrid ---

#[test]
fn hybrid_search_filters_dedupes_fuses_traces_prefers_symbols_and_tracks() {
    let storage = FakeStorage::new();
    let plan = SearchPlan {
        routes: vec![
            route(SearchPlanRouteMode::Fts, "find Namespace::AlphaSymbol"),
            route(SearchPlanRouteMode::Fts, "secondary"),
            route(SearchPlanRouteMode::Vector, "semantic alpha"),
            route(SearchPlanRouteMode::Vector, "semantic beta"),
        ],
        globs: Some(vec!["src/**".to_string(), "!**/*.test.ts".to_string()]),
        file_types: Some(vec!["ts".to_string()]),
        modified_after: Some(50),
        modified_before: Some(250),
        symbol_types: Some(vec![CodeSymbolType::Function]),
        prefer_symbol: Some(true),
        trace: true,
        limit: Some(1),
        track_entity_id: Some("entity-c".to_string()),
        ..Default::default()
    };
    let output = run_recall(&plan, &storage, Some(&FakeEmbedder), None).expect("recall");
    assert_eq!(
        output
            .plan
            .routes
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        vec!["fts", "fts-2", "vector", "vector-2"]
    );
    assert_eq!(output.hits[0].entity.id, "entity-a");
    assert_eq!(
        output.hits[0].matched_by,
        crate::search_types::SearchMatchedBy::FtsAndVector
    );
    assert!(output.hits[0].evidence.len() >= 2);
    let trace = output.hits[0].trace.as_ref().expect("trace");
    assert!(trace.final_trace.returned_by_limit);
    let tracked = output.tracked_hit.as_ref().expect("tracked");
    assert_eq!(tracked.entity.id, "entity-c");
    let tracked_trace = tracked.trace.as_ref().expect("tracked trace");
    assert!(!tracked_trace.final_trace.returned_by_limit);
    assert!(tracked_trace.recall.iter().all(|recall| {
        recall.reason.as_deref() == Some("Target entity file was excluded by the path filters")
    }));
    let calls = storage.calls.lock().expect("calls");
    assert!(calls.fts.iter().any(|(_, _, filter)| filter
        .symbol_names
        .contains(&"Namespace::AlphaSymbol".to_string())));
    assert!(calls.vector.len() >= 2);
    assert!(output
        .timings
        .iter()
        .any(|entry| entry.name == "search_total"));
}

// --- :262 short-circuit ---

#[test]
fn short_circuits_empty_path_filters_with_no_file_reasons() {
    let storage = FakeStorage::new();
    let plan = SearchPlan {
        routes: vec![
            route(SearchPlanRouteMode::Fts, "nothing"),
            route(SearchPlanRouteMode::Vector, "nothing"),
        ],
        include_paths: Some(vec!["missing/**".to_string()]),
        track_entity_id: Some("entity-a".to_string()),
        trace: true,
        ..Default::default()
    };
    let output = run_recall(&plan, &storage, Some(&FakeEmbedder), None).expect("recall");
    let calls = storage.calls.lock().expect("calls");
    assert!(calls.fts.is_empty());
    assert!(calls.vector.is_empty());
    assert_eq!(output.hits.len(), 1);
    let trace = output.hits[0].trace.as_ref().expect("trace");
    assert!(trace
        .recall
        .iter()
        .all(|recall| { recall.reason.as_deref() == Some("No files matched the path filters") }));
}

// --- :302 ordered globs ---

#[test]
fn ordered_globs_match_nested_basenames_with_overrides() {
    let storage = FakeStorage::new();
    let plan = SearchPlan {
        routes: vec![route(SearchPlanRouteMode::Fts, "symbol")],
        globs: Some(vec!["!*.ts".to_string(), "a.ts".to_string()]),
        ..Default::default()
    };
    let output = run_recall(&plan, &storage, None, None).expect("recall");
    // Only file-a survives; its entity-a hit carries the file.
    let paths: Vec<&str> = output
        .hits
        .iter()
        .map(|hit| hit.file.relative_path.as_str())
        .collect();
    assert_eq!(paths, vec!["src/a.ts"]);
}

// --- service:747 fused groups equivalent ---

#[test]
fn independent_routes_fuse_into_one_ranked_list() {
    let storage = FakeStorage::new();
    // Two routes, limit 2: both entities surface in one fused list.
    let plan = SearchPlan {
        routes: vec![
            route(SearchPlanRouteMode::Fts, "AlphaSymbol"),
            route(SearchPlanRouteMode::Fts, "BetaSymbol"),
        ],
        limit: Some(2),
        ..Default::default()
    };
    let output = run_recall(&plan, &storage, None, None).expect("recall");
    let ids: Vec<&str> = output
        .hits
        .iter()
        .map(|hit| hit.entity.id.as_str())
        .collect();
    assert_eq!(ids.len(), 2);
    // entity-a scores 1/61+1/61 (rank 1 in both fake routes... both routes
    // return all three in order) — fused order follows summed ranks.
    assert!(ids.contains(&"entity-a"));
    // Limit 1 keeps the head only.
    let plan = SearchPlan {
        routes: vec![
            route(SearchPlanRouteMode::Fts, "AlphaSymbol"),
            route(SearchPlanRouteMode::Fts, "BetaSymbol"),
        ],
        limit: Some(1),
        ..Default::default()
    };
    let output = run_recall(&plan, &storage, None, None).expect("recall");
    assert_eq!(output.hits.len(), 1);
}

fn summary_file(id: &str, relative: &str) -> search_vector::db::FileRecord {
    search_vector::db::FileRecord {
        id: id.to_string(),
        absolute_path: format!("/repo/{relative}"),
        relative_path: relative.to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 64,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    }
}

fn summary_fragment(
    id: &str,
    name: &str,
    doc: &str,
    text: &str,
    start_line: usize,
) -> crate::extractor::PreparedFragment {
    use crate::extractor::{FragmentMetadata, FragmentRange, PreparedFragment};
    PreparedFragment {
        id: id.to_string(),
        content_text: text.to_string(),
        embedding_text: None,
        range: FragmentRange {
            start_line,
            end_line: start_line + 2,
            start_offset: 0,
            end_offset: text.len(),
        },
        group: None,
        metadata: Some(FragmentMetadata::Code {
            symbol_type: crate::search_types::CodeSymbolType::Function,
            symbol_name: Some(name.to_string()),
            scope: None,
            node_type: "function_item".to_string(),
            signature: Some(format!("fn {name}()")),
            doc: Some(doc.to_string()),
            modifiers: Vec::new(),
        }),
        calls: Vec::new(),
    }
}

fn summary_block(node_key: &str, text: &str) -> crate::ast_parser::CommentBlock {
    crate::ast_parser::CommentBlock {
        node_key: node_key.to_string(),
        node_kind: "function_item".to_string(),
        name: node_key.to_string(),
        scope: String::new(),
        start_byte: 0,
        end_byte: text.len(),
        start_line: 1,
        end_line: 1,
        text: text.to_string(),
        comment_hash: common_core::hash::blake3_hex(text.as_bytes()),
    }
}

fn seed_summary_index() -> search_vector::db::GuidanceDb {
    use crate::query::writer::{FragmentWriter, VectorPolicy};
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let fragments = vec![
        summary_fragment("fa", "alpha", "Adds one", "fn alpha() {}", 1),
        summary_fragment("fb", "beta", "Removes two", "fn beta() {}", 4),
        crate::extractor::code::module_fragment_for(
            "f",
            &[summary_block("$file", "Crate helpers overview")],
        )
        .expect("module fragment"),
    ];
    let blocks = [
        summary_block("alpha", "Adds one"),
        summary_block("beta", "Removes two"),
        summary_block("$file", "Crate helpers overview"),
    ];
    FragmentWriter::write_items(
        &db,
        &summary_file("f", "lib.rs"),
        crate::search_types::FileKind::Code,
        &fragments,
        &blocks,
        &([] as [Vec<f32>; 0]),
        None,
        VectorPolicy::Optional,
    )
    .expect("commit");
    let fx = summary_fragment("fx", "oldfn", "Old words", "fn oldfn() {}", 1);
    FragmentWriter::write_items(
        &db,
        &summary_file("s", "stale.rs"),
        crate::search_types::FileKind::Code,
        &[fx],
        &[summary_block("oldfn", "Old words")],
        &[],
        None,
        VectorPolicy::Optional,
    )
    .expect("commit");
    let fu = summary_fragment("fu", "helper_fn", "", "fn helper_fn() {}", 1);
    FragmentWriter::write_items(
        &db,
        &summary_file("u", "util.rs"),
        crate::search_types::FileKind::Code,
        &[fu],
        &[],
        &([] as [Vec<f32>; 0]),
        None,
        VectorPolicy::Optional,
    )
    .expect("commit");
    let fq = summary_fragment("fq", "quick", "Qx", "fn quick() {}", 1);
    FragmentWriter::write_items(
        &db,
        &summary_file("t", "tiny.rs"),
        crate::search_types::FileKind::Code,
        &[fq],
        &[summary_block("quick", "Qx")],
        &([] as [Vec<f32>; 0]),
        None,
        VectorPolicy::Optional,
    )
    .expect("commit");
    db.replace_summaries(&[
        search_vector::db::SummaryRecord {
            fragment_id: "f#$file".to_string(),
            comment_hash: "hash-file".to_string(),
            model: "test-model".to_string(),
            prompt_version: 1,
            temperature: 0.0,
            seed: 42,
            summary: "Crate helpers overview.".to_string(),
            status: "ready".to_string(),
            generated_at: 1700000000000,
            authorized: false,
        },
        search_vector::db::SummaryRecord {
            fragment_id: "fa".to_string(),
            comment_hash: "hash-a".to_string(),
            model: "test-model".to_string(),
            prompt_version: 1,
            temperature: 0.0,
            seed: 42,
            summary: "Adds one to the total.".to_string(),
            status: "ready".to_string(),
            generated_at: 1700000000000,
            authorized: false,
        },
        search_vector::db::SummaryRecord {
            fragment_id: "fb".to_string(),
            comment_hash: "hash-b".to_string(),
            model: "test-model".to_string(),
            prompt_version: 1,
            temperature: 0.0,
            seed: 42,
            summary: "Removes two from the count.".to_string(),
            status: "ready".to_string(),
            generated_at: 1700000000000,
            authorized: false,
        },
        search_vector::db::SummaryRecord {
            fragment_id: "fx".to_string(),
            comment_hash: "hash-old".to_string(),
            model: "test-model".to_string(),
            prompt_version: 1,
            temperature: 0.0,
            seed: 42,
            summary: "Stale words here.".to_string(),
            status: "failed".to_string(),
            generated_at: 1700000000000,
            authorized: false,
        },
    ])
    .expect("summaries");
    db
}

fn summary_plan(query: &str) -> SearchPlan {
    SearchPlan {
        routes: vec![SearchPlanRoute {
            mode: SearchPlanRouteMode::Fts,
            query: query.to_string(),
        }],
        limit: Some(5),
        trace: true,
        ..Default::default()
    }
}

fn hit_ids(output: &crate::query::recall::RecallOutput) -> Vec<&str> {
    output
        .hits
        .iter()
        .map(|hit| hit.entity.id.as_str())
        .collect()
}

#[test]
fn summary_route_shadow_does_not_fuse() {
    use crate::query::db_storage::GuidanceDbStorage;
    use crate::query::recall::{run_recall_with_policy, RecallPolicy, SummaryMode};
    let db = seed_summary_index();
    let storage = GuidanceDbStorage::new(&db);
    let plan = summary_plan("total");
    let disabled = run_recall_with_policy(&plan, &storage, None, None, RecallPolicy::default())
        .expect("recall");
    let shadowed = run_recall_with_policy(
        &plan,
        &storage,
        None,
        None,
        RecallPolicy {
            summary: SummaryMode::Shadow,
        },
    )
    .expect("recall");
    assert_eq!(hit_ids(&disabled), hit_ids(&shadowed));
    assert!(
        !shadowed
            .hits
            .iter()
            .flat_map(|hit| hit.trace.iter().flat_map(|trace| trace.recall.iter()))
            .any(|entry| entry.path == RecallPath::Summary),
        "shadow must not fuse summary evidence"
    );
}

#[test]
fn summary_route_act_adds_candidates() {
    use crate::query::db_storage::GuidanceDbStorage;
    use crate::query::recall::{run_recall_with_policy, RecallPolicy, SummaryMode};
    let db = seed_summary_index();
    let storage = GuidanceDbStorage::new(&db);
    let output = run_recall_with_policy(
        &summary_plan("total"),
        &storage,
        None,
        None,
        RecallPolicy {
            summary: SummaryMode::Act,
        },
    )
    .expect("recall");
    assert_eq!(hit_ids(&output), vec!["fa"]);
    let trace = output.hits[0].trace.as_ref().expect("trace");
    assert!(
        trace
            .recall
            .iter()
            .any(|entry| entry.path == RecallPath::Summary && entry.found),
        "{trace:?}"
    );
    assert_eq!(
        output.hits[0].matched_by,
        crate::search_types::SearchMatchedBy::Summary
    );
}

#[test]
fn summary_route_citations_stay_source_only() {
    use crate::query::db_storage::GuidanceDbStorage;
    use crate::query::recall::{run_recall_with_policy, RecallPolicy, SummaryMode};
    let db = seed_summary_index();
    let storage = GuidanceDbStorage::new(&db);
    let output = run_recall_with_policy(
        &summary_plan("total"),
        &storage,
        None,
        None,
        RecallPolicy {
            summary: SummaryMode::Act,
        },
    )
    .expect("recall");
    let hit = output.hits.first().expect("hit");
    for evidence in &hit.evidence {
        assert!(
            matches!(
                evidence.range,
                crate::search_types::FragmentSpan::Text { .. }
            ),
            "citations cite source ranges, never generated text: {evidence:?}"
        );
        if let crate::search_types::FragmentContent::Text { text } = &evidence.content {
            assert!(
                !text.contains("to the total"),
                "generated summary text must not masquerade as source: {text:?}"
            );
        }
    }
}

#[test]
fn summary_route_fts_needs_no_model() {
    use crate::query::db_storage::GuidanceDbStorage;
    let db = seed_summary_index();
    let storage = GuidanceDbStorage::new(&db);
    let hits = storage
        .search_summaries("total", 10, &crate::search_types::StorageFilter::default())
        .expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].fragment.id, "fa");
}

#[test]
fn summary_route_control_identifier_only_stays_deterministic() {
    use crate::query::db_storage::GuidanceDbStorage;
    use crate::query::recall::{run_recall_with_policy, RecallPolicy, SummaryMode};
    let db = seed_summary_index();
    let storage = GuidanceDbStorage::new(&db);
    let plan = summary_plan("helper_fn");
    let disabled = run_recall_with_policy(&plan, &storage, None, None, RecallPolicy::default())
        .expect("recall");
    let acted = run_recall_with_policy(
        &plan,
        &storage,
        None,
        None,
        RecallPolicy {
            summary: SummaryMode::Act,
        },
    )
    .expect("recall");
    assert_eq!(hit_ids(&disabled), hit_ids(&acted));
    assert!(
        !acted
            .hits
            .iter()
            .flat_map(|hit| hit.trace.iter().flat_map(|trace| trace.recall.iter()))
            .any(|entry| entry.path == RecallPath::Summary && entry.found),
        "identifier-only queries must not fire the summary route"
    );
}

#[test]
fn summary_route_unavailable_rows_never_fire() {
    use crate::query::db_storage::GuidanceDbStorage;
    let db = seed_summary_index();
    let storage = GuidanceDbStorage::new(&db);
    let hits = storage
        .search_summaries("stale", 10, &crate::search_types::StorageFilter::default())
        .expect("search");
    assert!(
        hits.is_empty(),
        "failed summaries must never fire: {hits:?}"
    );
}

fn summary_corpus_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("corpora")
        .join("summary_index_seed.json")
}

#[test]
fn summary_route_corpus_loads() {
    use crate::query::recall::load_summary_corpus;
    let cases = load_summary_corpus(&summary_corpus_path()).expect("corpus loads");
    assert!(!cases.is_empty(), "corpus is not empty");
    assert!(
        cases.iter().any(|case| case.control),
        "corpus carries controls"
    );
    assert!(
        cases
            .iter()
            .any(|case| !case.control && !case.expected.is_empty()),
        "corpus carries expected targets"
    );
}

#[test]
fn summary_route_corpus_measures_zero_control_fires() {
    use crate::query::db_storage::GuidanceDbStorage;
    use crate::query::recall::{load_summary_corpus, measure_summary_recall};
    let db = seed_summary_index();
    let storage = GuidanceDbStorage::new(&db);
    let cases = load_summary_corpus(&summary_corpus_path()).expect("corpus loads");
    let report = measure_summary_recall(&storage, &cases, 10);
    assert_eq!(
        report.control_fires, 0,
        "controls must never fire: {report:?}"
    );
    assert_eq!(report.fpr, 0.0, "{report:?}");
    assert_eq!(report.precision, 1.0, "{report:?}");
    assert_eq!(report.recall, 1.0, "{report:?}");
    assert!(report.support > 0, "{report:?}");
}

#[test]
fn summary_route_stale_hash_fails_freshness() {
    use crate::summary::summary_is_fresh;
    use fluent_types::{CommentSummary, OverlayStatus};
    let fresh = CommentSummary {
        text: "Adds one.".into(),
        comment_hash: "hash-a".to_string(),
        code_hash: String::new(),
        model: "test-model".into(),
        prompt_version: crate::summary::SUMMARY_PROMPT_VERSION,
        temperature: 0.0,
        seed: 42,
        generated_at: 1700000000000,
        status: OverlayStatus::Ready,
        failure_kind: None,
        failure_reason: None,
    };
    assert!(summary_is_fresh(&fresh, "hash-a", "", "test-model"));
    assert!(!summary_is_fresh(&fresh, "hash-b", "", "test-model"));
    assert!(!summary_is_fresh(&fresh, "hash-a", "", "other-model"));
    assert!(!summary_is_fresh(
        &fresh,
        "hash-a",
        "code-hash-a",
        "test-model"
    ));
}
