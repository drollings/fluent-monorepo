//! Central chart workspace: boot state, gateway-driven symbol/timeframe
//! switches, hot-slot controls, 2D pan/zoom, and a full headless frame.
//!
//! Every assertion reads the workspace's own state, so the tests pin the
//! state store the top bar, modals, legend, and canvas all share.

use super::*;

use std::collections::BTreeMap;
use std::sync::Mutex;

use protopyne_chart_model::ChartRow;
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

#[test]
fn pane_layout_reserves_the_price_axis_and_time_strip() {
    let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(640.0, 320.0));
    let same = |a: f32, b: f32| a.to_bits() == b.to_bits();

    let main = PaneLayout::split(rect, true);
    assert!(same(main.plot.right(), rect.right() - AXIS_WIDTH), "the plot leaves room for the axis");
    assert!(
        same(main.plot.bottom(), rect.bottom() - TIME_STRIP),
        "the plot leaves room for time labels"
    );
    assert!(same(main.axis.left(), main.plot.right()), "the axis starts where the plot ends");
    let time = main.time.expect("the price pane reserves a time strip");
    assert!(same(time.top(), rect.bottom() - TIME_STRIP));
    assert!(same(time.left(), rect.left()));
    assert!(same(time.right(), rect.right()), "the time strip spans the full pane width");

    let sub = PaneLayout::split(rect, false);
    assert!(same(sub.plot.bottom(), rect.bottom()), "the sub-pane has no time strip");
    assert!(sub.time.is_none());
}

#[test]
fn crosshair_overlay_paints_when_the_pointer_is_in_the_plot() {
    egui::__run_test_ui(|ui| {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(640.0, 320.0));
        let layout = PaneLayout::split(rect, true);
        let viewport = Viewport::new(
            PriceScaleMode::Normal,
            90.0,
            110.0,
            PRICE_PLOT_HEIGHT,
            8.0,
            100.0,
            false,
        )
        .expect("valid viewport");
        let rows = ramp_rows(20);
        let hover = egui::pos2(layout.plot.left() + 32.0, layout.plot.top() + 40.0);
        let local = (hover.x - layout.plot.left(), hover.y - layout.plot.top());
        assert!(
            crosshair_data(local, &viewport, rows.len()).is_some(),
            "the pointer reads a bar and a price"
        );

        let painter = ui.painter_at(rect);
        paint_crosshair_overlay(
            &painter,
            &painter,
            viewport,
            &rows,
            layout,
            hover,
            &ThemeConfig::default(),
        )
        .expect("the crosshair paints");
    });
}

/// A ramp of `count` distinct bars; distinct counts make a switch observable.
fn ramp_rows(count: usize) -> Vec<ChartRow> {
    (0..count)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: i as i64,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

#[test]
fn seeded_timeframes_switch_the_loaded_candles_and_resample_coarser_keys() {
    let mut workspace = ChartWorkspace::new().expect("the workspace boots");
    workspace.seed_history(TimeframeKey::M1, ramp_rows(20)).expect("1M seeds");
    workspace.seed_history(TimeframeKey::M5, ramp_rows(12)).expect("5M seeds");
    workspace.seed_history(TimeframeKey::M15, ramp_rows(8)).expect("15M selects on seed");

    assert_eq!(workspace.timeframe(), TimeframeKey::M15, "the default timeframe is selected");
    assert_eq!(workspace.rows().len(), 8, "the seeded 15M history is active");

    workspace.select_timeframe(TimeframeKey::M1).expect("1M selects");
    assert_eq!(workspace.rows().len(), 20, "1M loads its own candles, not the 15M set");
    workspace.select_timeframe(TimeframeKey::M5).expect("5M selects");
    assert_eq!(workspace.rows().len(), 12, "5M loads its own candles");

    workspace.select_timeframe(TimeframeKey::H1).expect("1H resamples the 15M slot");
    assert_eq!(workspace.timeframe(), TimeframeKey::H1);
    assert_eq!(workspace.rows().len(), 2, "8 fifteen-minute rows fold into 2 hourly bars");

    let mut bare = ChartWorkspace::new().expect("the workspace boots");
    assert!(
        bare.select_timeframe(TimeframeKey::M1).is_err(),
        "an unseeded slot refuses rather than serving another timeframe"
    );
}

/// The two pane rects the wheel routes over: a price pane with its axis and
/// time strip, and the oscillator pane below it with its own axis.
fn wheel_layouts() -> (PaneLayout, PaneLayout) {
    let width = ui_width();
    let main = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(width, PRICE_PANE_HEIGHT));
    let sub = egui::Rect::from_min_size(
        egui::pos2(0.0, PRICE_PANE_HEIGHT),
        egui::vec2(width, SUB_PANE_HEIGHT),
    );
    (PaneLayout::split(main, true), PaneLayout::split(sub, false))
}

