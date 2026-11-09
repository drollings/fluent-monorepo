use super::{grounded_first, member_groundedness, MemberGroundedness};
use fluent_concept::ConceptStore;

fn seeded_store() -> fluent_concept::InMemoryConceptStore {
    use fluent_types::{ConceptMetadata, InterlinguaId, InterlinguaNamespace};
    let store = fluent_concept::InMemoryConceptStore::new();
    store
        .insert(ConceptMetadata {
            id: InterlinguaId::new(InterlinguaNamespace::YagoClass, 7),
            canonical_name: "schema:Addnumbers".to_string(),
            namespace: InterlinguaNamespace::YagoClass,
            yago_iri: None,
            yago_class_iri: None,
            label: None,
            node_id: None,
            parent_class_id: None,
        })
        .expect("insert");
    store
}

#[test]
fn unknown_novel_terms_stay_present_but_last() {
    // Unknown is absence of evidence, never a filter: novel terms stay.
    let store = seeded_store();
    let ranked = grounded_first(
        vec!["xyzzyQuux".to_string(), "addNumbers".to_string()],
        &store,
    );
    assert_eq!(
        ranked,
        vec!["addNumbers".to_string(), "xyzzyQuux".to_string()]
    );
}

#[test]
fn rerank_preserves_membership_and_stability() {
    // Order-only composition: same multiset, stable within groups.
    let store = seeded_store();
    let input = vec![
        "helloWorld".to_string(),
        "addNumbers".to_string(),
        "xyzzyQuux".to_string(),
        "anotherOne".to_string(),
    ];
    let ranked = grounded_first(input.clone(), &store);
    let mut before = input.clone();
    let mut after = ranked.clone();
    before.sort();
    after.sort();
    assert_eq!(before, after, "membership preserved");
    assert_eq!(ranked[0], "addNumbers", "grounded moves first, stably");
    let unknowns: Vec<&String> = ranked.iter().skip(1).collect();
    assert_eq!(
        unknowns,
        vec![
            &"helloWorld".to_string(),
            &"xyzzyQuux".to_string(),
            &"anotherOne".to_string()
        ],
        "unknowns keep relative order"
    );
}

#[test]
fn ready_and_loading_stores_rank_identically() {
    use fluent_concept::ConceptStoreState;
    let ready = seeded_store();
    let loading = seeded_store();
    loading.set_state(ConceptStoreState::Loading);
    let input = vec!["helloWorld".to_string(), "addNumbers".to_string()];
    assert_eq!(
        grounded_first(input.clone(), &ready),
        grounded_first(input, &loading)
    );
}

#[test]
fn empty_name_is_unknown_never_grounded() {
    let store = seeded_store();
    assert_eq!(member_groundedness("", &store), MemberGroundedness::Unknown);
    assert_eq!(
        member_groundedness("addNumbers", &store),
        MemberGroundedness::Grounded
    );
}

#[test]
fn control_set_for_any_future_filter_gate() {
    // Recorded controls: if a filter is ever proposed, these unknowns
    // must survive it (recall preserved). Today the composition is
    // rerank-only, so survival holds by construction.
    let store = seeded_store();
    let controls = vec!["xyzzyQuux", "helloWorld", "qzxWobble"];
    let input: Vec<String> = controls.iter().map(ToString::to_string).collect();
    let ranked = grounded_first(input, &store);
    for control in controls {
        assert!(
            ranked.contains(&control.to_string()),
            "control term must survive: {control}"
        );
    }
}
