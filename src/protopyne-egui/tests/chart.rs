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
    assert_eq!(
        config.symbol, "SPY",
        "the top bar defaults to the SPY ticker"
    );
    assert_eq!(
        DEFAULT_SYMBOL, "SPY",
        "the default ticker is the SPY constant"
    );
    assert_eq!(
        default_symbol(),
        "SPY",
        "the builder default reads the same constant"
    );
    assert_eq!(config.get_field("symbol").expect("symbol reads"), "SPY");
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
    let err = empty_symbol
        .validate()
        .expect_err("an empty symbol refuses");
    assert!(err.to_string().contains("symbol"), "names the field: {err}");
}

#[test]
fn chart_fields_round_trip_through_strings() {
    let mut config = default_chart();
    config
        .set_field("timeframe", "H1")
        .expect("timeframe writes");
    config
        .set_field("show_price", "false")
        .expect("bool writes");
    config.set_field("threshold", "150").expect("int writes");
    config.set_field("alpha_low", "0.2").expect("float writes");
    config.set_field("theme", "midnight").expect("theme writes");
    assert_eq!(config.get_field("timeframe").expect("reads"), "H1");
    assert!(!config.show_price);
    assert_eq!(config.threshold, 150);
    assert!((config.alpha_low - 0.2).abs() < 1e-12);
    assert_eq!(config.theme, "midnight");
    assert!(
        config.set_field("no_such_field", "x").is_err(),
        "unknown fields refuse"
    );
    let json = serde_json::to_value(&config).expect("chart config serializes");
    let back: ChartConfig = serde_json::from_value(json).expect("chart config deserializes");
    assert_eq!(back.timeframe, "H1");
    assert_eq!(back.threshold, 150);
}

#[test]
fn constraints_refuse_with_reasons() {
    let bad_threshold = ChartConfig::new().threshold(0).build();
    let err = bad_threshold
        .validate()
        .expect_err("zero threshold refuses");
    assert!(
        err.to_string().contains("threshold"),
        "names the field: {err}"
    );

    let flipped = ChartConfig::new().alpha_low(0.9).alpha_high(0.1).build();
    assert!(flipped.validate().is_err(), "low above high refuses");

    for (label, config) in [
        ("nan alpha", ChartConfig::new().alpha_low(f64::NAN).build()),
        ("high alpha", ChartConfig::new().alpha_high(1.5).build()),
        ("zero height", ChartConfig::new().height(0).build()),
        ("flat spacing", ChartConfig::new().bar_spacing(0.0).build()),
        (
            "wild spacing",
            ChartConfig::new().bar_spacing(f64::INFINITY).build(),
        ),
        (
            "empty timeframe",
            ChartConfig::new().timeframe(String::new()).build(),
        ),
        (
            "empty theme",
            ChartConfig::new().theme("  ".to_owned()).build(),
        ),
        (
            "unknown fast",
            ChartConfig::new().fast_oscillator("sma".to_owned()).build(),
        ),
        (
            "unknown slow",
            ChartConfig::new()
                .slow_oscillator("wavetrend".to_owned())
                .build(),
        ),
        (
            "refused fast",
            ChartConfig::new().fast_oscillator("cdv".to_owned()).build(),
        ),
        (
            "refused slow",
            ChartConfig::new().slow_oscillator("cdv".to_owned()).build(),
        ),
    ] {
        assert!(config.validate().is_err(), "{label} refuses");
    }
    // The listed-but-refused `cdv` row refuses with the catalog's own
    // reason, never as a generic unknown spelling and never by
    // substituting the default constants.
    let cdv = ChartConfig::new().fast_oscillator("cdv".to_owned()).build();
    let err = cdv.validate().expect_err("cdv refuses evaluation");
    assert!(
        err.to_string().contains("cdv has no kernel"),
        "carries the catalog refusal reason: {err}"
    );
    assert!(default_chart().validate().is_ok(), "defaults validate");
}

