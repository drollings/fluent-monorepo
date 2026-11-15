//! Intent calibration: the deterministic rule table is measured on
//! the eval corpus before production routing; below-bar predicted
//! classes route to `General` with a coverage gap. Exact symbol/path
//! controls are structurally unable to reach a model. Included via
//! `#[path]` forwarder from `read/calibration.rs`, per repo
//! convention. Hermetic: checked-in JSONL, no model, no endpoint.

use super::*;
use crate::eval::load_eval_tasks;

fn corpus() -> Vec<EvalTask> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../doc/guidance/eval/questions.jsonl");
    load_eval_tasks(&path).expect("checked-in eval loads")
}

#[test]
fn rule_table_is_measured_per_class_on_eval() {
    let tasks = corpus();
    let measurement = measure_intent_rules(&tasks);
    assert_eq!(
        measurement.evaluated,
        tasks.iter().filter(|task| !task.control).count()
    );
    assert_eq!(measurement.model_calls, 0, "the table never calls a model");
    let table: Vec<(String, usize, usize, f64, f64)> = measurement
        .classes
        .iter()
        .map(|class| {
            (
                format!("{:?}", class.intent),
                class.predicted,
                class.correct,
                class.precision(),
                class.recall(),
            )
        })
        .collect();
    eprintln!("intent measurement: {table:.2?}");
    eprintln!(
        "accuracy: {:.3} ({}/{}), ambiguous: {}",
        measurement.accuracy(),
        measurement.correct,
        measurement.evaluated,
        measurement.ambiguous
    );
    // Sanity on shape: every non-control task is predicted exactly
    // once, and per-class predictions sum to evaluated.
    let predicted: usize = measurement
        .classes
        .iter()
        .map(|class| class.predicted)
        .sum();
    assert_eq!(predicted, measurement.evaluated);
}

#[test]
fn exact_controls_never_reach_a_model() {
    use fluent_llm::decision::ScriptedDecideBackend;
    // Exact evidence resolves deterministically: even with a backend
    // wired up, these queries are not model-eligible, so an empty
    // script (any call errors loudly) stays untouched.
    let backend = ScriptedDecideBackend::new(vec![]);
    let _judge = crate::read::decide::DocJudge::new(&backend, "decision".to_string(), 2000);
    for query in [
        "clean_build_cache",
        "src/router/dispatch.rs",
        "FsmEngine::classify",
        "",
        "   ",
    ] {
        assert!(!model_callable(query), "{query:?} resolves without a model");
    }
    // Ambiguous queries are the only model-eligible set.
    assert!(model_callable(
        "how does guidance sync decide what to reprocess"
    ));
    assert!(model_callable("callers of gen_scoped"));
    assert!(backend.requests().is_empty());
}

#[test]
fn weak_classes_demote_to_general_with_a_gap() {
    let tasks = corpus();
    let measurement = measure_intent_rules(&tasks);
    let weak = weak_intent_classes(&measurement);
    // Pinned to the measured eval outcome: Ownership 0.667 (n=3),
    // Provenance 0.50 (n=2), HowTo 0.875 (n=8) sit below the 0.90
    // bar; Graph and Orientation hold at 1.00. Recomputed from the
    // corpus every run, so a corpus change fails here instead of
    // drifting the production route silently.
    assert_eq!(weak, WEAK_INTENT_CLASSES);
    // Below-bar predicted classes fall back; everything else keeps
    // its route. The gap names the demoted class for audit.
    for intent in weak.clone() {
        assert_eq!(
            demote_weak_intent(intent, &weak),
            QueryIntent::GeneralSearch
        );
        let gap = intent_coverage_gap(intent, &weak).expect("demoted classes log a gap");
        assert!(gap.contains("route kept"), "{gap}");
    }
    for class in &measurement.classes {
        if weak.contains(&class.intent) {
            continue;
        }
        assert_eq!(demote_weak_intent(class.intent, &weak), class.intent);
        assert!(intent_coverage_gap(class.intent, &weak).is_none());
    }
}

#[test]
fn ambiguous_predictions_are_model_expressible() {
    use crate::query::decision_points::{intent_label_to_intent, intent_spec};
    // Every table output that may reach the intent point names a
    // spec option; the two exact arms resolve before any backend
    // exists and stay out of the vocabulary. A twelfth intent would
    // fail here and hit the 10-option distribution cap — that forces
    // an explicit hierarchical-routing decision, not a silent
    // eleventh label.
    let options = intent_spec().options();
    assert!(options.len() <= 10, "single-token distribution cap");
    for intent in [
        QueryIntent::SingleIdentifier,
        QueryIntent::CapabilityQuery,
        QueryIntent::FilePath,
        QueryIntent::HowTo,
        QueryIntent::Conceptual,
        QueryIntent::MultiKeyword,
        QueryIntent::GeneralSearch,
        QueryIntent::Graph,
        QueryIntent::Ownership,
        QueryIntent::Orientation,
        QueryIntent::Provenance,
    ] {
        let label = format!("{intent:?}");
        if matches!(
            intent,
            QueryIntent::SingleIdentifier | QueryIntent::FilePath
        ) {
            assert!(!options.contains(&label), "{label} is exact-only");
        } else {
            assert!(
                options.contains(&label),
                "{label} must be model-expressible"
            );
            assert_eq!(intent_label_to_intent(&label), Some(intent));
        }
    }
}

#[test]
fn weak_class_reaches_the_plan_with_a_gap() {
    use crate::read::engine::{ReadEngine, ReadRequest};
    let engine = ReadEngine::new();
    // A below-bar class keeps its deterministic plane and records the
    // task-value gap; precision never deletes the route.
    let plan = engine.plan(&ReadRequest {
        query: "how does guidance sync decide what to reprocess".to_string(),
        budget_override: None,
    });
    assert_eq!(plan.intent, QueryIntent::HowTo);
    assert_eq!(plan.demoted_from, Some(QueryIntent::HowTo));
    assert_eq!(
        plan.route,
        crate::read::engine::route_for(QueryIntent::HowTo)
    );
    // Exact and strong classes keep their route with no gap marker.
    let plan = engine.plan(&ReadRequest {
        query: "callers of gen_scoped".to_string(),
        budget_override: None,
    });
    assert_eq!(plan.intent, QueryIntent::Graph);
    assert_eq!(plan.demoted_from, None);
    let plan = engine.plan(&ReadRequest {
        query: "clean_build_cache".to_string(),
        budget_override: None,
    });
    assert_eq!(plan.intent, QueryIntent::SingleIdentifier);
    assert_eq!(plan.demoted_from, None);
}
