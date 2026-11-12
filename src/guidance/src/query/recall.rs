//! Hybrid recall pipeline: plan validation, adaptive recall loop,
//! force-track, and materialization. Ports zvec
//! `engine/pipeline/search/index.ts` (`searchWorkspaceIndex` core:
//! `validateSearchPlan`, `collectAdaptiveRecall`, `buildRecallRoutes`,
//! `forceTrackEntity`, fusion slice, `candidateToHit`).
//!
//! Storage and embedding arrive as injected seams (`RecallStorage`,
//! `EmbeddingProvider`); the hermetic fake in `tests/query_recall.rs`
//! plays the `createFixture` role for the verbatim ports.

use std::collections::{HashMap, HashSet};

use crate::query::fusion::{
    apply_limit_and_track, build_hit, fuse_candidates, RecallCandidate, RecallEvidence,
    TimingCollector,
};
use crate::query::glob::{
    matches_file_selection, path_pattern_matches, resolve_file_type_patterns, FileTypePatterns,
};
use crate::search_constants::{
    recall_target_candidate_count, DEFAULT_LIMIT, RECALL_GROWTH_FACTOR, RECALL_INITIAL_DEPTH,
    RECALL_MAX_DEPTH,
};
use crate::search_types::{
    public_entity_id, CodeSymbolType, Entity, EntityFragment, FileInfo, RecallPath,
    ResolvedSearchPlan, ResolvedSearchPlanRoute, RrfScore, SearchHit, SearchPlan,
    SearchPlanRouteMode, SearchRecallTrace, StorageFilter, StorageHit, TimingEntry,
};

pub use super::plan::{codes, validate_search_plan, RecallError};

/// Index storage contract for the recall loop: lexical + vector + lemma
/// recall, entity lookup, file listing. The P1 GuidanceDb adapter and the
/// hermetic fake both implement it.
pub trait RecallStorage: Send + Sync {
    /// All indexed files (filter resolution input).
    fn list_files(&self) -> Result<Vec<FileInfo>, RecallError>;
    /// Ranked lexical recall (1-based storage ranks, best first).
    fn search_fts(
        &self,
        query: &str,
        limit: usize,
        filter: &StorageFilter,
    ) -> Result<Vec<StorageHit>, RecallError>;
    /// Ranked vector recall over a query embedding.
    fn search_vector(
        &self,
        embedding: &[f32],
        limit: usize,
        filter: &StorageFilter,
    ) -> Result<Vec<StorageHit>, RecallError>;
    /// Lemma-token lookup (fragment ids carrying any listed lemma).
    fn search_lemmas(
        &self,
        lemmas: &[String],
        limit: usize,
        filter: &StorageFilter,
    ) -> Result<Vec<StorageHit>, RecallError>;
    /// Generated-summary recall (fragment ids whose summary matches).
    /// Defaults to empty so existing fakes and adapters stay valid; the
    /// summary index serves it once the pass has projected summaries.
    fn search_summaries(
        &self,
        _query: &str,
        _limit: usize,
        _filter: &StorageFilter,
    ) -> Result<Vec<StorageHit>, RecallError> {
        Ok(Vec::new())
    }
    /// Entity + file lookup by public entity id.
    fn get_entity(&self, entity_id: &str) -> Result<Option<(Entity, FileInfo)>, RecallError>;
    /// Entities + files for one file id (diagnosis fallback).
    fn list_entities_by_file(&self, file_id: &str) -> Result<Vec<(Entity, FileInfo)>, RecallError>;
    /// Committed index generation, when the backend records one.
    /// Defaults to unknown so fakes and legacy adapters keep working;
    /// guarded reads refuse on unknown only when a caller requires it.
    fn stored_generation(&self) -> Option<String> {
        None
    }
}

