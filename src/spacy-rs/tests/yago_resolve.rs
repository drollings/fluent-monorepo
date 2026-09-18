//! M5.1 pins for `YagoResolveStage`: name/depends/provides/describe are
//! byte-pinned so the `describe`-helper migration (M5.9) proves zero output
//! change. The `FieldAccess` no-op trio is pinned too — the migration must
//! not alter the field surface.

use super::*;
use std::sync::Arc;

use fluent_concept::{ConceptStore, InMemoryConceptStore};

fn ascii(deps: &[internment::ArcIntern<str>]) -> Vec<String> {
    deps.iter().map(std::string::ToString::to_string).collect()
}

#[test]
fn yago_resolve_metadata_and_describe_are_pinned() {
    let store: Arc<InMemoryConceptStore> = Arc::new(InMemoryConceptStore::new());
    let stage = YagoResolveStage::new(Arc::clone(&store) as Arc<dyn ConceptStore>);
    assert_eq!(stage.name(), "yago_resolve");
    assert_eq!(ascii(stage.depends()), vec!["annotated_doc"]);
    assert_eq!(ascii(stage.provides()), vec!["yago_resolved"]);
    // No `purity` key today — the helper migration must not add one.
    assert_eq!(
        stage.describe(),
        serde_json::json!({
            "name": "yago_resolve",
            "depends": ["annotated_doc"],
            "provides": ["yago_resolved"]
        })
    );
    assert!(stage.describe().get("purity").is_none());
}

#[test]
fn yago_resolve_field_access_stays_fieldless() {
    let store: Arc<InMemoryConceptStore> = Arc::new(InMemoryConceptStore::new());
    let mut stage = YagoResolveStage::new(Arc::clone(&store) as Arc<dyn ConceptStore>);
    assert!(stage.field_names().is_empty());
    assert!(stage.set_field("enabled", "true").is_err());
    assert!(stage.get_field("enabled").is_err());
}

// ── Constructor consolidation pins ──
//
// Default shell leaves `semantic_plausibility` as `None`; enabled without a
// fetch stays `None` (fail-closed); enabled with a fetch fills from the
// owner's score; `oracle_margins` is never touched (E7).

use crate::doc::Doc;
use crate::llm::{attach, AnnotationSet, AnnotationSource};
use crate::pipeline::PipelineState;
use crate::triple::PlausibilityFetch;
use fluent_concept::PlausibilityTriple;

fn yago_doc() -> Doc {
    let vocab = Arc::new(crate::vocab::Vocab::new(
        crate::lexeme::LexiconConfig::default(),
    ));
    let mut doc = Doc::new(vocab);
    for t in ["The", "dog", "barks", "."] {
        doc.push_back(t, true).expect("push");
    }
    let set = AnnotationSet::parse_json(
        r#"[
        {"text":"The","pos":"det","dep":"det","head":1,"lemma":"the"},
        {"text":"dog","pos":"noun","dep":"nsubj","head":1,"lemma":"dog"},
        {"text":"barks","pos":"verb","dep":"root","head":0,"lemma":"bark"},
        {"text":".","pos":"punct","dep":"punct","head":-1,"lemma":"."}
    ]"#,
    )
    .expect("json");
    attach(&mut doc, &set).expect("attach");
    crate::sentencizer::Sentencizer::new().process(&mut doc);
    doc
}

fn scored_annotation() -> crate::llm::AnnotationResult {
    let margins = vec![0.5, 0.25];
    crate::llm::AnnotationResult::new(AnnotationSet::default(), AnnotationSource::ArcEager)
        .with_confidence(
            Some(vec![0.9, 0.9]),
            Some(crate::arc_eager::ParseConfidence {
                overall: 0.9,
                token_scores: vec![0.9, 0.9],
                role_coverage: 1.0,
                oracle_tie_count: 0,
                oracle_margins: margins,
                semantic_plausibility: None,
            }),
        )
}

fn stub_fetch(score: f64) -> PlausibilityFetch {
    Arc::new(move |_triples: &[PlausibilityTriple]| Some(score))
}

fn execute_over(stage: &YagoResolveStage, doc: Doc) -> (Option<f64>, Option<f64>, Vec<f64>) {
    let state = Arc::new(std::sync::Mutex::new(PipelineState {
        doc: Some(doc),
        annotation: Some(scored_annotation()),
        ..PipelineState::default()
    }));
    let mut ctx = WorkContext::default();
    ctx.set(STATE_KEY, Arc::clone(&state));
    stage.execute(&ctx).expect("execute");
    let state = state.lock().unwrap();
    let ann = state.annotation.as_ref().expect("annotation");
    let pc = ann.parse_confidence.as_ref().expect("confidence");
    (
        state.semantic_plausibility,
        pc.semantic_plausibility,
        pc.oracle_margins.clone(),
    )
}

fn store() -> Arc<dyn ConceptStore> {
    Arc::new(InMemoryConceptStore::new()) as Arc<dyn ConceptStore>
}

#[test]
fn default_shell_leaves_plausibility_none() {
    let stage = YagoResolveStage::new(store());
    assert!(!stage.is_enabled());
    assert!(!stage.has_fetch());
    let (state_p, ann_p, margins) = execute_over(&stage, yago_doc());
    assert_eq!(state_p, None);
    assert_eq!(ann_p, None);
    assert_eq!(margins, vec![0.5, 0.25]);
}

#[test]
fn enabled_without_fetch_stays_none() {
    let stage = YagoResolveStage::with_config(
        store(),
        YagoResolveConfig {
            enabled: true,
            fetch: None,
        },
    );
    assert!(stage.is_enabled());
    assert!(!stage.has_fetch());
    let (state_p, ann_p, margins) = execute_over(&stage, yago_doc());
    assert_eq!(state_p, None);
    assert_eq!(ann_p, None);
    assert_eq!(margins, vec![0.5, 0.25]);
}

#[test]
fn enabled_with_fetch_fills_plausibility_without_touching_margins() {
    let stage = YagoResolveStage::with_config(
        store(),
        YagoResolveConfig {
            enabled: true,
            fetch: Some(stub_fetch(0.75)),
        },
    );
    assert!(stage.is_enabled());
    assert!(stage.has_fetch());
    let (state_p, ann_p, margins) = execute_over(&stage, yago_doc());
    assert_eq!(state_p, Some(0.75));
    assert_eq!(ann_p, Some(0.75));
    assert_eq!(margins, vec![0.5, 0.25]);
}

#[test]
fn config_constructors_agree_with_legacy_shims() {
    let plain = YagoResolveStage::new(store());
    let via_config = YagoResolveStage::with_config(store(), YagoResolveConfig::default());
    assert_eq!(plain.is_enabled(), via_config.is_enabled());
    assert_eq!(plain.has_fetch(), via_config.has_fetch());

    let fetch = stub_fetch(0.5);
    let legacy = YagoResolveStage::with_fetch(store(), true, Arc::clone(&fetch));
    let canonical = YagoResolveStage::with_config(
        store(),
        YagoResolveConfig {
            enabled: true,
            fetch: Some(fetch),
        },
    );
    assert_eq!(legacy.is_enabled(), canonical.is_enabled());
    assert_eq!(legacy.has_fetch(), canonical.has_fetch());
}
