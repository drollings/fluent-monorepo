use super::*;
use std::sync::Arc;

use common_core::metrics::LatencyHistogram;
use fluent_egui::PaneConfig;
use protopyne_chart_model::{ChartRow, DateRange};
use protopyne_kernels::indicator::ParamSet;

fn default_chart() -> ChartConfig {
    ChartConfig::default()
}

#[test]
fn chart_schema_lists_every_field() {
    let config = default_chart();
    assert_eq!(config.symbol, "GLD", "the top bar defaults to the GLD ticker");
    assert_eq!(DEFAULT_SYMBOL, "GLD", "the default ticker is the GLD constant");
    assert_eq!(default_symbol(), "GLD", "the builder default reads the same constant");
    assert_eq!(config.get_field("symbol").expect("symbol reads"), "GLD");
    let schema = config.describe();
    assert_eq!(schema["type"], "object");
    for name in [
        "symbol",
        "timeframe",
        "show_price",
        "show_volume",
        "show_oscillator",
        "fast_oscillator",
        "slow_oscillator",
        "threshold",
        "alpha_low",
        "alpha_high",
        "height",
        "bar_spacing",
        "theme",
    ] {
        assert!(
            schema["properties"].get(name).is_some(),
            "schema lists {name}: {schema}"
        );
        assert!(config.get_field(name).is_ok(), "field {name} reads");
    }
    let empty_symbol = ChartConfig::new().symbol(String::new()).build();
    let err = empty_symbol.validate().expect_err("an empty symbol refuses");
    assert!(err.to_string().contains("symbol"), "names the field: {err}");
}

#[test]
fn chart_fields_round_trip_through_strings() {
    let mut config = default_chart();
    config.set_field("timeframe", "H1").expect("timeframe writes");
    config.set_field("show_price", "false").expect("bool writes");
    config.set_field("threshold", "150").expect("int writes");
    config.set_field("alpha_low", "0.2").expect("float writes");
    config.set_field("theme", "midnight").expect("theme writes");
    assert_eq!(config.get_field("timeframe").expect("reads"), "H1");
    assert!(!config.show_price);
    assert_eq!(config.threshold, 150);
    assert!((config.alpha_low - 0.2).abs() < 1e-12);
    assert_eq!(config.theme, "midnight");
    assert!(config.set_field("no_such_field", "x").is_err(), "unknown fields refuse");
    let json = serde_json::to_value(&config).expect("chart config serializes");
    let back: ChartConfig = serde_json::from_value(json).expect("chart config deserializes");
    assert_eq!(back.timeframe, "H1");
    assert_eq!(back.threshold, 150);
}

#[test]
fn constraints_refuse_with_reasons() {
    let bad_threshold =
        ChartConfig::new().threshold(0).build();
    let err = bad_threshold.validate().expect_err("zero threshold refuses");
    assert!(err.to_string().contains("threshold"), "names the field: {err}");

    let flipped = ChartConfig::new().alpha_low(0.9).alpha_high(0.1).build();
    assert!(flipped.validate().is_err(), "low above high refuses");

    for (label, config) in [
        ("nan alpha", ChartConfig::new().alpha_low(f64::NAN).build()),
        ("high alpha", ChartConfig::new().alpha_high(1.5).build()),
        ("zero height", ChartConfig::new().height(0).build()),
        ("flat spacing", ChartConfig::new().bar_spacing(0.0).build()),
        ("wild spacing", ChartConfig::new().bar_spacing(f64::INFINITY).build()),
        ("empty timeframe", ChartConfig::new().timeframe(String::new()).build()),
        ("empty theme", ChartConfig::new().theme("  ".to_owned()).build()),
        ("unknown fast", ChartConfig::new().fast_oscillator("sma".to_owned()).build()),
        ("unknown slow", ChartConfig::new().slow_oscillator("wavetrend".to_owned()).build()),
    ] {
        assert!(config.validate().is_err(), "{label} refuses");
    }
    assert!(default_chart().validate().is_ok(), "defaults validate");
}

#[test]
fn adapter_overrides_change_render_config_never_kernel_params() {
    let inner = Arc::new(default_chart());
    let kernel_before = ParamSet::new().with_int("length", 14).with_float("k", 1.2).fingerprint();
    let adapter = ComponentAdapter::new(inner.clone()).with_field_override("threshold", "150");
    assert_eq!(adapter.get_field("threshold").expect("override reads"), "150");
    assert_eq!(inner.get_field("threshold").expect("inner reads"), "200", "inner untouched");
    let kernel_after = ParamSet::new().with_int("length", 14).with_float("k", 1.2).fingerprint();
    assert_eq!(kernel_before, kernel_after, "kernel params never move under render overrides");
    let schema = adapter.describe();
    assert_eq!(schema["adapted"], true);
    assert!(schema.get("field_overrides").is_some(), "overrides stay visible in schema");
}

