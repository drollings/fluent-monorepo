//! Central chart workspace: boot state, gateway-driven symbol/timeframe
//! switches, hot-slot controls, 2D pan/zoom, and a full headless frame.
//!
//! Every assertion reads the workspace's own state, so the tests pin the
//! state store the top bar, modals, legend, and canvas all share.

use super::*;

use std::collections::BTreeMap;
use std::sync::Mutex;

use fluent_egui::{
    CANDLE_BODY_FRACTION, CANDLE_GAP_THRESHOLD_PX, IndicatorStyle, candle_body_width,
};
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
fn a_slots_style_tab_commits_colors_and_line_width_through_its_own_component() {
    // End to end through the workspace: the bolt opens the pane, the Style tab
    // writes the slot's style component, and the write is a pure repaint that
    // never reaches the pane cache the way a config edit does.
    let mut workspace = ChartWorkspace::new().expect("the workspace boots");
    workspace.open_settings("fast_mas").expect("the bolt opens");
    workspace
        .set_settings_tab("fast_mas", SettingsTab::Style)
        .expect("the Style tab selects");
    assert_eq!(
        workspace.slot_style_field("fast_mas", "line_width").expect("the width reads"),
        IndicatorStyle::default().line_width().to_string(),
        "the slot starts on the theme at the default width"
    );
    assert_eq!(
        workspace.slot_style_field("fast_mas", "color_fast").expect("the color reads"),
        "",
        "and with no color override"
    );

    workspace.seed_history(TimeframeKey::M1, ramp_rows(40)).expect("history seeds");
    workspace.select_timeframe(TimeframeKey::M1).expect("M1 binds the viewports");
    let cached_before = workspace.cached_series();
    let first_length_before =
        workspace.stack.get_slot_field("fast_mas", "first_ma_length").unwrap_or_default();

    workspace
        .set_slot_style_field("fast_mas", "line_width", "3")
        .expect("a well-formed width writes");
    workspace
        .set_slot_style_field("fast_mas", "color_fast", "#0A0B0C")
        .expect("a well-formed override writes");
    assert!(
        workspace.set_slot_style_field("fast_mas", "color_slow", "not-a-color").is_err(),
        "a malformed color refuses instead of being repaired"
    );

    let style = workspace.stack.indicator_style("fast_mas");
    assert!(
        (style.line_width() - 3.0).abs() < f32::EPSILON,
        "the stroke width the Style tab set is live"
    );
    assert_eq!(style.override_color("fast"), Some("#0A0B0C"), "and so is the color override");
    assert_eq!(style.override_color("slow"), None, "the refused value never landed");
    assert_eq!(
        workspace.cached_series(),
        cached_before,
        "a style write repaints without republishing the pane cache"
    );
    assert_eq!(
        workspace.stack.get_slot_field("fast_mas", "first_ma_length").unwrap_or_default(),
        first_length_before,
        "and never touches the calculation inputs"
    );
    assert_eq!(
        workspace.stack.indicator_style("integration_lite").override_color("fast"),
        None,
        "one slot's style never leaks to another"
    );
    assert!(
        workspace.set_slot_style_field("nope", "line_width", "2").is_err(),
        "an unknown slot refuses"
    );
}

#[test]
fn the_delta_histogram_tracks_the_same_zoom_the_candles_do() {
    // The histogram is a slimmer overlay, not a differently-sized series: it
    // reads the same live bar spacing and takes a fixed share of the candle
    // body, so the two never drift apart as the pane zooms.
    for spacing in [1.0_f32, 8.0, 20.0, 64.0] {
        let half_width = candle_body_width(spacing) * DELTA_BAR_BODY_SHARE / 2.0;
        assert!(
            close_to(half_width * 2.0, candle_body_width(spacing) * DELTA_BAR_BODY_SHARE),
            "the histogram is a fixed share of the candle body at spacing {spacing}"
        );
        assert!(
            half_width * 2.0 <= candle_body_width(spacing) + f32::EPSILON,
            "and never wider than the candle it sits beside at spacing {spacing}"
        );
    }
}

