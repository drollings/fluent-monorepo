//! Vector backend: single-route embedding recall over the fragment index.
//! Declines without storage or embedder (L3 degradation).

use crate::query::hybrid::stages_from_hits;
use crate::query::recall::{run_recall, RecallStorage};
use crate::query::search_backend::{SearchBackend, SearchContext};
use crate::query::strategy::QueryIntent;
use crate::query::synthesize::Stage;
use crate::query_engine::QueryEngineError;
use crate::search_types::{SearchPlan, SearchPlanRoute, SearchPlanRouteMode};
use fluent_types::GuidanceDoc;

/// Vector recall backend over `RecallStorage` + an embedding provider.
pub struct VectorBackend<'a> {
    /// Fragment index.
    pub storage: &'a dyn RecallStorage,
    /// Query embedder (`None` degrades L3 away).
    pub embedder: Option<&'a dyn fluent_llm::embeddings::EmbeddingProvider>,
    /// Result limit.
    pub limit: usize,
}

impl SearchBackend for VectorBackend<'_> {
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
                mode: SearchPlanRouteMode::Vector,
                query: query.to_string(),
            }],
            limit: Some(self.limit),
            trace: true,
            ..Default::default()
        };
        let output = run_recall(&plan, self.storage, self.embedder, None)
            .map_err(|error| QueryEngineError::Db(error.to_string()))?;
        if output.hits.is_empty() {
            return Err(QueryEngineError::NoResults);
        }
        Ok(stages_from_hits(&output.hits))
    }
}

#[cfg(test)]
#[path = "../../tests/query_vector_backend.rs"]
mod tests;
