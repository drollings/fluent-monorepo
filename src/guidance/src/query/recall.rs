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
    matches_file_selection, normalize_path_pattern, path_pattern_matches,
    resolve_file_type_patterns, FileTypePatterns,
};
use crate::zg_constants::{
    recall_target_candidate_count, DEFAULT_LIMIT, RECALL_GROWTH_FACTOR, RECALL_INITIAL_DEPTH,
    RECALL_MAX_DEPTH,
};
use crate::zg_types::{
    extract_symbol_names, public_entity_id, CodeSymbolType, Entity, EntityFragment, FileInfo,
    RecallPath, ResolvedSearchPlan, ResolvedSearchPlanRoute, SearchHit, SearchPlan,
    SearchPlanRouteMode, SearchRecallTrace, StorageFilter, StorageHit, TimingEntry, ZgError,
};
use thiserror::Error;

/// Recall failure: plan validation, storage, or embedding.
#[derive(Debug, Error)]
pub enum RecallError {
    /// Plan carries no routes.
    #[error("search plan requires at least one route")]
    EmptyRoutes,
    /// Route query is blank after trimming.
    #[error("search plan route requires a non-empty query: {route_id}")]
    EmptyRouteQuery {
        /// Assigned route id.
        route_id: String,
    },
    /// Negative modification timestamp.
    #[error("search plan modified time must be non-negative: {name}")]
    NegativeModifiedTime {
        /// Filter name.
        name: &'static str,
    },
    /// Modified-after later than modified-before.
    #[error("search plan modified-after must not be later than modified-before")]
    InvalidModifiedTimeRange,
    /// Unknown file-type name.
    #[error(transparent)]
    UnknownFileType(#[from] crate::query::glob::ZgGlobError),
    /// Storage failure.
    #[error(transparent)]
    Storage(#[from] ZgError),
    /// Database failure.
    #[error("database error: {0}")]
    Db(String),
    /// Query-embedding failure.
    #[error("query embedding failed: {0}")]
    Embed(String),
    /// Diagnosis target is not indexed.
    #[error("entity not found: {0}")]
    EntityNotFound(String),
}

/// Engine error codes, preserved verbatim for trace parity.
pub mod codes {
    /// `ZVEC_GREP.ENGINE.SEARCH_PLAN.EMPTY_ROUTES`.
    pub const EMPTY_ROUTES: &str = "ZVEC_GREP.ENGINE.SEARCH_PLAN.EMPTY_ROUTES";
    /// `ZVEC_GREP.ENGINE.SEARCH_PLAN.EMPTY_ROUTE_QUERY`.
    pub const EMPTY_ROUTE_QUERY: &str = "ZVEC_GREP.ENGINE.SEARCH_PLAN.EMPTY_ROUTE_QUERY";
    /// `ZVEC_GREP.ENGINE.SEARCH_PLAN.INVALID_MODIFIED_TIME_RANGE`.
    pub const INVALID_MODIFIED_TIME_RANGE: &str =
        "ZVEC_GREP.ENGINE.SEARCH_PLAN.INVALID_MODIFIED_TIME_RANGE";
}

impl RecallError {
    /// Machine-readable plan code, if this is a plan validation failure.
    #[must_use]
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::EmptyRoutes => Some(codes::EMPTY_ROUTES),
            Self::EmptyRouteQuery { .. } => Some(codes::EMPTY_ROUTE_QUERY),
            Self::InvalidModifiedTimeRange => Some(codes::INVALID_MODIFIED_TIME_RANGE),
            _ => None,
        }
    }
}

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
    /// Entity + file lookup by public entity id.
    fn get_entity(&self, entity_id: &str) -> Result<Option<(Entity, FileInfo)>, RecallError>;
    /// Entities + files for one file id (diagnosis fallback).
    fn list_entities_by_file(&self, file_id: &str) -> Result<Vec<(Entity, FileInfo)>, RecallError>;
}