/// The pane width the workspace uses for a headless frame.
fn ui_width() -> f32 {
    640.0
}

/// The zones the canvas builds, mirroring `step_from_response`.
fn chart_zones(
    layout: &PaneLayout,
    sub: &PaneLayout,
    list: Option<fluent_egui::WheelRect>,
) -> WheelZones {
    WheelZones::new(
        vec![
            PaneZones { plot: layout.plot.into(), axis: layout.axis.into() },
            PaneZones { plot: sub.plot.into(), axis: sub.axis.into() },
        ],
        list,
    )
}

#[test]
fn the_legend_sits_clear_of_the_right_price_axis() {
    let (layout, _) = wheel_layouts();
    let card_width = 168.0;
    let (x, y) = legend_origin(layout.plot.union(layout.axis), card_width);
    assert!(
        x + card_width <= layout.axis.left(),
        "the legend ends before the price strip starts: {} + {card_width} > {}",
        x,
        layout.axis.left()
    );
    assert!(y >= layout.plot.top(), "the legend hangs from the pane's top edge");
}

#[test]
fn the_oscillator_axis_wheel_zooms_only_the_oscillators_price_range() {
    let (layout, sub_layout) = wheel_layouts();
    let mut workspace = ChartWorkspace::new().expect("the workspace boots");
    workspace.seed_history(TimeframeKey::M1, ramp_rows(40)).expect("history seeds");
    workspace.select_timeframe(TimeframeKey::M1).expect("M1 selects and rebinds the viewports");

    let before_main = workspace.main_viewport().expect("main viewport").visible_range();
    let before_sub = workspace.sub_viewport().expect("sub viewport").visible_range();
    let before_spacing = workspace.main_viewport().expect("main viewport").scale().bar_spacing();

    // A pointer over the oscillator's own price labels.
    let hover = egui::pos2(sub_layout.axis.center().x, sub_layout.axis.center().y);
    let target = chart_zones(&layout, &sub_layout, None).route(hover.into());
    assert_eq!(
        target,
        Some(WheelTarget::Price { pane: SUB_PANE_ZONE, anchor_y: hover.y - sub_layout.plot.top() }),
        "the oscillator's strip routes to its own price axis"
    );

    if let Some(WheelTarget::Price { pane, anchor_y }) = target {
        workspace.zoom_pane_price(pane, 50.0, anchor_y);
    }

    let after_sub = workspace.sub_viewport().expect("sub viewport").visible_range();
    assert!(
        after_sub.max - after_sub.min < before_sub.max - before_sub.min,
        "the oscillator window narrowed: {} -> {}",
        before_sub.max - before_sub.min,
        after_sub.max - after_sub.min
    );
    assert_eq!(
        workspace.main_viewport().expect("main viewport").visible_range(),
        before_main,
        "the price pane's window is untouched"
    );
    assert!(
        unchanged(
            workspace.main_viewport().expect("main viewport").scale().bar_spacing(),
            before_spacing
        ),
        "the shared time axis is untouched"
    );
}

