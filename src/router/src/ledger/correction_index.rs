//! The router's [`CorrectionIndex`] over the `interlingua_index` table.
//!
//! `interlingua_index` is the router's durable **correction index** *and* the
//! audit of which ids were attached to which parse node. Pattern-cache
//! rows (the `(lemma_id, entity_id) → corrections` map) use the sentinel
//! [`PATTERN_NODE`] node id and `role = "correction"` so they never collide
//! with real parse-node audit rows (which carry a real `node_id`).

use std::sync::Arc;

use fluent_db::error::DbError;
use fluent_db::store::SqliteStore;
use fluent_types::InterlinguaId;
use rusqlite::params;
use spacy_rs::review::Correction;
use spacy_rs::CorrectionIndex;

/// Sentinel node id for pattern-cache rows (not a real parse node).
pub const PATTERN_NODE: i64 = 0;
/// Role discriminating correction-cache rows from per-node audit rows.
const CORRECTION_ROLE: &str = "correction";

/// One correction-pattern cache row. `entity_id` is `0` when
/// the pattern is not entity-scoped. Used by the atomic review write
/// (`ContentNodeStore::apply_review`) so the correction + `review_status` +
/// `parse_review` node commit in one SQLite transaction (§12.6).
#[derive(Debug, Clone)]
/// One correction-pattern cache row. `entity_id` is `0` when
/// the pattern is not entity-scoped. Used by the atomic review write
/// (`ContentNodeStore::apply_review`) so the correction + `review_status` +
/// `parse_review` node commit in one SQLite transaction (§12.6).
pub(crate) struct CorrectionRow {
    pub lemma_id: i64,
    pub entity_id: i64,
    pub corrections_json: String,
}

/// Upsert a pattern-cache row into `interlingua_index`. M8.2: the single
/// shared skeleton for the three pattern-cache writers (correction rows,
/// sense rows, span-cache rows) — identical SQL text, differing only in
/// bound values (`role` discriminates the caches; `span_key` is `''` except
/// for span-cache rows). One place keeps the conflict target and update set
/// from drifting across writers.
pub(crate) fn upsert_pattern_cache_row(
    conn: &rusqlite::Connection,
    node_id: i64,
    interlingua_id: i64,
    role: &str,
    entity_id: i64,
    payload_json: &str,
    span_key: &str,
) -> Result<(), DbError> {
    fluent_db::query::execute(
        conn,
        "INSERT INTO interlingua_index \
         (node_id, interlingua_id, interlingua_source, role, entity_id, review_status, corrections, span_key) \
         VALUES (?1, ?2, 'spacy_lemma', ?3, ?4, 'cached', ?5, ?6) \
         ON CONFLICT(node_id, interlingua_id, span_key, role, entity_id) DO UPDATE SET \
            review_status = excluded.review_status, corrections = excluded.corrections",
        params![
            node_id,
            interlingua_id,
            role,
            entity_id,
            payload_json,
            span_key,
        ],
    )?;
    Ok(())
}

/// Upsert a correction-pattern row into `interlingua_index`. The single SQL
/// for the pattern cache — shared by [`SqliteCorrectionIndex`] and the
/// ledger's atomic review transaction so the row shape lives in one place.
/// The cache keys on the real `entity_id` column (0 = not entity-scoped);
/// `review_status` is status-only (`'cached'`).
pub(crate) fn upsert_correction_row(
    conn: &rusqlite::Connection,
    row: &CorrectionRow,
) -> Result<(), DbError> {
    upsert_pattern_cache_row(
        conn,
        PATTERN_NODE,
        row.lemma_id,
        CORRECTION_ROLE,
        row.entity_id,
        &row.corrections_json,
        "",
    )
}

/// `CorrectionIndex` over `interlingua_index` (the shared ledger connection).
pub struct SqliteCorrectionIndex {
    store: Arc<SqliteStore>,
}

impl SqliteCorrectionIndex {
    /// A correction index over the shared connection.
    pub fn new(store: Arc<SqliteStore>) -> Self {
        Self { store }
    }
}

impl CorrectionIndex for SqliteCorrectionIndex {
    fn query_previous_corrections(
        &self,
        lemma_id: InterlinguaId,
        entity_id: Option<InterlinguaId>,
    ) -> Option<Vec<Correction>> {
        let entity = entity_id.unwrap_or(InterlinguaId::from_u64(0));
        let row = self
            .store
            .query_row(
                "SELECT corrections FROM interlingua_index \
                 WHERE node_id = ?1 AND interlingua_id = ?2 AND role = ?3 AND entity_id = ?4",
                params![
                    PATTERN_NODE,
                    lemma_id.as_i64(),
                    CORRECTION_ROLE,
                    entity.as_i64(),
                ],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()
            .flatten()?;
        serde_json::from_str(&row).ok()
    }

    fn record_correction(
        &self,
        lemma_id: InterlinguaId,
        entity_id: Option<InterlinguaId>,
        corrections: &[Correction],
    ) -> Result<(), fluent_concept::ConceptStoreError> {
        let entity = entity_id.unwrap_or(InterlinguaId::from_u64(0));
        let json = serde_json::to_string(corrections)
            .map_err(|e| fluent_concept::ConceptStoreError::Storage(e.to_string()))?;
        let row = CorrectionRow {
            lemma_id: lemma_id.as_i64(),
            entity_id: entity.as_i64(),
            corrections_json: json,
        };
        self.store
            .with_conn(|conn| upsert_correction_row(conn, &row))
            .map_err(|e| fluent_concept::ConceptStoreError::Storage(e.to_string()))?;
        Ok(())
    }
}
#[cfg(test)]
#[path = "../../tests/ledger_correction_index.rs"]
mod tests;