/// Validate a search plan: trimmed queries, per-mode route ids
/// (`fts`, `fts-2`, `vector`, `vector-2`), normalized filters, time range.
/// Source: `validateSearchPlan` (unsupported modes are rejected by the
/// `SearchPlanRouteMode` closed enum at parse time — the type system
/// absorbed that check).
pub fn validate_search_plan(plan: &SearchPlan) -> Result<ResolvedSearchPlan, RecallError> {
    if plan.routes.is_empty() {
        return Err(RecallError::EmptyRoutes);
    }
    let mut fts_count = 0_usize;
    let mut vector_count = 0_usize;
    let mut routes = Vec::with_capacity(plan.routes.len());
    for route in &plan.routes {
        let query = route.query.trim().to_string();
        let id = match route.mode {
            SearchPlanRouteMode::Fts => {
                fts_count += 1;
                if fts_count == 1 {
                    "fts".to_string()
                } else {
                    format!("fts-{fts_count}")
                }
            }
            SearchPlanRouteMode::Vector => {
                vector_count += 1;
                if vector_count == 1 {
                    "vector".to_string()
                } else {
                    format!("vector-{vector_count}")
                }
            }
        };
        if query.is_empty() {
            return Err(RecallError::EmptyRouteQuery { route_id: id });
        }
        routes.push(ResolvedSearchPlanRoute {
            id,
            mode: route.mode,
            query,
        });
    }
    let modified_after = check_modified_time(plan.modified_after, "modifiedAfter")?;
    let modified_before = check_modified_time(plan.modified_before, "modifiedBefore")?;
    if let (Some(after), Some(before)) = (modified_after, modified_before) {
        if after > before {
            return Err(RecallError::InvalidModifiedTimeRange);
        }
    }
    Ok(ResolvedSearchPlan {
        routes,
        limit: plan.limit,
        trace: plan.trace,
        track_entity_id: plan.track_entity_id.clone(),
        prefer_symbol: plan.prefer_symbol,
        symbol_types: plan.symbol_types.clone(),
        include_paths: normalize_path_filters(plan.include_paths.as_deref()),
        exclude_paths: normalize_path_filters(plan.exclude_paths.as_deref()),
        globs: normalize_string_filters(plan.globs.as_deref()),
        insensitive_globs: normalize_string_filters(plan.insensitive_globs.as_deref()),
        file_types: normalize_string_filters(plan.file_types.as_deref()),
        excluded_file_types: normalize_string_filters(plan.excluded_file_types.as_deref()),
        modified_after,
        modified_before,
    })
}

fn check_modified_time(value: Option<i64>, name: &'static str) -> Result<Option<i64>, RecallError> {
    match value {
        Some(time) if time < 0 => Err(RecallError::NegativeModifiedTime { name }),
        other => Ok(other),
    }
}

fn normalize_path_filters(patterns: Option<&[String]>) -> Option<Vec<String>> {
    patterns.map(|list| {
        list.iter()
            .map(|pattern| normalize_path_pattern(pattern))
            .collect()
    })
}

fn normalize_string_filters(patterns: Option<&[String]>) -> Option<Vec<String>> {
    patterns.map(|list| {
        list.iter()
            .map(|pattern| pattern.trim().to_string())
            .filter(|pattern| !pattern.is_empty())
            .collect()
    })
}

/// Resolve the storage file allow-list: include matchers, exclude matchers,
/// glob/type selection, mtime window. `None` = no path selection.
/// Source: `resolveFilteredFileIds`.
#[must_use]
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