#[test]
fn adapter_overrides_change_render_config_never_kernel_params() {
    let inner = Arc::new(default_chart());
    let kernel_before = ParamSet::new()
        .with_int("length", 14)
        .with_float("k", 1.2)
        .fingerprint();
    let adapter = ComponentAdapter::new(inner.clone()).with_field_override("threshold", "150");
    assert_eq!(
        adapter.get_field("threshold").expect("override reads"),
        "150"
    );
    assert_eq!(
        inner.get_field("threshold").expect("inner reads"),
        "200",
        "inner untouched"
    );
    let kernel_after = ParamSet::new()
        .with_int("length", 14)
        .with_float("k", 1.2)
        .fingerprint();
    assert_eq!(
        kernel_before, kernel_after,
        "kernel params never move under render overrides"
    );
    let schema = adapter.describe();
    assert_eq!(schema["adapted"], true);
    assert!(
        schema.get("field_overrides").is_some(),
        "overrides stay visible in schema"
    );
}

#[test]
fn chart_config_is_a_component_without_hand_rolled_downcasts() {
    let chart: Arc<dyn Component> = Arc::new(default_chart());
    assert!(fluent_wvr::component_downcast_ref::<ChartConfig>(&*chart).is_some());
    let pane: Arc<dyn Component> = Arc::new(PaneConfig::new().name("price".to_owned()).build());
    assert!(fluent_wvr::component_downcast_ref::<PaneConfig>(&*pane).is_some());
    assert!(fluent_wvr::component_downcast_ref::<PaneConfig>(&*chart).is_none());
}

#[test]
fn timed_plan_build_reports_panes_and_records_latency() {
    let histogram = Arc::new(LatencyHistogram::new());
    let unit = timed_plan_build(default_chart(), Arc::clone(&histogram));
    let output = unit
        .execute(&WorkContext::default())
        .expect("valid config builds");
    assert!(output.success);
    assert!(
        output.message.contains("9 panes"),
        "plan size reported: {}",
        output.message
    );
    assert!(histogram.count() >= 1, "plan-build timing observed");

    let broken = ChartConfig::new().threshold(0).build();
    let unit = timed_plan_build(broken, Arc::new(LatencyHistogram::new()));
    assert!(
        unit.execute(&WorkContext::default()).is_err(),
        "invalid config refuses"
    );
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
    let reduced = ChartConfig::new()
        .show_volume(false)
        .show_oscillator(false)
        .build();
    let layout = reduced.pane_layout();
    assert_eq!(layout.len(), 1, "switches filter the layout");
    assert_eq!(layout[0].name, "price");
}

