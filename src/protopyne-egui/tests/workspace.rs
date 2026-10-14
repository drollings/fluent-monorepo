//! Central chart workspace: boot state, gateway-driven symbol/timeframe
//! switches, hot-slot controls, 2D pan/zoom, and a full headless frame.
//!
//! Every assertion reads the workspace's own state, so the tests pin the
//! state store the top bar, modals, legend, and canvas all share.

use super::*;

use std::collections::BTreeMap;
use std::sync::Mutex;

use protopyne_view::{
    ArmResponse, ChartBar, GatewayError, HealthView, JournalRowView, MockGateway, OverlaySeries,
    PortfolioView, ProposalView, ReplayView, WorstCase,
};

/// A gateway that scripts distinct bar counts per symbol, so a switch's
/// "pulled fresh history" claim is observable, and records every
/// `chart_bars_for` call.
struct ScriptGateway {
    calls: Mutex<Vec<(String, String)>>,
}

impl ScriptGateway {
    fn new() -> Self {
        Self { calls: Mutex::new(Vec::new()) }
    }

    fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().expect("call log locks").clone()
    }
}

fn unavailable() -> GatewayError {
    GatewayError::Unavailable("script gateway".to_owned())
}

fn script_bars(count: usize) -> ChartBars {
    let rows = (0..count)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartBar {
                time: i as i64,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 1_000 + i as u64,
            }
        })
        .collect();
    ChartBars { rows, dropped: 0 }
}

impl TerminalGateway for ScriptGateway {
    fn health(&self) -> Result<HealthView, GatewayError> {
        Err(unavailable())
    }

    fn proposals(&self) -> Result<Vec<ProposalView>, GatewayError> {
        Err(unavailable())
    }

    fn portfolio(&self) -> Result<PortfolioView, GatewayError> {
        Err(unavailable())
    }

    fn journal(&self) -> Result<Vec<JournalRowView>, GatewayError> {
        Err(unavailable())
    }

    fn chart_bars(&self) -> Result<ChartBars, GatewayError> {
        Err(unavailable())
    }

    fn chart_overlays(&self, _kind: &str, _period: i64) -> Result<OverlaySeries, GatewayError> {
        Err(unavailable())
    }

    fn replay(&self) -> Result<ReplayView, GatewayError> {
        Err(unavailable())
    }

    fn preview_arm(&self, _patch: serde_json::Value) -> Result<WorstCase, GatewayError> {
        Err(unavailable())
    }

    fn grant_tiers(&self) -> Result<BTreeMap<String, String>, GatewayError> {
        Err(unavailable())
    }

    fn arm(&self, _grant: serde_json::Value) -> Result<ArmResponse, GatewayError> {
        Err(unavailable())
    }

    fn chart_bars_for(&self, symbol: &str, timeframe: &str) -> Result<ChartBars, GatewayError> {
        self.calls
            .lock()
            .expect("call log locks")
            .push((symbol.to_owned(), timeframe.to_owned()));
        let count = match symbol {
            "GLD" => 200,
            "SPY" => 240,
            _ => 120,
        };
        Ok(script_bars(count))
    }
}

#[test]
fn workspace_boots_with_gld_default_and_hot_slotted_integration_lite_and_fast_mas() {
    let gateway = MockGateway::new();
    let workspace = ChartWorkspace::with_gateway(&gateway).expect("the workspace boots");

    assert_eq!(workspace.active_symbol(), "GLD", "the top bar defaults to the GLD ticker");
    assert_eq!(workspace.timeframe(), TimeframeKey::M15, "the default timeframe is 15M");
    let ids = workspace.slot_ids();
    assert!(ids.contains(&"integration_lite".to_owned()), "IntegrationLite is hot-slotted");
    assert!(ids.contains(&"fast_mas".to_owned()), "Fast MAs is hot-slotted");
    assert!(!workspace.rows().is_empty(), "the gateway supplied the first history");
    assert!(
        workspace.cached_series() > 0,
        "the DAG plan evaluated into the pane cache"
    );
    assert!(!workspace.plan_order().expect("plan orders").is_empty(), "the plan holds nodes");

    // The symbol catalog spans every asset class and is searchable.
    let mut catalog = build_symbol_catalog().expect("the symbol catalog builds");
    catalog.set_category(Some(AssetClass::Crypto));
    let crypto: Vec<String> =
        catalog.filtered().into_iter().map(|entry| entry.id().to_owned()).collect();
    assert_eq!(crypto, vec!["BTC-USD".to_owned()], "only crypto shows under the Crypto tab");
    assert_eq!(AssetClass::All.label(), "All");
    assert_eq!(AssetClass::all().len(), 5, "five symbol tabs");
}