/// Run the hybrid recall pipeline end to end. Source: `searchWorkspaceIndex`.
/// The lemma route (P1 L2) always participates; it yields nothing when
/// normalization is empty.
pub fn run_recall(
    plan: &SearchPlan,
    storage: &dyn RecallStorage,
    embedder: Option<&dyn fluent_llm::embeddings::EmbeddingProvider>,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
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
) -> Result<usize, RecallError> {
    let routes = build_recall_routes(plan, filter, nlp);
    let target = recall_target_candidate_count(limit);
    let mut previous_depth = 0;
    let mut depth = RECALL_INITIAL_DEPTH;
    loop {
        let mut saturated = false;
        for route in &routes {
            let hits = recall_route_hits(route, vector_by_route, depth, storage)?;
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

fn build_recall_routes(
    plan: &ResolvedSearchPlan,
    filter: &StorageFilter,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
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
            query: lemma_query,
            filter: filter.clone(),
            vector_route_id: None,
            lemmas,
        });
    }
    if plan.prefer_symbol != Some(true) {
        return output;
    }
    let mut seen = HashSet::new();
    for route in &plan.routes {
        let symbol_names = extract_symbol_names(&route.query);
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
                    score: 0.0,
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
            score: 0.0,
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

/// Entity-search diagnosis: the stored entity + file plus a traced,
/// force-tracked search. Source: `diagnoseEntitySearch`.
#[derive(Debug)]
pub struct EntityDiagnosis {
    /// Diagnosed entity.
    pub entity: Entity,
    /// Owning file.
    pub file: FileInfo,
    /// Traced search with the entity force-tracked.
    pub search: RecallOutput,
}

/// Diagnose why an entity does or does not surface for a query.
pub fn diagnose_entity(
    query: &str,
    entity_id: &str,
    storage: &dyn RecallStorage,
    embedder: Option<&dyn fluent_llm::embeddings::EmbeddingProvider>,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> Result<EntityDiagnosis, RecallError> {
    let Some((entity, file)) = storage.get_entity(entity_id)? else {
        return Err(RecallError::EntityNotFound(entity_id.to_string()));
    };
    let plan = SearchPlan {
        routes: vec![
            crate::zg_types::SearchPlanRoute {
                mode: SearchPlanRouteMode::Fts,
                query: query.to_string(),
            },
            crate::zg_types::SearchPlanRoute {
                mode: SearchPlanRouteMode::Vector,
                query: query.to_string(),
            },
        ],
        trace: true,
        track_entity_id: Some(entity_id.to_string()),
        ..Default::default()
    };
    let search = run_recall(&plan, storage, embedder, nlp)?;
    Ok(EntityDiagnosis {
        entity,
        file,
        search,
    })
}

/// File-search diagnosis: resolve the file by absolute path, run the query,
/// and take the first hit's entity — falling back to the file's first
/// indexed entity when recall is empty. Source: `diagnoseFileSearch`.
pub fn diagnose_file(
    query: &str,
    absolute_path: &str,
    storage: &dyn RecallStorage,
    embedder: Option<&dyn fluent_llm::embeddings::EmbeddingProvider>,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> Result<Option<EntityDiagnosis>, RecallError> {
    let files = storage.list_files()?;
    let Some(file) = files
        .iter()
        .find(|file| file.absolute_path == absolute_path)
    else {
        return Ok(None);
    };
    let search = run_recall(
        &SearchPlan {
            routes: vec![
                crate::zg_types::SearchPlanRoute {
                    mode: SearchPlanRouteMode::Fts,
                    query: query.to_string(),
                },
                crate::zg_types::SearchPlanRoute {
                    mode: SearchPlanRouteMode::Vector,
                    query: query.to_string(),
                },
            ],
            ..Default::default()
        },
        storage,
        embedder,
        nlp,
    )?;
    if let Some(hit) = search.hits.first() {
        return Ok(Some(EntityDiagnosis {
            entity: hit.entity.clone(),
            file: hit.file.clone(),
            search,
        }));
    }
    let mut entities = storage.list_entities_by_file(&file.id)?;
    if entities.is_empty() {
        return Ok(None);
    }
    let (entity, file) = entities.remove(0);
    Ok(Some(EntityDiagnosis {
        entity,
        file,
        search,
    }))
}

#[cfg(test)]
#[path = "../../tests/query_recall.rs"]
mod tests;
