//! Search-plan validation: closed-enum modes, trimmed queries,
//! normalized filters, mtime range.
//!
//! Moved verbatim from `query/recall.rs` (composability split): `RecallError`
//! (whole enum — splitting variants across modules is worse), the `codes`
//! module, and the pure plan helpers. The run loop in `recall.rs` composes
//! these; `db_storage.rs` takes `RecallError` from here.

use thiserror::Error;

use crate::query::glob::normalize_path_pattern;
use crate::search_types::{
    FragmentError, ResolvedSearchPlan, ResolvedSearchPlanRoute, SearchPlan, SearchPlanRouteMode,
};

/// Recall failure: plan validation, storage, or embedding.
#[derive(Debug, Error)]
/// Recall failure: plan validation, storage, or embedding.
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
    UnknownFileType(#[from] crate::query::glob::GlobError),
    /// Storage failure.
    #[error(transparent)]
    Storage(#[from] FragmentError),
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

/// Engine error codes for trace diagnostics.
pub mod codes {
    /// `GUIDANCE.ENGINE.SEARCH_PLAN.EMPTY_ROUTES`.
    pub const EMPTY_ROUTES: &str = "GUIDANCE.ENGINE.SEARCH_PLAN.EMPTY_ROUTES";
    /// `GUIDANCE.ENGINE.SEARCH_PLAN.EMPTY_ROUTE_QUERY`.
    pub const EMPTY_ROUTE_QUERY: &str = "GUIDANCE.ENGINE.SEARCH_PLAN.EMPTY_ROUTE_QUERY";
    /// `GUIDANCE.ENGINE.SEARCH_PLAN.INVALID_MODIFIED_TIME_RANGE`.
    pub const INVALID_MODIFIED_TIME_RANGE: &str =
        "GUIDANCE.ENGINE.SEARCH_PLAN.INVALID_MODIFIED_TIME_RANGE";
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
