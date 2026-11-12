//! The overlay/candidate plane.
//!
//! `overlay_candidates` is the durable surface for async overlay outputs —
//! entity links, PII-shaped spans, parse corrections, concept summaries. It is
//! the **only** surface overlays write: no overlay ever writes
//! `TokenRecord.interlingua_entity_id` or `concept_ids` at runtime (boot-only
//! registration is an invariant). The async entity-link worker (M6.2) and
//! future overlays land candidates here; review surfaces them (M6.5).
//!
//! Conventions mirror `interlingua_index`:
//!
//! - **First-wins** on `(node_id, span_start, kind, entity_id)` — the first
//!   candidate for a given span/kind/entity wins (`INSERT OR IGNORE`), so a
//!   re-run overlay never overwrites an accepted candidate.
//! - **Id-membership**: a non-zero `entity_id` must resolve in
//!   `interlingua_concepts`; [`reconcile_ids`] asserts this (boot
//!   reconciliation).
//!
//! `entity_id` is `0` for non-entity-shaped candidates (the sentinel shared
//! with `interlingua_index.entity_id`).

use std::sync::Arc;

use fluent_db::store::SqliteStore;
use fluent_llm::backend::ResidualKind;
use fluent_types::{InterlinguaId, NodeId};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::ledger::LedgerError;
use crate::node_store::ContentNodeStore;

/// A persisted `overlay_candidates` row projection (node, span, kind,
/// entity, score, source, status) — kept as an ordered tuple for the store's
/// column mapping.
type CandidateRow = (i64, i64, i64, String, i64, Option<f64>, String, String);

/// The lifecycle of an overlay candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// The lifecycle of an overlay candidate.
pub enum CandidateStatus {
    /// Awaiting human review / promotion.
    Pending,
    /// Accepted by human review and promoted (e.g. folded into a correction).
    Promoted,
    /// Rejected by review; kept for the audit trail.
    Dismissed,
}

/// The persisted status string for a [`CandidateStatus`].
#[must_use]
/// The persisted status string for a [`CandidateStatus`].
pub fn status_sql(status: CandidateStatus) -> &'static str {
    match status {
        CandidateStatus::Pending => "pending",
        CandidateStatus::Promoted => "promoted",
        CandidateStatus::Dismissed => "dismissed",
    }
}

/// Parse a persisted status string back into a [`CandidateStatus`]. Unknown
/// values degrade to [`CandidateStatus::Pending`] (fail-open).
#[must_use]
/// Parse a persisted status string back into a [`CandidateStatus`]. Unknown
/// values degrade to [`CandidateStatus::Pending`] (fail-open).
pub fn status_from_sql(s: &str) -> CandidateStatus {
    match s {
        "promoted" => CandidateStatus::Promoted,
        "dismissed" => CandidateStatus::Dismissed,
        _ => CandidateStatus::Pending,
    }
}

/// One overlay candidate row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// One overlay candidate row.
pub struct OverlayCandidate {
    pub node_id: NodeId,
    /// Byte span into the source text.
    pub span_start: usize,
    pub span_end: usize,
    /// The residual kind that produced this candidate.
    pub kind: ResidualKind,
    /// The resolved entity id, when entity-shaped. `None` for PII/parse/
    /// summary candidates.
    pub entity_id: Option<InterlinguaId>,
    /// The overlay's confidence score, when score-shaped.
    pub score: Option<f64>,
    /// Which overlay produced the candidate (e.g. `"entity_link"`).
    pub source: String,
    pub status: CandidateStatus,
}

impl OverlayCandidate {
    /// An entity-link candidate for a PROPN span, pending review.
    #[must_use]
    pub fn entity_link(
        node_id: NodeId,
        span_start: usize,
        span_end: usize,
        entity_id: InterlinguaId,
        score: f64,
        source: impl Into<String>,
    ) -> Self {
        Self {
            node_id,
            span_start,
            span_end,
            kind: ResidualKind::EntityLink,
            entity_id: Some(entity_id),
            score: Some(score),
            source: source.into(),
            status: CandidateStatus::Pending,
        }
    }
}

/// The durable `overlay_candidates` table, over the shared ledger connection.
#[derive(Clone)]
/// The durable `overlay_candidates` table, over the shared ledger connection.
pub struct OverlayCandidateStore {
    store: Arc<SqliteStore>,
}

impl OverlayCandidateStore {
    /// The candidate plane over the ledger's shared connection. `None` for an
    /// ephemeral store (no durable candidate surface).
    #[must_use]
    pub fn new(store: Arc<SqliteStore>) -> Self {
        Self { store }
    }

