//! Work plans: import, merge-preserving revision, acceptance-gate
//! regression reopening, and crash-safe revision intents. The loop
//! tracks and verifies plans but never executes their content.

use std::path::Path;
use std::sync::Arc;

use crate::gates::TestReport;
use crate::journal::{reconcile_intent, Reconcile};
use crate::plans::{AcceptanceGate, PlanItem, PlanItemState, WorkPlan};
use crate::seal::BuildVerdict;

fn item(id: &str, target: &str) -> PlanItem {
    PlanItem {
        id: id.to_string(),
        target: target.to_string(),
        acceptance: AcceptanceGate {
            package: "pkg".to_string(),
            test_name: Some("t".to_string()),
        },
        state: PlanItemState::Open,
    }
}

fn runners(build_clean: bool, failures: Vec<String>) -> (crate::seal::BuildRunner, crate::seal::TestRunner) {
    let build: crate::seal::BuildRunner = Arc::new(move |package: &Path| {
        Ok(BuildVerdict {
            package: package.display().to_string(),
            clean: build_clean,
            output: String::new(),
        })
    });
    let report = TestReport { new_failures: failures, output: String::new() };
    let test: crate::seal::TestRunner = Arc::new(move |_: &Path| Ok(report.clone()));
    (build, test)
}

#[test]
fn import_assigns_stable_ids_with_changelog() {
    let plan = WorkPlan::import("p", vec![item("a", "t1"), item("b", "t2")]).expect("import");
    assert_eq!(plan.order, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(plan.changelog, vec!["add a".to_string(), "add b".to_string()]);
}

#[test]
fn revise_preserves_completed_merges_new_and_changed() {
    let mut plan = WorkPlan::import("p", vec![item("a", "t1"), item("b", "t2")]).expect("import");
    let (build, test) = runners(true, Vec::new());
    plan.verify(&build, &test);
    assert!(plan.is_complete());
    plan.revise(vec![item("a", "t1"), item("b", "t2-changed"), item("c", "t3")]);
    assert_eq!(plan.items["a"].state, PlanItemState::Verified, "completed preserved");
    assert_eq!(plan.items["b"].state, PlanItemState::Open, "changed reopens");
    assert_eq!(plan.items["c"].state, PlanItemState::Open, "new items open");
    assert!(plan.changelog.iter().any(|line| line == "change b"));
    assert!(plan.changelog.iter().any(|line| line == "add c"));
}

#[test]
fn regressed_acceptance_gate_reopens_verified_item() {
    let mut plan = WorkPlan::import("p", vec![item("a", "t1")]).expect("import");
    let (build, test) = runners(true, Vec::new());
    plan.verify(&build, &test);
    assert!(plan.is_complete());
    let (build_ok, failing) = runners(true, vec!["t".to_string()]);
    let reopened = plan.verify(&build_ok, &failing);
    assert_eq!(reopened, vec!["a".to_string()]);
    assert_eq!(plan.items["a"].state, PlanItemState::Open);
    assert!(!plan.is_complete());
}

#[test]
fn revision_intent_reconciles_crash_without_loss() {
    let plan = WorkPlan::import("p", vec![item("a", "t1")]).expect("import");
    let mut next = plan.clone();
    next.revise(vec![item("a", "t1"), item("b", "t2")]);
    let intent = plan.revision_intent(&next);
    assert_eq!(reconcile_intent(&intent, &intent.post_hash), Reconcile::Applied);
    assert_eq!(reconcile_intent(&intent, &intent.pre_hash), Reconcile::Requeue);
    assert_eq!(reconcile_intent(&intent, "plan:edited-elsewhere"), Reconcile::HumanEdit);
    let roundtrip: WorkPlan =
        serde_json::from_str(&serde_json::to_string(&next).expect("json")).expect("parse");
    assert_eq!(roundtrip, next, "persisted plans survive a restart byte-identically");
}
