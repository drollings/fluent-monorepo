//! Two-gate promotion for entity-link candidates.
//!
//! The overlay plane writes entity-link **candidates** (score + entity id per
//! span); `TokenRecord.interlingua_entity_id` stays `None` at parse time by
//! default. This module is the opt-in promotion gate that stamps parse-time
//! ids — per pattern, behind two independent gates:
//!
//! - **Gate 1 — producer confidence**: the candidate `score` must reach
//!   [`EntityStampGate::score_threshold`]. A low score never stamps, however
//!   useful the entity would be.
//! - **Gate 2 — task-value**: stamping must introduce a *distinct* entity
//!   into the sentence routing signal ([`crate::routing`] `concept_ids`). A
//!   high-score candidate with zero route impact stays a candidate —
//!   confidence never substitutes for task-value. Confirmation counts as
//!   change only through new information: re-stamping an already-routed
//!   entity adds no distinct signal, so duplicates stay candidates (the
//!   entity is already routed from the first span).
//!
//! First-wins: a token that already carries an entity id (parser- or
//! resolver-stamped — the Deterministic tier) is never overwritten by a
//! gate stamp (the LocalModel tier). Stamps are content-keyed
//! ([`StampedSpan::text_hash`]); [`verify_stamps`] clears stamps whose token
//! text mutated, so a wrong id cannot survive an edit.
//!
//! Detection detail: the only reader of `interlingua_entity_id` in the
//! routing signal is the `concept_ids` collection, so gate 2 is decided
//! structurally (YaGO-namespaced entity absent from the sentence set ⇒
//! stamps) rather than by cloning the doc — pinned by the route-impact
//! pairing test.

use std::collections::HashSet;

use fluent_types::InterlinguaId;

use crate::doc::{sentence_spans, Doc};
use crate::labels::Upos;
use crate::routing::extract_routing_signals;

/// Calibration date for the entity-stamp numbers below: the labeled PROPN
/// corpus (should-stamp spans vs ambiguous / already-routed / non-YaGO /
/// low-score controls) the threshold was measured on. The gate tests pin the
/// constant with the date; re-calibration updates both together.
pub const ENTITY_CALIBRATION_DATE: &str = "2026-09-15";

/// Calibrated gate-1 bar (**[A]** producer self-doubt): should-stamp scores
/// sit at 0.80–0.95, the control group tops at 0.70 — 0.75 separates the
/// bands with margin on both sides. The threshold stays an approval floor,
/// never a ranking input.
pub const CALIBRATED_SCORE_THRESHOLD: f64 = 0.75;

/// The two-gate promotion policy. Stamping is per-pattern opt-in: nothing in
/// the pipeline constructs this by default (candidates-only default holds).
/// There is deliberately no `Default` impl and no production default value —
/// callers pass an explicit threshold (the calibrated constant) so the gate
/// stays opt-in.
#[derive(Debug, Clone, Copy)]
/// The two-gate promotion policy. Stamping is per-pattern opt-in: nothing in
/// the pipeline constructs this by default (candidates-only default holds).
/// There is deliberately no `Default` impl and no production default value —
/// callers pass an explicit threshold (the calibrated constant) so the gate
/// stays opt-in.
pub struct EntityStampGate {
    /// Gate-1 bar: candidate scores reach (`>=`) this to stamp. NaN never
    /// reaches any bar (`NaN >= t` is false).
    pub score_threshold: f64,
}

impl EntityStampGate {
    /// A gate stamping scores at or above `threshold`.
    pub fn new(threshold: f64) -> Self {
        Self {
            score_threshold: threshold,
        }
    }
}

/// Why a candidate stayed a candidate (audit-facing, not an error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Why a candidate stayed a candidate (audit-facing, not an error).
pub enum KeepReason {
    /// No such token in the doc.
    UnknownToken,
    /// Entity-link candidates are PROPN-scoped; other tags stay candidates.
    NonProperNoun,
    /// The slot already carries an entity id — first-wins, never overwrite.
    AlreadyStamped,
    /// Gate 1: score below threshold (includes NaN).
    BelowThreshold,
    /// Gate 2: stamping adds no distinct entity to the sentence signal.
    NoRouteDelta,
}

/// One promotion request: a candidate score for an entity at a token.
#[derive(Debug, Clone, Copy)]
/// One promotion request: a candidate score for an entity at a token.
pub struct StampCandidate {
    /// Token index in the doc.
    pub token: usize,
    /// The resolved entity to stamp.
    pub entity: InterlinguaId,
    /// The overlay's confidence score.
    pub score: f64,
}

/// A stamp that passed both gates, content-keyed for invalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// A stamp that passed both gates, content-keyed for invalidation.
pub struct StampedSpan {
    /// Token index in the doc.
    pub token: usize,
    /// The stamped entity.
    pub entity: InterlinguaId,
    /// `fnv1a64` of the token text at stamp time (the invalidation key).
    pub text_hash: u64,
}

