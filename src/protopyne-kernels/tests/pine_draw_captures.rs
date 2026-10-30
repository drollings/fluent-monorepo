//! Drawing/model shims as captured data: `plotcandle` legs, `barcolor`
//! series, `fill` plot-pairings, and `label`/`table`/`box`/`line` lifecycle
//! events recorded as data rows, rendering nothing.
//!
//! Every drawing call executes to a capture: a run that draws but records
//! nothing is a defect, never a silent no-op. Unknown drawing methods still
//! refuse with a reason.
//!
//! Multi-line drivers below carry explicit `\n    ` indents: Rust `\`
//! continuations strip leading whitespace, which would drop guarded bodies
//! to column zero.
//!
//! Exact float comparisons below are over exact small integral prices and
//! counters, never measured quantities.
//! `#[allow(clippy::float_cmp)]` covers those literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::parse_program;
use chrono::TimeZone as _;
use protopyne_types::Timeframe;

fn bar_at(index: i64) -> Bar {
    Bar::new()
        .ts(chrono::Utc
            .timestamp_opt(1_700_000_000 + index * 60, 0)
            .single()
            .expect("valid test timestamp"))
        .o("100.00".parse().expect("decimal"))
        .h("101.00".parse().expect("decimal"))
        .l("99.00".parse().expect("decimal"))
        .c(format!("{}.00", 100 + index).parse().expect("decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

fn five_bars() -> Vec<Bar> {
    (0..5).map(bar_at).collect()
}

fn run(script: &str) -> RunOutcome {
    let program = parse_program(script).expect("script parses");
    PineEngine::new(PineRuntimeConfig::new().build())
        .run(&program, &five_bars(), InputSet::new())
        .expect("script runs")
}

fn run_err(script: &str) -> String {
    let program = parse_program(script).expect("script parses");
    PineEngine::new(PineRuntimeConfig::new().build())
        .run(&program, &five_bars(), InputSet::new())
        .expect_err("script refuses")
        .to_string()
}

fn num(value: Option<&PineValue>) -> f64 {
    match value {
        Some(PineValue::Float(v)) => *v,
        Some(PineValue::Int(v)) => *v as f64,
        other => panic!("expected number, got {other:?}"),
    }
}

fn flag(outcome: &RunOutcome, name: &str) -> bool {
    match outcome.value_at(4, name) {
        Some(Some(PineValue::Bool(v))) => v,
        other => panic!("expected bool for {name}, got {other:?}"),
    }
}

#[test]
fn plotcandle_legs_captured_per_bar() {
    // Buy-side and sell-side delta candles mirror the indicator shape:
    // four legs plus titles, one entry per evaluated bar.
    let outcome = run(
        "plotcandle(0.0, high, 0.0, high, title=\"Delta Buy\", color=close)\n\
         plotcandle(0.0, 0.0, -low, -low, title=\"Delta Sell\", color=open)",
    );
    assert_eq!(outcome.plotcandles.len(), 2, "one capture per call site");
    let buy = &outcome.plotcandles[0];
    assert_eq!(buy.title, "Delta Buy");
    assert_eq!(buy.opens.len(), 5);
    assert_eq!(buy.highs.len(), 5);
    assert_eq!(buy.lows.len(), 5);
    assert_eq!(buy.closes.len(), 5);
    for bar in 0..5 {
        assert_eq!(num(buy.opens[bar].as_ref()), 0.0, "buy open pins zero");
        assert_eq!(
            num(buy.highs[bar].as_ref()),
            101.0,
            "buy top rides the high leg"
        );
        assert_eq!(num(buy.lows[bar].as_ref()), 0.0, "buy bottom pins zero");
        assert_eq!(
            num(buy.closes[bar].as_ref()),
            101.0,
            "buy close rides the high leg"
        );
    }
    let sell = &outcome.plotcandles[1];
    assert_eq!(sell.title, "Delta Sell");
    for bar in 0..5 {
        assert_eq!(
            num(sell.lows[bar].as_ref()),
            -99.0,
            "sell bottom negates the low leg"
        );
        assert_eq!(
            num(sell.closes[bar].as_ref()),
            -99.0,
            "sell close negates the low leg"
        );
    }
    // Existing `plot` behavior is untouched: no plots recorded here.
    assert!(outcome.plots.is_empty(), "plotcandle never feeds plots");
}

#[test]
fn barcolor_conditional_na_captured() {
    // The indicator's `barcolor(flag ? color : na)` shape: present colors
    // on flagged bars, absence elsewhere.
    let outcome = run("up = close > open\nbarcolor(up ? close : na, title=\"Trend\")");
    assert_eq!(outcome.barcolors.len(), 1, "one capture per call site");
    let colors = &outcome.barcolors[0];
    assert_eq!(colors.title, "Trend");
    assert_eq!(colors.values.len(), 5);
    // Bar 0 paints `na` (close equals open there); later bars paint close.
    assert!(
        colors.values[0].is_none(),
        "bar 0 ties open and stays absent"
    );
    for bar in 1..5 {
        assert_eq!(
            num(colors.values[bar].as_ref()),
            100.0 + bar as f64,
            "bar {bar} paints its close"
        );
    }
    let quiet = run("barcolor(close < open ? close : na)");
    assert!(
        quiet.barcolors[0].values.iter().all(Option::is_none),
        "unflagged bars stay absent, never a default color"
    );
}

#[test]
fn fill_pairs_two_plot_sites() {
    // The ribbon shape: two inline `plot()` calls paired by `fill`, with
    // the pair's per-bar region captured alongside the plots themselves.
    // Titles ride the named argument (positional titles stay the plot
    // arm's existing fallback behavior, unchanged here).
    let outcome = run(
        "fill(plot(high, title=\"Fast\"), plot(low, title=\"Slow\"), color=close, title=\"Ribbon\")",
    );
    assert_eq!(outcome.plots.len(), 2, "inner plots still capture");
    assert_eq!(outcome.fills.len(), 1, "one fill per call site");
    let fill = &outcome.fills[0];
    assert_eq!(fill.title, "Ribbon");
    assert_eq!(fill.first_title, "Fast");
    assert_eq!(fill.second_title, "Slow");
    assert_eq!(fill.values.len(), 5);
    for bar in 0..5 {
        match &fill.values[bar] {
            Some(PineValue::Array(pair)) => {
                assert_eq!(pair.len(), 2, "fill region is the two plot values");
                assert_eq!(num(pair[0].as_ref()), 101.0, "first leg rides high");
                assert_eq!(num(pair[1].as_ref()), 99.0, "second leg rides low");
            }
            other => panic!("expected fill pair on bar {bar}, got {other:?}"),
        }
    }
    // A fill over non-plot series refuses instead of guessing a pairing.
    let err = run_err("fill(close, open)");
    assert!(
        err.contains("plot"),
        "fill over raw series refuses naming plots, got: {err}"
    );
}

#[test]
fn plotshape_and_hline_capture_per_bar() {
    // Marker and level captures mirror `plot`: one entry per call site,
    // titles from the named or second-positional slot, one value per bar.
    let outcome = run(
        "plotshape(close > open ? high : na, title=\"Up\", style=shape.labelup, location=location.absolute)\n\
         hline(0, \"Neutral\", color=close, linestyle=hline.style_dashed)",
    );
    assert_eq!(
        outcome.plotshapes.len(),
        1,
        "one shape capture per call site"
    );
    let shape = &outcome.plotshapes[0];
    assert_eq!(shape.title, "Up");
    assert_eq!(shape.values.len(), 5);
    assert!(shape.values[0].is_none(), "bar 0 ties and stays absent");
    for bar in 1..5 {
        assert_eq!(
            num(shape.values[bar].as_ref()),
            101.0,
            "marked bars carry high"
        );
    }
    assert_eq!(outcome.hlines.len(), 1, "one hline capture per call site");
    let level = &outcome.hlines[0];
    assert_eq!(level.title, "Neutral");
    assert_eq!(level.values.len(), 5);
    for bar in 0..5 {
        assert_eq!(
            num(level.values[bar].as_ref()),
            0.0,
            "constant level repeats"
        );
    }
}

#[test]
fn fill_pairs_variable_held_plots() {
    // The shipped-indicator shape (`p1 = plot(...); fill(p1, p2, ...)`):
    // variables bound from `plot()` calls pair through their recorded
    // site tags, resolving to the same captures as the inline form.
    let outcome = run("p1 = plot(high, title=\"Fast\")\n\
         p2 = plot(low, title=\"Slow\")\n\
         fill(p1, p2, title=\"Ribbon\")");
    assert_eq!(outcome.plots.len(), 2, "both plots capture");
    assert_eq!(outcome.fills.len(), 1, "one fill per call site");
    let fill = &outcome.fills[0];
    assert_eq!(fill.first_title, "Fast");
    assert_eq!(fill.second_title, "Slow");
    assert_eq!(fill.values.len(), 5);
    match &fill.values[0] {
        Some(PineValue::Array(pair)) => {
            assert_eq!(num(pair[0].as_ref()), 101.0);
            assert_eq!(num(pair[1].as_ref()), 99.0);
        }
        other => panic!("expected fill pair, got {other:?}"),
    }
}

#[test]
fn fill_gradient_bounds_pair_with_top_color_ink() {
    // The gradient overload (`plot, plot, top, bottom, top color, bottom
    // color`): the region still spans the two plots while the capture ink
    // reads the top color first, the bottom only as a fallback.
    let outcome = run("p1 = plot(high, title=\"Fast\")\n\
         p2 = plot(low, title=\"Slow\")\n\
         fill(p1, p2, 50, -50, close, na, title=\"Band\")");
    assert_eq!(outcome.fills.len(), 1);
    let fill = &outcome.fills[0];
    assert_eq!(fill.title, "Band");
    assert_eq!(fill.first_title, "Fast");
    assert_eq!(fill.second_title, "Slow");
    assert_eq!(
        num(fill.color.as_ref()),
        104.0,
        "top color inks the capture (last bar's close)"
    );
    match &fill.values[0] {
        Some(PineValue::Array(pair)) => {
            assert_eq!(num(pair[0].as_ref()), 101.0, "region spans the plots");
            assert_eq!(num(pair[1].as_ref()), 99.0);
        }
        other => panic!("expected fill pair, got {other:?}"),
    }
    // Bottom color is the fallback when the top is absent.
    let fallback = run("p1 = plot(high, title=\"Fast\")\n\
         p2 = plot(low, title=\"Slow\")\n\
         fill(p1, p2, 50, -50, na, open, title=\"Band\")");
    assert_eq!(
        num(fallback.fills[0].color.as_ref()),
        100.0,
        "bottom color inks when the top is absent"
    );
}

#[test]
fn fill_untagged_variable_refuses() {
    // A variable never bound from `plot()` has no site tag: the fill
    // refuses instead of pairing by value coincidence.
    let err = run_err("x = close\np2 = plot(low)\nfill(x, p2)");
    assert!(
        err.contains("no plot tag"),
        "untagged variable refuses, got: {err}"
    );
    // Rebinding a plot variable away from `plot()` clears its tag.
    let stale = run_err("p1 = plot(high)\np1 := close\np2 = plot(low)\nfill(p1, p2)");
    assert!(
        stale.contains("no plot tag"),
        "rebound variable refuses, got: {stale}"
    );
}

#[test]
fn label_pivot_markers_gate_on_type_and_show() {
    // Pivot markers record only when the pivot fired (`pivot_type != 0`)
    // and display is enabled (`i_pivots_show`): both gates scripted across
    // one five-bar run.
    let outcome = run("show = bar_index < 2\n\
         piv = bar_index == 1 ? 0 : 1\n\
         if piv != 0 and show\n\
         \x20   label.new(bar_index, high, \"x\", style=label.style_label_down)");
    let labels: Vec<&crate::pine::eval::DrawingEvent> = outcome
        .draw_events
        .iter()
        .filter(|event| {
            event.object == "label" && event.kind == crate::pine::eval::DrawingEventKind::Created
        })
        .collect();
    assert_eq!(labels.len(), 1, "only bar 0 passes both gates");
    assert_eq!(labels[0].bar, 0);
}

#[test]
fn table_create_delete_lifecycle_on_first_last() {
    // Managed-table shape without the matrix dependency: create plus a
    // header cell on the first bar, delete on the last.
    let outcome = run("var t = na\n\
         if barstate.isfirst\n\
         \x20   t := table.new(position.top_right, 2, 1)\n\
         \x20   table.cell(t, 0, 0, \"h\")\n\
         if barstate.islast\n\
         \x20   table.delete(t)");
    let kinds: Vec<crate::pine::eval::DrawingEventKind> = outcome
        .draw_events
        .iter()
        .filter(|event| event.object == "table")
        .map(|event| event.kind.clone())
        .collect();
    assert_eq!(
        kinds,
        vec![
            crate::pine::eval::DrawingEventKind::Created,
            crate::pine::eval::DrawingEventKind::Updated,
            crate::pine::eval::DrawingEventKind::Deleted,
        ],
        "create, cell, delete in order, got: {:?}",
        outcome.draw_events
    );
    assert_eq!(outcome.draw_events[0].bar, 0);
    assert_eq!(outcome.draw_events[2].bar, 4);
}

#[test]
fn table_merge_cells_records_an_update_row() {
    // Merging is display-only: the range records as an update against the
    // live table handle, and nothing readable changes.
    let outcome = run("var t = na\n\
         if barstate.isfirst\n\
         \x20   t := table.new(position.top_right, 2, 1)\n\
         \x20   table.merge_cells(t, 0, 0, 1, 0)");
    let tables: Vec<&crate::pine::eval::DrawingEvent> = outcome
        .draw_events
        .iter()
        .filter(|event| event.object == "table")
        .collect();
    assert_eq!(
        tables.len(),
        2,
        "create plus merge, got: {:?}",
        outcome.draw_events
    );
    assert_eq!(tables[0].kind, crate::pine::eval::DrawingEventKind::Created);
    assert_eq!(tables[1].kind, crate::pine::eval::DrawingEventKind::Updated);
    assert_eq!(tables[1].bar, 0);
    assert_eq!(tables[0].key, tables[1].key, "merge targets the live table");
}

#[test]
fn drawing_getters_read_back_created_state() {
    // Getters observe the recorded object: creation args plus later moves.
    let outcome = run("lb = label.new(3, high, \"x\")\n\
         label.set_x(lb, 7)\n\
         gx = label.get_x(lb)\n\
         bx = box.new(1, high, 4, low)\n\
         box.set_right(bx, 9)\n\
         gr = box.get_right(bx)\n\
         ln = line.new(1, low, 2, high)\n\
         line.set_x2(ln, 8)\n\
         g2 = line.get_x2(ln)");
    let get = |name: &str| match outcome.value_at(4, name) {
        Some(Some(PineValue::Float(v))) => v,
        Some(Some(PineValue::Int(v))) => v as f64,
        other => panic!("expected number for {name}, got {other:?}"),
    };
    assert_eq!(get("gx"), 7.0, "label move observed");
    assert_eq!(get("gr"), 9.0, "box move observed");
    assert_eq!(get("g2"), 8.0, "line move observed");
}

#[test]
fn drawing_handles_compare_without_arithmetic() {
    let outcome = run("a = label.new(1, high, \"x\")\n\
         b = a\n\
         c = label.new(2, low, \"y\")\n\
         same = a == b\n\
         diff = a != c\n\
         present = not na(a)");
    assert!(flag(&outcome, "same"), "same object compares equal");
    assert!(flag(&outcome, "diff"), "distinct objects compare unequal");
    assert!(flag(&outcome, "present"), "fresh handle is not na");
    let err = run_err("a = label.new(1, high, \"x\")\nz = a + 1");
    assert!(
        err.contains("does not apply"),
        "arithmetic on handles refuses, got: {err}"
    );
}

#[test]
fn dropped_capture_regression_every_shape_records() {
    // Kitchen sink: every drawing shape the shipped libraries call must
    // execute to a capture. Objects are created once, touched every bar
    // through persistent handles, and deleted on the last bar — so the
    // test also pins cross-bar handle persistence. If a new drawing call
    // is added without a recorder, it lands here and fails.
    let outcome = run("var lb = na\n\
         var tb = na\n\
         var bx = na\n\
         var ln = na\n\
         if barstate.isfirst\n\
         \x20   lb := label.new(bar_index, high, \"x\", style=label.style_label_down)\n\
         \x20   tb := table.new(position.top_right, 1, 1)\n\
         \x20   table.cell(tb, 0, 0, \"h\")\n\
         \x20   bx := box.new(bar_index, high, bar_index, low)\n\
         \x20   ln := line.new(bar_index, low, bar_index, high)\n\
         plotcandle(0.0, high, 0.0, high, title=\"Buy\")\n\
         barcolor(close)\n\
         fill(plot(high, title=\"F\"), plot(low, title=\"S\"), title=\"R\")\n\
         label.set_text(lb, \"y\")\n\
         gx = label.get_x(lb)\n\
         box.set_text(bx, \"b\")\n\
         gr = box.get_right(bx)\n\
         line.set_color(ln, close)\n\
         g2 = line.get_x2(ln)\n\
         if barstate.islast\n\
         \x20   label.delete(lb)\n\
         \x20   table.delete(tb)\n\
         \x20   box.delete(bx)\n\
         \x20   line.delete(ln)");
    assert_eq!(outcome.plotcandles.len(), 1);
    assert_eq!(outcome.barcolors.len(), 1);
    assert_eq!(outcome.fills.len(), 1);
    assert_eq!(outcome.plots.len(), 2, "fill still captures its plots");
    let created = outcome
        .draw_events
        .iter()
        .filter(|event| event.kind == crate::pine::eval::DrawingEventKind::Created)
        .count();
    let updated = outcome
        .draw_events
        .iter()
        .filter(|event| event.kind == crate::pine::eval::DrawingEventKind::Updated)
        .count();
    let deleted = outcome
        .draw_events
        .iter()
        .filter(|event| event.kind == crate::pine::eval::DrawingEventKind::Deleted)
        .count();
    assert_eq!(created, 4, "label, table, box, line created once");
    assert_eq!(
        updated, 16,
        "one cell plus three per-bar touches over five bars"
    );
    assert_eq!(deleted, 4, "every created object deleted");
    assert!(
        !outcome.draw_events.is_empty(),
        "lifecycle rows exist, nothing drawn silently"
    );
}

#[test]
fn unknown_drawing_methods_still_refuse() {
    // The recorder is not a silent sink: outside the recorded surface,
    // drawing calls refuse with a reason.
    let err = run_err("lb = label.new(1, high, \"x\")\nlabel.frobenicate(lb)");
    assert_eq!(
        err, "pine evaluation refused: unknown method 'label.frobenicate'",
        "exact refusal pins the recorded surface"
    );
    let gone = run_err("lb = label.new(1, high, \"x\")\nlabel.delete(lb)\nlabel.get_x(lb)");
    assert!(
        gone.contains("unknown label object"),
        "getters on deleted objects refuse, got: {gone}"
    );
}

// ---------------------------------------------------------------------------
// M5 — per-indicator capture contracts over linked task closures.
// ---------------------------------------------------------------------------
//
// Each discovered fixture closure links (shared `fixture::linked_fixture`)
// and runs headlessly with the datasets its `request.*` sites need.
// Captures record data only — rendering never happens and no `egui`
// dependency appears.

use crate::pine::test_fixture as fixture;

/// Engine for one chart timeframe: SPY scope with zero warmup bars, the
/// same evaluated-bar semantics the server headless proof runs under.
fn capture_engine(period: &str) -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period(period.to_owned())
            .warmup_bars(0)
            .build(),
    )
}

/// Daily dataset for the `"D"` pair, resampled from the fifteen-minute
/// tape (shared helper, same buckets the injection proof serves).
fn daily_dataset() -> LtfDataset {
    LtfDataset::of_bars(
        &fixture::daily_of(&fixture::history_bars("historical/SPY_15M.csv")),
        Timeframe::D1,
    )
}

/// Assert the lifecycle well-formedness property over one outcome: every
/// update/delete references a previously created key, and per-namespace
/// creations stay within the indicator header's maxima.
fn assert_lifecycle_wellformed(outcome: &RunOutcome, maxima: &[(&str, usize)], label: &str) {
    use std::collections::HashSet;
    let mut created: HashSet<&str> = HashSet::new();
    for event in &outcome.draw_events {
        match event.kind {
            crate::pine::eval::DrawingEventKind::Created => {
                created.insert(event.key.as_str());
            }
            crate::pine::eval::DrawingEventKind::Updated
            | crate::pine::eval::DrawingEventKind::Deleted => {
                assert!(
                    created.contains(event.key.as_str()),
                    "{label}: {} on '{}' precedes its creation",
                    if event.kind == crate::pine::eval::DrawingEventKind::Deleted {
                        "delete"
                    } else {
                        "update"
                    },
                    event.key
                );
            }
        }
    }
    for (namespace, max) in maxima {
        let count = created
            .iter()
            .filter(|key| key.starts_with(&format!("{namespace}#")))
            .count();
        assert!(
            count <= *max,
            "{label}: {count} {namespace} objects exceed the header maximum {max}"
        );
    }
}

#[test]
fn swing_label_mints_bounded_with_wellformed_lifecycle() {
    // Engine property the PPAS five-bar contract used to carry: a
    // condition that fires on exactly one bar mints exactly one label on
    // that bar, with no plots and a well-formed lifecycle. The script is
    // synthetic; the minting bound is what the engine guarantees.
    let outcome = run("if bar_index == 3\n\
         \x20   label.new(bar_index, high, \"ST-HH\")");
    assert_eq!(outcome.evaluated, 5, "every sliced bar evaluates");
    assert!(outcome.plots.is_empty(), "the script calls no plot");
    assert_eq!(outcome.draw_events.len(), 1, "one drawing event");
    let event = &outcome.draw_events[0];
    assert_eq!(event.object, "label", "the event mints a label");
    assert_eq!(event.kind, crate::pine::eval::DrawingEventKind::Created);
    assert_eq!(event.bar, 3, "the label lands on the firing bar");
    // Positional creation args run x, y, text: the lone string is the pin.
    let texts: Vec<String> = event
        .args
        .iter()
        .filter(|arg| arg.name.is_none())
        .filter_map(|arg| match &arg.value {
            Some(PineValue::Str(text)) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["ST-HH"], "short-term swing-high text pinned");
    assert_lifecycle_wellformed(
        &outcome,
        &[("label", 500), ("box", 500), ("line", 500)],
        "swing label",
    );
}

#[test]
fn version_skewed_pattern_reference_refuses() {
    // Fail-closed pin for cross-module references the exporter lacks: an
    // importer calling a function its library does not export refuses
    // instead of resolving across the alias. The modules are synthetic;
    // the exact refusal shape is what the engine guarantees.
    let lib = fixture::synthetic_library(
        "showmethegrail",
        "libCandles",
        "//@version=6\nlibrary(\"libCandles\")\nexport helper() => 1\n",
    );
    let driver = fixture::driver_module(
        "import showmethegrail/libCandles/11 as libCandles\nisPatternType = libCandles.isPatternType(close)\nplot(isPatternType)",
    );
    let modules = fixture::linked_modules(vec![lib, driver]);
    let chart = fixture::history_bars("historical/SPY_1M.csv");
    let mut engine = capture_engine("1");
    engine
        .inject_security("SPY", "D", daily_dataset())
        .expect("daily dataset injects");
    engine
        .inject_lower_tf("SPY", "1", LtfDataset::of_bars(&chart[..60], Timeframe::M1))
        .expect("lower-timeframe dataset injects");
    let reason = engine
        .run_linked(&modules, &chart[..30], InputSet::new())
        .expect_err("skewed reference refuses")
        .to_string();
    assert_eq!(
        reason, "pine evaluation refused: unknown function 'libCandles.isPatternType'",
        "exact refusal pins the missing export"
    );
}

#[test]
fn full_drawing_surface_captures_plots_fills_shapes_levels() {
    // Engine property the RSIMD contract used to carry: the full drawing
    // surface emits — nine plots, two gradient fills, four markers, three
    // levels — with zero label/box/line/table object events. The script is
    // synthetic; the capture arithmetic is what the engine guarantees.
    let outcome = run("r = plot(close, title=\"RSI\")\n\
         z = plot(0, title=\"plot:1\")\n\
         p1 = plot(close + 1, title=\"Bullish RSI HL\")\n\
         p2 = plot(close - 1, title=\"Bearish RSI LH\")\n\
         p3 = plot(low, title=\"Bullish Price LL\")\n\
         p4 = plot(low, title=\"Bullish Price LL BG\")\n\
         p5 = plot(high, title=\"Bearish Price HH\")\n\
         p6 = plot(high, title=\"Bearish Price HH BG\")\n\
         p7 = plot(50, title=\"Trend Health\")\n\
         fill(r, z, title=\"Fill A\")\n\
         fill(r, z, title=\"Fill B\")\n\
         plotshape(close > open ? high : na, title=\"Bullish Label\")\n\
         plotshape(close < open ? low : na, title=\"Bearish Label\")\n\
         plotshape(close > open ? low : na, title=\"Bullish Price LL Label\")\n\
         plotshape(close < open ? high : na, title=\"Bearish Price HH Label\")\n\
         hline(70, title=\"Trend Neutral\")\n\
         hline(30, title=\"Trending Threshold\")\n\
         hline(10, title=\"Oversold\")");
    assert_eq!(outcome.evaluated, 5, "every sliced bar evaluates");
    let titles: Vec<&str> = outcome
        .plots
        .iter()
        .map(|plot| plot.title.as_str())
        .collect();
    assert_eq!(
        titles,
        vec![
            "RSI",
            "plot:1",
            "Bullish RSI HL",
            "Bearish RSI LH",
            "Bullish Price LL",
            "Bullish Price LL BG",
            "Bearish Price HH",
            "Bearish Price HH BG",
            "Trend Health",
        ],
        "all nine plot sites capture"
    );
    for plot in &outcome.plots {
        assert_eq!(plot.values.len(), 5, "plot values cover every bar");
    }
    assert_eq!(outcome.fills.len(), 2, "both gradient fills capture");
    for fill in &outcome.fills {
        assert_eq!(fill.first_title, "RSI", "fills pair the RSI plot");
        assert_eq!(fill.second_title, "plot:1", "fills pair the zero line");
        assert_eq!(fill.values.len(), 5);
    }
    let shapes: Vec<&str> = outcome
        .plotshapes
        .iter()
        .map(|shape| shape.title.as_str())
        .collect();
    assert_eq!(
        shapes,
        vec![
            "Bullish Label",
            "Bearish Label",
            "Bullish Price LL Label",
            "Bearish Price HH Label",
        ],
        "all four markers capture"
    );
    let levels: Vec<&str> = outcome
        .hlines
        .iter()
        .map(|line| line.title.as_str())
        .collect();
    assert_eq!(
        levels,
        vec!["Trend Neutral", "Trending Threshold", "Oversold"],
        "all three levels capture with positional titles"
    );
    assert!(
        outcome.draw_events.is_empty(),
        "the script mints no label/box/line/table objects"
    );
    assert_lifecycle_wellformed(&outcome, &[], "drawing surface");
}

#[test]
fn same_titled_plots_pair_by_site_key() {
    // Site-keying distinguishes same-titled plots: two `plot()` calls
    // with one title capture as two series under two site keys, and a
    // fill over their variables pairs the tagged sites, not the title.
    let outcome = run("a = plot(high, title=\"Same\")\n\
         b = plot(low, title=\"Same\")\n\
         fill(a, b, title=\"Pair\")");
    assert_eq!(outcome.plots.len(), 2, "two captures, one title");
    assert_eq!(outcome.plots[0].title, "Same");
    assert_eq!(outcome.plots[1].title, "Same");
    assert_eq!(outcome.fills.len(), 1);
    let fill = &outcome.fills[0];
    assert_eq!(fill.first_title, "Same");
    assert_eq!(fill.second_title, "Same");
    match &fill.values[0] {
        Some(PineValue::Array(pair)) => {
            assert_eq!(num(pair[0].as_ref()), 101.0, "first site rides high");
            assert_eq!(num(pair[1].as_ref()), 99.0, "second site rides low");
        }
        other => panic!("expected fill pair, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// M5.3 — drawing surface probe: every drawing-namespace token across the
// four closures either resolves/captures through the served surface or
// names a checked-in deferred item with its reason. A new spelling fails
// here before it can run anywhere.
// ---------------------------------------------------------------------------

/// Pure-constant drawing namespaces: these heads can never be variables,
/// so every non-call token must resolve through `resolve_constant`.
const CONSTANT_NAMESPACES: &[&str] = &["display", "size", "position", "shape", "location", "hline"];

/// Drawing-namespace methods the recorder serves (mirrors the
/// `call_drawing` dispatch table: new/delete/cell/merge_cells plus the
/// setter/getter arms).
const SERVED_DRAWING_METHODS: &[&str] = &[
    "new",
    "delete",
    "cell",
    "merge_cells",
    "set_x",
    "set_y",
    "set_textcolor",
    "set_style",
    "set_tooltip",
    "set_text",
    "set_color",
    "set_left",
    "set_top",
    "set_right",
    "set_bottom",
    "set_bgcolor",
    "set_border_color",
    "set_xy1",
    "set_xy2",
    "set_width",
    "set_x2",
    "get_x",
    "get_right",
    "get_x2",
];

/// Drawing-namespace methods observed in the frozen task files but dead on
/// every headless regime (DVP's profile/curve paths need filled bins, which
/// stay empty from 8-bar slices through full-history defaults): refused if
/// evaluated, never silently sunk.
const DEFERRED_DRAWING_METHODS: &[(&str, &str)] = &[
    ("box", "set_lefttop"),
    ("box", "set_rightbottom"),
    ("box", "set_border_width"),
    ("polyline", "new"),
    ("polyline", "delete"),
];

/// Bare drawing calls the engine captures.
const SERVED_BARE_CALLS: &[&str] = &[
    "plot",
    "plotshape",
    "plotcandle",
    "barcolor",
    "fill",
    "hline",
];

/// Closure files under proof: every linkable fixture closure plus every
/// linkable live closure (libraries shared; refused indicators contribute
/// no file — their verdict is the refusal, pinned by the discovery
/// suites).
fn discovered_closure_files() -> Vec<(String, String)> {
    use crate::ClosureOutcome;
    let mut files = Vec::new();
    for outcome in fixture::fixture_outcomes()
        .into_iter()
        .chain(fixture::discovered_outcomes())
    {
        let ClosureOutcome::Linked(closure) = outcome else {
            continue;
        };
        for (key, source) in closure.sources {
            files.push((format!("linked:{key}"), source.text));
        }
    }
    files
}

/// Fixture closure files only: the exact-equality display round-trip runs
/// here, because display formatting over arbitrary third-party text is not
/// a churn-proof property.
fn m5_closure_files() -> Vec<(String, String)> {
    use crate::ClosureOutcome;
    let mut files = Vec::new();
    for outcome in fixture::fixture_outcomes() {
        let ClosureOutcome::Linked(closure) = outcome else {
            continue;
        };
        for (key, source) in closure.sources {
            files.push((format!("fixture:{key}"), source.text));
        }
    }
    files
}

/// Scan one source for `head.member` tokens with word boundaries (decimal
/// points like `692.73` never qualify: the head must start alphabetic).
/// Line comments strip first (`//` inside string literals stays).
/// Returns `(head, member, is_call)` per occurrence in file order.
fn dotted_tokens(text: &str) -> Vec<(String, String, bool)> {
    let mut code = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let mut quote = None;
        let mut cut = line.len();
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
            } else if c == '\'' || c == '"' {
                quote = Some(c);
            } else if c == '/' && chars.get(i + 1) == Some(&'/') {
                cut = line.char_indices().nth(i).map_or(line.len(), |(b, _)| b);
                break;
            }
            i += 1;
        }
        code.push_str(&line[..cut]);
    }
    let text = code;
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'.' {
            let mut start = i;
            while start > 0
                && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_')
            {
                start -= 1;
            }
            let mut end = i + 1;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            let head = &text[start..i];
            let member = &text[i + 1..end];
            let head_ok = head
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
            let member_ok = member
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
            let boundary_before = start == 0
                || !(bytes[start - 1].is_ascii_alphanumeric()
                    || bytes[start - 1] == b'_'
                    || bytes[start - 1] == b'.');
            if head_ok && member_ok && boundary_before && !head.is_empty() && !member.is_empty() {
                let mut rest = end;
                while rest < bytes.len() && bytes[rest].is_ascii_whitespace() {
                    rest += 1;
                }
                out.push((
                    head.to_owned(),
                    member.to_owned(),
                    rest < bytes.len() && bytes[rest] == b'(',
                ));
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn drawing_constants_resolve_or_name_deferred() {
    // Every drawing-namespace token across every discovered closure lands in
    // exactly one bucket: a resolving constant, a served call, or a
    // checked-in deferred item. Anything else fails the probe.
    use crate::pine::builtins_core::resolve_constant;
    let mut failures = Vec::new();
    for (file, text) in discovered_closure_files() {
        for (head, member, is_call) in dotted_tokens(&text) {
            let dotted = format!("{head}.{member}");
            if CONSTANT_NAMESPACES.contains(&head.as_str()) {
                if is_call {
                    failures.push(format!("{file}: unexpected call {dotted}()"));
                } else if resolve_constant(&dotted).is_none() {
                    failures.push(format!("{file}: constant {dotted} refuses"));
                }
            } else if head == "color" {
                if is_call {
                    if !["new", "rgb", "from_gradient", "t", "r", "g", "b"]
                        .contains(&member.as_str())
                    {
                        failures.push(format!("{file}: unserved color call {dotted}()"));
                    }
                } else if resolve_constant(&dotted).is_none() {
                    failures.push(format!("{file}: named color {dotted} refuses"));
                }
            } else if ["label", "line", "text"].contains(&head.as_str()) {
                if !is_call && resolve_constant(&dotted).is_none() {
                    failures.push(format!("{file}: constant {dotted} refuses"));
                }
            } else if ["box", "table", "polyline"].contains(&head.as_str()) {
                if !is_call {
                    failures.push(format!("{file}: unexpected constant {dotted}"));
                } else if !SERVED_DRAWING_METHODS.contains(&member.as_str())
                    && !DEFERRED_DRAWING_METHODS.contains(&(head.as_str(), member.as_str()))
                {
                    failures.push(format!("{file}: unserved drawing call {dotted}()"));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "drawing surface probe:\n{}",
        failures.join("\n")
    );
}

#[test]
fn bare_drawing_calls_stay_captured() {
    // No `plotchar`/`plotbar`/`bgcolor` (or any other bare drawing call)
    // appears in any discovered closure: every bare call site captures.
    let mut found = Vec::new();
    for (file, text) in discovered_closure_files() {
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'(' {
                let mut start = i;
                while start > 0
                    && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_')
                {
                    start -= 1;
                }
                let name = &text[start..i];
                let boundary_before = start == 0
                    || !(bytes[start - 1].is_ascii_alphanumeric()
                        || bytes[start - 1] == b'_'
                        || bytes[start - 1] == b'.');
                if boundary_before
                    && [
                        "plot",
                        "plotshape",
                        "plotchar",
                        "plotbar",
                        "plotcandle",
                        "barcolor",
                        "bgcolor",
                        "fill",
                        "hline",
                    ]
                    .contains(&name)
                    && !SERVED_BARE_CALLS.contains(&name)
                {
                    found.push(format!("{file}: uncaptured bare call {name}()"));
                }
                i += 1;
            } else {
                i += 1;
            }
        }
    }
    assert!(
        found.is_empty(),
        "bare drawing calls:\n{}",
        found.join("\n")
    );
}

#[test]
fn display_round_trip_preserves_drawing_sites() {
    // `display_program` re-parse equality already holds per M1; here the
    // drawing surface is pinned explicitly: every drawing call site in
    // every fixture file survives the round trip.
    let site_counts = |text: &str| {
        let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
        for (head, member, is_call) in dotted_tokens(text) {
            if ["label", "box", "line", "table", "polyline"].contains(&head.as_str()) && is_call {
                *counts.entry(format!("{head}.{member}")).or_default() += 1;
            }
        }
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'(' {
                let mut start = i;
                while start > 0
                    && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_')
                {
                    start -= 1;
                }
                let name = &text[start..i];
                let boundary_before = start == 0
                    || !(bytes[start - 1].is_ascii_alphanumeric()
                        || bytes[start - 1] == b'_'
                        || bytes[start - 1] == b'.');
                if boundary_before && SERVED_BARE_CALLS.contains(&name) {
                    *counts.entry(name.to_owned()).or_default() += 1;
                }
                i += 1;
            } else {
                i += 1;
            }
        }
        counts
    };
    for (file, text) in m5_closure_files() {
        let program =
            parse_program(&text).unwrap_or_else(|err| panic!("closure file {file} parses: {err}"));
        let displayed = crate::display_program(&program);
        assert_eq!(
            site_counts(&displayed),
            site_counts(&text),
            "{file} keeps every drawing call site through display"
        );
    }
}

#[test]
fn delete_of_absence_is_a_silent_noop() {
    // Published scripts guard rolling drawings with `line.delete(l[1])`
    // over an `na`-seeded holder: deleting absence drops silently (no
    // event, no refusal), exactly like the reference runtime. Any other
    // misuse still refuses.
    let outcome = run("line l = na\nline.delete(l)\nplot(close, title=\"C\")");
    assert!(
        outcome.draw_events.is_empty(),
        "no event for deleting na: {:?}",
        outcome.draw_events
    );
}

#[test]
fn fill_four_positional_bounds_pair_like_six() {
    // The legacy gradient overload (`plot, plot, top value, bottom
    // value`): the region still spans the two plots and the value bounds
    // evaluate (their refusals stand) while the capture records the
    // plots — exactly like the six-positional form.
    let outcome = run("p1 = plot(high, title=\"Fast\")\n\
         p2 = plot(low, title=\"Slow\")\n\
         fill(p1, p2, 50, -50, title=\"Band\")");
    assert_eq!(outcome.fills.len(), 1);
    let fill = &outcome.fills[0];
    assert_eq!(fill.title, "Band");
    match &fill.values[0] {
        Some(PineValue::Array(pair)) => {
            assert_eq!(num(pair[0].as_ref()), 101.0, "region spans the plots");
            assert_eq!(num(pair[1].as_ref()), 99.0);
        }
        other => panic!("expected fill pair, got {other:?}"),
    }
}
