use super::*;
use crate::topology::{HEALTH_MA_NODE, HEALTH_RIBBON_NODE, RIBBON_NODE};
use protopyne_kernels::indicator::ParamSet;

fn pane_params(tag: i64) -> ParamSet {
    ParamSet::new().with_int("length", tag)
}

fn pane_values(fill: f64) -> Vec<Option<f64>> {
    vec![None, Some(fill), Some(fill + 1.0), None, Some(fill + 2.0)]
}

fn complete_all(controller: &mut fluent_egui::ReplayController) {
    let order = controller.plan().order().expect("plan orders");
    for pane in &order {
        controller.complete(pane);
    }
}

#[test]
fn checkpoint_goldens_mark_rewind_and_refuse() {
    let mut controller = standard_replay().expect("standard replay builds");
    let order = controller.plan().order().expect("plan orders");
    assert_eq!(order.len(), 9, "standard topology holds nine panes");
    let position = |name: &str| order.iter().position(|pane| pane == name).expect("pane known");
    assert!(position(ADAPTIVE_NODE) < position(SIGNAL_NODE));
    assert!(position(SIGNAL_NODE) < position(RIBBON_NODE));
    assert!(position(RIBBON_NODE) < position(HEALTH_RIBBON_NODE));

    controller.mark(RIBBON_NODE).expect("known pane marks");
    assert!(controller.is_checkpoint(RIBBON_NODE));
    assert!(controller.mark("nope").is_err(), "unknown pane refuses the mark");
    assert!(controller.rewind_to("nope").is_err(), "unknown mark refuses the rewind");

    complete_all(&mut controller);
    let suffix = controller.rewind_to(RIBBON_NODE).expect("marked rewind runs");
    assert_eq!(suffix, vec![RIBBON_NODE, HEALTH_RIBBON_NODE, HEALTH_MA_NODE]);
    assert!(!controller.is_checkpoint(RIBBON_NODE), "rewind consumes the mark");
    assert_eq!(
        controller.ready(),
        vec![RIBBON_NODE.to_owned(), HEALTH_MA_NODE.to_owned()],
        "catch-up opens with every unblocked suffix pane (health:ma keys off price alone)"
    );
}

#[test]
fn rewind_rerun_reproduces_identical_revisions() {
    let mut controller = standard_replay().expect("standard replay builds");
    let order = controller.plan().order().expect("plan orders");
    controller.mark(RIBBON_NODE).expect("mark records");
    // First run: complete every pane in plan order, publishing per-pane series.
    let mut before = Vec::new();
    for (tag, pane) in order.iter().enumerate() {
        let revision =
            controller.publish(cache_key_for(pane, &pane_params(tag as i64)), pane_values(tag as f64));
        controller.complete(pane);
        before.push((pane.clone(), tag as i64, revision));
    }
    assert!(controller.ready().is_empty(), "full run drains the plan");
    // Scrub back and re-run the suffix with bit-identical content.
    let suffix = controller.rewind_to(RIBBON_NODE).expect("marked rewind runs");
    assert_eq!(suffix.len(), 3);
    for (pane, tag, revision) in &before {
        if suffix.contains(pane) {
            let rerun =
                controller.publish(cache_key_for(pane, &pane_params(*tag)), pane_values(*tag as f64));
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
    let changed =
        controller.publish(cache_key_for(RIBBON_NODE, &pane_params(6)), pane_values(900.0));
    let held =
        controller.revision_of(cache_key_for(RIBBON_NODE, &pane_params(6))).expect("published");
    assert_eq!(changed, held);
    assert!(
        before
            .iter()
            .find(|(pane, _, _)| pane == RIBBON_NODE)
            .is_none_or(|(_, _, revision)| changed != *revision),
        "changed content must move the revision"
    );
}