    /// Write one candidate, **first-wins** on `(node_id, span_start, kind,
    /// entity_id)`. Returns `true` when a new row was inserted, `false` when a
    /// first-wins candidate already exists (the existing row is untouched —
    /// overlays never overwrite an accepted candidate). Fail-open at the call
    /// site: a write error is logged, never a gate.
    pub fn write_candidate(&self, cand: &OverlayCandidate) -> Result<bool, LedgerError> {
        let inserted = self
            .store
            .execute(
                "INSERT OR IGNORE INTO overlay_candidates \
                 (node_id, span_start, span_end, kind, entity_id, score, source, status) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    cand.node_id.as_int(),
                    cand.span_start as i64,
                    cand.span_end as i64,
                    serde_json::to_string(&cand.kind).unwrap_or_default(),
                    cand.entity_id.map_or(0, InterlinguaId::as_i64),
                    cand.score,
                    cand.source,
                    status_sql(cand.status),
                ],
            )
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        Ok(inserted > 0)
    }

    /// Write a batch of candidates (first-wins each). Returns the number of
    /// newly-inserted rows.
    pub fn write_candidates(&self, cands: &[OverlayCandidate]) -> Result<usize, LedgerError> {
        let mut inserted = 0usize;
        for cand in cands {
            if self.write_candidate(cand)? {
                inserted += 1;
            }
        }
        Ok(inserted)
    }

    /// All candidates for a parse node, oldest first.
    pub fn for_node(&self, node_id: NodeId) -> Result<Vec<OverlayCandidate>, LedgerError> {
        let rows: Vec<CandidateRow> = self
            .store
            .query_rows(
                "SELECT node_id, span_start, span_end, kind, entity_id, score, source, status \
                 FROM overlay_candidates WHERE node_id = ?1 ORDER BY span_start, entity_id",
                params![node_id.as_int()],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        Ok(rows.into_iter().map(row_to_candidate).collect())
    }

    /// Pending candidates for a node (the review surface, M6.5).
    pub fn pending_for_node(&self, node_id: NodeId) -> Result<Vec<OverlayCandidate>, LedgerError> {
        Ok(self
            .for_node(node_id)?
            .into_iter()
            .filter(|c| c.status == CandidateStatus::Pending)
            .collect())
    }

    /// Persist an approved stamp, first-wins on `(node_id, token)`.
    /// Returns `true` when a new row was inserted.
    pub fn write_stamp(
        &self,
        node_id: NodeId,
        token: usize,
        entity_id: InterlinguaId,
        text_hash: u64,
    ) -> Result<bool, LedgerError> {
        let inserted = self
            .store
            .execute(
                "INSERT OR IGNORE INTO entity_stamps (node_id, token, entity_id, text_hash) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    node_id.as_int(),
                    token as i64,
                    entity_id.as_i64(),
                    format!("{text_hash:016x}"),
                ],
            )
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        Ok(inserted > 0)
    }

    /// All persisted stamps for a node, in token order.
    pub fn stamps_for_node(&self, node_id: NodeId) -> Result<Vec<EntityStampRow>, LedgerError> {
        let rows: Vec<(i64, i64, String)> = self
            .store
            .query_rows(
                "SELECT token, entity_id, text_hash FROM entity_stamps \
                 WHERE node_id = ?1 ORDER BY token",
                params![node_id.as_int()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        rows.into_iter()
            .map(|(token, entity_id, text_hash)| {
                Ok(EntityStampRow {
                    token: token as usize,
                    entity_id: InterlinguaId::from_u64(entity_id as u64),
                    text_hash: u64::from_str_radix(&text_hash, 16)
                        .map_err(|e| LedgerError::Db(e.to_string()))?,
                })
            })
            .collect()
    }

    /// Delete a stamp row (after `verify_stamps` clears a mutated slot).
    pub fn delete_stamp(&self, node_id: NodeId, token: usize) -> Result<(), LedgerError> {
        self.store
            .execute(
                "DELETE FROM entity_stamps WHERE node_id = ?1 AND token = ?2",
                params![node_id.as_int(), token as i64],
            )
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        Ok(())
    }

    /// Promote a candidate (human review accepted it). First-wins preserved —
    /// only a `Pending` candidate can be promoted, and promotion is idempotent.
    pub fn promote(&self, node_id: NodeId, cand: &OverlayCandidate) -> Result<(), LedgerError> {
        self.set_status(node_id, cand, CandidateStatus::Promoted)
    }

    /// Dismiss a candidate (human review rejected it; kept for the audit).
    pub fn dismiss(&self, node_id: NodeId, cand: &OverlayCandidate) -> Result<(), LedgerError> {
        self.set_status(node_id, cand, CandidateStatus::Dismissed)
    }

    /// Transition a candidate's status. The candidate is keyed by its
    /// `(span_start, kind, entity_id)` (its `status` field is not part of the
    /// key). Only transitions from `Pending` are honored (first-wins).
    fn set_status(
        &self,
        node_id: NodeId,
        cand: &OverlayCandidate,
        status: CandidateStatus,
    ) -> Result<(), LedgerError> {
        let updated = self
            .store
            .execute(
                "UPDATE overlay_candidates SET status = ?1 \
                 WHERE node_id = ?2 AND span_start = ?3 AND kind = ?4 AND entity_id = ?5 \
                   AND status = 'pending'",
                params![
                    status_sql(status),
                    node_id.as_int(),
                    cand.span_start as i64,
                    serde_json::to_string(&cand.kind).unwrap_or_default(),
                    cand.entity_id.map_or(0, InterlinguaId::as_i64),
                ],
            )
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        let _ = updated;
        Ok(())
    }
}

/// One persisted entity stamp: a gate-approved entity id for a token,
/// content-keyed so text mutation invalidates it.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One persisted entity stamp: a gate-approved entity id for a token,
/// content-keyed so text mutation invalidates it.
pub struct EntityStampRow {
    pub token: usize,
    pub entity_id: InterlinguaId,
    pub text_hash: u64,
}

impl OverlayCandidateStore {
    /// Promote every `Pending` `EntityLink` candidate on a node whose
    /// `entity_id` is in `linked` — the M6.5 integration: after the review
    /// worker links entities (via `apply_corrections`' linked-entities
    /// handoff), the corresponding candidates are promoted in the same plane.
    pub fn promote_linked_for_node(
        &self,
        node_id: NodeId,
        linked: &[InterlinguaId],
    ) -> Result<usize, LedgerError> {
        let mut promoted = 0usize;
        for cand in self.pending_for_node(node_id)? {
            let Some(eid) = cand.entity_id else {
                continue;
            };
            if linked.contains(&eid) {
                self.promote(node_id, &cand)?;
                promoted += 1;
            }
        }
        Ok(promoted)
    }

    /// Id-membership reconciliation: every non-zero `entity_id` in
    /// `overlay_candidates` must resolve in `interlingua_concepts`. Returns the
    /// count of candidate entity ids that failed to resolve (0 = consistent).
    /// Mirrors the boot `id-membership` reconciliation of `interlingua_index`
    /// / `interlingua_concepts` (collision-tolerant — distinct ids, never a
    /// raw count comparison).
    pub fn reconcile_ids(&self) -> Result<usize, LedgerError> {
        let rows: Vec<i64> = self
            .store
            .query_rows(
                "SELECT DISTINCT entity_id FROM overlay_candidates WHERE entity_id <> 0",
                &[],
                |r| r.get(0),
            )
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        let mut unresolved = 0usize;
        for entity_id in rows {
            let present: Option<i64> = self
                .store
                .query_row(
                    "SELECT 1 FROM interlingua_concepts WHERE id = ?1 LIMIT 1",
                    params![entity_id],
                    |r| r.get(0),
                )
                .map_err(|e| LedgerError::Db(e.to_string()))?;
            if present.is_none() {
                unresolved += 1;
            }
        }
        Ok(unresolved)
    }
}

/// Map a persisted row back to a [`OverlayCandidate`].
fn row_to_candidate(row: CandidateRow) -> OverlayCandidate {
    let (node_id, span_start, span_end, kind, entity_id, score, source, status) = row;
    OverlayCandidate {
        node_id: NodeId::from_int(node_id),
        span_start: span_start as usize,
        span_end: span_end as usize,
        kind: serde_json::from_str(&kind).unwrap_or(ResidualKind::EntityLink),
        entity_id: if entity_id == 0 {
            None
        } else {
            Some(InterlinguaId::from_u64(entity_id as u64))
        },
        score,
        source,
        status: status_from_sql(&status),
    }
}

/// Open the candidate plane over a store's shared connection.
#[must_use]
/// Open the candidate plane over a store's shared connection.
pub fn candidate_store(store: &ContentNodeStore) -> Option<OverlayCandidateStore> {
    store.shared_sqlite().map(OverlayCandidateStore::new)
}
#[cfg(test)]
#[path = "../../tests/ledger_overlay.rs"]
mod tests;