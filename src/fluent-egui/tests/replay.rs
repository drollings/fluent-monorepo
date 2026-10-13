use super::*;
use std::sync::Arc;

use crate::{PaneConfig, cache_key};

fn pane_values(fill: f64) -> Vec<Option<f64>> {
    vec![None, Some(fill), Some(fill + 1.0), None, Some(fill + 2.0)]
}

/// Small domain-blind chain with a fork: `root` feeds `mid` and `side`,
/// both feed `sink`.
fn full_controller() -> ReplayController {
    let mut controller = ReplayController::new();
    let plan = controller.plan_mut();
    plan.register_node("root", &[], 0).expect("root registers");
    plan.register_node("mid", &["root"], 0).expect("mid registers");
    plan.register_node("side", &["root"], 0).expect("side registers");
    plan.register_node("sink", &["mid", "side"], 0).expect("sink registers");
    controller
}

fn complete_all(controller: &mut ReplayController) {
    let order = controller.plan().order().expect("plan orders");
    for pane in &order {
        controller.complete(pane);
    }
}

#[test]
fn checkpoint_goldens_mark_rewind_and_refuse() {
    let mut controller = full_controller();
    let order = controller.plan().order().expect("plan orders");
    assert_eq!(order.len(), 4, "test topology holds four panes");

    controller.mark("mid").expect("known pane marks");
    assert!(controller.is_checkpoint("mid"));
    assert!(controller.mark("nope").is_err(), "unknown pane refuses the mark");
    assert!(controller.rewind_to("nope").is_err(), "unknown mark refuses the rewind");

    complete_all(&mut controller);
    let suffix = controller.rewind_to("mid").expect("marked rewind runs");
    // Rewind returns the insertion-ordered suffix from the mark: the fork
    // sibling `side` re-runs too (catch-up is a valid schedule, not the
    // dependent cone).
    assert_eq!(suffix, vec!["mid", "side", "sink"]);
    assert!(!controller.is_checkpoint("mid"), "rewind consumes the mark");
    assert_eq!(
        controller.ready(),
        vec!["mid".to_owned(), "side".to_owned()],
        "catch-up opens with every unblocked suffix pane"
    );
}

#[test]
fn rewind_rerun_reproduces_identical_revisions() {
    let mut controller = full_controller();
    let order = controller.plan().order().expect("plan orders");
    controller.mark("mid").expect("mark records");
    // First run: complete every pane in plan order, publishing per-pane series.
    let mut before = Vec::new();
    for (tag, pane) in order.iter().enumerate() {
        let revision = controller.publish(cache_key(pane, tag as u64), pane_values(tag as f64));
        controller.complete(pane);
        before.push((pane.clone(), tag as u64, revision));
    }
    assert!(controller.ready().is_empty(), "full run drains the plan");
    // Scrub back and re-run the suffix with bit-identical content.
    let suffix = controller.rewind_to("mid").expect("marked rewind runs");
    assert_eq!(suffix.len(), 3);
    for (pane, tag, revision) in &before {
        if suffix.contains(pane) {
            let rerun = controller.publish(cache_key(pane, *tag), pane_values(*tag as f64));
            assert_eq!(rerun, *revision, "{pane} backdates on identical content");
        }
    }
    // Catch-up waves drain in plan order from the mark.
    let mut waves = Vec::new();
    while !controller.ready().is_empty() {
        let wave = controller.ready();
        for pane in &wave {
            controller.complete(pane);
        }
        waves.push(wave);
    }
    assert_eq!(waves.concat().len(), suffix.len(), "catch-up replays every suffix pane once");
    let mut replayed = waves.concat();
    replayed.sort();
    let mut expected = suffix.clone();
    expected.sort();
    assert_eq!(replayed, expected, "catch-up replays exactly the suffix set");
    // The identity proof is not vacuous: changed content bumps the revision.
    let changed = controller.publish(cache_key("mid", 1), pane_values(900.0));
    let held = controller.revision_of(cache_key("mid", 1)).expect("published");
    assert_eq!(changed, held);
    assert!(
        before
            .iter()
            .find(|(pane, _, _)| pane == "mid")
            .is_none_or(|(_, _, revision)| changed != *revision),
        "changed content must move the revision"
    );
}

#[test]
fn with_plan_reuses_a_built_topology() {
    let mut plan = PanePlan::new();
    plan.register_node("only", &[], 0).expect("registers");
    let controller = ReplayController::with_plan(plan);
    assert_eq!(controller.plan().order().expect("orders"), vec!["only".to_owned()]);
    assert_eq!(ReplayController::default().plan().order().expect("empty orders").len(), 0);
}

#[test]
fn replay_state_renders_from_schema_and_validates() {
    let state = ReplayState::new().cursor(3).total_bars(8).mark("half".to_owned()).build();
    let schema = state.describe();
    for field in ["cursor", "total_bars", "mark"] {
        assert!(schema["properties"].get(field).is_some(), "schema lists {field}");
    }
    let mut state = state;
    state.set_field("cursor", "5").expect("cursor writes by name");
    assert_eq!(state.get_field("cursor").expect("cursor reads"), "5");
    assert!(state.get_field("nope").is_err());

    let ctx = WorkContext::default();
    assert!(state.execute(&ctx).is_ok(), "cursor inside the window executes");
    let past = ReplayState::new().cursor(9).total_bars(8).mark("half".to_owned()).build();
    assert!(past.execute(&ctx).is_err(), "cursor past the window refuses");
    let blank = ReplayState::new().cursor(0).total_bars(8).mark("  ".to_owned()).build();
    assert!(blank.execute(&ctx).is_err(), "blank mark refuses");
}

#[test]
fn replay_state_is_a_component_without_hand_rolled_downcasts() {
    let state: Arc<dyn Component> =
        Arc::new(ReplayState::new().cursor(1).total_bars(4).mark("q1".to_owned()).build());
    assert!(fluent_wvr::component_downcast_ref::<ReplayState>(&*state).is_some());
    let pane: Arc<dyn Component> =
        Arc::new(PaneConfig::new().name("price".to_owned()).build());
    assert!(fluent_wvr::component_downcast_ref::<ReplayState>(&*pane).is_none());
}
