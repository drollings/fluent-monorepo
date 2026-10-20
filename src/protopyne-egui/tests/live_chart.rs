//! Live chart: feed-frame parsing, the forming/closed live sequence, replay
//! no-lookahead, three-pane layout and routing, pane scales with pad
//! calibration, and the default-workspace byte pin.
//!
//! All assertions read headless state (no paint): the live path is data in,
//! rows and recompute out.

use super::*;

fn ladder(count: usize) -> Vec<ChartRow> {
    (0..count)
        .map(|index| {
            let close = 100.0 + index as f64;
            ChartRow {
                time: index as i64,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 1_000,
            }
        })
        .collect()
}

fn bar_frame(time: i64, close: f64) -> String {
    serde_json::json!({
        "type": "bar",
        "cycle": 0,
        "index": time,
        "time": time,
        "open": close - 0.5,
        "high": close + 0.5,
        "low": close - 1.0,
        "close": close,
        "volume": 1_000,
    })
    .to_string()
}

/// Feed frames parse to closed-bar updates with the exact row; anything else
/// refuses by name.
#[test]
fn live_frames_parse_to_closed_updates_and_anything_else_refuses() {
    match live_update_of_frame(&bar_frame(1_700_000_000, 101.5)).expect("bar parses") {
        LiveChartUpdate::Closed(row) => {
            assert_eq!(row.time, 1_700_000_000);
            assert_eq!(row.open, 101.0);
            assert_eq!(row.high, 102.0);
            assert_eq!(row.low, 100.5);
            assert_eq!(row.close, 101.5);
            assert_eq!(row.volume, 1_000);
        }
        LiveChartUpdate::Forming(_) => panic!("a bar frame is a closed bar"),
    }
    for bad in [
        "not json",
        r#"{"type":"forming"}"#,
        r#"{"type":"bar"}"#,
        r#"{"type":"bar","time":"soon","open":1.0,"high":1.0,"low":1.0,"close":1.0,"volume":1}"#,
        r#"{"type":"bar","time":1,"open":1.0,"high":1.0,"low":1.0,"close":1.0}"#,
    ] {
        assert!(
            live_update_of_frame(bad).is_err(),
            "malformed frame refuses: {bad}"
        );
    }
}

/// The live sequence: forming rows repaint without recomputing, closed rows
/// append or revise and recompute. Observed through row counts and the
/// derived-build counter, never timing.
#[test]
fn live_sequence_repaints_forming_and_recomputes_closed() {
    let mut workspace = ChartWorkspace::with_rows(ladder(60)).expect("workspace boots");
    let built = workspace.derived_builds();
    assert!(built > 0, "boot built the derived panes once");

    // A forming bar at the open time replaces the row and touches nothing.
    let mut forming = ladder(60);
    forming[59].close = 159.5;
    workspace
        .apply_live_update(LiveChartUpdate::Forming(forming[59].clone()))
        .expect("forming applies");
    assert_eq!(workspace.rows().len(), 60, "forming never grows the rows");
    assert_eq!(
        workspace.rows()[59].close, 159.5,
        "forming revises the open bar"
    );
    assert_eq!(
        workspace.derived_builds(),
        built,
        "forming repaints without recomputing"
    );

    // A forming bar past the end extends the display rows, still no recompute.
    let mut next = ladder(61);
    next[60].close = 160.5;
    workspace
        .apply_live_update(LiveChartUpdate::Forming(next[60].clone()))
        .expect("forming applies");
    assert_eq!(workspace.rows().len(), 61, "a new forming bar shows");
    assert_eq!(
        workspace.derived_builds(),
        built,
        "forming still recomputes nothing"
    );

    // Closing that bar recomputes exactly once.
    workspace
        .apply_live_update(LiveChartUpdate::Closed(next[60].clone()))
        .expect("close applies");
    assert_eq!(workspace.rows().len(), 61, "closing keeps the row");
    assert_eq!(
        workspace.derived_builds(),
        built + 1,
        "one closed bar recomputes once"
    );

    // A revision of the same close recomputes again (content moved).
    let mut revised = next[60].clone();
    revised.close = 161.5;
    workspace
        .apply_live_update(LiveChartUpdate::Closed(revised))
        .expect("revision applies");
    assert_eq!(workspace.rows().len(), 61, "a revision adds no row");
    assert_eq!(workspace.rows()[60].close, 161.5, "the revision lands");
    assert_eq!(
        workspace.derived_builds(),
        built + 2,
        "the revision recomputes"
    );
}

