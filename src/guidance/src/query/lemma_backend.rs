//! Lemma backend: query lemmas looked up in `fragment_lemmas` (P1 L2).
//! Normalization is spacy rule lemmas when a pipeline is wired, lowercase
//! tokens otherwise — L2 works with no model either way.

use crate::query::hybrid::stage_from_storage_hit;
use crate::query::ingest::query_lemmas;
use crate::query::recall::RecallStorage;
use crate::query::search_backend::{SearchBackend, SearchContext};
use crate::query::strategy::QueryIntent;
use crate::query::synthesize::Stage;
use crate::query_engine::QueryEngineError;
use crate::zg_types::StorageFilter;
use fluent_types::GuidanceDoc;

/// Lemma recall backend over `RecallStorage`.
pub struct LemmaBackend<'a> {
    /// Fragment index.
    pub storage: &'a dyn RecallStorage,
    /// Query lemmatizer (`None` falls back to token normalization).
    pub nlp: Option<&'a spacy_rs::pipeline::NlpPipeline>,
    /// Result limit.
    pub limit: usize,
}

impl SearchBackend for LemmaBackend<'_> {
    fn matches(&self, _intent: QueryIntent) -> bool {
        true
    }

    fn search(
        &self,
        query: &str,
        _doc: &GuidanceDoc,
        _ctx: &SearchContext<'_>,
    ) -> Result<Vec<Stage>, QueryEngineError> {
        let lemmas = query_lemmas(self.nlp, query);
        if lemmas.is_empty() {
            return Err(QueryEngineError::NoResults);
        }
        let hits = self
            .storage
            .search_lemmas(&lemmas, self.limit, &StorageFilter::default())
            .map_err(|error| QueryEngineError::Db(error.to_string()))?;
        if hits.is_empty() {
            return Err(QueryEngineError::NoResults);
        }
        Ok(hits.iter().map(stage_from_storage_hit).collect())
    }
}

#[cfg(test)]
#[path = "../../tests/query_lemma_backend.rs"]
mod tests;
