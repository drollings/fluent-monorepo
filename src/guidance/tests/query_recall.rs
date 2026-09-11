use super::*;
use crate::search_types::{
    CodeSymbolType, Entity, EntityFragment, FileInfo, RecallPath, SearchPlan, SearchPlanRoute,
    SearchPlanRouteMode, StorageFilter, StorageHit, FragmentContent, FragmentSpan,
};
use std::sync::Mutex;

// Verbatim ports of `test/unit/search.test.mjs` against a fixture-faithful
// fake storage (`createFixture` role): `:160` validation, `:215` hybrid,
// `:262` short-circuit, `:302` ordered globs, `:286` diagnosis.

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

// --- :286 diagnosis ---

#[test]
fn diagnosis_handles_missing_targets_and_fallback_selection() {
    let storage = FakeStorage::new();
    let err = diagnose_entity("query", "missing", &storage, None, None).unwrap_err();
    assert!(matches!(err, RecallError::EntityNotFound(_)));

    assert!(
        diagnose_file("query", "/repo/missing.ts", &storage, None, None)
            .expect("diagnose")
            .is_none()
    );
    let diagnosis = diagnose_file("query", "/repo/src/a.ts", &storage, None, None)
        .expect("diagnose")
        .expect("found");
    assert_eq!(diagnosis.entity.id, "entity-a");
    assert_eq!(diagnosis.file.id, "file-a");

    // Empty search falls back to the file's first entity.
    let empty = EmptySearchStorage;
    let fallback = diagnose_file("query", "/repo/src/b.test.ts", &empty, None, None)
        .expect("diagnose")
        .expect("found");
    assert_eq!(fallback.entity.id, "entity-b");
}

struct EmptySearchStorage;

impl RecallStorage for EmptySearchStorage {
    fn list_files(&self) -> Result<Vec<FileInfo>, RecallError> {
        Ok(FILES.iter().map(fixture_file).collect())
    }

    fn search_fts(
        &self,
        _query: &str,
        _limit: usize,
        _filter: &StorageFilter,
    ) -> Result<Vec<StorageHit>, RecallError> {
        Ok(Vec::new())
    }

    fn search_vector(
        &self,
        _embedding: &[f32],
        _limit: usize,
        _filter: &StorageFilter,
    ) -> Result<Vec<StorageHit>, RecallError> {
        Ok(Vec::new())
    }

    fn search_lemmas(
        &self,
        _lemmas: &[String],
        _limit: usize,
        _filter: &StorageFilter,
    ) -> Result<Vec<StorageHit>, RecallError> {
        Ok(Vec::new())
    }

    fn get_entity(&self, entity_id: &str) -> Result<Option<(Entity, FileInfo)>, RecallError> {
        FakeStorage::new().get_entity(entity_id)
    }

    fn list_entities_by_file(&self, file_id: &str) -> Result<Vec<(Entity, FileInfo)>, RecallError> {
        FakeStorage::new().list_entities_by_file(file_id)
    }
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
