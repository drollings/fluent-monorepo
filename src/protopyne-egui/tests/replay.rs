//! Terminal replay: the scrub over the standard topology, plus the pane
//! replay it drives.
//!
//! The suite pins two things separately on purpose. The pane-replay tests
//! exercise the controller directly (marks, rewinds, catch-up waves,
//! revisions). The scrub tests exercise the seam: a core-supplied window, a
//! bar cursor, and the panes a move makes due.

use super::*;

use std::collections::HashMap;
use std::path::PathBuf;

use protopyne_view::MockGateway;

use crate::topology::{
    ADAPTIVE_NODE, HEALTH_MA_NODE, HEALTH_RIBBON_NODE, PRICE_NODE, RIBBON_NODE, SIGNAL_NODE,
    VOLUME_NODE,
};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

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

// ---------------------------------------------------------------------------
// The scrub seam
// ---------------------------------------------------------------------------

/// A scrub over the standard topology with the mock gateway's window read.
fn scrubbed() -> ReplayScrub {
    let mut scrub = ReplayScrub::standard().expect("standard scrub builds");
    scrub.ingest(&MockGateway::new()).expect("the mock supplies a replay window");
    scrub
}

/// Parameters per pane, from the plan order, so a re-run publishes the same
/// parameters and any revision move can only come from the series changing.
fn pane_tags(scrub: &ReplayScrub) -> HashMap<String, i64> {
    scrub
        .controller()
        .plan()
        .order()
        .expect("plan orders")
        .into_iter()
        .enumerate()
        .map(|(tag, pane)| (pane, tag as i64))
        .collect()
}

/// Run every catch-up wave to completion, publishing each pane's series.
fn drain(scrub: &mut ReplayScrub) {
    let tags = pane_tags(scrub);
    while !scrub.ready().is_empty() {
        for pane in scrub.ready() {
            let tag = tags[&pane];
            scrub
                .settle(&pane, &pane_params(tag), pane_values(tag as f64))
                .expect("a wave pane settles");
        }
    }
    assert!(scrub.settled(), "a drained scrub owes no pane");
}

/// Every pane's published revision. A pane with no series has no revision,
/// so the map says so rather than inventing one.
fn revisions(scrub: &ReplayScrub) -> HashMap<String, Option<u64>> {
    let tags = pane_tags(scrub);
    tags.iter().map(|(pane, tag)| (pane.clone(), scrub.revision_of(pane, &pane_params(*tag)))).collect()
}

fn plan_order(scrub: &ReplayScrub) -> Vec<String> {
    scrub.controller().plan().order().expect("plan orders")
}

/// The applied position as a comparable triple. `ReplayState` is a control
/// value with no domain equality to carry, so its three fields are what a
/// test compares.
fn position(scrub: &ReplayScrub) -> (u64, u64, String) {
    let state = scrub.state();
    (state.cursor, state.total_bars, state.mark.clone())
}

#[test]
fn the_scrub_stands_where_the_core_read_puts_it() {
    let scrub = scrubbed();
    assert_eq!(scrub.state().total_bars, 3, "the window is the core's, not a guess");
    assert_eq!(scrub.state().cursor, 3, "a fresh read stands at the head");
    assert_eq!(scrub.state().mark, "live", "the untouched position keeps its live name");
    let window = scrub.window().expect("the adopted window is readable");
    assert_eq!(window.steps.len(), 3);
    assert!(
        window.steps.iter().any(|step| step.checkpoint) && window.steps.iter().any(|step| !step.checkpoint),
        "the core's own anchors stay distinct from the terminal's mark"
    );
}