fn row_at(time: i64) -> ChartRow {
    ChartRow {
        time,
        open: 100.0,
        high: 101.0,
        low: 99.0,
        close: 100.0,
        volume: 1,
    }
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
fn timeframe_keys_cover_intraday_to_daily_in_toolbar_order() {
    let keys = TimeframeKey::all();
    assert_eq!(
        keys,
        vec![
            TimeframeKey::M1,
            TimeframeKey::M5,
            TimeframeKey::M15,
            TimeframeKey::H1,
            TimeframeKey::H4,
            TimeframeKey::D1,
        ]
    );
    assert_eq!(
        TimeframeKey::default(),
        TimeframeKey::M5,
        "five-minute default"
    );
    let labels: Vec<&str> = keys.iter().map(|key| key.label()).collect();
    assert_eq!(labels, vec!["1M", "5M", "15M", "1H", "4H", "1D"]);
    let seconds: Vec<u64> = keys
        .iter()
        .map(|key| key.timeframe().total_seconds())
        .collect();
    assert_eq!(seconds, vec![60, 300, 900, 3_600, 14_400, 86_400]);
    assert_eq!(
        TimeframeKey::H1.timeframe().duration_ms(),
        3_600_000,
        "one hour holds four quarter-hours"
    );
    assert_eq!(TimeframeKey::H4.timeframe().duration_ms(), 14_400_000);
    assert_eq!(
        TimeframeKey::D1.timeframe().duration_ms(),
        86_400_000,
        "one day holds 96 quarter-hours"
    );
    assert!(TimeframeKey::H1
        .timeframe()
        .is_whole_multiple_of(TimeframeKey::M15.timeframe()));
    assert!(TimeframeKey::H4
        .timeframe()
        .is_whole_multiple_of(TimeframeKey::M15.timeframe()));
    assert!(TimeframeKey::D1
        .timeframe()
        .is_whole_multiple_of(TimeframeKey::M15.timeframe()));
    // The one toolbar-label bridge: every toolbar label resolves through
    // `Timeframe::from_chart_label` to the key's own spacing, so the serving
    // boundary (`timeframe_of_label`) and the terminal cannot disagree.
    for key in TimeframeKey::all() {
        assert_eq!(
            protopyne_types::Timeframe::from_chart_label(key.label()),
            Some(key.timeframe()),
            "{key:?} label resolves to the key's spacing"
        );
    }
    // The key grid and the store/kernel grid are one mapping.
    assert_eq!(TimeframeKey::D1.timeframe(), protopyne_types::Timeframe::D1);
    assert_eq!(TimeframeKey::M1.timeframe(), protopyne_types::Timeframe::M1);
    assert_eq!(TimeframeKey::H4.timeframe(), protopyne_types::Timeframe::H4);
    for key in keys {
        assert_eq!(
            key.timeframe().duration_ms(),
            key.timeframe().total_seconds() as i64 * 1_000,
            "{key:?} seconds agree with the store grid"
        );
    }
}

#[test]
fn prefold_levels_come_from_the_toolbar_grid_in_whole_multiples() {
    // The levels a base can pre-fold into are toolbar keys at or coarser
    // than the base that tile it whole — the same one-bridge grid the
    // toolbar and the store already share, never a second table.
    assert_eq!(
        prefold_levels(TimeframeKey::M1),
        vec![
            TimeframeKey::M1,
            TimeframeKey::M5,
            TimeframeKey::M15,
            TimeframeKey::H1,
            TimeframeKey::H4,
            TimeframeKey::D1,
        ]
    );
    assert_eq!(
        prefold_levels(TimeframeKey::M15),
        vec![
            TimeframeKey::M15,
            TimeframeKey::H1,
            TimeframeKey::H4,
            TimeframeKey::D1
        ]
    );
    assert_eq!(prefold_levels(TimeframeKey::D1), vec![TimeframeKey::D1]);
    for base in TimeframeKey::all() {
        let levels = prefold_levels(base);
        assert_eq!(levels[0], base, "{base:?} always keeps its native level");
        for pair in levels.windows(2) {
            assert!(
                pair[1]
                    .timeframe()
                    .is_whole_multiple_of(pair[0].timeframe()),
                "{base:?} ladder stays tileable: {pair:?}"
            );
        }
    }
}

#[test]
fn stateless_pick_selects_the_finest_level_at_or_above_two_px() {
    // Native pixels per bar times the fold multiple is the folded bar's
    // screen width; the picker keeps the finest level still at or above two
    // pixels. Multiples above M1 read 1/5/15/60/240/1440, so the switch
    // points sit at 2/5 = 0.4, 2/15, 2/60, 2/240, and 2/1440 native px.
    let cases = [
        (8.0, TimeframeKey::M1),
        (2.0, TimeframeKey::M1),
        (1.9, TimeframeKey::M5),
        (0.4, TimeframeKey::M5),
        (0.39, TimeframeKey::M15),
        (2.0 / 15.0, TimeframeKey::M15),
        (2.0 / 15.0 - 1e-9, TimeframeKey::H1),
        (2.0 / 60.0, TimeframeKey::H1),
        (2.0 / 60.0 - 1e-9, TimeframeKey::H4),
        (2.0 / 240.0, TimeframeKey::H4),
        (2.0 / 240.0 - 1e-9, TimeframeKey::D1),
        (2.0 / 1440.0, TimeframeKey::D1),
        // Past the coarsest level nothing reaches two pixels; the picker
        // holds the coarsest rather than inventing a wider one.
        (0.0005, TimeframeKey::D1),
        (0.05, TimeframeKey::H1),
    ];
    for (px, want) in cases {
        assert_eq!(
            prefold_pick(px, TimeframeKey::M1),
            want,
            "native {px} px/bar"
        );
    }
    // A non-finite spacing never switches: fail-safe, like the column
    // dispatcher it feeds.
    for px in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] {
        assert_eq!(
            prefold_pick(px, TimeframeKey::M15),
            TimeframeKey::M15,
            "non-positive spacing {px} holds native"
        );
    }
    // From a coarser base the ladder shortens but the rule is the same.
    assert_eq!(prefold_pick(8.0, TimeframeKey::H1), TimeframeKey::H1);
    assert_eq!(prefold_pick(0.5, TimeframeKey::H1), TimeframeKey::H4);
    assert_eq!(prefold_pick(8.0, TimeframeKey::D1), TimeframeKey::D1);
    assert_eq!(prefold_pick(0.0001, TimeframeKey::D1), TimeframeKey::D1);
}

