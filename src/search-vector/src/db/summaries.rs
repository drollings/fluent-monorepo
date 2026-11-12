//! Generated summary storage: a derived table plus its own lexical index.
//!
//! Summaries are generated commentary, never source: they live in a
//! separate table and FTS index so source citations stay source-only.
//! `fragments_fts` remains source-only; `fragment_summaries_fts` is the
//! only model-text lexical index, joined back to `fragments` by id.

use super::fragments::ScoredFragment;
use super::VectorDbError;

/// One generated summary projected for retrieval.
#[derive(Debug, Clone, PartialEq)]
/// One generated summary projected for retrieval.
pub struct SummaryRecord {
    /// Owning fragment id (joins back to `fragments`).
    pub fragment_id: String,
    /// Blake3 of the normalized comment text summarized.
    pub comment_hash: String,
    /// Model identity that produced the text.
    pub model: String,
    /// Prompt version that produced the text.
    pub prompt_version: i64,
    /// Sampling temperature used.
    pub temperature: f64,
    /// Fixed seed used.
    pub seed: i64,
    /// Generated summary text.
    pub summary: String,
    /// Derivation status (`ready` | `failed`).
    pub status: String,
    /// Generation time (ms epoch).
    pub generated_at: i64,
    /// Whether the summary route is authorized to act on this row.
    pub authorized: bool,
}

impl SummaryRecord {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            fragment_id: row.get(0)?,
            comment_hash: row.get(1)?,
            model: row.get(2)?,
            prompt_version: row.get(3)?,
            temperature: row.get(4)?,
            seed: row.get(5)?,
            summary: row.get(6)?,
            status: row.get(7)?,
            generated_at: row.get(8)?,
            authorized: row.get::<_, i64>(9)? != 0,
        })
    }
}

/// Build the summary FTS `MATCH` expression: ASCII tokens hit the summary
/// column. Returns `None` when nothing is indexable — callers return
/// empty (never match-all).
#[must_use]
/// Build the summary FTS `MATCH` expression: ASCII tokens hit the summary
/// column. Returns `None` when nothing is indexable — callers return
/// empty (never match-all).
pub fn build_summary_fts_match(query: &str) -> Option<String> {
    use common_core::string::{fts_match_tokens, fts_quote};
    let ascii = fts_match_tokens(query);
    if ascii.is_empty() {
        return None;
    }
    let or: Vec<String> = ascii.iter().map(|t| fts_quote(t)).collect();
    Some(format!("{{summary}} : ({})", or.join(" OR ")))
}

impl super::GuidanceDb {
    /// Insert or replace summary rows without touching fragments.
    pub fn replace_summaries(&self, rows: &[SummaryRecord]) -> Result<usize, VectorDbError> {
        self.store
            .transaction(|tx| {
                for row in rows {
                    fluent_db::query::execute(
                        tx,
                        "INSERT OR REPLACE INTO fragment_summaries
                         (fragment_id, comment_hash, model, prompt_version, temperature,
                          seed, summary, status, generated_at, authorized)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        rusqlite::params![
                            row.fragment_id,
                            row.comment_hash,
                            row.model,
                            row.prompt_version,
                            row.temperature,
                            row.seed,
                            row.summary,
                            row.status,
                            row.generated_at,
                            i64::from(row.authorized),
                        ],
                    )?;
                }
                Ok(rows.len())
            })
            .map_err(VectorDbError::from)
    }

    /// Summary rows for one fragment id.
    pub fn summaries_for_fragment(
        &self,
        fragment_id: &str,
    ) -> Result<Vec<SummaryRecord>, VectorDbError> {
        self.store
            .query_rows(
                "SELECT fragment_id, comment_hash, model, prompt_version, temperature,
                        seed, summary, status, generated_at, authorized
                 FROM fragment_summaries WHERE fragment_id = ?1",
                &[&fragment_id],
                SummaryRecord::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// Ranked lexical recall over the summary index. Scores are `-bm25`
    /// (higher is better). Only `ready` rows participate: failed or
    /// unavailable summaries never fire, however they read. Summaries
    /// without a fragment row stay invisible. An unindexable query yields
    /// no rows (never match-all).
    pub fn search_summaries(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ScoredFragment>, VectorDbError> {
        let Some(match_expr) = build_summary_fts_match(query) else {
            return Ok(Vec::new());
        };
        let sql = String::from(
            "SELECT s.fragment_id, f.file_id, bm25(fragment_summaries_fts) AS r
             FROM fragment_summaries_fts
             JOIN fragment_summaries s ON s.rowid = fragment_summaries_fts.rowid
             JOIN fragments f ON f.id = s.fragment_id
             WHERE fragment_summaries_fts MATCH ?1 AND s.status = 'ready'
             ORDER BY r ASC LIMIT ?2",
        );
        self.store
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(
                    conn,
                    &sql,
                    vec![
                        rusqlite::types::Value::Text(match_expr),
                        rusqlite::types::Value::Integer(limit as i64),
                    ],
                    |row| {
                        let rank_value: f64 = row.get(2)?;
                        Ok(ScoredFragment {
                            id: row.get(0)?,
                            file_id: row.get(1)?,
                            score: -rank_value,
                        })
                    },
                )
            })
            .map_err(VectorDbError::from)
    }
}

#[cfg(test)]
#[path = "../../tests/summaries.rs"]
mod tests;