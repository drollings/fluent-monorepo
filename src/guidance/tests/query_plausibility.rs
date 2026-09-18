//! Hermetic pins for `query::concept_ground::query_plausibility`: fail-open
//! inputs (`None` NLP, `Loading` store, empty query) yield `None`; the SVO
//! ladder yields the scorer's discrete values. Pipeline, vocab, tokenizer,
//! resolver, and store are all built in-test — no model, no download.

use super::*;
use fluent_concept::{ConceptStore, ConceptStoreState, InMemoryConceptStore};
use fluent_types::{ConceptMetadata, InterlinguaId, InterlinguaNamespace};
use std::sync::Arc;

fn seeded_store() -> Arc<InMemoryConceptStore> {
    let store = Arc::new(InMemoryConceptStore::new());
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

fn hermetic_pipeline(
    store: &Arc<InMemoryConceptStore>,
) -> (
    spacy_rs::pipeline::NlpPipeline,
    spacy_rs::interlingua::InterlinguaResolver,
) {
    let vocab = Arc::new(spacy_rs::vocab::Vocab::new(
        spacy_rs::lang::en::lexicon_config(),
    ));
    let tokenizer = spacy_rs::lang::en::tokenizer(vocab.clone()).expect("hermetic tokenizer");
    let resolver = spacy_rs::interlingua::InterlinguaResolver::new(
        Arc::clone(store) as Arc<dyn fluent_concept::ConceptStore>,
        Arc::clone(vocab.strings()),
    );
    let pipeline = spacy_rs::pipeline::NlpPipeline::new(
        vocab,
        tokenizer,
        spacy_rs::AnnotationValidator::new(),
    )
    .expect("hermetic pipeline");
    (pipeline, resolver)
}

#[test]
fn none_nlp_is_none() {
    let store = seeded_store();
    let (_pipeline, resolver) = hermetic_pipeline(&store);
    assert_eq!(
        query_plausibility(None, &resolver, store.as_ref(), "dog bites person"),
        None
    );
}

#[test]
fn loading_store_is_none() {
    let store = seeded_store();
    store.set_state(ConceptStoreState::Loading);
    let (pipeline, resolver) = hermetic_pipeline(&store);
    assert_eq!(
        query_plausibility(
            Some(&pipeline),
            &resolver,
            store.as_ref(),
            "dog bites person"
        ),
        None
    );
}

#[test]
fn empty_query_is_none() {
    let store = seeded_store();
    let (pipeline, resolver) = hermetic_pipeline(&store);
    assert_eq!(
        query_plausibility(Some(&pipeline), &resolver, store.as_ref(), ""),
        None
    );
}

#[test]
fn known_object_scores_one() {
    // Hermetic parsing yields imperative predicate + direct object (no
    // subject without a dep model); object registered ⇒ fully plausible.
    // Predicate known-ness is irrelevant when the present argument resolves.
    let store = seeded_store();
    let (pipeline, resolver) = hermetic_pipeline(&store);
    assert_eq!(
        query_plausibility(Some(&pipeline), &resolver, store.as_ref(), "show dog"),
        Some(1.0)
    );
    assert_eq!(
        query_plausibility(Some(&pipeline), &resolver, store.as_ref(), "find person"),
        Some(1.0)
    );
}

#[test]
fn unknown_object_scores_zero() {
    let store = seeded_store();
    let (pipeline, resolver) = hermetic_pipeline(&store);
    assert_eq!(
        query_plausibility(Some(&pipeline), &resolver, store.as_ref(), "show widget"),
        Some(0.0)
    );
}

// --- M6 calibration: triple-evidence corpus --------------------------------------
//
// Labeled query corpus over the seeded store. Relevance labels are task
// truth as judged for this corpus (grounded queries about registered
// concepts vs queries about nothing registered); evidence values come from
// `query_plausibility`. Measured — precision/recall/FPR are REPORTED via
// the shared harness + artifact, NOT enforced as a gate: evidence doesn't
// gate anything, so there is no operating point to lock. Any future gating
// use is a new roadmap that must first turn these measurements into an
// enforced gate.

struct EvidenceCase {
    query: &'static str,
    relevant: bool,
}

fn evidence_corpus() -> Vec<EvidenceCase> {
    vec![
        EvidenceCase {
            query: "show dog",
            relevant: true,
        },
        EvidenceCase {
            query: "find person",
            relevant: true,
        },
        EvidenceCase {
            query: "show widget",
            relevant: false,
        },
        EvidenceCase {
            query: "show zvecwidget",
            relevant: false,
        },
    ]
}

fn evidence_of(
    pipeline: &spacy_rs::pipeline::NlpPipeline,
    resolver: &spacy_rs::interlingua::InterlinguaResolver,
    store: &Arc<InMemoryConceptStore>,
    query: &str,
) -> f64 {
    // Every corpus query yields evidence (fail-open `None` inputs are pinned
    // by the M5 tests, not the sweep — the harness needs scalars).
    query_plausibility(Some(pipeline), resolver, store.as_ref(), query)
        .expect("corpus queries must yield evidence")
}

#[test]
fn triple_evidence_corpus_measured() {
    struct Case {
        score: f64,
        label: bool,
    }
    let store = seeded_store();
    let (pipeline, resolver) = hermetic_pipeline(&store);
    let cases: Vec<Case> = evidence_corpus()
        .iter()
        .map(|c| Case {
            score: evidence_of(&pipeline, &resolver, &store, c.query),
            label: c.relevant,
        })
        .collect();
    // Sanity: the corpus spans both evidence values on both label sides of
    // the 0.5 point (a degenerate all-one-side corpus would measure nothing).
    assert!(cases
        .iter()
        .any(|c| (c.score - 1.0).abs() < f64::EPSILON && c.label));
    assert!(cases
        .iter()
        .any(|c| c.score.abs() < f64::EPSILON && !c.label));
    let thresholds: Vec<f64> = (0..=20).map(|i| f64::from(i) * 0.05).collect();
    let reports =
        common_core::calibration::sweep_thresholds(&cases, |c| c.score, |c| c.label, &thresholds);
    assert_eq!(reports.len(), thresholds.len());
    common_core::calibration::emit_markdown_artifact("guidance_concept_triple_evidence", &reports);
    let at_05 =
        common_core::calibration::calibrate_threshold(&cases, |c| c.score, |c| c.label, 0.5);
    // Measured on this corpus: precision 1.0, recall 1.0, FPR 0.0.
    // REPORTED, not enforced — see module header above.
    assert!((at_05.precision - 1.0).abs() < f64::EPSILON);
    assert!((at_05.recall - 1.0).abs() < f64::EPSILON);
    assert!(at_05.fpr.abs() < f64::EPSILON);
}
