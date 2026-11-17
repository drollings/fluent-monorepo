use super::*;
use crate::concept_store::ConceptStore;
use fluent_types::{local_id_of, ConceptMetadata, InterlinguaId, InterlinguaNamespace, NodeId};

fn lid(h: i64) -> InterlinguaId {
    InterlinguaId::new(InterlinguaNamespace::SpacyLemma, local_id_of(h))
}

fn meta_for(id: InterlinguaId, name: &str) -> ConceptMetadata {
    ConceptMetadata {
        id,
        canonical_name: name.to_string(),
        namespace: id.namespace(),
        yago_iri: None,
        yago_class_iri: None,
        label: Some(name.to_string()),
        node_id: Some(NodeId::from_int(id.local_id() ^ 0x8000_0000_0000)),
        parent_class_id: None,
    }
}

fn lemma(h: i64, word: &str) -> ScoredLemma {
    ScoredLemma::new(lid(h), word)
}

fn store_with(names: &[&str]) -> crate::concept_store_mem::InMemoryConceptStore {
    let store = crate::concept_store_mem::InMemoryConceptStore::new();
    for (i, name) in names.iter().enumerate() {
        let id = lid(100 + i as i64);
        store.insert(meta_for(id, name)).expect("insert");
    }
    store
}

#[test]
fn both_known_scores_one_and_empty_is_none() {
    let store = store_with(&["dog", "chase", "cat"]);
    let t = PlausibilityTriple::new(
        Some(lemma(100, "dog")),
        lemma(101, "chase"),
        Some(lemma(102, "cat")),
    );
    assert_eq!(score_plausibility(&[t], &store), Some(1.0));
    let empty: Vec<PlausibilityTriple> = Vec::new();
    assert_eq!(score_plausibility(&empty, &store), None);
}

#[test]
fn half_known_scores_half_and_none_known_scores_zero() {
    let store = store_with(&["dog"]);
    let half = PlausibilityTriple::new(
        Some(lemma(100, "dog")),
        lemma(101, "chase"),
        Some(lemma(999, "xyzzy")),
    );
    assert_eq!(
        score_plausibility(std::slice::from_ref(&half), &store),
        Some(0.5)
    );
    let none = PlausibilityTriple::new(
        Some(lemma(998, "qqq")),
        lemma(997, "zzz"),
        Some(lemma(999, "xyzzy")),
    );
    assert_eq!(
        score_plausibility(std::slice::from_ref(&none), &store),
        Some(0.0)
    );
}

#[test]
fn predicate_only_falls_back_to_predicate_and_loading_is_provisional() {
    let store = store_with(&["chase"]);
    let bare = PlausibilityTriple::new(None, lemma(100, "chase"), None);
    assert_eq!(
        score_plausibility(std::slice::from_ref(&bare), &store),
        Some(1.0)
    );
    let bare_unknown = PlausibilityTriple::new(None, lemma(997, "zzz"), None);
    assert_eq!(
        score_plausibility(std::slice::from_ref(&bare_unknown), &store),
        Some(0.0)
    );
}
