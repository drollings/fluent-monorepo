//! Generated-answer cache: one row per evidence manifest hash.
//!
//! The cache key is the evidence manifest (source, line range,
//! content, model, prompt version): identical evidence yields
//! identical bytes without paying the model twice. A hit is
//! reproducibility, never correctness — the support verdict travels
//! with the answer, and changed source always misses by construction.

use super::VectorDbError;

/// One cached generative answer, keyed by evidence manifest hash.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One cached generative answer, keyed by evidence manifest hash.
pub struct AnswerCacheRecord {
    /// Evidence manifest hash (primary key).
    pub manifest_hash: String,
    /// Rendered answer body.
    pub answer: String,
    /// JSON-encoded bound claims.
    pub claims_json: String,
    /// JSON-encoded citations.
    pub citations_json: String,
    /// Support status stored with the answer.
    pub support: String,
    /// Generative model identity.
    pub model: String,
    /// Prompt version that produced the answer.
    pub prompt_version: i64,
    /// Index generation the answer was computed over. A different
    /// generation invalidates the row before it is served.
    pub generation: String,
    /// Store time (ms epoch).
    pub created_at: i64,
}

impl AnswerCacheRecord {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            manifest_hash: row.get(0)?,
            answer: row.get(1)?,
            claims_json: row.get(2)?,
            citations_json: row.get(3)?,
            support: row.get(4)?,
            model: row.get(5)?,
            prompt_version: row.get(6)?,
            generation: row.get(7)?,
            created_at: row.get(8)?,
        })
    }
}

impl super::GuidanceSearchDb {
    /// Look up one cached answer by manifest hash. `None` reads
    /// through to generation; a hit never invents correctness.
    pub fn answer_cache_get(
        &self,
        manifest_hash: &str,
        generation: &str,
    ) -> Result<Option<AnswerCacheRecord>, VectorDbError> {
        let row = self
            .docs
            .sqlite_store()
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(
                    conn,
                    "SELECT manifest_hash, answer, claims_json, citations_json,
                            support, model, prompt_version, generation, created_at
                     FROM answer_cache WHERE manifest_hash = ?1",
                    vec![rusqlite::types::Value::Text(manifest_hash.to_string())],
                    AnswerCacheRecord::from_row,
                )
            })
            .map_err(VectorDbError::from)?
            .pop();
        match row {
            Some(row) if row.generation != generation => {
                // The index moved under the answer: drop the row and
                // read through, never serve a mixed pair.
                self.answer_cache_invalidate(manifest_hash)?;
                Ok(None)
            }
            row => Ok(row),
        }
    }

    /// Store one generated answer. Only fully bound answers with a
    /// support status reach here; the caller enforces that.
    pub fn answer_cache_put(&self, row: &AnswerCacheRecord) -> Result<(), VectorDbError> {
        self.docs
            .sqlite_store()
            .transaction(|tx| {
                fluent_db::query::execute(
                    tx,
                    "INSERT OR REPLACE INTO answer_cache
                     (manifest_hash, answer, claims_json, citations_json,
                      support, model, prompt_version, generation, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    rusqlite::params![
                        row.manifest_hash,
                        row.answer,
                        row.claims_json,
                        row.citations_json,
                        row.support,
                        row.model,
                        row.prompt_version,
                        row.generation,
                        row.created_at,
                    ],
                )?;
                Ok(())
            })
            .map_err(VectorDbError::from)
    }

    /// Drop one cached answer by manifest hash. Returns rows removed.
    pub fn answer_cache_invalidate(&self, manifest_hash: &str) -> Result<usize, VectorDbError> {
        self.docs
            .sqlite_store()
            .transaction(|tx| {
                let removed = fluent_db::query::execute(
                    tx,
                    "DELETE FROM answer_cache WHERE manifest_hash = ?1",
                    rusqlite::params![manifest_hash],
                )?;
                Ok(removed)
            })
            .map_err(VectorDbError::from)
    }
}
