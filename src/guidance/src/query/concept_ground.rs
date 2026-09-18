//! Taxonomy groundedness for concept search (task-value axis, not producer
//! confidence): whether a member name resolves to a registered concept.
//!
//! Pure string-in/verdict-out over the shared primitives —
//! `search_vector::tokens::split_tokens` (tokenization, no second splitter),
//! `guidance_ontology::normalize::{token_variants, lemma_to_curies}` (plural
//! handling + curie fallbacks, no second table), `ConceptStore::resolve_name`
//! (lookup). Reads the store, never gates on it: readiness and unknown names
//! yield `Unknown`, and callers decide what `Unknown` means (rerank, never
//! filter, until a calibrated threshold earns a gate).

/// Taxonomy verdict for one member name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberGroundedness {
    /// At least one name token resolves to a registered concept.
    Grounded,
    /// No token resolves (novel term, empty name, or unloaded store) —
    /// unknown, never wrong.
    Unknown,
}

/// Whether a member name resolves to a registered concept: any token hit
/// grounds the member. Total over inputs (never errors, never panics).
#[must_use]
pub fn member_groundedness(
    name: &str,
    store: &dyn fluent_concept::ConceptStore,
) -> MemberGroundedness {
    for token in search_vector::tokens::split_tokens(name) {
        for variant in guidance_ontology::normalize::token_variants(&token) {
            for curie in guidance_ontology::normalize::lemma_to_curies(&variant) {
                if store.resolve_name(&curie).is_ok() {
                    return MemberGroundedness::Grounded;
                }
            }
        }
    }
    MemberGroundedness::Unknown
}

/// Stable grounded-first partition of gate-passing member names: grounded
/// members keep their relative order up front, unknown members keep theirs
/// behind. Membership always preserved (reorder, never filter) — the only
/// composition the uncalibrated axis is trusted with.
#[must_use]
pub fn grounded_first(names: Vec<String>, store: &dyn fluent_concept::ConceptStore) -> Vec<String> {
    let mut grounded = Vec::new();
    let mut unknown = Vec::new();
    for name in names {
        if member_groundedness(&name, store) == MemberGroundedness::Grounded {
            grounded.push(name);
        } else {
            unknown.push(name);
        }
    }
    grounded.extend(unknown);
    grounded
}

/// Query-triple plausibility evidence: the taxonomy groundedness of what the
/// query *says* (subject/predicate/object lemmas via the existing
/// `extract_triples` → `build_plausibility_inputs` → `score_plausibility`
/// chain). Pure evidence — NOT wired into any backend here; any gating use
/// needs its own calibration first.
///
/// Fail-open contract: `None` NLP, NLP error, zero triples, and `Loading`
/// store all yield `None` (today's behavior preserved — absence of evidence
/// is not evidence). All three inputs are caller-owned parameters (NLP +
/// resolver live in the pipeline owner, the store in the router); this
/// function introduces no seam and no second scorer.
#[must_use]
pub fn query_plausibility(
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
    resolver: &spacy_rs::interlingua::InterlinguaResolver,
    store: &dyn fluent_concept::ConceptStore,
    query: &str,
) -> Option<f64> {
    let nlp = nlp?;
    let doc = nlp.process_sync(query, None).ok()?;
    let triples = spacy_rs::triple::extract_triples(&doc);
    if triples.is_empty() {
        return None;
    }
    let inputs = spacy_rs::triple::build_plausibility_inputs(&doc, &triples, resolver);
    guidance_ontology::plausibility::score_plausibility(&inputs, store)
}

#[cfg(test)]
#[path = "../../tests/query_concept_ground.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/query_plausibility.rs"]
mod plausibility_tests;
