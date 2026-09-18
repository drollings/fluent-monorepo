//! Hermetic pins for `query::concept_ground::member_groundedness`: known,
//! plural-variant, unknown, empty, and store-state cases against a
//! hand-seeded `InMemoryConceptStore`. No taxonomy download, no model.

use super::*;
use fluent_concept::{ConceptStore, ConceptStoreState, InMemoryConceptStore};
use fluent_types::{ConceptMetadata, InterlinguaId, InterlinguaNamespace};

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
fn known_member_name_is_grounded() {
    let store = seeded_store();
    assert_eq!(
        member_groundedness("Dog", &store),
        MemberGroundedness::Grounded
    );
    assert_eq!(
        member_groundedness("person", &store),
        MemberGroundedness::Grounded
    );
}

#[test]
fn plural_variant_is_grounded() {
    let store = seeded_store();
    assert_eq!(
        member_groundedness("dogs", &store),
        MemberGroundedness::Grounded
    );
}

#[test]
fn unknown_member_name_is_unknown() {
    let store = seeded_store();
    assert_eq!(
        member_groundedness("ZvecWidget", &store),
        MemberGroundedness::Unknown
    );
}

#[test]
fn empty_name_is_unknown() {
    let store = seeded_store();
    assert_eq!(member_groundedness("", &store), MemberGroundedness::Unknown);
}

#[test]
fn loading_store_reads_identically() {
    // Grounding reads, never gates: store readiness must not change the
    // verdict (gating on readiness would turn "not loaded yet" into
    // false-unknowns).
    let store = seeded_store();
    store.set_state(ConceptStoreState::Loading);
    assert_eq!(
        member_groundedness("Dog", &store),
        MemberGroundedness::Grounded
    );
    assert_eq!(
        member_groundedness("ZvecWidget", &store),
        MemberGroundedness::Unknown
    );
}

// --- M3 calibration: grounded-first rerank corpus ---------------------------------
//
// Labeled member corpus over the seeded store (`yago:Dog`, `yago:Cat`,
// `schema:Person`). Groundedness is the task-value proxy; relevance labels
// are the task truth. Note the deliberate false negative (`fetchRelated`:
// relevant but project-specific, hence unknown) — it documents unknown ≠
// wrong and must survive every composition below (never dropped, only
// unordered-first).
//
// The ordering helper lives in `query::concept_ground` (promoted verbatim
// in M4); these tests exercise the production function through `super::*`.

fn corpus_store() -> InMemoryConceptStore {
    let store = InMemoryConceptStore::new();
    for (local, canonical) in [(1, "yago:Dog"), (2, "yago:Cat"), (3, "schema:Person")] {
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

struct CorpusMember {
    name: &'static str,
    relevant: bool,
}

fn corpus() -> Vec<CorpusMember> {
    vec![
        CorpusMember {
            name: "Dog",
            relevant: true,
        },
        CorpusMember {
            name: "dogs",
            relevant: true,
        },
        CorpusMember {
            name: "Person",
            relevant: true,
        },
        CorpusMember {
            name: "Cat",
            relevant: true,
        },
        CorpusMember {
            name: "ZvecWidget",
            relevant: false,
        },
        CorpusMember {
            name: "FooBar",
            relevant: false,
        },
        // Project-specific relevant term: unknown by design (control).
        CorpusMember {
            name: "fetchRelated",
            relevant: true,
        },
    ]
}

#[test]
fn rerank_never_drops_members() {
    let store = corpus_store();
    let names: Vec<String> = corpus().iter().map(|m| m.name.to_string()).collect();
    let ranked = grounded_first(names.clone(), &store);
    let mut before = names.clone();
    let mut after = ranked;
    before.sort();
    after.sort();
    assert_eq!(before, after, "rerank is a reorder, never a filter");
}

#[test]
fn rerank_empty_store_is_byte_identical() {
    // Control: all-unknown store ⇒ fail-open, order untouched.
    let store = InMemoryConceptStore::new();
    let names: Vec<String> = corpus().iter().map(|m| m.name.to_string()).collect();
    assert_eq!(grounded_first(names.clone(), &store), names);
}

#[test]
fn rerank_grounded_first_stable() {
    let store = corpus_store();
    let names = vec![
        "ZvecWidget".to_string(),
        "Dog".to_string(),
        "FooBar".to_string(),
    ];
    assert_eq!(
        grounded_first(names, &store),
        vec![
            "Dog".to_string(),
            "ZvecWidget".to_string(),
            "FooBar".to_string()
        ]
    );
    // Already-ordered input is a fixed point.
    let names = vec!["Dog".to_string(), "Person".to_string()];
    assert_eq!(grounded_first(names.clone(), &store), names);
}

#[test]
fn rerank_unknown_relevant_survives() {
    // The false negative stays in the output (just not first).
    let store = corpus_store();
    let names: Vec<String> = corpus().iter().map(|m| m.name.to_string()).collect();
    let ranked = grounded_first(names, &store);
    assert!(ranked.contains(&"fetchRelated".to_string()));
}

#[test]
fn groundedness_calibration_passes_gate() {
    // Groundedness-as-relevance-proxy on the corpus: score 1.0/0.0 at the
    // 0.5 operating point, same harness and bar as the ConceptBackend gate.
    struct Case {
        score: f64,
        label: bool,
    }
    let store = corpus_store();
    let cases: Vec<Case> = corpus()
        .iter()
        .map(|m| Case {
            score: match member_groundedness(m.name, &store) {
                MemberGroundedness::Grounded => 1.0,
                MemberGroundedness::Unknown => 0.0,
            },
            label: m.relevant,
        })
        .collect();
    let thresholds: Vec<f64> = (0..=20).map(|i| f64::from(i) * 0.05).collect();
    let reports =
        common_core::calibration::sweep_thresholds(&cases, |c| c.score, |c| c.label, &thresholds);
    common_core::calibration::emit_markdown_artifact("guidance_concept_grounded_rerank", &reports);
    let report_at_05 =
        common_core::calibration::calibrate_threshold(&cases, |c| c.score, |c| c.label, 0.5);
    assert!(
        report_at_05.passes_gate(),
        "groundedness 0.5 must pass gate: precision {} FPR {}\n{}",
        report_at_05.precision,
        report_at_05.fpr,
        common_core::calibration::render_markdown_table(&reports)
    );
}
