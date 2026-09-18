//! Deterministic RDF triple extraction + the knowledge-half
//! seam for YaGO taxonomy plausibility.
//!
//! Derives `(subject, predicate, object)` triples from the ArcEager roles
//! (`nsubj`/`dobj` filling `role_coverage`) — the same dependency-tree
//! discipline as `routing.rs` and `frame.rs`.
//!
//! Scoring lives with the knowledge owner (`guidance-ontology`
//! `plausibility::score_plausibility`, over the shared
//! [`PlausibilityTriple`](fluent_concept::PlausibilityTriple) input); this
//! module keeps only the pure derivation plus the dependency-free
//! [`PlausibilityFetch`] seam and the [`build_plausibility_inputs`] text-half
//! adapter, so no `guidance` edge ever enters this crate (M2b template).
//!
//! The spike is **default-off** and `semantic_plausibility` stays a separate
//! `ParseConfidence` field (roadmap E7 — never blended into `oracle_margins`).

#![forbid(unsafe_code)]

use std::sync::Arc;

use crate::doc::{sentence_roles, Doc};
use crate::interlingua::InterlinguaResolver;
use fluent_concept::{PlausibilityTriple, ScoredLemma};
/// The knowledge-half scoring seam (M5): given the scorable triples the
/// text half derived, return the taxonomy plausibility, or `None` when the
/// taxonomy is not ready. The owner wires its kernel at the construction
/// site (same pattern as the router injecting `LlmFetch`); unwired, the
/// `YagoResolveStage` stays fail-closed (`None`, the pre-M3 behavior).
pub type PlausibilityFetch = Arc<dyn Fn(&[PlausibilityTriple]) -> Option<f64> + Send + Sync>;

/// One deterministic triple from a single sentence: the predicate is the
/// sentence root, `subject` is an `nsubj` dependent of that root, `object` is a
/// `dobj` dependent. Either argument may be `None` (intransitive etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Triple {
    /// Token index of the subject, when an `nsubj` child exists.
    pub subject: Option<usize>,
    /// Token index of the predicate (always `Some` — the sentence root).
    pub predicate: usize,
    /// Token index of the direct object, when a `dobj` child exists.
    pub object: Option<usize>,
    /// Sentence span `[start, end)` the triple was derived from.
    pub sentence_span: (usize, usize),
}

/// Dep labels naming the subject/object roles — the canonical sets from
/// `labels.rs` (same closed vocabulary as the router's `routing.rs` and the
/// frame extractor).
/// Extract triples deterministically from an **attached** doc (deps/heads set).
/// One triple per sentence — the predicate is always the root, arguments are the
/// first matching child per role (matching `routing.rs` single-slot extraction).
#[must_use]
pub fn extract_triples(doc: &Doc) -> Vec<Triple> {
    if doc.is_empty() {
        return Vec::new();
    }
    // Shared sentence walk: span + root come from the single traversal.
    // The candidate set stays root-children-restricted (narrower than the
    // walk's all-span-tokens lists): the first span-ordered candidate that
    // is a child of the root wins, preserving the historical
    // children-iteration first-match exactly (children are lefts-then-rights,
    // both ascending, so first-in-children-order is first-in-span-order
    // restricted to children).
    let mut out = Vec::new();
    for roles in sentence_roles(doc) {
        let (s, e) = roles.span;
        let root = roles.root;
        let kids = doc.children(root);
        let subject = roles.subjects.iter().copied().find(|c| kids.contains(c));
        let object = roles
            .direct_objects
            .iter()
            .copied()
            .find(|c| kids.contains(c));
        out.push(Triple {
            subject,
            predicate: root,
            object,
            sentence_span: (s, e),
        });
    }
    out
}

/// Build the knowledge-half scoring input for `triples`: one
/// [`PlausibilityTriple`] per triple, pairing each role's content-addressed
/// id (via `resolver`) with its surface lemma. Pure over `Doc` + resolver;
/// the store is never consulted here (registration is boot-only).
#[must_use]
pub fn build_plausibility_inputs(
    doc: &Doc,
    triples: &[Triple],
    resolver: &InterlinguaResolver,
) -> Vec<PlausibilityTriple> {
    triples
        .iter()
        .map(|t| {
            PlausibilityTriple::new(
                t.subject.map(|i| scored_lemma(doc, i, resolver)),
                scored_lemma(doc, t.predicate, resolver),
                t.object.map(|i| scored_lemma(doc, i, resolver)),
            )
        })
        .collect()
}

fn scored_lemma(doc: &Doc, idx: usize, resolver: &InterlinguaResolver) -> ScoredLemma {
    let lemma = doc
        .vocab()
        .strings()
        .get(doc.token(idx).lemma)
        .map(|s| s.to_string())
        .unwrap_or_default();
    let id = resolver.lemma_id(&lemma);
    ScoredLemma::new(id, lemma)
}

/// Score `triples` through an injected knowledge-half fetch: the text-half
/// adapter over [`build_plausibility_inputs`]. Returns `None` when there are
/// no triples or no fetch is wired (fail-closed, the pre-M3 behavior).
#[must_use]
pub fn semantic_plausibility_via_fetch(
    doc: &Doc,
    triples: &[Triple],
    resolver: &InterlinguaResolver,
    fetch: Option<&PlausibilityFetch>,
) -> Option<f64> {
    if triples.is_empty() {
        return None;
    }
    let fetch = fetch?;
    let inputs = build_plausibility_inputs(doc, triples, resolver);
    fetch(&inputs)
}

#[cfg(test)]
#[path = "../tests/triple.rs"]
mod tests;