#[test]
fn workspace_symbol_and_timeframe_switches_pull_from_gateway_and_rebind_2d_interaction() {
    let gateway = ScriptGateway::new();
    let mut workspace = ChartWorkspace::with_gateway(&gateway).expect("the workspace boots");
    let gld_rows = workspace.rows().len();

    // Pan the time axis, then prove the switch resets the 2D bounds.
    workspace.step_interaction(
        1.0 / 60.0,
        &[PointerInput::Pan2D { dx_px: 30.0, dy_px: 0.0 }],
    );
    assert!(
        workspace.main_viewport().expect("main viewport").x_offset_px().abs() > f32::EPSILON,
        "the pan moved the time axis"
    );

    workspace.switch_symbol("SPY", &gateway).expect("a symbol switch pulls fresh history");
    assert_eq!(workspace.active_symbol(), "SPY", "the symbol switched");
    assert_ne!(workspace.rows().len(), gld_rows, "SPY pulled a different history");
    assert!(
        workspace.main_viewport().expect("main viewport").x_offset_px().abs() <= f32::EPSILON,
        "the switch reset the 2D interaction bounds"
    );

    workspace.switch_timeframe(TimeframeKey::M5, &gateway).expect("a timeframe switch pulls");
    assert_eq!(workspace.timeframe(), TimeframeKey::M5, "the timeframe switched");

    let calls = gateway.calls();
    assert!(calls.iter().any(|(symbol, _)| symbol == "SPY"), "SPY was pulled: {calls:?}");
    assert!(calls.iter().any(|(_, timeframe)| timeframe == "5M"), "5M was pulled: {calls:?}");
}

#[test]
fn workspace_hot_slot_eye_and_bolt_controls_and_2d_pan_zoom_work_headlessly() {
    let gateway = ScriptGateway::new();
    let mut workspace = ChartWorkspace::with_gateway(&gateway).expect("the workspace boots");

    // Eye toggles one slot instantly.
    assert_eq!(workspace.slot_visible("fast_mas"), Some(true));
    workspace.toggle_visibility("fast_mas").expect("the eye hides");
    assert_eq!(workspace.slot_visible("fast_mas"), Some(false));
    workspace.toggle_visibility("fast_mas").expect("the eye shows");
    assert_eq!(workspace.slot_visible("fast_mas"), Some(true));

    // Bolt opens one slot's isolated settings; the tabbed edit is live.
    workspace.open_settings("integration_lite").expect("the bolt opens");
    workspace
        .set_settings_tab("integration_lite", SettingsTab::Inputs)
        .expect("the Inputs tab selects");
    workspace
        .set_slot_field("integration_lite", "eit_alpha", "0.2")
        .expect("the field edits");

    // 2D pan and cursor-anchored wheel zoom lock the sub-pane time axis.
    workspace.step_interaction(
        1.0 / 60.0,
        &[PointerInput::Pan2D { dx_px: 30.0, dy_px: -6.0 }],
    );
    workspace.step_interaction(
        1.0 / 60.0,
        &[PointerInput::WheelTime { delta_px: 40.0, anchor_x: 160.0 }],
    );
    let main = workspace.main_viewport().expect("main viewport");
    let sub = workspace.sub_viewport().expect("sub viewport");
    assert!(
        (sub.x_offset_px() - main.x_offset_px()).abs() <= f32::EPSILON,
        "the sub-pane time axis locks to the main chart area"
    );
    assert!(
        (sub.scale().bar_spacing() - main.scale().bar_spacing()).abs() <= f32::EPSILON,
        "bar spacing stays locked across the panes"
    );

    // A full frame renders headlessly.
    egui::__run_test_ui(|ui| {
        paint_workspace(ui, &mut workspace, Some(&gateway)).expect("the workspace paints");
    });
}
