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

fn bar_at(index: i64) -> Bar {
    Bar::new()
        .ts(
            chrono::Utc
                .timestamp_opt(1_700_000_000 + index * 60, 0)
                .single()
                .expect("valid test timestamp"),
        )
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
        assert_eq!(num(buy.highs[bar].as_ref()), 101.0, "buy top rides the high leg");
        assert_eq!(num(buy.lows[bar].as_ref()), 0.0, "buy bottom pins zero");
        assert_eq!(num(buy.closes[bar].as_ref()), 101.0, "buy close rides the high leg");
    }
    let sell = &outcome.plotcandles[1];
    assert_eq!(sell.title, "Delta Sell");
    for bar in 0..5 {
        assert_eq!(num(sell.lows[bar].as_ref()), -99.0, "sell bottom negates the low leg");
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
fn label_pivot_markers_gate_on_type_and_show() {
    // Pivot markers record only when the pivot fired (`pivot_type != 0`)
    // and display is enabled (`i_pivots_show`): both gates scripted across
    // one five-bar run.
    let outcome = run(
        "show = bar_index < 2\n\
         piv = bar_index == 1 ? 0 : 1\n\
         if piv != 0 and show\n\
         \x20   label.new(bar_index, high, \"x\", style=label.style_label_down)",
    );
    let labels: Vec<&crate::pine::eval::DrawingEvent> = outcome
        .draw_events
        .iter()
        .filter(|event| {
            event.object == "label"
                && event.kind == crate::pine::eval::DrawingEventKind::Created
        })
        .collect();
    assert_eq!(labels.len(), 1, "only bar 0 passes both gates");
    assert_eq!(labels[0].bar, 0);
}

#[test]
fn table_create_delete_lifecycle_on_first_last() {
    // Managed-table shape without the matrix dependency: create plus a
    // header cell on the first bar, delete on the last.
    let outcome = run(
        "var t = na\n\
         if barstate.isfirst\n\
         \x20   t := table.new(position.top_right, 2, 1)\n\
         \x20   table.cell(t, 0, 0, \"h\")\n\
         if barstate.islast\n\
         \x20   table.delete(t)",
    );
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
fn drawing_getters_read_back_created_state() {
    // Getters observe the recorded object: creation args plus later moves.
    let outcome = run(
        "lb = label.new(3, high, \"x\")\n\
         label.set_x(lb, 7)\n\
         gx = label.get_x(lb)\n\
         bx = box.new(1, high, 4, low)\n\
         box.set_right(bx, 9)\n\
         gr = box.get_right(bx)\n\
         ln = line.new(1, low, 2, high)\n\
         line.set_x2(ln, 8)\n\
         g2 = line.get_x2(ln)",
    );
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
    let outcome = run(
        "a = label.new(1, high, \"x\")\n\
         b = a\n\
         c = label.new(2, low, \"y\")\n\
         same = a == b\n\
         diff = a != c\n\
         present = not na(a)",
    );
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
    let outcome = run(
        "var lb = na\n\
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
         \x20   line.delete(ln)",
    );
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
        err,
        "pine evaluation refused: unknown method 'label.frobenicate'",
        "exact refusal pins the recorded surface"
    );
    let gone = run_err("lb = label.new(1, high, \"x\")\nlabel.delete(lb)\nlabel.get_x(lb)");
    assert!(
        gone.contains("unknown label object"),
        "getters on deleted objects refuse, got: {gone}"
    );
}