#[test]
fn an_oscillator_zoom_out_stops_at_the_panes_design_band() {
    let (_, sub_layout) = wheel_layouts();
    let mut workspace = ChartWorkspace::new().expect("the workspace boots");
    workspace.seed_history(TimeframeKey::M1, ramp_rows(40)).expect("history seeds");
    workspace.select_timeframe(TimeframeKey::M1).expect("M1 selects and rebinds the viewports");
    let anchor = sub_layout.plot.height() / 2.0;

    for _ in 0..200 {
        workspace.zoom_pane_price(SUB_PANE_ZONE, -500.0, anchor);
    }
    let range = workspace.sub_viewport().expect("sub viewport").visible_range();
    let base = SUB_MAX - SUB_MIN;
    assert!(
        (range.max - range.min - base).abs() < 1e-6,
        "zooming out never exceeds the pane's own band: {}",
        range.max - range.min
    );

    for _ in 0..400 {
        workspace.zoom_pane_price(SUB_PANE_ZONE, 500.0, anchor);
    }
    let tight = workspace.sub_viewport().expect("sub viewport").visible_range();
    assert!(
        tight.max - tight.min >= base * SUB_MIN_SPAN_FRACTION,
        "zooming in stops above the legibility rail: {}",
        tight.max - tight.min
    );
}

#[test]
fn the_price_axis_wheel_is_answered_by_the_interaction_machine() {
    let (layout, _) = wheel_layouts();
    let hover = egui::pos2(layout.axis.center().x, layout.axis.center().y);
    let target = chart_zones(&layout, &layout, None).route(hover.into());
    assert_eq!(
        target,
        Some(WheelTarget::Price { pane: 0, anchor_y: hover.y - layout.plot.top() }),
        "the price pane's strip routes to pane 0, which the machine steps"
    );
}

#[test]
fn a_wheel_over_the_legend_never_moves_the_chart() {
    let (layout, sub_layout) = wheel_layouts();
    let mut workspace = ChartWorkspace::new().expect("the workspace boots");
    workspace.seed_history(TimeframeKey::M1, ramp_rows(40)).expect("history seeds");
    workspace.select_timeframe(TimeframeKey::M1).expect("M1 selects and rebinds the viewports");
    workspace.stack.set_origin((layout.plot.right() - 180.0, layout.plot.top() + 8.0));

    let before_main = workspace.main_viewport().expect("main viewport").visible_range();
    let before_spacing = workspace.main_viewport().expect("main viewport").scale().bar_spacing();
    let before_scroll = workspace.stack.scroll_offset();

    let list = workspace.stack.list_rect(layout.plot.height()).expect("a list to scroll");
    let zones = chart_zones(&layout, &sub_layout, Some(list.into()));
    assert_eq!(zones.route(egui::pos2(list.center().x, list.center().y).into()), Some(WheelTarget::List));

    // The gesture the router selects: scroll the list, chart untouched.
    if let Some(WheelTarget::List) = zones.route(egui::pos2(list.center().x, list.center().y).into()) {
        workspace.stack.scroll_by(-40.0, layout.plot.height());
    }

    // The default legend fits inside the pane, so the scroll offset correctly
    // stays put; what matters is that the gesture was claimed by the list
    // rather than reaching the chart.
    assert!(
        unchanged(workspace.stack.scroll_offset(), before_scroll),
        "a legend that fits the pane has nothing to scroll"
    );
    assert!(
        workspace.stack.content_height() <= layout.plot.height(),
        "the premise: the default legend column fits the chart pane"
    );
    assert_eq!(workspace.main_viewport().expect("main viewport").visible_range(), before_main, "price held");
    assert!(
        unchanged(
            workspace.main_viewport().expect("main viewport").scale().bar_spacing(),
            before_spacing
        ),
        "the time axis held"
    );
}

/// Bit-exact equality: an "untouched" assertion must not forgive a drift, so
/// it compares the representation rather than a tolerance.
fn unchanged(actual: f32, expected: f32) -> bool {
    actual.to_bits() == expected.to_bits()
}