#[test]
fn a_moved_cursor_makes_the_whole_topology_due_in_plan_order() {
    let mut scrub = scrubbed();
    drain(&mut scrub);
    let outcome = scrub.scrub_to(1).expect("scrub inside the window");
    assert_eq!(
        outcome,
        ScrubOutcome::Rewound { cursor: 1, panes: plan_order(&scrub) },
        "every pane series is a function of the window, so a move makes the topology due \
         — in plan order, which is the order the catch-up waves run in"
    );
    assert_eq!(scrub.state().cursor, 1);
    assert_eq!(
        scrub.ready(),
        vec![PRICE_NODE.to_owned(), VOLUME_NODE.to_owned()],
        "the two window roots open the first catch-up wave"
    );
    assert_eq!(
        scrub.state().mark,
        PRICE_NODE,
        "the mark names the terminal's own rewind anchor, not a core bar anchor"
    );
}

#[test]
fn a_scrub_to_either_end_settles_with_every_revision_present() {
    let mut scrub = scrubbed();
    drain(&mut scrub);
    let first_run = revisions(&scrub);
    assert_eq!(first_run.len(), 9, "every pane published a series");
    assert!(first_run.values().all(|revision| revision.is_some_and(|held| held > 0)));

    for bar in [0, 3] {
        assert!(
            matches!(scrub.scrub_to(bar), Ok(ScrubOutcome::Rewound { .. })),
            "scrub to bar {bar} rewinds"
        );
        assert!(!scrub.settled(), "the move to bar {bar} made the topology due again");
        drain(&mut scrub);
        assert_eq!(
            revisions(&scrub),
            first_run,
            "re-running at bar {bar} reproduced every series, so every revision backdated"
        );
    }

    // The backdating above is not vacuous: a pane whose content really
    // changes moves its revision on the same key.
    let price = pane_tags(&scrub)[PRICE_NODE];
    let moved = scrub.settle(PRICE_NODE, &pane_params(price), pane_values(900.0)).expect("settles");
    assert_ne!(Some(moved), first_run[PRICE_NODE], "changed content moves the revision");
}

#[test]
fn a_no_op_scrub_backdates_instead_of_bumping_revisions() {
    let mut scrub = scrubbed();
    drain(&mut scrub);
    assert!(scrub.scrub_to(2).is_ok(), "the first scrub moves");
    drain(&mut scrub);
    let settled = revisions(&scrub);

    assert_eq!(
        scrub.scrub_to(2).expect("scrubbing to the bar already on screen is not a refusal"),
        ScrubOutcome::Held { cursor: 2 },
        "a cursor that does not move rewinds nothing"
    );
    assert!(scrub.settled(), "nothing is left pending after a held scrub");
    drain(&mut scrub);
    assert_eq!(
        revisions(&scrub),
        settled,
        "a held scrub publishes nothing, so no revision moved"
    );
}

#[test]
fn every_refusal_names_what_was_wrong_and_moves_nothing() {
    let mut scrub = ReplayScrub::standard().expect("standard scrub builds");
    assert_eq!(
        scrub.scrub_to(0).expect_err("nothing to scrub over"),
        ScrubRefusal::NoTimeline
    );
    assert!(scrub.last_refusal().is_some_and(|reason| reason.contains("no replay window read")));

    // Windows that disagree with themselves are refused whole: the scrub
    // indexes steps by bar, so a window whose steps are not the bars they
    // claim would answer with another bar's step.
    let good = MockGateway::new().replay().expect("mock window reads");
    for (label, view) in [
        ("bar count", ReplayView { total_bars: 2, ..good.clone() }),
        (
            "step order",
            ReplayView {
                steps: vec![
                    protopyne_view::ReplayStepView { bar_index: 1, ts: "t".to_owned(), done: true, checkpoint: false },
                    protopyne_view::ReplayStepView { bar_index: 2, ts: "t".to_owned(), done: true, checkpoint: false },
                    protopyne_view::ReplayStepView { bar_index: 3, ts: "t".to_owned(), done: true, checkpoint: false },
                ],
                ..good.clone()
            },
        ),
        ("cursor", ReplayView { cursor: 9, ..good.clone() }),
    ] {
        let refusal = scrub.apply_window(view.clone()).expect_err(label);
        assert!(
            matches!(refusal, ScrubRefusal::InconsistentWindow(_)),
            "{label} refuses as inconsistent, got: {refusal}"
        );
        assert!(scrub.window().is_none(), "{label} is not half-adopted");
    }

    scrub.ingest(&MockGateway::new()).expect("a consistent window is adopted");
    let refusal = scrub.scrub_to(4).expect_err("a bar past the window refuses");
    assert_eq!(refusal, ScrubRefusal::UnknownBar { bar: 4, total_bars: 3 });
    assert_eq!(scrub.state().cursor, 3, "a refused scrub moves nothing");
    assert!(scrub.last_refusal().is_some_and(|reason| reason.contains("bar 4")));

    assert!(
        scrub.rename("  ").is_err(),
        "a blank mark refuses: a position with no name is not a position"
    );
    assert_eq!(scrub.state().mark, "live", "the refused rename changed nothing");
    scrub.rename("session open").expect("a named position is accepted");
    assert_eq!(scrub.state().mark, "session open");
    assert_eq!(scrub.state().cursor, 3, "renaming is a label, not an axis");
}