/// Replay evaluates the decided prefix only: every stored value at or past
/// the cursor is a hole, so no painter can read a bar the replay has not
/// decided yet. Readouts answer prefix values, never future ones.
#[test]
fn replay_prefix_hides_every_bar_past_the_cursor() {
    use protopyne_types::Money;
    use protopyne_view::{ReplayDecisionView, ReplayPositionView, ReplayStateView};

    let mut workspace = ChartWorkspace::with_rows(ladder(60)).expect("workspace boots");
    let cursor = 40;
    workspace.live_replay = Some(LiveReplay::new(
        "terminal".to_owned(),
        ReplayStateView {
            session_id: "terminal".to_owned(),
            active: true,
            cursor,
            total_bars: 60,
            decisions: Vec::<ReplayDecisionView>::new(),
            positions: Vec::<ReplayPositionView>::new(),
            filled_orders: 0,
            total_fees: Money::from(0),
            journal: Vec::new(),
        },
    ));
    // A same-time revision moves content, so the gate re-evaluates — over
    // the prefix, not the full store.
    let built = workspace.derived_builds();
    let mut revised = ladder(60);
    revised[59].close = 200.0;
    workspace
        .apply_live_update(LiveChartUpdate::Closed(revised[59].clone()))
        .expect("revision applies");
    assert_eq!(
        workspace.derived_builds(),
        built + 1,
        "the revision recomputes over the prefix"
    );
    let mut checked = 0;
    for evaluation in workspace.evaluations.values() {
        for series in &evaluation.series {
            assert_eq!(
                series.values.len(),
                60,
                "stored series keep full slot length"
            );
            assert!(
                series.values[cursor as usize..].iter().all(Option::is_none),
                "{} hides every bar past the cursor",
                series.node
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "the replay evaluated at least one series");
}

/// Three panes tile the canvas exactly: equal sub heights, shared seams, and
/// only the price pane reserves a time strip.
#[test]
fn three_panes_tile_the_canvas_with_equal_sub_heights() {
    let canvas = egui::Rect::from_min_size(
        egui::pos2(0.0, 0.0),
        egui::vec2(640.0, NOMINAL_CANVAS_HEIGHT),
    );
    let rects = split_canvas_many(canvas, DEFAULT_PANE_SHARE, 2);
    assert_eq!(rects.len(), 3, "price plus two sub-panes");
    let union = rects[0].union(rects[1]).union(rects[2]);
    assert_eq!(union, canvas, "three panes are the whole sensing surface");
    assert!(
        (rects[1].height() - rects[2].height()).abs() <= 1e-4,
        "sub-panes split evenly: {} vs {}",
        rects[1].height(),
        rects[2].height()
    );
    assert!(
        rects[0].bottom().to_bits() == rects[1].top().to_bits()
            && rects[1].bottom().to_bits() == rects[2].top().to_bits(),
        "panes share both seams exactly"
    );
    let layouts: Vec<PaneLayout> = std::iter::once(true)
        .chain(std::iter::repeat(false).take(2))
        .zip(rects.iter())
        .map(|(time_strip, rect)| PaneLayout::split(*rect, time_strip))
        .collect();
    assert!(
        layouts[0].time.is_some(),
        "only the price pane reserves a time strip"
    );
    assert!(
        layouts[1].time.is_none() && layouts[2].time.is_none(),
        "sub-panes never reserve one"
    );
    assert!(
        layouts[0].plot.left().to_bits() == layouts[1].plot.left().to_bits()
            && layouts[1].plot.left().to_bits() == layouts[2].plot.left().to_bits(),
        "all plots share a left-aligned time origin"
    );
}

/// The wheel router indexes the pane it lands on: the third pane's plot and
/// strip route with pane 2, the second pane's with pane 1.
#[test]
fn wheel_routing_indexes_the_third_pane() {
    let canvas = egui::Rect::from_min_size(
        egui::pos2(0.0, 0.0),
        egui::vec2(640.0, NOMINAL_CANVAS_HEIGHT),
    );
    let rects = split_canvas_many(canvas, DEFAULT_PANE_SHARE, 2);
    let layouts: Vec<PaneLayout> = std::iter::once(true)
        .chain(std::iter::repeat(false).take(2))
        .zip(rects.iter())
        .map(|(time_strip, rect)| PaneLayout::split(*rect, time_strip))
        .collect();
    let zones = WheelZones::new(
        layouts
            .iter()
            .map(|layout| PaneZones {
                plot: layout.plot.into(),
                axis: layout.axis.into(),
            })
            .collect(),
        None,
    );
    for (pane, layout) in layouts.iter().enumerate() {
        let plot_hover = egui::pos2(layout.plot.center().x, layout.plot.center().y);
        assert_eq!(
            zones.route(plot_hover.into()),
            Some(WheelTarget::Time {
                pane,
                anchor_x: plot_hover.x - layout.plot.left()
            }),
            "pane {pane}'s plot routes its own time zoom"
        );
        let axis_hover = egui::pos2(layout.axis.center().x, layout.axis.center().y);
        assert_eq!(
            zones.route(axis_hover.into()),
            Some(WheelTarget::Price {
                pane,
                anchor_y: axis_hover.y - layout.plot.top()
            }),
            "pane {pane}'s strip routes its own price zoom"
        );
    }
}

/// Pane scales: bounded panes union every declared bound plus exactly the
/// display pad (no clipped extremes, no dead space); auto panes fit the
/// visible extent plus exactly the pad; values are never clamped; an empty
/// pane falls back to the design band. The pad fraction is frozen.
#[test]
fn pane_scales_pad_declared_bounds_without_clipping_or_dead_space() {
    assert_eq!(
        AUTO_HEIGHT_PAD_FRACTION.to_bits(),
        0.05_f64.to_bits(),
        "the display pad is frozen at five percent"
    );
    let mut workspace = ChartWorkspace::with_rows(ladder(120)).expect("workspace boots");
    for id in ["stoch", "sma"] {
        workspace.slot_indicator(id).expect("kernel slots");
    }
    workspace.recompute().expect("evaluations build");
    let ids = workspace.custom_pane_ids();
    assert_eq!(
        ids,
        vec!["kernel:sma".to_owned(), "kernel:stoch".to_owned()],
        "custom panes sort by id"
    );

    // Bounded pane: the declared 0–100 band holds even where values sit
    // interior, widened by exactly the pad.
    let (lo, hi) = workspace.custom_pane_range("kernel:stoch");
    assert!(lo <= 0.0, "the pad reaches past the declared floor: {lo}");
    assert!(hi >= 100.0, "the pad reaches past the declared ceiling: {hi}");
    let span = hi - lo;
    let pad = span * AUTO_HEIGHT_PAD_FRACTION / (1.0 + 2.0 * AUTO_HEIGHT_PAD_FRACTION);
    assert!(
        (lo - (0.0 - pad)).abs() <= 1e-9,
        "no dead space below the declared band: {lo}"
    );
    assert!(
        (hi - (100.0 + pad)).abs() <= 1e-9,
        "no dead space above the declared band: {hi}"
    );

    // Auto pane: the visible extent plus exactly the pad on each side.
    let (sma_lo, sma_hi) = workspace.custom_pane_range("kernel:sma");
    let mut data_lo = f64::INFINITY;
    let mut data_hi = f64::NEG_INFINITY;
    for evaluation in workspace.evaluations.values() {
        for series in &evaluation.series {
            if series.pane_id("sub") != "kernel:sma" {
                continue;
            }
            for value in series.values.iter().flatten() {
                data_lo = data_lo.min(*value);
                data_hi = data_hi.max(*value);
            }
        }
    }
    assert!(data_lo.is_finite() && data_hi > data_lo, "sma values exist");
    let data_pad = (data_hi - data_lo) * AUTO_HEIGHT_PAD_FRACTION;
    assert!(
        (sma_lo - (data_lo - data_pad)).abs() <= 1e-9,
        "the auto floor is the extent minus exactly the pad: {sma_lo}"
    );
    assert!(
        (sma_hi - (data_hi + data_pad)).abs() <= 1e-9,
        "the auto ceiling is the extent plus exactly the pad: {sma_hi}"
    );

    // Empty panes fall back to the design band; values never clamp.
    assert_eq!(
        workspace.custom_pane_range("kernel:nope"),
        (SUB_MIN, SUB_MAX),
        "an empty pane reads the design band"
    );
}

/// The default workspace is byte-pinned: boot slots, timeframe, and the full
/// persisted prefs document reproduce exactly.
#[test]
fn default_workspace_is_byte_pinned() {
    let workspace = ChartWorkspace::new().expect("the workspace boots");
    assert_eq!(
        workspace.slot_ids(),
        vec![
            "integration_lite".to_owned(),
            "fast_mas".to_owned(),
            "delta_volume_profile".to_owned()
        ],
        "boot slots never change order"
    );
    assert_eq!(workspace.timeframe().label(), "5M", "the default timeframe");
    assert_eq!(
        serde_json::to_string_pretty(&workspace.prefs()).expect("prefs serialize"),
        DEFAULT_PREFS_JSON,
        "the default prefs document reproduces byte for byte"
    );
}

/// The exact default prefs document: any added, removed, renamed, or
/// re-defaulted field fails this pin on purpose.
const DEFAULT_PREFS_JSON: &str = r#"{
  "symbol": "SPY",
  "timeframe": "5M",
  "range": "1M",
  "chart_type": "Candles",
  "theme": "default",
  "pane_share": 0.75,
  "dock_open": false,
  "favorites": {
    "symbols": [],
    "indicators": []
  },
  "slots": [
    {
      "id": "integration_lite",
      "visible": true,
      "config": {
        "delta_unit": "80",
        "eit_alpha": "0.14",
        "fill_alpha": "0.5",
        "ma_health_k": "1.2",
        "ribbon_health_k": "1.8",
        "show_ribbon": "true"
      },
      "style": {
        "color_bear": "",
        "color_bear_up": "",
        "color_bull": "",
        "color_bull_down": "",
        "color_fast": "",
        "color_ribbon_bear": "",
        "color_ribbon_bull": "",
        "color_slow": "",
        "line_width": "1"
      }
    },
    {
      "id": "fast_mas",
      "visible": true,
      "config": {
        "base_ma_length": "55",
        "base_ma_style": "slow",
        "base_ma_type": "ema",
        "first_ma_length": "3",
        "first_ma_style": "slow",
        "first_ma_type": "wma",
        "mean_ma_length": "21",
        "mean_ma_style": "slow",
        "mean_ma_type": "ema",
        "second_ma_length": "8",
        "second_ma_style": "fast",
        "second_ma_type": "alma",
        "show_extra_mas": "true",
        "show_first_ma": "true",
        "show_second_ma": "false",
        "show_smas": "false",
        "signal_ma_length": "8",
        "signal_ma_style": "fast",
        "signal_ma_type": "ema",
        "sma_style": "bull_down",
        "source": "close",
        "trend_ma_length": "34",
        "trend_ma_style": "slow",
        "trend_ma_type": "ema"
      },
      "style": {
        "color_bear": "",
        "color_bear_up": "",
        "color_bull": "",
        "color_bull_down": "",
        "color_fast": "",
        "color_ribbon_bear": "",
        "color_ribbon_bull": "",
        "color_slow": "",
        "line_width": "1"
      }
    },
    {
      "id": "delta_volume_profile",
      "visible": true,
      "config": {
        "bins": "60",
        "lookback": "100",
        "offset": "5",
        "show_poc": "true",
        "show_profile": "true",
        "show_value_area": "true",
        "show_vwap": "true",
        "value_area_pct": "0.7",
        "width": "30"
      },
      "style": {
        "color_bear": "",
        "color_bear_up": "",
        "color_bull": "",
        "color_bull_down": "",
        "color_fast": "",
        "color_ribbon_bear": "",
        "color_ribbon_bull": "",
        "color_slow": "",
        "line_width": "1"
      }
    }
  ]
}"#;
