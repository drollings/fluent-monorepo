//! Capability brief storage: one derived row per doc chunk plus its
//! own lexical index and lemma postings.
//!
//! Briefs are generated commentary, never source: they live in a
//! separate table and FTS index so source citations stay source-only.
//! `fragments_fts` remains source-only; `capabilities_fts` is the only
//! brief-text lexical index, joined back to `capabilities` by rowid.

use super::VectorDbError;

/// One stored capability brief: a normalized doc chunk plus the
/// model-written purpose and its SVO normalization.
#[derive(Debug, Clone, PartialEq)]
/// One stored capability brief: a normalized doc chunk plus the
/// model-written purpose and its SVO normalization.
pub struct CapabilityRecord {
    /// Stable chunk id (`source_path#anchor`).
    pub id: String,
    /// Workspace identity (root directory name).
    pub project: String,
    /// Owning crate from the path decomposition (`unknown` fallback).
    pub crate_name: String,
    /// Doc domain (first `doc/` segment, crate name, or `meta`).
    pub domain: String,
    /// Model-written purpose brief.
    pub purpose: String,
    /// SVO predicate lemma (empty when the brief yields none).
    pub predicate: String,
    /// JSON array of SVO role lemmas.
    pub roles_json: String,
    /// Workspace-relative doc path.
    pub source_path: String,
    /// Section anchor within the doc.
    pub anchor: String,
    /// Blake3 of the chunk text (incremental cache key input).
    pub content_hash: String,
    /// Model identity that produced the brief.
    pub model: String,
    /// Prompt version that produced the brief.
    pub prompt_version: i64,
    /// Derivation status (`ready` | `held` | `gap`).
    pub status: String,
    /// Failure or hold reason (`None` for ready rows).
    pub gap_reason: Option<String>,
    /// Generation time (ms epoch).
    pub generated_at: i64,
}

impl CapabilityRecord {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            project: row.get(1)?,
            crate_name: row.get(2)?,
            domain: row.get(3)?,
            purpose: row.get(4)?,
            predicate: row.get(5)?,
            roles_json: row.get(6)?,
            source_path: row.get(7)?,
            anchor: row.get(8)?,
            content_hash: row.get(9)?,
            model: row.get(10)?,
            prompt_version: row.get(11)?,
            status: row.get(12)?,
            gap_reason: row.get(13)?,
            generated_at: row.get(14)?,
        })
    }
}

/// One ranked capability hit (rank assignment stays with the recall loop).
#[derive(Debug, Clone, PartialEq)]
/// One ranked capability hit (rank assignment stays with the recall loop).
pub struct ScoredCapability {
    /// Capability id.
    pub id: String,
    /// Raw score (higher is better; scale varies by axis).
    pub score: f64,
}

/// Lemma row for the `capability_lemmas` L2 table.
#[derive(Debug, Clone, PartialEq)]
/// Lemma row for the `capability_lemmas` L2 table.
pub struct CapabilityLemma {
    /// Owning capability id.
    pub capability_id: String,
    /// Lemma string.
    pub lemma: String,
    /// Parse confidence.
    pub confidence: f64,
}

/// Build the capability FTS `MATCH` expression: ASCII tokens hit the
/// purpose and predicate columns. Returns `None` when nothing is
/// indexable — callers return empty (never match-all).
#[must_use]
/// Build the capability FTS `MATCH` expression: ASCII tokens hit the
/// purpose and predicate columns. Returns `None` when nothing is
/// indexable — callers return empty (never match-all).
pub fn build_capability_fts_match(query: &str) -> Option<String> {
    use common_core::string::{fts_match_tokens, fts_quote};
    let ascii = fts_match_tokens(query);
    if ascii.is_empty() {
        return None;
    }
    let or: Vec<String> = ascii.iter().map(|t| fts_quote(t)).collect();
    Some(format!("{{purpose predicate}} : ({})", or.join(" OR ")))
}

const CAPABILITY_COLUMNS: &str = "id, project, crate_name, domain, purpose, predicate,
        roles_json, source_path, anchor, content_hash, model, prompt_version,
        status, gap_reason, generated_at";

