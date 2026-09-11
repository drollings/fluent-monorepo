//! Lexical backend: single-route FTS recall over the fragment index.
//! Declines (`NoResults`) without storage so legacy doc backends serve.

use crate::query::hybrid::stages_from_hits;
use crate::query::recall::{run_recall, RecallStorage};
use crate::query::search_backend::{SearchBackend, SearchContext};
use crate::query::strategy::QueryIntent;
use crate::query::synthesize::Stage;
use crate::query_engine::QueryEngineError;
use crate::search_types::{SearchPlan, SearchPlanRoute, SearchPlanRouteMode};
use fluent_types::GuidanceDoc;

/// FTS recall backend over `RecallStorage`.
pub struct FtsBackend<'a> {
    /// Fragment index.
    pub storage: &'a dyn RecallStorage,
    /// Result limit.
    pub limit: usize,
}

impl SearchBackend for FtsBackend<'_> {
    fn matches(&self, _intent: QueryIntent) -> bool {
        true
    }

    fn search(
        &self,
        query: &str,
        _doc: &GuidanceDoc,
        _ctx: &SearchContext<'_>,
    ) -> Result<Vec<Stage>, QueryEngineError> {
        let plan = SearchPlan {
            routes: vec![SearchPlanRoute {
                mode: SearchPlanRouteMode::Fts,
                query: query.to_string(),
            }],
            limit: Some(self.limit),
            trace: true,
            ..Default::default()
        };
        let output = run_recall(&plan, self.storage, None, None)
            .map_err(|error| QueryEngineError::Db(error.to_string()))?;
        if output.hits.is_empty() {
            return Err(QueryEngineError::NoResults);
        }
        Ok(stages_from_hits(&output.hits))
    }
}

#[cfg(test)]
#[path = "../../tests/query_fts_backend.rs"]
mod tests;