#[test]
fn hysteretic_pick_moves_each_direction_past_its_own_edge() {
    // The band around each switch point has a low edge (zooming out switches
    // coarser below it) and a high edge (zooming in switches finer above
    // it); between the edges the current level holds either way. Ladder
    // multiples above M1 read 1/5/15/60/240/1440.
    let keys = prefold_levels(TimeframeKey::M1);
    let multiples = [1.0, 5.0, 15.0, 60.0, 240.0, 1440.0];
    for (depth, window) in keys.windows(2).enumerate() {
        let (finer, coarser) = (window[0], window[1]);
        let fine_multiple = multiples[depth];
        let low_px = (PREFOLD_TARGET_PX_PER_BAR - PREFOLD_HYSTERESIS_PX) / fine_multiple;
        let high_px = (PREFOLD_TARGET_PX_PER_BAR + PREFOLD_HYSTERESIS_PX) / fine_multiple;
        // Zooming out from the finer level: holds at the low edge, steps
        // coarser just below it.
        assert_eq!(
            prefold_pick_hysteretic(finer, low_px, TimeframeKey::M1),
            finer,
            "{finer:?} holds at its low edge {low_px}"
        );
        assert_eq!(
            prefold_pick_hysteretic(finer, low_px * 0.999, TimeframeKey::M1),
            coarser,
            "{finer:?} steps to {coarser:?} just below {low_px}"
        );
        // Zooming in from the coarser level: holds just below the high
        // edge, steps finer at it.
        assert_eq!(
            prefold_pick_hysteretic(coarser, high_px * 0.999, TimeframeKey::M1),
            coarser,
            "{coarser:?} holds just below its high edge {high_px}"
        );
        assert_eq!(
            prefold_pick_hysteretic(coarser, high_px, TimeframeKey::M1),
            finer,
            "{coarser:?} steps back to {finer:?} at {high_px}"
        );
        // Between the edges the current level holds from either side: the
        // band is stability around one switch point, never a second rule.
        let mid_px = PREFOLD_TARGET_PX_PER_BAR / fine_multiple;
        assert_eq!(
            prefold_pick_hysteretic(finer, mid_px, TimeframeKey::M1),
            finer,
            "{finer:?} holds mid-band coming out"
        );
        assert_eq!(
            prefold_pick_hysteretic(coarser, mid_px, TimeframeKey::M1),
            coarser,
            "{coarser:?} holds mid-band coming in"
        );
    }
    // A large zoom jump crosses several levels at once, never stopping
    // mid-ladder: hysteresis steadies edges, it does not ratchet jumps.
    assert_eq!(
        prefold_pick_hysteretic(TimeframeKey::M1, 0.0005, TimeframeKey::M1),
        TimeframeKey::D1,
        "a deep zoom-out lands on the coarsest level"
    );
    assert_eq!(
        prefold_pick_hysteretic(TimeframeKey::D1, 8.0, TimeframeKey::M1),
        TimeframeKey::M1,
        "a full zoom-in lands back on native"
    );
    // Non-finite spacing never switches from any level.
    for px in [f64::NAN, f64::INFINITY, 0.0, -2.0] {
        assert_eq!(
            prefold_pick_hysteretic(TimeframeKey::H1, px, TimeframeKey::M1),
            TimeframeKey::H1,
            "spacing {px} holds the current level"
        );
    }
}