impl super::GuidanceDb {
    /// Insert or replace capability rows without touching fragments.
    pub fn replace_capabilities(&self, rows: &[CapabilityRecord]) -> Result<usize, VectorDbError> {
        self.store
            .transaction(|tx| {
                for row in rows {
                    fluent_db::query::execute(
                        tx,
                        "INSERT OR REPLACE INTO capabilities
                         (id, project, crate_name, domain, purpose, predicate,
                          roles_json, source_path, anchor, content_hash, model,
                          prompt_version, status, gap_reason, generated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                        rusqlite::params![
                            row.id,
                            row.project,
                            row.crate_name,
                            row.domain,
                            row.purpose,
                            row.predicate,
                            row.roles_json,
                            row.source_path,
                            row.anchor,
                            row.content_hash,
                            row.model,
                            row.prompt_version,
                            row.status,
                            row.gap_reason,
                            row.generated_at,
                        ],
                    )?;
                }
                Ok(rows.len())
            })
            .map_err(VectorDbError::from)
    }

    /// Capability rows for one source doc (the incremental cache check).
    pub fn capabilities_for_source(
        &self,
        source_path: &str,
    ) -> Result<Vec<CapabilityRecord>, VectorDbError> {
        self.store
            .query_rows(
                &format!("SELECT {CAPABILITY_COLUMNS} FROM capabilities WHERE source_path = ?1"),
                &[&source_path],
                CapabilityRecord::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// Capability rows for explicit ids (the branch-1 candidate
    /// fetch). Only `ready` rows participate: held or gap rows never
    /// fire, however they read.
    pub fn capabilities_for_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<CapabilityRecord>, VectorDbError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT {CAPABILITY_COLUMNS} FROM capabilities
             WHERE id IN ({placeholders}) AND status = 'ready'"
        );
        let values: Vec<rusqlite::types::Value> = ids
            .iter()
            .map(|id| rusqlite::types::Value::Text(id.clone()))
            .collect();
        self.store
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(
                    conn,
                    &sql,
                    values,
                    CapabilityRecord::from_row,
                )
            })
            .map_err(VectorDbError::from)
    }

    /// Ranked lexical recall over the brief index. Scores are `-bm25`
    /// (higher is better). Only `ready` rows participate: held or gap
    /// rows never fire, however they read. An unindexable query yields
    /// no rows (never match-all).
    pub fn search_capabilities(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ScoredCapability>, VectorDbError> {
        let Some(match_expr) = build_capability_fts_match(query) else {
            return Ok(Vec::new());
        };
        let sql = String::from(
            "SELECT c.id, bm25(capabilities_fts) AS r
             FROM capabilities_fts
             JOIN capabilities c ON c.rowid = capabilities_fts.rowid
             WHERE capabilities_fts MATCH ?1 AND c.status = 'ready'
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
                        let rank_value: f64 = row.get(1)?;
                        Ok(ScoredCapability {
                            id: row.get(0)?,
                            score: -rank_value,
                        })
                    },
                )
            })
            .map_err(VectorDbError::from)
    }

    /// Insert or replace lemma rows without touching capabilities.
    pub fn insert_capability_lemmas(
        &self,
        lemmas: &[CapabilityLemma],
    ) -> Result<usize, VectorDbError> {
        self.store
            .transaction(|tx| {
                let mut term_of: std::collections::HashMap<String, i64> =
                    std::collections::HashMap::new();
                let mut insert = tx.prepare_cached(
                    "INSERT OR REPLACE INTO capability_lemmas (capability_rowid, lemma_id, confidence)
                     VALUES (?1, ?2, ?3)",
                )?;
                for lemma in lemmas {
                    let capability_rowid: Option<i64> = fluent_db::query::query_row(
                        tx,
                        "SELECT rowid FROM capabilities WHERE id = ?1",
                        rusqlite::params![lemma.capability_id],
                        |row| row.get(0),
                    )?;
                    let Some(capability_rowid) = capability_rowid else {
                        continue;
                    };
                    let lemma_id = Self::lemma_term_id(tx, &mut term_of, &lemma.lemma)?;
                    insert
                        .execute(rusqlite::params![capability_rowid, lemma_id, lemma.confidence])
                        .map_err(fluent_db::error::DbError::from)?;
                }
                Ok(lemmas.len())
            })
            .map_err(VectorDbError::from)
    }

    /// Capability ids carrying any of the given lemmas (stable rowid
    /// order). Capability postings are unique per `(rowid, lemma)`, so
    /// `COUNT(*)` is the number of distinct query lemmas matched.
    pub fn capabilities_for_lemmas(
        &self,
        lemmas: &[String],
        limit: usize,
    ) -> Result<Vec<String>, VectorDbError> {
        if lemmas.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = lemmas.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT c.id, COUNT(*) AS matches FROM capability_lemmas l \
              JOIN lemma_terms d ON d.id = l.lemma_id \
              JOIN capabilities c ON c.rowid = l.capability_rowid \
              WHERE d.lemma IN ({placeholders}) AND c.status = 'ready' \
              GROUP BY c.id ORDER BY matches DESC, c.rowid ASC LIMIT ?"
        );
        let mut values: Vec<rusqlite::types::Value> = lemmas
            .iter()
            .map(|lemma| rusqlite::types::Value::Text(lemma.clone()))
            .collect();
        values.push(rusqlite::types::Value::Integer(limit as i64));
        self.store
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(conn, &sql, values, |row| row.get(0))
            })
            .map_err(VectorDbError::from)
    }
}

#[cfg(test)]
#[path = "../../tests/capabilities.rs"]
mod tests;