#[test]
fn chart_config_is_a_component_without_hand_rolled_downcasts() {
    let chart: Arc<dyn Component> = Arc::new(default_chart());
    assert!(fluent_wvr::component_downcast_ref::<ChartConfig>(&*chart).is_some());
    let pane: Arc<dyn Component> =
        Arc::new(PaneConfig::new().name("price".to_owned()).build());
    assert!(fluent_wvr::component_downcast_ref::<PaneConfig>(&*pane).is_some());
    assert!(fluent_wvr::component_downcast_ref::<PaneConfig>(&*chart).is_none());
}

#[test]
fn timed_plan_build_reports_panes_and_records_latency() {
    let histogram = Arc::new(LatencyHistogram::new());
    let unit = timed_plan_build(default_chart(), Arc::clone(&histogram));
    let output = unit.execute(&WorkContext::default()).expect("valid config builds");
    assert!(output.success);
    assert!(output.message.contains("9 panes"), "plan size reported: {}", output.message);
    assert!(histogram.count() >= 1, "plan-build timing observed");

    let broken = ChartConfig::new().threshold(0).build();
    let unit = timed_plan_build(broken, Arc::new(LatencyHistogram::new()));
    assert!(unit.execute(&WorkContext::default()).is_err(), "invalid config refuses");
}

#[test]
fn pane_layout_projects_switches_onto_generic_panes() {
    let full = default_chart();
    let layout = full.pane_layout();
    assert_eq!(layout.len(), 3, "all three switches on by default");
    for pane in &layout {
        assert!(pane.validate().is_ok(), "projected panes validate");
    }
    let names: Vec<&str> = layout.iter().map(|pane| pane.name.as_str()).collect();
    assert_eq!(names, vec!["price", "volume", "oscillator"]);
    let reduced = ChartConfig::new().show_volume(false).show_oscillator(false).build();
    let layout = reduced.pane_layout();
    assert_eq!(layout.len(), 1, "switches filter the layout");
    assert_eq!(layout[0].name, "price");
}

fn row_at(time: i64) -> ChartRow {
    ChartRow { time, open: 100.0, high: 101.0, low: 99.0, close: 100.0, volume: 1 }
}

#[test]
fn window_for_range_keeps_the_trailing_window() {
    let rows: Vec<ChartRow> = (0..100).map(row_at).collect();
    // Day1 at 60s bars estimates 1440 bars: more than held, so everything stays.
    assert_eq!(window_for_range(&rows, DateRange::Day1, 60).len(), 100);
    assert_eq!(window_for_range(&rows, DateRange::All, 60).len(), 100);
    let many: Vec<ChartRow> = (0..2000).map(row_at).collect();
    let window = window_for_range(&many, DateRange::Day1, 60);
    assert_eq!(window.len(), 1440, "Day1/60s keeps the trailing 1440 bars");
    assert_eq!(window[0].time, 560, "window is the trailing slice");
    assert!(window_for_range(&[], DateRange::Day1, 60).is_empty());
}

#[test]
fn timeframe_keys_cover_intraday_in_toolbar_order() {
    let keys = TimeframeKey::all();
    assert_eq!(keys, vec![TimeframeKey::M1, TimeframeKey::M5, TimeframeKey::M15, TimeframeKey::H1, TimeframeKey::H4]);
    assert_eq!(TimeframeKey::default(), TimeframeKey::M15);
    let labels: Vec<&str> = keys.iter().map(|key| key.label()).collect();
    assert_eq!(labels, vec!["1M", "5M", "15M", "1H", "4H"]);
    let seconds: Vec<u64> = keys.iter().map(|key| key.seconds()).collect();
    assert_eq!(seconds, vec![60, 300, 900, 3_600, 14_400]);
    assert_eq!(TimeframeKey::H1.m15_group(), 4, "one hour resamples four quarter-hours");
    assert_eq!(TimeframeKey::H4.m15_group(), 16);
    assert_eq!(TimeframeKey::M15.m15_group(), 1, "native slots need no grouping");
}