#[test]
fn hysteresis_holds_through_drift_boundary_hover_and_single_level() {
    // Control group one — slow drift inside the band: the M1/M5 low edge
    // sits at 1.5 native px and the high edge at 2.5, so oscillating ±1%
    // around 2.0 never flips the level from either side.
    let mut level = TimeframeKey::M1;
    for step in 0..40 {
        let px = 2.0 * if step % 2 == 0 { 1.01 } else { 0.99 };
        level = prefold_pick_hysteretic(level, px, TimeframeKey::M1);
    }
    assert_eq!(level, TimeframeKey::M1, "drift inside the band holds M1");
    let mut level = TimeframeKey::M5;
    for step in 0..40 {
        let px = 2.0 * if step % 2 == 0 { 1.01 } else { 0.99 };
        level = prefold_pick_hysteretic(level, px, TimeframeKey::M1);
    }
    assert_eq!(level, TimeframeKey::M5, "the same drift holds M5 coming in");
    // Control group two — edge hover: the low edge itself (strict `<`
    // switches below it) holds from either side, and mid-band holds too.
    for px in [1.5, 2.0] {
        assert_eq!(
            prefold_pick_hysteretic(TimeframeKey::M1, px, TimeframeKey::M1),
            TimeframeKey::M1,
            "M1 holds at {px} px/bar"
        );
        assert_eq!(
            prefold_pick_hysteretic(TimeframeKey::M5, px, TimeframeKey::M1),
            TimeframeKey::M5,
            "M5 holds at {px} px/bar"
        );
    }
    // Control group three — single-level data: the daily base has no coarser
    // level, so every zoom holds it.
    for px in [8.0, 0.4, 0.001, 0.00001] {
        assert_eq!(
            prefold_pick_hysteretic(TimeframeKey::D1, px, TimeframeKey::D1),
            TimeframeKey::D1,
            "D1 holds at {px} px/bar"
        );
        assert_eq!(prefold_pick(px, TimeframeKey::D1), TimeframeKey::D1);
    }
}

fn zoom_sweep_with_hover() -> Vec<f64> {
    // A slow zoom-out with hover: geometric descent past every switch point
    // with ±0.5% oscillation samples around each stateless switch, the way a
    // wheel gesture stutters across a threshold instead of crossing cleanly.
    let switches = [0.4, 2.0 / 15.0, 2.0 / 60.0, 2.0 / 240.0, 2.0 / 1440.0];
    let mut sweep = Vec::new();
    let mut px = 8.0;
    while px > 0.0004 {
        sweep.push(px);
        for switch in switches {
            if px > switch && px * 0.9 <= switch {
                for _ in 0..6 {
                    sweep.push(switch * 1.005);
                    sweep.push(switch * 0.995);
                }
            }
        }
        px *= 0.9;
    }
    sweep
}

fn count_switches(levels: &[TimeframeKey]) -> usize {
    levels.windows(2).filter(|pair| pair[0] != pair[1]).count()
}