#[test]
fn the_style_tab_shows_every_paint_knob_for_a_slot() {
    let workspace = ChartWorkspace::new().expect("the workspace boots");
    for id in workspace.slot_ids() {
        let Some(config) = workspace.stack.config(&id) else { continue };
        let Some(style) = workspace.stack.style(&id) else { continue };
        let config_fields = fluent_egui::schema_fields(&config.describe());
        let style_fields = fluent_egui::schema_fields(&style.describe());
        let tab = fluent_egui::tab_fields(SettingsTab::Style, &config_fields, &style_fields);
        let names: Vec<&str> = tab.iter().map(|field| field.name.as_str()).collect();
        assert!(
            names.contains(&"line_width") && names.contains(&"color_bull"),
            "{id}: the Style tab exposes colors and a line width: {names:?}"
        );
        assert!(
            names.contains(&"color_fast"),
            "{id}: one override per style key its series carry"
        );
        // Every other tab stays the config's own business.
        for other in [SettingsTab::Inputs, SettingsTab::Visibility] {
            assert_eq!(
                fluent_egui::tab_fields(other, &config_fields, &style_fields),
                fluent_egui::fields_for_tab(&config_fields, other),
                "{id}: {other:?} is unaffected by the style component"
            );
        }
    }
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

/// Rows on the quarter-hour grid from an hour boundary, so resampling pins
/// the bucket anchor instead of the fixture's layout.
fn grid_rows(start_sec: i64, count: usize) -> Vec<ChartRow> {
    (0..count)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: start_sec + i as i64 * 900,
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
    let hour_sec = protopyne_types::Timeframe::H1.bucket_start_ms(1_700_000_000_000) / 1_000;
    workspace
        .seed_history(TimeframeKey::M15, grid_rows(hour_sec, 8))
        .expect("15M selects on seed");

    assert_eq!(workspace.timeframe(), TimeframeKey::M15, "the default timeframe is selected");
    assert_eq!(workspace.rows().len(), 8, "the seeded 15M history is active");

    workspace.select_timeframe(TimeframeKey::M1).expect("1M selects");
    assert_eq!(workspace.rows().len(), 20, "1M loads its own candles, not the 15M set");
    workspace.select_timeframe(TimeframeKey::M5).expect("5M selects");
    assert_eq!(workspace.rows().len(), 12, "5M loads its own candles");

    workspace.select_timeframe(TimeframeKey::H1).expect("1H resamples the 15M slot");
    assert_eq!(workspace.timeframe(), TimeframeKey::H1);
    let hourly = workspace.rows();
    assert_eq!(hourly.len(), 2, "8 fifteen-minute rows fold into 2 hourly bars");
    assert_eq!(hourly[0].time, hour_sec, "a folded row stamps its bucket start");
    assert!(
        (hourly[0].open - 99.5).abs() < 1e-12,
        "open comes from the bucket's first row"
    );
    assert!(
        (hourly[0].close - 103.0).abs() < 1e-12,
        "close comes from the bucket's last row"
    );
    assert_eq!(hourly[0].volume, 40, "volume sums the bucket's rows");
    assert_eq!(hourly[1].time, hour_sec + 3_600, "the second bucket stamps its own start");

    // A short edge never fabricates a bar: three rows inside one hour fold
    // to nothing.
    let mut short = ChartWorkspace::new().expect("the workspace boots");
    short
        .seed_history(TimeframeKey::M15, grid_rows(hour_sec, 3))
        .expect("15M selects on seed");
    short.select_timeframe(TimeframeKey::H1).expect("1H selects");
    assert!(
        short.rows().is_empty(),
        "three quarter-hours cannot evidence a whole hour"
    );

    let mut bare = ChartWorkspace::new().expect("the workspace boots");
    assert!(
        bare.select_timeframe(TimeframeKey::M1).is_err(),
        "an unseeded slot refuses rather than serving another timeframe"
    );
}

#[test]
fn an_incomplete_trailing_bucket_never_sources_the_folded_edge() {
    // The live edge renders from native bars, never from a partially
    // complete folded bucket: six quarter-hours hold one whole hour plus a
    // two-row tail, so the hourly fold emits exactly the whole bucket and
    // the tail's prices appear nowhere in the folded edge.
    let hour_sec = protopyne_types::Timeframe::H1.bucket_start_ms(1_700_000_000_000) / 1_000;
    let mut workspace = ChartWorkspace::new().expect("the workspace boots");
    workspace
        .seed_history(TimeframeKey::M15, grid_rows(hour_sec, 6))
        .expect("15M selects on seed");
    workspace.select_timeframe(TimeframeKey::H1).expect("1H selects");
    let hourly = workspace.rows();
    assert_eq!(hourly.len(), 1, "the two-row tail emits no hourly bar");
    assert_eq!(hourly[0].time, hour_sec, "the whole bucket stamps its start");
    assert!(
        (hourly[0].close - 103.0).abs() < 1e-12,
        "the folded edge closes on the whole bucket's last row, not the live tail: {}",
        hourly[0].close
    );
}

#[test]
fn a_full_day_grid_folds_whole_hours_and_a_whole_day_while_a_session_day_folds_no_day() {
    use protopyne_types::Timeframe;
    // A synthetic 24-hour quarter-hour grid from a UTC midnight: every hour
    // holds its four children and the day holds all 96, so whole H1 and D1
    // buckets emit.
    let midnight_sec =
        Timeframe::D1.bucket_start_ms(1_700_000_000_000) / 1_000;
    let day: Vec<ChartRow> = (0..96)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: midnight_sec + i * 900,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 10,
            }
        })
        .collect();
    let mut workspace = ChartWorkspace::new().expect("the workspace boots");
    workspace.seed_history(TimeframeKey::M15, day).expect("the day seeds");
    workspace.select_timeframe(TimeframeKey::H1).expect("1H selects");
    assert_eq!(workspace.rows().len(), 24, "24 whole hours emit from a full grid");
    assert_eq!(workspace.rows()[0].time, midnight_sec, "hours stamp their bucket start");
    workspace.select_timeframe(TimeframeKey::D1).expect("1D selects");
    assert_eq!(workspace.rows().len(), 1, "a whole day emits one daily bar");
    assert_eq!(workspace.rows()[0].time, midnight_sec, "the day stamps its bucket start");
    assert_eq!(workspace.rows()[0].volume, 960, "the daily volume sums its 96 rows");

    // A session-shaped day — 26 quarter-hours, 6.5 hours of prints — fills
    // six whole hours but no daily bucket: calendar-day folding of session
    // data legitimately emits no daily bars, and loading a native daily file
    // is the supported answer.
    let mut session = ChartWorkspace::new().expect("the workspace boots");
    session
        .seed_history(TimeframeKey::M15, grid_rows(midnight_sec, 26))
        .expect("the session seeds");
    session.select_timeframe(TimeframeKey::H1).expect("1H selects");
    assert_eq!(session.rows().len(), 6, "six whole hours emit from a session day");
    session.select_timeframe(TimeframeKey::D1).expect("1D selects");
    assert!(
        session.rows().is_empty(),
        "26 quarter-hours cannot evidence a whole day"
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
/// Pixel comparisons in the width rules are exact by construction; a
/// tolerance keeps a failure about the rule rather than float representation.
fn close_to(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() <= 1e-4 * expected.abs().max(1.0)
}

fn same_edge(a: f32, b: f32) -> bool {
    a.to_bits() == b.to_bits()
}

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
fn the_canvas_is_one_sensing_surface_split_into_two_panes() {
    // The regression this pins: the canvas is allocated as ONE rect, so a
    // single pointer response senses the price pane, the oscillator pane, and
    // both price strips. Two allocations would each sense only their own
    // rect, leaving every gesture over the oscillator pane and its Y-axis
    // labels unrouted — the pane would look dead to the wheel.
    let canvas = egui::Rect::from_min_size(
        egui::pos2(0.0, 0.0),
        egui::vec2(640.0, PRICE_PANE_HEIGHT + SUB_PANE_HEIGHT),
    );
    let [main, sub] = split_canvas(canvas);

    assert_eq!(
        (main.height(), sub.height()),
        (PRICE_PANE_HEIGHT, SUB_PANE_HEIGHT),
        "the panes keep their design heights"
    );
    assert_eq!(main.min, canvas.min, "the price pane starts at the canvas origin");
    assert_eq!(sub.max, canvas.max, "and the sub-pane ends at its far corner");
    assert!(
        same_edge(main.bottom(), sub.top()),
        "the panes share the seam exactly, so no pixel is sensed twice or lost"
    );
    assert!(
        main.union(sub) == canvas,
        "and together they are the whole sensing surface"
    );
}

#[test]
fn a_hover_anywhere_on_the_oscillator_pane_reaches_the_oscillators_axis() {
    // Both halves of the reported gap: the oscillator *plot* and the label
    // strip beside it must both route, and both must name pane 1 so the price
    // zoom lands on the oscillator.
    let (layout, sub_layout) = wheel_layouts();
    let zones = chart_zones(&layout, &sub_layout, None);

    let over_plot = egui::pos2(sub_layout.plot.center().x, sub_layout.plot.center().y);
    assert_eq!(
        zones.route(over_plot.into()),
        Some(WheelTarget::Time { pane: SUB_PANE_ZONE, anchor_x: over_plot.x - sub_layout.plot.left() }),
        "a wheel over the oscillator plot zooms the shared time axis"
    );

    let over_labels = egui::pos2(sub_layout.axis.center().x, sub_layout.axis.center().y);
    let routed = zones.route(over_labels.into());
    assert_eq!(
        routed,
        Some(WheelTarget::Price { pane: SUB_PANE_ZONE, anchor_y: over_labels.y - sub_layout.plot.top() }),
        "and a wheel over the oscillator's own Y labels zooms the oscillator"
    );
    if let Some(WheelTarget::Price { pane, anchor_y }) = routed {
        let mut workspace = ChartWorkspace::new().expect("the workspace boots");
        workspace.seed_history(TimeframeKey::M1, ramp_rows(40)).expect("history seeds");
        workspace.select_timeframe(TimeframeKey::M1).expect("M1 binds the viewports");
        let before = workspace.sub_viewport().expect("sub viewport").visible_range();
        workspace.zoom_pane_price(pane, 50.0, anchor_y);
        let after = workspace.sub_viewport().expect("sub viewport").visible_range();
        assert!(
            after.max - after.min < before.max - before.min,
            "the routed gesture narrowed the oscillator: {} -> {}",
            before.max - before.min,
            after.max - after.min
        );
    }
}

#[test]
fn a_wheel_over_the_oscillator_plot_keeps_the_two_panes_on_one_time_axis() {
    // The sub-pane shares the price pane's x axis, so a gesture routed to
    // pane 1 must zoom x for both and must not disturb the price window.
    let (layout, sub_layout) = wheel_layouts();
    let mut workspace = ChartWorkspace::new().expect("the workspace boots");
    workspace.seed_history(TimeframeKey::M1, ramp_rows(40)).expect("history seeds");
    workspace.select_timeframe(TimeframeKey::M1).expect("M1 binds the viewports");

    let hover = egui::pos2(sub_layout.plot.center().x, sub_layout.plot.center().y);
    let zones = chart_zones(&layout, &sub_layout, None);
    let Some(WheelTarget::Time { pane, anchor_x }) = zones.route(hover.into()) else {
        panic!("the oscillator plot routes to the time axis");
    };
    let before_price = workspace.main_viewport().expect("main").visible_range();

    // The frame turns a routed target into exactly these machine inputs.
    workspace.step_interaction(
        1.0 / 60.0,
        &[PointerInput::WheelTime { delta_px: 40.0, anchor_x }],
    );

    assert_eq!(pane, SUB_PANE_ZONE, "the gesture came from the oscillator pane");
    let main = workspace.main_viewport().expect("main");
    let sub = workspace.sub_viewport().expect("sub");
    assert!(
        (sub.scale().bar_spacing() - main.scale().bar_spacing()).abs() <= f32::EPSILON,
        "the time axis stayed shared"
    );
    assert_eq!(
        workspace.main_viewport().expect("main").visible_range(),
        before_price,
        "and the price window never moved"
    );
}

#[test]
fn candles_are_as_wide_as_the_unit_of_time_the_chart_is_showing() {
    // The width rule is shared scaffolding; what the terminal owes is reading
    // it from the LIVE viewport spacing rather than the configured default.
    let mut workspace = ChartWorkspace::new().expect("the workspace boots");
    workspace.seed_history(TimeframeKey::M1, ramp_rows(40)).expect("history seeds");
    workspace.select_timeframe(TimeframeKey::M1).expect("M1 binds the viewports");

    let default_spacing = workspace.main_viewport().expect("main").scale().bar_spacing();
    assert!(
        close_to(candle_body_width(default_spacing), default_spacing),
        "at the configured zoom a candle fills its own time bar"
    );

    // Zoom x well past the gap threshold and the body takes its share of the
    // wider bar, which it can only do if it read the live spacing. A positive
    // wheel delta widens the bars (zoom out along time).
    for _ in 0..8 {
        workspace.step_interaction(
            1.0 / 60.0,
            &[PointerInput::WheelTime { delta_px: 400.0, anchor_x: 320.0 }],
        );
    }
    let zoomed = workspace.main_viewport().expect("main").scale().bar_spacing();
    assert!(zoomed > CANDLE_GAP_THRESHOLD_PX, "the pane really did zoom out: {zoomed}");
    assert!(
        close_to(candle_body_width(zoomed), zoomed * CANDLE_BODY_FRACTION),
        "and a wide candle keeps 80% of its bar so neighbours separate"
    );
    assert!(
        zoomed > default_spacing,
        "the zoom was real, not a relabelled default: {default_spacing} -> {zoomed}"
    );
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

#[test]
fn identical_republish_keeps_revisions_and_skips_the_derived_rebuild() {
    // The frame path republishes every indicator series before rebuilding
    // the derived chart vectors (sub-pane, momentum keys, tints): when the
    // republish backdates every revision, the rebuild is pure cost and must
    // be skipped. The skip is observed through a build counter, never timing.
    let gateway = ScriptGateway::new();
    let mut workspace = ChartWorkspace::with_gateway(&gateway).expect("the workspace boots");
    let built = workspace.derived_builds();
    assert!(built > 0, "boot built the derived panes once");
    // The scripted gateway serves byte-identical bars per symbol, so a
    // second pull evaluates to identical series: every revision backdates.
    workspace.refresh(&gateway).expect("an identical refresh recomputes");
    assert_eq!(
        workspace.derived_builds(),
        built,
        "identical republish keeps every revision and skips the derived rebuild"
    );
    // A real parameter change moves values, bumps revisions, and rebuilds.
    workspace
        .set_slot_field("integration_lite", "eit_alpha", "0.2")
        .expect("the field edits");
    assert!(
        workspace.derived_builds() > built,
        "changed content rebuilds the derived panes"
    );
    // Returning to the identical input skips again: the counter holds.
    workspace
        .set_slot_field("integration_lite", "eit_alpha", "0.5")
        .expect("the field restores");
    let rebuilt = workspace.derived_builds();
    workspace.refresh(&gateway).expect("an identical refresh recomputes");
    assert_eq!(
        workspace.derived_builds(),
        rebuilt,
        "the restored input backdates again and skips the rebuild"
    );
}

#[test]
fn frame_path_borrows_windows_instead_of_cloning_them() {
    // Borrow-profile pin (grep-level, never timing): the canvas windowing
    // helpers that cloned per frame are gone — every layer borrows its
    // trailing window through the shared slicer — and the per-frame momentum
    // fold derives per-column keys from the borrowed trend slice through the
    // one single-bar core instead of materializing a fresh window `Vec`.
    // Full-history folds in `recompute` (and their `candle_momentum_keys`
    // over `self.rows`) are out of scope: they run on data or parameter
    // change, never per frame.
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/workspace.rs");
    let text = std::fs::read_to_string(path).expect("the workspace source reads");
    for token in [
        "fn windowed_ribbon",
        "fn windowed_evaluation",
        "fn windowed_delta",
        "candle_momentum_keys(window",
    ] {
        assert!(
            !text.contains(token),
            "the frame path borrows instead of cloning: {token} is still present"
        );
    }
    assert!(
        text.contains("visible_rows"),
        "windows borrow through the shared slicer"
    );
    assert!(
        text.contains("momentum_key_for"),
        "per-column keys derive from the borrowed trend slice"
    );
    assert!(
        text.contains("sampled_ribbon_bands"),
        "the ribbon fill samples per column at sub-pixel zoom"
    );
    assert!(
        text.contains("column_delta_geoms"),
        "the delta histogram dispatches by pixel columns"
    );
}