/// Resolve the storage file allow-list: include matchers, exclude matchers,
/// glob/type selection, mtime window. `None` = no path selection.
/// Source: `resolveFilteredFileIds`.
#[must_use]
/// Resolve the storage file allow-list: include matchers, exclude matchers,
/// glob/type selection, mtime window. `None` = no path selection.
/// Source: `resolveFilteredFileIds`.
pub fn resolve_filtered_file_ids(
    files: &[FileInfo],
    plan: &ResolvedSearchPlan,
    types: &FileTypePatterns,
) -> Option<Vec<String>> {
    let includes = plan.include_paths.clone().unwrap_or_default();
    let excludes = plan.exclude_paths.clone().unwrap_or_default();
    let has_modified = plan.modified_after.is_some() || plan.modified_before.is_some();
    let has_selection = plan.globs.as_ref().is_some_and(|g| !g.is_empty())
        || plan
            .insensitive_globs
            .as_ref()
            .is_some_and(|g| !g.is_empty())
        || !types.include.is_empty()
        || !types.exclude.is_empty();
    if includes.is_empty() && excludes.is_empty() && !has_modified && !has_selection {
        return None;
    }
    let globs = plan.globs.clone().unwrap_or_default();
    let insensitive = plan.insensitive_globs.clone().unwrap_or_default();
    Some(
        files
            .iter()
            .filter(|file| {
                (includes.is_empty()
                    || includes
                        .iter()
                        .any(|pattern| path_pattern_matches(pattern, &file.relative_path)))
                    && !excludes
                        .iter()
                        .any(|pattern| path_pattern_matches(pattern, &file.relative_path))
                    && matches_file_selection(&file.relative_path, &globs, &insensitive, types)
                    && matches_modified_time(file, plan)
            })
            .map(|file| file.id.clone())
            .collect(),
    )
}

/// Closed symbol-type name in zvec wire spelling (`"function"`, …).
/// Used by the GuidanceDb adapter when pushing the filter into SQL.
#[must_use]
/// Closed symbol-type name in zvec wire spelling (`"function"`, …).
/// Used by the GuidanceDb adapter when pushing the filter into SQL.
pub fn symbol_type_name(symbol: &CodeSymbolType) -> String {
    match symbol {
        CodeSymbolType::Module => "module",
        CodeSymbolType::Class => "class",
        CodeSymbolType::Interface => "interface",
        CodeSymbolType::Function => "function",
        CodeSymbolType::Value => "value",
        CodeSymbolType::Alias => "alias",
    }
    .to_string()
}

fn matches_modified_time(file: &FileInfo, plan: &ResolvedSearchPlan) -> bool {
    if let Some(after) = plan.modified_after {
        if file.last_modified_time < after {
            return false;
        }
    }
    if let Some(before) = plan.modified_before {
        if file.last_modified_time > before {
            return false;
        }
    }
    true
}

/// Recall output: resolved plan, ranked hits, force-tracked hit, timings.
#[derive(Debug)]
/// Recall output: resolved plan, ranked hits, force-tracked hit, timings.
pub struct RecallOutput {
    /// Resolved plan.
    pub plan: ResolvedSearchPlan,
    /// Ranked hits (limit-cut, track-appended).
    pub hits: Vec<SearchHit>,
    /// Force-tracked hit, when requested.
    pub tracked_hit: Option<SearchHit>,
    /// Stage timings (`search_total` last).
    pub timings: Vec<TimingEntry>,
}

struct RecallRoute {
    id: String,
    mode: RecallPath,
    query: String,
    filter: StorageFilter,
    vector_route_id: Option<String>,
    /// Precomputed lemma lookup for `Lemma` routes.
    lemmas: Vec<String>,
}

/// Summary route mode: generated commentary participates additively,
/// never as source. Disabled by default; shadow observes without fusing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// Summary route mode: generated commentary participates additively,
/// never as source. Disabled by default; shadow observes without fusing.
pub enum SummaryMode {
    /// Summary route off: zero extra work, byte-identical results.
    #[default]
    Disabled,
    /// Compute the route and log agreement; never fuse candidates.
    Shadow,
    /// Fuse summary candidates; citations stay source ranges.
    Act,
}

