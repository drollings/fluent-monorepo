//! Parse-time entity promotion (M16): the flag-gated overlay acceptance seam.
//!
//! The worker plane writes entity-link **candidates**; this seam accepts them
//! through the calibrated [`EntityStampGate`](spacy_rs::entity_gate::EntityStampGate)
//! into parse-time `interlingua_entity_id` stamps:
//!
//! - [`EntityPromotion::promote`] maps each pending `EntityLink` candidate's
//!   byte span to a token, runs the shared `stamp_gated` two-gate decide,
//!   persists the approved [`StampedSpan`](spacy_rs::entity_gate::StampedSpan)s
//!   (content-keyed), and promotes the stamped rows. Refused candidates stay
//!   `Pending` — the candidates-only default is preserved, and human review
//!   keeps sole ownership of dismissal.
//! - [`EntityPromotion::verify`] reloads the persisted stamps and re-runs
//!   `verify_stamps` against a (possibly edited) doc, deleting cleared rows.
//!
//! Provenance: stamped ids are LocalModel-tier by construction (the gate
//! never touches a Deterministic-tier slot — `AlreadyStamped` first-wins).

use fluent_llm::backend::ResidualKind;
use fluent_types::NodeId;
use spacy_rs::doc::Doc;
use spacy_rs::entity_gate::{
    stamp_gated, verify_stamps, EntityStampGate, StampCandidate, StampOutcome, StampedSpan,
};

use crate::ledger::overlay::OverlayCandidateStore;
use crate::ledger::LedgerError;

/// The promotion seam: an explicit enabled flag plus the calibrated gate.
/// Disabled (the default) is a complete no-op — no doc mutation, no stamp
/// rows, no status transitions.
pub struct EntityPromotion {
    enabled: bool,
    gate: EntityStampGate,
}

impl EntityPromotion {
    /// The inert seam: [`Self::promote`] returns empty and writes nothing.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            gate: EntityStampGate::new(f64::INFINITY),
        }
    }

    /// The armed seam: promotion runs through `gate` (the calibrated
    /// constant once wired behind the config flag).
    pub fn new(gate: EntityStampGate) -> Self {
        Self { enabled: true, gate }
    }

    /// Whether promotion runs.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Accept pending `EntityLink` candidates for `node_id` through the
    /// gate: stamp the passing subset into `doc`, persist their content keys,
    /// and promote the stamped rows. Candidates whose span does not map to a
    /// token, and every gate refusal, stay `Pending` (fail-open).
    pub fn promote(
        &self,
        doc: &mut Doc,
        source_text: &str,
        node_id: NodeId,
        store: &OverlayCandidateStore,
    ) -> Result<Vec<StampedSpan>, LedgerError> {
        if !self.enabled {
            return Ok(Vec::new());
        }
        let pending: Vec<_> = store
            .pending_for_node(node_id)?
            .into_iter()
            .filter(|cand| {
                cand.kind == ResidualKind::EntityLink
                    && cand.entity_id.is_some()
                    && cand.score.is_some()
            })
            .collect();
        let mut rows = Vec::new();
        let mut cands = Vec::new();
        for cand in &pending {
            let (Some(entity), Some(score)) = (cand.entity_id, cand.score) else {
                continue;
            };
            let Some(token) = token_for_span(doc, source_text, cand.span_start, cand.span_end)
            else {
                continue;
            };
            rows.push(cand.clone());
            cands.push(StampCandidate {
                token,
                entity,
                score,
            });
        }
        let outcomes = stamp_gated(doc, &self.gate, &cands);
        let mut stamped = Vec::new();
        for (cand, outcome) in rows.into_iter().zip(outcomes) {
            if let StampOutcome::Stamped(span) = outcome {
                store.write_stamp(node_id, span.token, span.entity, span.text_hash)?;
                store.promote(node_id, &cand)?;
                stamped.push(span);
            }
        }
        Ok(stamped)
    }

    /// Re-verify persisted stamps against `doc` (call on load/edit):
    /// mutated slots are cleared and their rows deleted. Returns the cleared
    /// token indices. Correctness, not a feature — runs regardless of the
    /// flag (disabled promotion simply has no rows).
    pub fn verify(
        &self,
        doc: &mut Doc,
        node_id: NodeId,
        store: &OverlayCandidateStore,
    ) -> Result<Vec<usize>, LedgerError> {
        let stamps: Vec<StampedSpan> = store
            .stamps_for_node(node_id)?
            .into_iter()
            .map(|row| StampedSpan {
                token: row.token,
                entity: row.entity_id,
                text_hash: row.text_hash,
            })
            .collect();
        let cleared = verify_stamps(doc, &stamps);
        for token in &cleared {
            store.delete_stamp(node_id, *token)?;
        }
        Ok(cleared)
    }
}

/// Map a candidate byte span to a token index by walking the doc's tokens in
/// order over `source_text` (repeats resolve positionally). Exact span match
/// only — anything unmappable stays a candidate (fail-open).
fn token_for_span(
    doc: &Doc,
    source_text: &str,
    span_start: usize,
    span_end: usize,
) -> Option<usize> {
    let mut cursor = 0usize;
    for i in 0..doc.tokens().len() {
        let orth = doc.token_text(i);
        if orth.is_empty() {
            continue;
        }
        let rest = source_text.get(cursor..)?;
        let rel = rest.find(orth.as_str())?;
        let start = cursor + rel;
        cursor = start + orth.len();
        if start == span_start
            && cursor == span_end
            && doc.tokens()[i].pos == spacy_rs::labels::Upos::Propn
        {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
#[path = "../../tests/ledger_entity_promotion.rs"]
mod tests;
