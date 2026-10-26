//! Backtest form, run control, and results panel: one envelope per submit,
//! a refused run keeps the model, and the panel paints headlessly.

use crate::common_fakes::FakeGateway as CannedBacktest;

use super::{paint_backtest_panel, BacktestAction, BacktestFormState, BacktestModel};

fn canned_schema() -> serde_json::Value {
    serde_json::json!({
        "indicator": {"properties": {
            "session": {"type": "string", "description": "Session window"},
            "session_filter": {"type": "boolean", "description": "Filter by session"},
        }},
        "entry": {"properties": {
            "setup_window": {"type": "integer", "description": "Bars a trigger stays armed"},
        }},
        "exit": {"properties": {
            "stop_atr_mult": {"type": "number", "description": "Stop multiple"},
        }},
        "risk": {"properties": {
            "risk_pct": {"type": "number", "description": "Risk fraction"},
        }},
        "session": {"properties": {
            "filter": {"type": "boolean", "description": "Session filter armed"},
        }},
        "display": {"properties": {
            "enabled": {"type": "boolean", "description": "Master switch"},
        }},
        "triggers": ["momentum"],
        "filters": ["ribbon"],
        "sizers": ["equity"],
        "strategies": ["momentum"],
        "defaults": {
            "indicator": {"session": "0930-1600", "session_filter": true},
            "entry": {"setup_window": 5},
            "exit": {"stop_atr_mult": 3.0},
            "risk": {"risk_pct": 1.0},
            "session": {"filter": true},
            "display": {"enabled": true},
            "legacy": {},
        },
    })
}

#[test]
fn the_form_lifts_sections_and_leaves_from_the_schema() {
    let form = BacktestFormState::from_schema(&canned_schema());
    assert_eq!(
        form.sections(),
        vec!["indicator", "entry", "exit", "risk", "session", "display"]
    );
    let paths: Vec<&str> = form
        .fields()
        .iter()
        .map(|field| field.path.as_str())
        .collect();
    assert!(paths.contains(&"indicator.session"));
    assert!(paths.contains(&"indicator.session_filter"));
    assert!(paths.contains(&"entry.setup_window"));
    assert!(paths.contains(&"exit.stop_atr_mult"));
    assert_eq!(form.draft_of("entry.setup_window"), "");
    assert_eq!(form.seed_text(), "7");
}

#[test]
fn a_refused_field_sends_nothing_and_keeps_its_draft() {
    let gateway = CannedBacktest::backtest();
    let mut form = BacktestFormState::from_schema(&canned_schema());
    form.set_draft("entry.setup_window", "abc".to_owned());
    let refused = form
        .submit(&gateway)
        .expect_err("an integer leaf refuses text");
    assert!(
        refused.to_string().contains("entry.setup_window"),
        "the refusal names the field: {refused}"
    );
    assert!(gateway.envelopes().is_empty(), "nothing was sent");
    assert_eq!(
        form.draft_of("entry.setup_window"),
        "abc",
        "the refused row keeps its draft"
    );
}

#[test]
fn submit_sends_one_envelope_and_stores_its_reference() {
    let gateway = CannedBacktest::backtest();
    let mut form = BacktestFormState::from_schema(&canned_schema());
    form.set_draft("entry.setup_window", "9".to_owned());
    let reference = form.submit(&gateway).expect("a drafted submit runs");
    assert_eq!(reference.run_id, "backtest-07");
    let envelopes = gateway.envelopes();
    assert_eq!(envelopes.len(), 1, "submit sends one envelope");
    assert_eq!(envelopes[0].seed, 7);
    assert_eq!(envelopes[0].initial_equity, "100000");
    assert_eq!(envelopes[0].spec["entry"]["setup_window"], 9);
    assert_eq!(
        envelopes[0].spec["indicator"]["session"], "0930-1600",
        "undrafted leaves ride the schema defaults"
    );
    assert_eq!(form.last_run().expect("stored").run_id, "backtest-07");
}

#[test]
fn a_refused_run_keeps_the_previous_model() {
    let gateway = CannedBacktest::refusing_runs();
    let mut form = BacktestFormState::from_schema(&canned_schema());
    let before = BacktestModel::pull(&gateway, "backtest-01");
    assert!(before.view().is_some(), "the canned state reads");
    let mut model = Some(before);
    let refused = BacktestAction::Run.apply(&gateway, &mut form, &mut model);
    assert!(refused.is_err(), "the refused run surfaces its reason");
    let held = model.expect("the previous model is kept");
    assert_eq!(held.run_id(), "backtest-01");
    assert!(held.view().is_some());
}

#[test]
fn refresh_pulls_and_stop_clears_through_the_panel() {
    let gateway = CannedBacktest::backtest();
    let mut model = Some(BacktestModel::pull(&gateway, "backtest-07"));
    BacktestAction::Refresh("backtest-07".to_owned())
        .apply(
            &gateway,
            &mut BacktestFormState::from_schema(&canned_schema()),
            &mut model,
        )
        .expect("refresh reads");
    assert_eq!(model.as_ref().expect("model").run_id(), "backtest-07");
    BacktestAction::Stop("backtest-07".to_owned())
        .apply(
            &gateway,
            &mut BacktestFormState::from_schema(&canned_schema()),
            &mut model,
        )
        .expect("stop drops");
    assert!(model.is_none(), "a stopped run leaves no stale model");
}

#[test]
fn the_backtest_panel_paints_headlessly() {
    let gateway = CannedBacktest::backtest();
    let mut form = BacktestFormState::from_schema(&canned_schema());
    let mut model = Some(BacktestModel::pull(&gateway, "backtest-07"));
    egui::__run_test_ui(|ui| {
        paint_backtest_panel(ui, &mut form, &mut model, Some(&gateway));
    });
    assert!(model.is_some(), "painting sends nothing by itself");
}

#[test]
fn run_params_refuse_locally_before_anything_is_sent() {
    let gateway = CannedBacktest::backtest();
    let mut form = BacktestFormState::from_schema(&canned_schema());
    form.set_seed_text("seven".to_owned());
    let refused = form.submit(&gateway).expect_err("a text seed refuses");
    assert!(refused.to_string().contains("seed"), "{refused}");
    assert!(gateway.envelopes().is_empty());
    let mut form = BacktestFormState::from_schema(&canned_schema());
    form.set_start_text("not-a-time".to_owned());
    let refused = form.submit(&gateway).expect_err("a text start refuses");
    assert!(refused.to_string().contains("start"), "{refused}");
    assert!(gateway.envelopes().is_empty());
}
