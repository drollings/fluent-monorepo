//! Stage tests for the pure term funnel (`query::funnel`): alias
//! hit/miss, unknown-concept passthrough, scaffolding exclusion,
//! empty-query totality. Included via `#[path]` forwarder, per repo
//! convention. Hermetic: hand-built aliases, hand-seeded store, no
//! model, no taxonomy download.

use super::*;
use fluent_concept::InMemoryConceptStore;
use fluent_types::{ConceptMetadata, InterlinguaId, InterlinguaNamespace};

fn alias_table() -> search_vector::aliases::SemanticAliases {
    search_vector::aliases::SemanticAliases::from_json(
        r#"{"components": ["modules", "parts"], "runs": ["executes"]}"#,
    )
    .expect("alias fixture parses")
}

fn seeded_store() -> InMemoryConceptStore {
    let store = InMemoryConceptStore::new();
    for (local, canonical) in [(1, "yago:Dog"), (2, "schema:Person")] {
        store
            .insert(ConceptMetadata {
                id: InterlinguaId::new(InterlinguaNamespace::YagoClass, local),
                canonical_name: canonical.to_string(),
                namespace: InterlinguaNamespace::YagoClass,
                yago_iri: None,
                yago_class_iri: None,
                label: None,
                node_id: None,
                parent_class_id: None,
            })
            .expect("insert");
    }
    store
}

#[test]
fn alias_hit_expands_term_union() {
    let funneled = funnel_terms("components", &alias_table(), None);
    assert_eq!(funneled.significant, vec!["components".to_string()]);
    assert_eq!(
        funneled.expanded,
        vec![
            "components".to_string(),
            "modules".to_string(),
            "parts".to_string()
        ]
    );
}

#[test]
fn alias_miss_keeps_term_untouched() {
    let funneled = funnel_terms("sprocket", &alias_table(), None);
    assert_eq!(funneled.significant, vec!["sprocket".to_string()]);
    assert_eq!(funneled.expanded, vec!["sprocket".to_string()]);
}

#[test]
fn unknown_concepts_pass_through_unfiltered() {
    // Nothing grounds `sprocket`, yet it stays in every stage —
    // the funnel ranks, never filters.
    let funneled = funnel_terms("sprocket", &alias_table(), Some(&seeded_store()));
    assert_eq!(funneled.significant, vec!["sprocket".to_string()]);
    assert_eq!(funneled.concepts.len(), 1);
    assert_eq!(funneled.concepts[0].term, "sprocket");
    assert!(!funneled.concepts[0].grounded);
    assert!(!funneled.concepts[0].curies.is_empty());
}

#[test]
fn known_concept_grounds_through_variants() {
    // `dogs` grounds via the `dog` variant curie, exactly like the
    // member-groundedness rule.
    let funneled = funnel_terms("dogs", &alias_table(), Some(&seeded_store()));
    assert_eq!(funneled.concepts.len(), 1);
    assert!(funneled.concepts[0].grounded, "{:?}", funneled.concepts[0]);
    assert!(
        funneled.concepts[0]
            .curies
            .iter()
            .any(|curie| curie == "yago:Dog"),
        "{:?}",
        funneled.concepts[0].curies
    );
}

#[test]
fn scaffolding_never_reaches_symbols() {
    // `what`/`are`/`the` are question scaffolding, not identifier
    // intent: only `components` survives to every stage.
    let funneled = funnel_terms(
        "What are the components",
        &alias_table(),
        Some(&seeded_store()),
    );
    assert_eq!(funneled.significant, vec!["components".to_string()]);
    assert_eq!(
        funneled.symbols,
        search_vector::tokens::symbol_names("components")
    );
    assert!(
        !funneled.symbols.iter().any(|s| s == "what" || s == "are"),
        "{:?}",
        funneled.symbols
    );
}

#[test]
fn empty_query_funnels_to_empty() {
    let funneled = funnel_terms("", &alias_table(), None);
    assert!(funneled.is_empty());
    assert!(funneled.significant.is_empty());
    assert!(funneled.expanded.is_empty());
    assert!(funneled.concepts.is_empty());
    assert!(funneled.symbols.is_empty());
    assert!(funneled.stage_log().is_empty());
}

#[test]
fn stage_log_pins_each_stage() {
    let funneled = funnel_terms("dogs runs", &alias_table(), Some(&seeded_store()));
    let log = funneled.stage_log();
    assert_eq!(log.len(), 4, "{log:?}");
    assert!(log[0].starts_with("significant: "), "{log:?}");
    assert!(
        log[0].contains("dogs") && log[0].contains("runs"),
        "{log:?}"
    );
    assert!(log[1].starts_with("expanded: "), "{log:?}");
    assert!(log[1].contains("executes"), "{log:?}");
    assert!(log[2].starts_with("concepts: "), "{log:?}");
    assert!(log[2].contains("grounded"), "{log:?}");
    assert!(log[2].contains("unknown"), "{log:?}");
    assert!(log[3].starts_with("symbols: "), "{log:?}");
    assert!(log[3].contains("dogs"), "{log:?}");
}
