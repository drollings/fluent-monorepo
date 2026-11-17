//! Text-half / knowledge-half bridge for taxonomy plausibility.
//!
//! [`PlausibilityTriple`] is the neutral input to taxonomy scoring: the
//! text half (`spacy-rs` `triple::build_plausibility_inputs`) fills one per
//! extracted triple — the content-addressed [`InterlinguaId`] plus the raw
//! lemma string each role resolved from — and the knowledge half
//! (`guidance-ontology` `plausibility::score_plausibility`) scores them
//! against the [`ConceptStore`]. Neither half names the other's domain
//! types, so `spacy-rs` keeps no `guidance` edge and the ontology keeps no
//! `spacy-rs` edge (both import-boundary rules hold).
//!
//! The lemma string rides alongside the id because the curie fallback
//! (`"dog"` → `yago:Dog` / `schema:Dog`) needs the surface form when the
//! lemma id itself is unregistered; the id alone cannot rebuild it.

#![forbid(unsafe_code)]

use fluent_types::InterlinguaId;

/// One scorable argument: the content-addressed id plus the surface lemma
/// it resolved from (needed for the curie fallback).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoredLemma {
    /// The `SpacyLemma` content id for [`ScoredLemma::lemma`].
    pub id: InterlinguaId,
    /// The raw (lowercased) lemma string, e.g. `"dog"`.
    pub lemma: String,
}

impl ScoredLemma {
    /// A scorable argument from its id + surface lemma.
    #[must_use]
    pub fn new(id: InterlinguaId, lemma: impl Into<String>) -> Self {
        Self {
            id,
            lemma: lemma.into(),
        }
    }
}

/// One deterministic triple in scorable form: the predicate is always
/// present, either argument may be `None` (intransitive etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlausibilityTriple {
    /// The subject lemma, when the sentence has an `nsubj`-role argument.
    pub subject: Option<ScoredLemma>,
    /// The predicate lemma (always present — the sentence root).
    pub predicate: ScoredLemma,
    /// The direct-object lemma, when the sentence has a `dobj` argument.
    pub object: Option<ScoredLemma>,
}

impl PlausibilityTriple {
    /// A scorable triple from its role lemmas.
    #[must_use]
    pub fn new(
        subject: Option<ScoredLemma>,
        predicate: ScoredLemma,
        object: Option<ScoredLemma>,
    ) -> Self {
        Self {
            subject,
            predicate,
            object,
        }
    }
}

#[cfg(test)]
#[path = "../tests/plausibility_score.rs"]
mod plausibility_score_tests;
use crate::concept_store::{ConceptStore, ConceptStoreState};
use crate::normalize::lemma_to_curies;

/// Score `triples` against the YaGO taxonomy: for each triple, test whether
/// the subject/object lemmas are known in `store`
/// (`store.contains` on the content id, else the `yago:Capitalized` /
/// `schema:Capitalized` curie fallback). A known argument counts as
/// plausible; the per-triple score is `1.0` (both), `0.5` (one), `0.0`
/// (neither). The sentence-level plausibility is the mean over triples;
/// `None` when there are no triples.
///
/// When `store` is in `Loading` state the call returns `None` (provisional —
/// taxonomy not yet ready, mirrors `FrameKey` provisional gating).
#[must_use]
pub fn score_plausibility(triples: &[PlausibilityTriple], store: &dyn ConceptStore) -> Option<f64> {
    if triples.is_empty() {
        return None;
    }
    if store.state() == ConceptStoreState::Loading {
        return None;
    }
    let mut scores = Vec::with_capacity(triples.len());
    for t in triples {
        let subj_known = t.subject.as_ref().is_some_and(|s| lemma_known(s, store));
        let obj_known = t.object.as_ref().is_some_and(|o| lemma_known(o, store));
        let triple_score = match (
            subj_known,
            obj_known,
            t.subject.is_some(),
            t.object.is_some(),
        ) {
            // Both arguments present and known, or the single structurally
            // present argument resolved — fully plausible.
            (true, true, true, true) | (true, _, true, false) | (_, true, false, true) => 1.0,
            // Both arguments present, exactly one known — half plausible.
            (true, false, true, true) | (false, true, true, true) => 0.5,
            // Present but unknown — implausible.
            (false, false, true, true) | (false, _, true, false) | (_, false, false, true) => 0.0,
            // No arguments (verbless or bare predicate) — predicate-only triple
            // is vacuously plausible when the predicate lemma itself is known.
            (_, _, false, false) => {
                if lemma_known(&t.predicate, store) {
                    1.0
                } else {
                    0.0
                }
            }
        };
        scores.push(triple_score);
    }
    // `triples` is non-empty (checked above) and every triple pushes
    // exactly one score, so the mean is always defined.
    Some(scores.iter().sum::<f64>() / scores.len() as f64)
}

fn lemma_known(lemma: &ScoredLemma, store: &dyn ConceptStore) -> bool {
    if lemma.lemma.is_empty() {
        return false;
    }
    if store.contains(lemma.id) {
        return true;
    }
    // YaGO curie fallback: a common noun lemma like "dog" is stored as
    // `yago:Dog` / `schema:Dog` when loaded from the taxonomy. Try the
    // capitalized curie forms (shared prefix table in `normalize`).
    for curie in lemma_to_curies(&lemma.lemma) {
        if store.resolve_name(&curie).is_ok() {
            return true;
        }
    }
    // Hierarchy check: if the lemma id resolves to a class that is a subclass
    // of a known upper class, the store's `is_subclass_of` will surface via
    // `ancestors_of` — but we already checked `contains`; only an
    // is-subclass edge needs no extra lookup here because the store's
    // `contains` covers the node itself and the subclass relationship is
    // transitive through the graph we built at insert time.
    false
}
