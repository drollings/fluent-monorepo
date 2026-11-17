use super::*;
use fluent_types::{local_id_of, InterlinguaId, InterlinguaNamespace};

fn cid(h: i64) -> InterlinguaId {
    InterlinguaId::new(InterlinguaNamespace::YagoClass, local_id_of(h))
}

#[test]
fn hierarchy_pins_nearest_first_order() {
    let animal = cid(1);
    let mammal = cid(2);
    let dog = cid(3);
    let h = TaxonomyHierarchy::from_edges(&[(mammal, animal), (dog, mammal)]).expect("edges");
    assert_eq!(h.ancestors(dog), vec![mammal, animal]);
    assert!(h.is_subclass(dog, animal));
    assert!(h.is_subclass(dog, dog));
}

#[test]
fn hierarchy_pins_cycle_and_unknown() {
    let a = cid(10);
    let b = cid(11);
    let h = TaxonomyHierarchy::from_edges(&[(a, b), (b, a)]).expect("edges");
    let _ = h.ancestors(a);
    assert!(h.contains(a) && h.contains(b));
    assert!(h.ancestors(cid(999)).is_empty());
}

#[test]
fn plausibility_triple_shape_pins_roles() {
    let subj = ScoredLemma::new(cid(20), "dog");
    let pred = ScoredLemma::new(cid(21), "chase");
    let obj = ScoredLemma::new(cid(22), "cat");
    let t = PlausibilityTriple::new(Some(subj.clone()), pred.clone(), Some(obj.clone()));
    assert_eq!(t.subject, Some(subj));
    assert_eq!(t.predicate, pred);
    assert_eq!(t.object, Some(obj));
    let bare = PlausibilityTriple::new(None, pred, None);
    assert!(bare.subject.is_none() && bare.object.is_none());
}

#[test]
fn normalize_helpers_pin_fallback_and_capitalize() {
    assert_eq!(
        crate::normalize::lemma_to_curies("dog"),
        ["yago:Dog".to_string(), "schema:Dog".to_string()]
    );
    assert_eq!(crate::normalize::capitalize("dog"), "Dog");
    assert_eq!(crate::normalize::capitalize(""), "");
    assert_eq!(
        crate::normalize::iri_to_curie("http://schema.org/Person"),
        "schema:Person"
    );
}

#[test]
fn prefix_split_pins_closed_and_open() {
    let (p, rest) = crate::prefix::split_prefix("yago:Dog").expect("split");
    assert_eq!(p.as_str(), "yago");
    assert_eq!(rest, "Dog");
    assert!(crate::prefix::split_prefix("bare").is_none());
}

#[test]
fn canonical_table_pins_first_wins_and_identity() {
    let t = crate::canonical::CanonicalFormTable::new(
        "1",
        vec![
            ("runs".to_string(), "run".to_string()),
            ("runs".to_string(), "ran".to_string()),
        ],
    );
    assert_eq!(t.canonical("runs"), "run");
    assert_eq!(t.canonical("unknown"), "unknown");
    assert_eq!(t.len(), 1);
    assert!(!t.is_empty());
    assert!(crate::canonical::CanonicalFormTable::empty().is_empty());
}

#[test]
fn taxonomy_pins_normalize_idempotence_and_closure() {
    let once = crate::taxonomy::normalize_taxonomy_curie("yago:Dog");
    let twice = crate::taxonomy::normalize_taxonomy_curie(&once);
    assert_eq!(once, twice);
    let direct: std::collections::BTreeMap<String, Vec<String>> = [
        ("c".to_string(), vec!["b".to_string()]),
        ("b".to_string(), vec!["a".to_string()]),
    ]
    .into_iter()
    .collect();
    let closure = crate::taxonomy::transitive_ancestors(&direct);
    assert_eq!(closure["c"], vec!["a".to_string(), "b".to_string()]);
    let cyclic: std::collections::BTreeMap<String, Vec<String>> = [
        ("a".to_string(), vec!["b".to_string()]),
        ("b".to_string(), vec!["a".to_string()]),
    ]
    .into_iter()
    .collect();
    let _ = crate::taxonomy::transitive_ancestors(&cyclic);
}