/// Additive recall policy: behavior beyond the deterministic routes.
/// `run_recall` keeps the disabled default for existing callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// Additive recall policy: behavior beyond the deterministic routes.
/// `run_recall` keeps the disabled default for existing callers.
pub struct RecallPolicy {
    /// Summary route mode.
    pub summary: SummaryMode,
}

/// Run the hybrid recall pipeline end to end. Source: `searchWorkspaceIndex`.
/// The lemma route (P1 L2) always participates; it yields nothing when
/// normalization is empty.
pub fn run_recall(
    plan: &SearchPlan,
    storage: &dyn RecallStorage,
    embedder: Option<&dyn fluent_llm::embeddings::EmbeddingProvider>,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> Result<RecallOutput, RecallError> {
    run_recall_with_policy(plan, storage, embedder, nlp, RecallPolicy::default())
}

/// Policy-aware recall: `run_recall` with an explicit [`RecallPolicy`].
/// `Shadow` computes the summary route for agreement logging without
/// fusing; `Act` fuses summary candidates with source-only citations.
pub fn run_recall_with_policy(
    plan: &SearchPlan,
    storage: &dyn RecallStorage,
    embedder: Option<&dyn fluent_llm::embeddings::EmbeddingProvider>,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
    policy: RecallPolicy,
) -> Result<RecallOutput, RecallError> {
    let mut timings = TimingCollector::new();
    let normalized = validate_search_plan(plan)?;
    timings.record("search_plan");
    let limit = normalized.limit.unwrap_or(DEFAULT_LIMIT);
    let trace = normalized.trace || normalized.track_entity_id.is_some();
    let types = resolve_file_type_patterns(
        &normalized.file_types.clone().unwrap_or_default(),
        &normalized.excluded_file_types.clone().unwrap_or_default(),
    )?;
    timings.record("search_file_types");
    let files = storage.list_files()?;
    let file_ids = resolve_filtered_file_ids(&files, &normalized, &types);
    let symbol_types = normalized.symbol_types.clone().unwrap_or_default();
    let filter = StorageFilter {
        file_ids,
        symbol_types,
        ..Default::default()
    };
    timings.record("search_filter");
    let has_searchable_files = !filter_matches_no_files(&filter);
    let vector_by_route = if has_searchable_files && plan_uses_vector(&normalized) {
        embed_vector_routes(&normalized.routes, embedder)?
    } else {
        HashMap::new()
    };
    timings.record("query_embedding");
    let mut candidates: HashMap<String, RecallCandidate> = HashMap::new();
    let mut recall_depth = RECALL_INITIAL_DEPTH;
    if has_searchable_files {
        recall_depth = collect_adaptive_recall(
            &normalized,
            &filter,
            &vector_by_route,
            nlp,
            limit,
            storage,
            &mut candidates,
            policy,
        )?;
    }
    timings.record("recall");
    if let Some(tracked) = normalized.track_entity_id.clone() {
        force_track_entity(
            &tracked,
            &normalized.routes,
            &vector_by_route,
            recall_depth,
            &filter,
            storage,
            &mut candidates,
        )?;
    }
    timings.record("force_track");
    let fused = fuse_candidates(candidates.into_values().collect());
    timings.record("fusion");
    let visible = apply_limit_and_track(fused, limit, normalized.track_entity_id.as_deref());
    let hits: Vec<SearchHit> = visible
        .iter()
        .map(|candidate| build_hit(candidate, limit, trace))
        .collect();
    let tracked_hit = normalized
        .track_entity_id
        .as_deref()
        .and_then(|tracked| hits.iter().find(|hit| hit.entity.id == *tracked).cloned());
    timings.record("materialize");
    Ok(RecallOutput {
        plan: normalized,
        hits,
        tracked_hit,
        timings: timings.finish(),
    })
}

/// Guarded recall: refuse before reading when the backend reports a
/// generation different from the caller-computed expected value.
/// The check happens before any row is in hand, so a crash between
/// sidecar write and index commit can cost work but never serve a
/// mixed pair. `run_recall` keeps its shape for existing callers.
pub fn run_recall_guarded(
    plan: &SearchPlan,
    storage: &dyn RecallStorage,
    embedder: Option<&dyn fluent_llm::embeddings::EmbeddingProvider>,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
    expected_generation: &str,
) -> Result<RecallOutput, RecallError> {
    match storage.stored_generation() {
        Some(committed) if committed == expected_generation => {}
        _ => {
            return Err(RecallError::Db(format!(
                "index generation mismatch: expected {expected_generation}"
            )));
        }
    }
    run_recall(plan, storage, embedder, nlp)
}

fn plan_uses_vector(plan: &ResolvedSearchPlan) -> bool {
    plan.routes
        .iter()
        .any(|route| route.mode == SearchPlanRouteMode::Vector)
}

fn filter_matches_no_files(filter: &StorageFilter) -> bool {
    filter.file_ids.as_ref().is_some_and(Vec::is_empty)
}

/// Embed every vector route in one batch call. No embedder (or an empty
/// result) degrades L3 away: vector routes yield nothing. A hard embed
/// failure aborts the recall.
fn embed_vector_routes(
    routes: &[ResolvedSearchPlanRoute],
    embedder: Option<&dyn fluent_llm::embeddings::EmbeddingProvider>,
) -> Result<HashMap<String, Vec<f32>>, RecallError> {
    let vector_routes: Vec<&ResolvedSearchPlanRoute> = routes
        .iter()
        .filter(|route| route.mode == SearchPlanRouteMode::Vector)
        .collect();
    if vector_routes.is_empty() {
        return Ok(HashMap::new());
    }
    let Some(embedder) = embedder else {
        return Ok(HashMap::new());
    };
    let queries: Vec<&str> = vector_routes
        .iter()
        .map(|route| route.query.as_str())
        .collect();
    let batch = embedder
        .embed_batch(&queries)
        .map_err(|error| RecallError::Embed(error.to_string()))?;
    let mut vectors = HashMap::new();
    for (index, route) in vector_routes.iter().enumerate() {
        if let Some(vector) = batch.try_vector(index) {
            if !vector.is_empty() {
                vectors.insert(route.id.clone(), vector.to_vec());
            }
        }
    }
    Ok(vectors)
}

fn collect_adaptive_recall(
    plan: &ResolvedSearchPlan,
    filter: &StorageFilter,
    vector_by_route: &HashMap<String, Vec<f32>>,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
    limit: usize,
    storage: &dyn RecallStorage,
    candidates: &mut HashMap<String, RecallCandidate>,
    policy: RecallPolicy,
) -> Result<usize, RecallError> {
    // M5: fan-out, not a first-accept walk — every route contributes hits
    // each depth (results merge for fusion). The ladder combinator
    // (first-Ok-wins) would drop recalls. Stays.
    let routes = build_recall_routes(plan, filter, nlp, policy);
    let target = recall_target_candidate_count(limit);
    let mut previous_depth = 0;
    let mut depth = RECALL_INITIAL_DEPTH;
    loop {
        let mut saturated = false;
        for route in &routes {
            let hits = recall_route_hits(route, vector_by_route, depth, storage)?;
            // Shadow observations never perturb the deterministic loop:
            // saturation and fusion see exactly the pre-summary behavior.
            let shadow_only =
                route.mode == RecallPath::Summary && policy.summary == SummaryMode::Shadow;
            if shadow_only {
                record_summary_shadow(candidates, &hits);
                continue;
            }
            saturated = saturated || hits.len() >= depth;
            add_recall_hits(candidates, &hits, route, storage, previous_depth)?;
        }
        if candidates.len() >= target || !saturated || depth >= RECALL_MAX_DEPTH {
            return Ok(depth);
        }
        previous_depth = depth;
        depth = (depth * RECALL_GROWTH_FACTOR).min(RECALL_MAX_DEPTH);
    }
}

/// Log summary-route agreement against the deterministic candidates without
/// fusing: which summary hits the deterministic path already finds, and
/// which it would add.
fn record_summary_shadow(candidates: &HashMap<String, RecallCandidate>, hits: &[StorageHit]) {
    use std::collections::HashSet;
    let authoritative: HashSet<&str> = candidates.keys().map(String::as_str).collect();
    let summary: Vec<String> = hits
        .iter()
        .map(|hit| public_entity_id(&hit.fragment).to_string())
        .collect();
    let record = crate::query::shadow::summary_route_shadow(
        &authoritative,
        &summary.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    tracing::info!(
        target: "summary_shadow",
        point = record.point.as_str(),
        label = record.label.as_str(),
        confidence = record.confidence,
        would_have_acted = record.would_have_acted,
        "summary route shadow agreement",
    );
}

/// One labelled retrieval case for summary-index calibration: the queries
/// a summary route must answer plus the controls that must never fire it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
/// One labelled retrieval case for summary-index calibration: the queries
/// a summary route must answer plus the controls that must never fire it.
pub struct SummaryRecallCase {
    /// Query text.
    pub query: String,
    /// Fragment ids the summary route must return.
    #[serde(default)]
    pub expected: Vec<String>,
    /// Control queries must yield no summary hits.
    #[serde(default)]
    pub control: bool,
    /// Why this case exists.
    #[serde(default)]
    pub reason: String,
}

/// Task-value report for one summary-index measurement: precision/recall
/// over expected hits, false-positive rate over controls.
#[derive(Debug, Clone, PartialEq)]
/// Task-value report for one summary-index measurement: precision/recall
/// over expected hits, false-positive rate over controls.
pub struct SummaryRecallReport {
    /// Hits that matched expectations / all summary hits.
    pub precision: f64,
    /// Expected hits found / all expected hits.
    pub recall: f64,
    /// Controls that fired / all controls.
    pub fpr: f64,
    /// Control queries with any summary hit.
    pub control_fires: usize,
    /// Non-control cases measured.
    pub support: usize,
}

/// Load a summary-index corpus (`summary_index_seed.json` shape).
pub fn load_summary_corpus(path: &std::path::Path) -> Result<Vec<SummaryRecallCase>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("read corpus {}: {e}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("parse corpus: {e}"))?;
    let point = value.get("point").and_then(|v| v.as_str()).unwrap_or("");
    if point != crate::query::decision_points::SUMMARY_INDEX_POINT {
        return Err(format!("corpus point is {point:?}, want summary_index"));
    }
    let queries = value
        .get("queries")
        .ok_or_else(|| "corpus has no queries".to_string())?;
    serde_json::from_value(queries.clone()).map_err(|e| format!("parse cases: {e}"))
}

/// Measure the summary route against labelled cases: expected fragment ids
/// per query, zero tolerance for control fires. Hermetic: the caller seeds
/// precomputed summary fixtures; no model runs here.
#[must_use]
/// Measure the summary route against labelled cases: expected fragment ids
/// per query, zero tolerance for control fires. Hermetic: the caller seeds
/// precomputed summary fixtures; no model runs here.
pub fn measure_summary_recall(
    storage: &dyn RecallStorage,
    cases: &[SummaryRecallCase],
    limit: usize,
) -> SummaryRecallReport {
    use std::collections::HashSet;
    let mut retrieved = 0usize;
    let mut relevant_retrieved = 0usize;
    let mut relevant = 0usize;
    let mut control_fires = 0usize;
    let mut controls = 0usize;
    let mut support = 0usize;
    let filter = StorageFilter::default();
    for case in cases {
        let Ok(hits) = storage.search_summaries(&case.query, limit, &filter) else {
            continue;
        };
        let hit_ids: HashSet<&str> = hits
            .iter()
            .map(|hit| public_entity_id(&hit.fragment))
            .collect();
        if case.control {
            controls += 1;
            if !hit_ids.is_empty() {
                control_fires += 1;
            }
            continue;
        }
        support += 1;
        let expected: HashSet<&str> = case.expected.iter().map(String::as_str).collect();
        relevant_retrieved += hit_ids.intersection(&expected).count();
        retrieved += hit_ids.len();
        relevant += expected.len();
    }
    SummaryRecallReport {
        precision: if retrieved == 0 {
            1.0
        } else {
            relevant_retrieved as f64 / retrieved as f64
        },
        recall: if relevant == 0 {
            1.0
        } else {
            relevant_retrieved as f64 / relevant as f64
        },
        fpr: if controls == 0 {
            0.0
        } else {
            control_fires as f64 / controls as f64
        },
        control_fires,
        support,
    }
}

fn build_recall_routes(
    plan: &ResolvedSearchPlan,
    filter: &StorageFilter,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
    policy: RecallPolicy,
) -> Vec<RecallRoute> {
    let mut output: Vec<RecallRoute> = plan
        .routes
        .iter()
        .map(|route| RecallRoute {
            id: route.id.clone(),
            mode: route.mode.into(),
            query: route.query.clone(),
            filter: filter.clone(),
            vector_route_id: (route.mode == SearchPlanRouteMode::Vector).then(|| route.id.clone()),
            lemmas: Vec::new(),
        })
        .collect();
    // Lemma route (P1 L2): normalized query lemmas looked up in
    // `fragment_lemmas`. Skipped on empty normalization.
    let lemma_query = plan
        .routes
        .first()
        .map(|route| route.query.clone())
        .unwrap_or_default();
    let lemmas = crate::query::ingest::query_lemmas(nlp, &lemma_query);
    if !lemmas.is_empty() {
        output.push(RecallRoute {
            id: "lemma".to_string(),
            mode: RecallPath::Lemma,
            query: lemma_query.clone(),
            filter: filter.clone(),
            vector_route_id: None,
            lemmas,
        });
    }
    // Summary route: generated-comment recall over `fragment_summaries`.
    // Computed in both Shadow and Act; fused only in Act.
    if policy.summary != SummaryMode::Disabled && !lemma_query.trim().is_empty() {
        output.push(RecallRoute {
            id: "summary".to_string(),
            mode: RecallPath::Summary,
            query: lemma_query,
            filter: filter.clone(),
            vector_route_id: None,
            lemmas: Vec::new(),
        });
    }
    if plan.prefer_symbol != Some(true) {
        return output;
    }
    let mut seen = HashSet::new();
    for route in &plan.routes {
        let symbol_names = search_vector::tokens::symbol_names(&route.query);
        if symbol_names.is_empty() {
            continue;
        }
        let key = format!("{}\0{}", route.id, symbol_names.join("\0"));
        if !seen.insert(key) {
            continue;
        }
        let mut symbol_filter = filter.clone();
        symbol_filter.symbol_names = symbol_names;
        output.push(RecallRoute {
            id: format!("{}.prefer-symbol", route.id),
            mode: RecallPath::Fts,
            query: route.query.clone(),
            filter: symbol_filter,
            vector_route_id: None,
            lemmas: Vec::new(),
        });
    }
    output
}

fn recall_route_hits(
    route: &RecallRoute,
    vector_by_route: &HashMap<String, Vec<f32>>,
    depth: usize,
    storage: &dyn RecallStorage,
) -> Result<Vec<StorageHit>, RecallError> {
    match route.mode {
        RecallPath::Fts => storage.search_fts(&route.query, depth, &route.filter),
        RecallPath::Vector => {
            let route_id = route.vector_route_id.as_deref().unwrap_or(&route.id);
            match vector_by_route.get(route_id) {
                Some(vector) => storage.search_vector(vector, depth, &route.filter),
                None => Ok(Vec::new()),
            }
        }
        RecallPath::Lemma => {
            if route.lemmas.is_empty() {
                return Ok(Vec::new());
            }
            storage.search_lemmas(&route.lemmas, depth, &route.filter)
        }
        RecallPath::Summary => storage.search_summaries(&route.query, depth, &route.filter),
    }
}

fn add_recall_hits(
    candidates: &mut HashMap<String, RecallCandidate>,
    hits: &[StorageHit],
    route: &RecallRoute,
    storage: &dyn RecallStorage,
    previous_depth: usize,
) -> Result<(), RecallError> {
    for (index, hit) in hits.iter().enumerate().skip(previous_depth) {
        let rank = index + 1;
        let entity_id = public_entity_id(&hit.fragment).to_string();
        if !candidates.contains_key(&entity_id) {
            let Some((entity, file)) = resolve_hit_entity(hit, &entity_id, storage)? else {
                continue;
            };
            candidates.insert(
                entity_id.clone(),
                RecallCandidate {
                    id: entity_id.clone(),
                    entity,
                    file,
                    sources: Vec::new(),
                    recall: Vec::new(),
                    evidence: Vec::new(),
                    score: RrfScore::new(0.0),
                    rank: 0,
                    forced: false,
                },
            );
        }
        let candidate = candidates.get_mut(&entity_id).expect("just inserted");
        if !candidate.sources.contains(&route.mode) {
            candidate.sources.push(route.mode);
        }
        candidate.evidence.push(RecallEvidence {
            fragment: hit.fragment.clone(),
            path: route.mode,
            route_id: route.id.clone(),
            query: route.query.clone(),
            rank: Some(rank),
            score: Some(hit.score),
            forced: None,
        });
        add_or_update_recall(candidate, found_trace(route, rank, hit.score));
    }
    Ok(())
}

fn resolve_hit_entity(
    hit: &StorageHit,
    entity_id: &str,
    storage: &dyn RecallStorage,
) -> Result<Option<(Entity, FileInfo)>, RecallError> {
    if hit
        .fragment
        .group
        .as_deref()
        .is_some_and(|group| !group.is_empty())
        && hit.fragment.id != entity_id
    {
        return storage.get_entity(entity_id);
    }
    Ok(Some((fragment_to_entity(&hit.fragment), hit.file.clone())))
}

fn fragment_to_entity(fragment: &EntityFragment) -> Entity {
    Entity {
        id: public_entity_id(fragment).to_string(),
        file_id: fragment.file_id.clone(),
        range: fragment.range.clone(),
        content: fragment.content.clone(),
        metadata: fragment.metadata.clone(),
    }
}

fn found_trace(route: &RecallRoute, rank: usize, score: f64) -> SearchRecallTrace {
    SearchRecallTrace {
        path: route.mode,
        route_id: Some(route.id.clone()),
        query: Some(route.query.clone()),
        found: true,
        forced: None,
        rank: Some(rank),
        score: Some(score),
        reason: None,
    }
}

fn add_or_update_recall(candidate: &mut RecallCandidate, recall: SearchRecallTrace) {
    let Some(existing) = candidate
        .recall
        .iter_mut()
        .find(|item| item.path == recall.path && item.route_id == recall.route_id)
    else {
        candidate.recall.push(recall);
        return;
    };
    if !recall.found {
        return;
    }
    let replace = !existing.found
        || existing.rank.is_none()
        || recall.rank.is_some_and(|rank| Some(rank) < existing.rank);
    if replace {
        let forced = existing.forced == Some(true) || recall.forced == Some(true);
        *existing = recall;
        existing.forced = forced.then_some(true);
        return;
    }
    if recall.forced == Some(true) {
        existing.forced = Some(true);
    }
}

#[allow(clippy::too_many_arguments)]
fn force_track_entity(
    entity_id: &str,
    routes: &[ResolvedSearchPlanRoute],
    vector_by_route: &HashMap<String, Vec<f32>>,
    recall_depth: usize,
    filter: &StorageFilter,
    storage: &dyn RecallStorage,
    candidates: &mut HashMap<String, RecallCandidate>,
) -> Result<(), RecallError> {
    let Some((entity, file)) = storage.get_entity(entity_id)? else {
        return Ok(());
    };
    candidates
        .entry(entity_id.to_string())
        .or_insert_with(|| RecallCandidate {
            id: entity_id.to_string(),
            entity: entity.clone(),
            file: file.clone(),
            sources: Vec::new(),
            recall: Vec::new(),
            evidence: Vec::new(),
            score: RrfScore::new(0.0),
            rank: 0,
            forced: false,
        });
    let seen: HashSet<Option<String>> = candidates[entity_id]
        .recall
        .iter()
        .map(|trace| trace.route_id.clone())
        .collect();
    for route in routes {
        if seen.contains(&Some(route.id.clone())) {
            continue;
        }
        let candidate = candidates.get_mut(entity_id).expect("just inserted");
        if filter_matches_no_files(filter) {
            push_miss(candidate, route, "No files matched the path filters");
            continue;
        }
        if filter
            .file_ids
            .as_ref()
            .is_some_and(|ids| !ids.contains(&file.id))
        {
            push_miss(
                candidate,
                route,
                "Target entity file was excluded by the path filters",
            );
            continue;
        }
        match route.mode {
            SearchPlanRouteMode::Fts => {
                let mut targeted = filter.clone();
                targeted.file_ids = Some(vec![file.id.clone()]);
                targeted.group_ids = vec![entity_id.to_string()];
                let hits = storage.search_fts(&route.query, recall_depth, &targeted)?;
                match hits
                    .iter()
                    .find(|hit| public_entity_id(&hit.fragment) == entity_id)
                {
                    Some(hit) => {
                        push_found(candidate, route, recall_depth + 1, hit.score, &hit.fragment);
                    }
                    None => push_miss(
                        candidate,
                        route,
                        "Target entity did not match the FTS query",
                    ),
                }
            }
            SearchPlanRouteMode::Vector => {
                let Some(vector) = vector_by_route.get(&route.id) else {
                    push_miss(
                        candidate,
                        route,
                        "Vector route was not available for this query",
                    );
                    continue;
                };
                let mut targeted = filter.clone();
                targeted.file_ids = Some(vec![file.id.clone()]);
                targeted.group_ids = vec![entity_id.to_string()];
                let hits = storage.search_vector(vector, recall_depth, &targeted)?;
                match hits
                    .iter()
                    .find(|hit| public_entity_id(&hit.fragment) == entity_id)
                {
                    Some(hit) => {
                        push_found(candidate, route, recall_depth + 1, hit.score, &hit.fragment);
                    }
                    None => push_miss(
                        candidate,
                        route,
                        "Target entity could not be scored by vector search",
                    ),
                }
            }
        }
    }
    Ok(())
}

fn push_miss(candidate: &mut RecallCandidate, route: &ResolvedSearchPlanRoute, reason: &str) {
    candidate.recall.push(SearchRecallTrace {
        path: route.mode.into(),
        route_id: Some(route.id.clone()),
        query: Some(route.query.clone()),
        found: false,
        forced: Some(true),
        rank: None,
        score: None,
        reason: Some(reason.to_string()),
    });
}

fn push_found(
    candidate: &mut RecallCandidate,
    route: &ResolvedSearchPlanRoute,
    rank: usize,
    score: f64,
    fragment: &EntityFragment,
) {
    if !candidate.sources.contains(&route.mode.into()) {
        candidate.sources.push(route.mode.into());
    }
    candidate.evidence.push(RecallEvidence {
        fragment: fragment.clone(),
        path: route.mode.into(),
        route_id: route.id.clone(),
        query: route.query.clone(),
        rank: Some(rank),
        score: Some(score),
        forced: Some(true),
    });
    add_or_update_recall(
        candidate,
        SearchRecallTrace {
            path: route.mode.into(),
            route_id: Some(route.id.clone()),
            query: Some(route.query.clone()),
            found: true,
            forced: Some(true),
            rank: Some(rank),
            score: Some(score),
            reason: None,
        },
    );
}

#[cfg(test)]
#[path = "../../tests/query_recall.rs"]
mod tests;