#[test]
fn a_pane_settles_only_once_its_inputs_have() {
    let mut scrub = scrubbed();
    let refusal = scrub
        .settle(RIBBON_NODE, &pane_params(0), pane_values(1.0))
        .expect_err("the ribbon needs its smoothing pair first");
    assert!(
        matches!(&refusal, ScrubRefusal::NotSettleable { pane, reason }
            if pane == RIBBON_NODE && reason.contains("not settled")),
        "got: {refusal}"
    );
    let unknown =
        scrub.settle("nope", &pane_params(0), pane_values(1.0)).expect_err("unknown pane refuses");
    assert!(
        matches!(&unknown, ScrubRefusal::NotSettleable { reason, .. } if reason.contains("no such pane")),
        "got: {unknown}"
    );
    assert!(scrub.revision_of(RIBBON_NODE, &pane_params(0)).is_none(), "a refused settle publishes nothing");
    drain(&mut scrub);
    let ribbon_tag = pane_tags(&scrub)[RIBBON_NODE];
    assert!(
        scrub.revision_of(RIBBON_NODE, &pane_params(ribbon_tag)).is_some(),
        "a drained scrub published the ribbon under its own parameters"
    );
}

#[test]
fn a_refresh_widens_the_window_without_yanking_the_scrub_back() {
    let mut scrub = scrubbed();
    scrub.scrub_to(2).expect("the operator scrubs back from the head");
    let mut wider = MockGateway::new().replay().expect("mock window reads");
    wider.total_bars = 5;
    wider.steps.push(protopyne_view::ReplayStepView {
        bar_index: 3,
        ts: "2023-11-14T22:58:20+00:00".to_owned(),
        done: true,
        checkpoint: true,
    });
    wider.steps.push(protopyne_view::ReplayStepView {
        bar_index: 4,
        ts: "2023-11-14T23:13:20+00:00".to_owned(),
        done: true,
        checkpoint: false,
    });
    wider.cursor = 5;
    scrub.apply_window(wider.clone()).expect("a wider window is adopted");
    assert_eq!(
        scrub.state().cursor, 2,
        "a repeated read never moves the operator's position under them"
    );
    assert_eq!(scrub.state().total_bars, 5);

    // A window that no longer spans the applied cursor is refused whole:
    // the position moves when the operator moves it, or not at all. The
    // read's own cursor is legal here — it is the *applied* one that the
    // window would drop.
    let shrunk = ReplayView { cursor: 1, total_bars: 1, steps: wider.steps[..1].to_vec() };
    let refusal = scrub
        .apply_window(shrunk)
        .expect_err("a window that drops the applied cursor refuses");
    assert!(
        matches!(&refusal, ScrubRefusal::InconsistentWindow(reason)
            if reason.contains("the applied cursor 2")),
        "got: {refusal}"
    );
    assert_eq!(scrub.state().total_bars, 5, "the refused window is not adopted");
    assert_eq!(scrub.state().cursor, 2, "and the position never moved under the operator");
}