/// The outcome per candidate, in request order.
#[derive(Debug, Clone)]
/// The outcome per candidate, in request order.
pub enum StampOutcome {
    /// Both gates passed; the doc now carries the id.
    Stamped(StampedSpan),
    /// Stayed a candidate, with the reason.
    Kept { token: usize, reason: KeepReason },
}

/// Stamp the gated subset of `cands` into `doc`, in order (later candidates
/// see earlier stamps: duplicates collapse to [`KeepReason::AlreadyStamped`]
/// or [`KeepReason::NoRouteDelta`]).
pub fn stamp_gated(
    doc: &mut Doc,
    gate: &EntityStampGate,
    cands: &[StampCandidate],
) -> Vec<StampOutcome> {
    cands
        .iter()
        .map(|cand| match decide(doc, *gate, cand) {
            Ok(stamped) => {
                doc.token_mut(stamped.token).interlingua_entity_id = Some(stamped.entity);
                StampOutcome::Stamped(stamped)
            }
            Err(reason) => StampOutcome::Kept {
                token: cand.token,
                reason,
            },
        })
        .collect()
}

/// Re-verify content-keyed stamps: any stamp whose token text no longer
/// hashes to its key is cleared (only when the slot still holds the stamped
/// entity — a newer writer owns anything else) and reported. Returns the
/// cleared token indices.
pub fn verify_stamps(doc: &mut Doc, stamps: &[StampedSpan]) -> Vec<usize> {
    let mut cleared = Vec::new();
    for stamp in stamps {
        let Some(token) = doc.tokens().get(stamp.token) else {
            continue;
        };
        if token.interlingua_entity_id != Some(stamp.entity) {
            continue;
        }
        if common_core::hash::fnv1a64(doc.token_text(stamp.token).as_bytes()) != stamp.text_hash {
            doc.token_mut(stamp.token).interlingua_entity_id = None;
            cleared.push(stamp.token);
        }
    }
    cleared
}

/// The two gates as a pure decision (no doc mutation).
fn decide(
    doc: &Doc,
    gate: EntityStampGate,
    cand: &StampCandidate,
) -> Result<StampedSpan, KeepReason> {
    let token = doc
        .tokens()
        .get(cand.token)
        .ok_or(KeepReason::UnknownToken)?;
    if token.pos != Upos::Propn {
        return Err(KeepReason::NonProperNoun);
    }
    if token.interlingua_entity_id.is_some() {
        return Err(KeepReason::AlreadyStamped);
    }
    // NaN compares less than nothing: `partial_cmp` is `None` when either
    // side is NaN, and `None` fails the bar exactly like `!(score >= t)`.
    if cand
        .score
        .partial_cmp(&gate.score_threshold)
        .is_none_or(|ord| ord == std::cmp::Ordering::Less)
    {
        return Err(KeepReason::BelowThreshold);
    }
    // Gate 2 (task-value): the stamp must introduce a distinct YaGO entity
    // into this sentence's routing signal. `concept_ids` is the only
    // consumer of `interlingua_entity_id` there, so the delta is structural:
    // non-YaGO entities never route (no delta possible); present entities
    // add no distinct signal.
    if !cand.entity.is_yago() {
        return Err(KeepReason::NoRouteDelta);
    }
    let in_signal = sentence_concept_set(doc, cand.token).contains(&cand.entity);
    if in_signal {
        return Err(KeepReason::NoRouteDelta);
    }
    Ok(StampedSpan {
        token: cand.token,
        entity: cand.entity,
        text_hash: common_core::hash::fnv1a64(doc.token_text(cand.token).as_bytes()),
    })
}

/// The distinct YaGO entities routed from the sentence containing `token`.
/// Sentence-index-aligned with [`extract_routing_signals`] (same walk); a
/// length divergence falls back to the union over sentences (conservative:
/// over-approximating the signal keeps candidates, never mis-stamps).
fn sentence_concept_set(doc: &Doc, token: usize) -> HashSet<InterlinguaId> {
    let spans = sentence_spans(doc);
    let signals = extract_routing_signals(doc);
    match spans
        .iter()
        .position(|(start, end)| (*start..*end).contains(&token))
    {
        Some(idx) => signals
            .get(idx)
            .and_then(|signal| signal.interlingua.as_ref())
            .map(|frame| frame.concept_ids.iter().copied().collect())
            .unwrap_or_default(),
        None => signals
            .into_iter()
            .filter_map(|signal| signal.interlingua)
            .flat_map(|frame| frame.concept_ids)
            .collect(),
    }
}

#[cfg(test)]
#[path = "../tests/entity_gate_cal.rs"]
mod calibration_tests;
#[cfg(test)]
#[path = "../tests/entity_gate.rs"]
mod tests;