#[test]
fn flicker_sweep_stays_within_its_bound() {
    let sweep = zoom_sweep_with_hover();
    assert!(sweep.len() > 100, "the sweep stutters, it does not glide");
    let stateless: Vec<TimeframeKey> = sweep
        .iter()
        .map(|px| prefold_pick(*px, TimeframeKey::M1))
        .collect();
    let naive = count_switches(&stateless);
    let mut level = TimeframeKey::M1;
    let mut hysteretic = Vec::with_capacity(sweep.len());
    for px in &sweep {
        level = prefold_pick_hysteretic(level, *px, TimeframeKey::M1);
        hysteretic.push(level);
    }
    let steadied = count_switches(&hysteretic);
    eprintln!(
        "flicker sweep samples={} naive={naive} hysteretic={steadied}",
        sweep.len()
    );
    assert!(
        naive > steadied,
        "hover stutters flip the stateless picker: {naive} vs {steadied}"
    );
    assert!(
        steadied <= 6,
        "one switch per ladder step, nothing more: {steadied}"
    );
}

#[test]
fn default_off_resolves_the_base_level_at_every_zoom() {
    // The picker ships default-off: the const assertion below fails the
    // build the moment the flag flips, and the sweep proves the resolver
    // answers the base at every zoom from either starting level — so the
    // column dispatcher below it runs exactly its M2 path.
    const { assert!(!PREFOLD_ENABLED) };
    let sweep = zoom_sweep_with_hover();
    for px in sweep {
        for current in TimeframeKey::all() {
            assert_eq!(
                resolve_prefold_level(false, current, px, TimeframeKey::M1),
                TimeframeKey::M1,
                "flag off holds the base at {px} px/bar from {current:?}"
            );
        }
    }
    // Flag on delegates to the hysteretic picker, non-finite included.
    assert_eq!(
        resolve_prefold_level(true, TimeframeKey::M1, 8.0, TimeframeKey::M1),
        TimeframeKey::M1
    );
    assert_eq!(
        resolve_prefold_level(true, TimeframeKey::M1, 0.0005, TimeframeKey::M1),
        TimeframeKey::D1
    );
    assert_eq!(
        resolve_prefold_level(true, TimeframeKey::H1, f64::NAN, TimeframeKey::M1),
        TimeframeKey::H1
    );
}

/// The toolbar keys carry no seconds table of their own: each one's seconds are
/// the shared `Timeframe` table's, and the values still match the retired local
/// table (`1M` 60 … `1D` 86_400), so the bridge is a rename, never a
/// renumbering.
#[test]
fn every_timeframe_key_seconds_come_from_the_one_table() {
    let expected = [
        (TimeframeKey::M1, 60_u64),
        (TimeframeKey::M5, 300),
        (TimeframeKey::M15, 900),
        (TimeframeKey::H1, 3_600),
        (TimeframeKey::H4, 14_400),
        (TimeframeKey::D1, 86_400),
    ];
    for (key, seconds) in expected {
        assert_eq!(
            key.timeframe().total_seconds(),
            seconds,
            "{key:?}: seconds come from Timeframe::total_seconds"
        );
    }
    assert_eq!(
        expected.len(),
        TimeframeKey::all().len(),
        "every toolbar key is pinned"
    );
}

#[test]
fn timeframe_keys_map_to_pine_period_strings() {
    // The chart owns the timeframe: Pine slots inherit these strings for
    // `timeframe.period` instead of carrying their own copy.
    let expected = [
        (TimeframeKey::M1, "1"),
        (TimeframeKey::M5, "5"),
        (TimeframeKey::M15, "15"),
        (TimeframeKey::H1, "60"),
        (TimeframeKey::H4, "240"),
        (TimeframeKey::D1, "D"),
    ];
    for (key, period) in expected {
        assert_eq!(key.pine_period(), period, "{key:?} maps to {period}");
    }
    assert_eq!(
        expected.len(),
        TimeframeKey::all().len(),
        "every toolbar key is pinned"
    );
}