#[test]
fn an_edited_draft_scrubs_renames_and_refuses_what_it_cannot_act_on() {
    let mut scrub = scrubbed();
    let applied = scrub.state().clone();

    // Nothing edited: nothing happens, and no refusal is invented.
    assert_eq!(scrub.apply_draft(&applied).expect("an untouched draft applies"), None);
    assert_eq!(position(&scrub), (applied.cursor, applied.total_bars, applied.mark.clone()));

    // The window is the core's read; an editor that resizes it is refused
    // out loud rather than silently ignored.
    let resized = ReplayState::new()
        .cursor(applied.cursor)
        .total_bars(applied.total_bars + 4)
        .mark(applied.mark.clone())
        .build();
    assert_eq!(
        scrub.apply_draft(&resized).expect_err("the control may not resize the window"),
        ScrubRefusal::WindowNotResizable
    );
    assert_eq!(
        position(&scrub),
        (applied.cursor, applied.total_bars, applied.mark.clone()),
        "the refused draft moved nothing"
    );

    // A moved cursor scrubs; a renamed position does not recompute.
    let moved = ReplayState::new().cursor(0).total_bars(applied.total_bars).mark("open".to_owned()).build();
    let outcome = scrub.apply_draft(&moved).expect("a moved cursor applies").expect("it rewound");
    assert_eq!(outcome, ScrubOutcome::Rewound { cursor: 0, panes: plan_order(&scrub) });
    assert_eq!(scrub.state().mark, "open", "the draft's own mark survives the scrub");
    assert!(!scrub.settled(), "and the move still made the topology due");

    let settled_cursor = scrub.state().clone();
    let renamed =
        ReplayState::new().cursor(0).total_bars(settled_cursor.total_bars).mark("first bar".to_owned()).build();
    assert_eq!(scrub.apply_draft(&renamed).expect("a renamed position applies"), None);
    assert_eq!(scrub.state().mark, "first bar", "a label is not an axis");
    assert!(!scrub.settled(), "renaming left the scrub exactly as due as it was");
}

#[test]
fn the_scrub_control_reads_its_fields_from_the_cursor_schema() {
    // Non-comment lines of the scrub module: the no-hand-written-field-list
    // rule is pinned as text — no cursor field may appear as a code
    // literal. (`//` lines document but never depend, same rule as the
    // ledger scans.)
    let text = std::fs::read_to_string(manifest_dir().join("src").join("replay.rs"))
        .expect("the scrub module reads");
    let code: Vec<&str> =
        text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('/')).collect();
    assert!(!code.is_empty(), "the scrub module must exist");
    for field in ["cursor", "total_bars", "mark"] {
        let literal = format!("\"{field}\"");
        assert!(
            !code.iter().any(|line| line.contains(&literal)),
            "{field} must come from the cursor's schema, not a literal (line: {})",
            code.iter().find(|line| line.contains(&literal)).unwrap_or(&"")
        );
    }
    assert!(
        code.iter().any(|line| line.contains("paint_schema_fields")),
        "the control must render through the shared schema editor"
    );
}

#[test]
fn the_scrub_control_paints_headlessly_without_moving_the_cursor() {
    let mut scrub = scrubbed();
    egui::__run_test_ui(|ui| {
        assert_eq!(paint_replay_scrub(ui, &mut scrub), None, "an untouched frame moves nothing");
        assert_eq!(paint_replay_scrub(ui, &mut scrub), None, "and a second frame");
    });
    assert_eq!(scrub.state().cursor, 3);
    assert_eq!(scrub.state().mark, "live");
}