#![cfg(feature = "bytecode")]
//! Contiguous series behavior: the numeric fast path reads history through
//! indexed float columns with a hole for absence, never a stored zero and
//! never a wrap past the first bar. Every test below runs both engines and
//! pins the observable contract — absence stays absence, offsets at the
//! window edge read absence, and numeric batches agree within tolerance.
//!
//! Test path note: this file rides the `pine` unit module, so the
//! runnable filter is `pine`, not the file stem.

use super::bytecode_diff_tests::{
    compile_case, link_case, link_script, ramp_bars, run_tree_walk_result, run_vm,
};
use crate::pine::bytecode_exec::{FloatSeries, VmConfig};
use crate::pine::test_fixture as fixture;
use crate::{InputSet, PineEngine, PineRuntimeConfig, RunOutcome};
use protopyne_types::Bar;

fn engine_config() -> PineRuntimeConfig {
    PineRuntimeConfig::new()
        .chart_symbol("SPY".to_owned())
        .chart_period("1".to_owned())
        .warmup_bars(0)
        .build()
}

fn run_tree(modules: &[crate::LinkedModule], bars: &[Bar]) -> RunOutcome {
    let mut engine = PineEngine::new(engine_config());
    engine.set_bytecode_mode(crate::pine::BytecodeMode::TreeWalk);
    engine
        .run_linked(modules, bars, InputSet::new())
        .expect("series case evaluates")
}

fn run_vm_headless(modules: &[crate::LinkedModule], bars: &[Bar]) -> RunOutcome {
    let program = compile_case(modules);
    let linked = link_case(&program, modules).expect("series case links");
    run_vm(&linked, bars, &VmConfig::headless()).expect("vm series run evaluates")
}

fn plot_numbers(outcome: &RunOutcome, index: usize) -> Vec<Option<f64>> {
    outcome.plots[index]
        .values
        .iter()
        .map(|cell| match cell {
            None => None,
            Some(crate::PineValue::Float(v)) => Some(*v),
            Some(crate::PineValue::Int(v)) => Some(*v as f64),
            Some(other) => panic!("numeric series holds numbers, got {other:?}"),
        })
        .collect()
}

#[test]
fn offset_before_first_bar_reads_absence_not_zero() {
    // `close[5] + 1.0` on the first five bars must be absence: a stored
    // zero would surface as `Some(1.0)` here instead of `None`.
    let modules = link_script("v = close[5]\nplot(v + 1.0, title=\"V\")");
    let bars = ramp_bars(10);
    for outcome in [run_tree(&modules, &bars), run_vm_headless(&modules, &bars)] {
        let got = plot_numbers(&outcome, 0);
        assert_eq!(got.len(), 10, "one slot per evaluated bar");
        for (bar, value) in got.iter().enumerate().take(5) {
            assert_eq!(*value, None, "bar {bar}: offset past start is absence");
        }
        for (bar, value) in got.iter().enumerate().skip(5) {
            assert!(
                value.is_some(),
                "bar {bar}: offset inside the window resolves"
            );
        }
    }
}

#[test]
fn hole_on_every_leg_reads_absence_without_wrapping() {
    // Every lowerable chart leg shares the same edge rule: an offset past
    // the first bar reads absence, never zero and never a wrapped tail
    // value. Derived legs (`hl2`/`hlc3`/`ohlc4`/`wcl`) stay on the
    // tree-walk by design, so they pin the same rule tree-only below.
    let legs = ["open", "high", "low", "close", "volume", "time"];
    let bars = ramp_bars(10);
    for leg in legs {
        let script = format!("v = {leg}[5]\nplot(v + 1.0, title=\"V\")");
        let modules = link_script(&script);
        for outcome in [run_tree(&modules, &bars), run_vm_headless(&modules, &bars)] {
            let got = plot_numbers(&outcome, 0);
            assert_eq!(got.len(), 10, "{leg}: one slot per evaluated bar");
            for (bar, value) in got.iter().enumerate().take(5) {
                assert_eq!(*value, None, "{leg} bar {bar}: hole, not zero");
            }
            for (bar, value) in got.iter().enumerate().skip(5) {
                assert!(
                    value.is_some(),
                    "{leg} bar {bar}: in-window offset resolves"
                );
            }
        }
        let tree = run_tree(&modules, &bars);
        let vm = run_vm_headless(&modules, &bars);
        assert_eq!(
            super::bytecode_diff_tests::canonical_form(&vm),
            super::bytecode_diff_tests::canonical_form(&tree),
            "{leg}: engines agree bar for bar"
        );
    }
    // Derived legs ride the tree-walk (the compiler lowers chart legs
    // only): the hole rule still pins there, never a zero default.
    for leg in ["hl2", "hlc3", "ohlc4", "wcl"] {
        let script = format!("v = {leg}[5]\nplot(v + 1.0, title=\"V\")");
        let modules = link_script(&script);
        let outcome = run_tree(&modules, &bars);
        let got = plot_numbers(&outcome, 0);
        assert_eq!(got.len(), 10, "{leg}: one slot per evaluated bar");
        for (bar, value) in got.iter().enumerate().take(5) {
            assert_eq!(*value, None, "{leg} bar {bar}: hole, not zero");
        }
    }
}

#[test]
fn absent_input_propagates_through_arithmetic() {
    // `na + 1.0` is absence on every bar, on both engines — the hole is
    // never widened into a zero before the add.
    let cases = [
        "v = na\nplot(v + 1.0, title=\"V\")",
        "v = close > open ? high : na\nplot(v + 1.0, title=\"V\")",
    ];
    let bars = ramp_bars(30);
    for script in cases {
        let modules = link_script(script);
        let tree = run_tree(&modules, &bars);
        let vm = run_vm_headless(&modules, &bars);
        assert_eq!(
            super::bytecode_diff_tests::canonical_form(&vm),
            super::bytecode_diff_tests::canonical_form(&tree),
            "{script}: absence propagates identically"
        );
    }
}

#[test]
fn float_and_tree_runs_agree_on_numeric_corpus() {
    // Straight-line numeric scripts exercise the float columns on every
    // bar: user series, offsets, persistence, calls, and the `ta` bridge.
    let scripts = [
        "plot(close + open * 2.0 + 1.0, title=\"A\")",
        "v = close[3] - open[0]\nplot(v, title=\"W\")",
        "var x = 0.0\nx := x + close\nplot(x, title=\"X\")",
        "f(v) => v * 2.0 + 1.0\nplot(f(close), title=\"F\")",
        "plot(ta.sma(close, 14), title=\"S\")",
    ];
    let bars = ramp_bars(60);
    for script in scripts {
        let modules = link_script(script);
        let tree = run_tree(&modules, &bars);
        let vm = run_vm_headless(&modules, &bars);
        assert_eq!(
            super::bytecode_diff_tests::canonical_form(&vm),
            super::bytecode_diff_tests::canonical_form(&tree),
            "{script}: float columns stay byte-identical"
        );
    }
}

#[test]
fn kernel_batch_matches_tree_within_tolerance() {
    // The float columns must agree with the tree-walk batch at `1e-9`
    // through the shared comparator — a looser gate than byte-identity
    // that proves the numeric path, not just the scalar fallback.
    let scripts = [
        "plot(ta.sma(close, 14), title=\"S\")",
        "v = close[3] - open[0]\nplot(v, title=\"W\")",
        "var x = 0.0\nx := x + close\nplot(x, title=\"X\")",
    ];
    let bars = ramp_bars(60);
    for script in scripts {
        let modules = link_script(script);
        let tree = plot_numbers(&run_tree(&modules, &bars), 0);
        let vm = plot_numbers(&run_vm_headless(&modules, &bars), 0);
        crate::parity::assert_expr_parity(&vm, &tree, 1e-9, script)
            .expect("float batch matches tree within tolerance");
    }
    // Fixture closures: those that link run the same parity; those the
    // linker refuses stay tree-only by design (coverage measured, not gated).
    for stem in ["_FastMAs", "_IntegrationLite"] {
        let modules = fixture::linked_fixture(stem);
        let program = compile_case(&modules);
        match link_case(&program, &modules) {
            Ok(linked) => {
                let config = VmConfig::headless();
                let bars = ramp_bars(30);
                let tree = PineEngine::new(engine_config())
                    .run_linked(&modules, &bars, InputSet::new())
                    .expect("fixture evaluates on tree");
                let vm = run_vm(&linked, &bars, &config).expect("linked fixture runs");
                assert_eq!(vm.evaluated, tree.evaluated, "{stem}: same bar count");
                for (index, plot) in tree.plots.iter().enumerate() {
                    let want: Vec<Option<f64>> = plot
                        .values
                        .iter()
                        .map(|cell| match cell {
                            None => None,
                            Some(crate::PineValue::Float(v)) => Some(*v),
                            Some(crate::PineValue::Int(v)) => Some(*v as f64),
                            _ => None,
                        })
                        .collect();
                    let have: Vec<Option<f64>> = vm.plots[index]
                        .values
                        .iter()
                        .map(|cell| match cell {
                            None => None,
                            Some(crate::PineValue::Float(v)) => Some(*v),
                            Some(crate::PineValue::Int(v)) => Some(*v as f64),
                            _ => None,
                        })
                        .collect();
                    crate::parity::assert_expr_parity(&have, &want, 1e-9, stem)
                        .expect("linked fixture matches within tolerance");
                }
            }
            Err(_) => {
                // Whole-run fallback by design: coverage is measured, never
                // gated. The fixture compiled to at least an entry block
                // (the M3 shape), and the tree remains its oracle in M8.
                assert!(
                    program
                        .get(&modules[0].key)
                        .is_some_and(|module| !module.blocks.is_empty()),
                    "{stem}: fallback still compiles to blocks"
                );
            }
        }
    }
}

#[test]
fn evaluated_bars_fill_one_slot_per_column() {
    // One slot per evaluated bar in every recorded column: snapshots,
    // plots, and the contiguous float series all advance together.
    let modules = link_script("var x = 0.0\nx := x + close\nplot(x + close[1], title=\"X\")");
    let bars = ramp_bars(30);
    for outcome in [run_tree(&modules, &bars), run_vm_headless(&modules, &bars)] {
        assert_eq!(outcome.evaluated, 30, "every bar evaluates");
        assert_eq!(outcome.snapshots.len(), 30, "one snapshot per bar");
        assert_eq!(outcome.plots.len(), 1, "one capture site");
        assert_eq!(outcome.plots[0].values.len(), 30, "one plot value per bar");
    }
    let mut series = FloatSeries::new();
    assert!(series.is_empty(), "fresh series holds nothing");
    for bar in 0..30 {
        series.push_number(Some(bar as f64));
    }
    assert_eq!(series.len(), 30, "one float slot per bar");
}

#[test]
fn float_series_hole_round_trip() {
    // Absence maps to the hole and back: `None` stores the hole value
    // and reads back as absence, never as zero.
    let mut series = FloatSeries::new();
    series.push_number(None);
    series.push_number(Some(2.5));
    series.push_number(None);
    assert_eq!(series.len(), 3, "pushes append one slot each");
    assert_eq!(series.get(0), None, "hole reads as absence");
    assert_eq!(series.get(1), Some(2.5), "finite value round-trips");
    assert_eq!(series.get(2), None, "trailing hole reads as absence");
    assert_eq!(series.get(99), None, "out-of-range reads as absence");
}

#[test]
fn float_series_distinguishes_hole_from_zero() {
    // Zero is data, the hole is absence: `0.0 + 1.0` is `1.0` while
    // `hole + 1.0` stays absence. A zero-defaulting column would fail here.
    let mut series = FloatSeries::new();
    series.push_number(Some(0.0));
    series.push_number(None);
    assert_eq!(series.get(0), Some(0.0), "zero is a value");
    assert_eq!(series.get(1), None, "hole is absence");
    assert_ne!(
        series.get(0),
        series.get(1),
        "zero and hole never compare equal"
    );
}

#[test]
fn history_edge_refusals_match_across_engines() {
    // The reachable history edges refuse identically out of both
    // evaluators: a negative offset names the offset, an array overrun
    // names the index. Both selections must refuse with the same
    // strings — the loop through the shared constructors, the tree
    // through the same call sites. The coverage assertion keeps the
    // comparison honest: a fallback would run the tree twice and agree
    // vacuously.
    for (script, wording) in [
        (
            "v = close[0 - 2]\nplot(v, title=\"V\")",
            "negative history offset -2 is not a series read",
        ),
        (
            "a = [1.0, 2.0]\nv = a[5]\nplot(v, title=\"V\")",
            "array index 5 out of bounds",
        ),
        (
            "f(n) => f(n + 1)\nplot(f(0), title=\"V\")",
            "call depth exceeds 64",
        ),
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).expect("history shapes link");
        assert!(
            (linked.coverage() - 1.0).abs() < f64::EPSILON,
            "{script}: the loop actually runs; fallback would vacate the comparison"
        );
        let bars = ramp_bars(10);
        let tree = run_tree_walk_result(&modules, &bars).expect_err("tree refuses the edge");
        let vm = run_vm(&linked, &bars, &VmConfig::headless()).expect_err("loop refuses the edge");
        assert!(
            tree.to_string().contains(wording),
            "{script}: tree names the shared string: {tree}"
        );
        assert_eq!(
            vm.to_string(),
            tree.to_string(),
            "{script}: loop matches the tree exactly"
        );
    }
}

#[test]
fn history_undefined_variable_names_the_variable_on_the_tree() {
    // The unrecorded-name edge never links (unknown names do not lower),
    // so no loop run can reach it — but the wording is still shared, and
    // the tree side pins it here.
    let modules = link_script("v = nosuchvar[0]\nplot(v, title=\"V\")");
    let refused = run_tree_walk_result(&modules, &ramp_bars(3)).expect_err("unknown name refuses");
    assert!(
        refused
            .to_string()
            .contains("undefined variable 'nosuchvar'"),
        "the tree names the shared string: {refused}"
    );
}

#[test]
fn history_refusal_constructors_name_the_shared_strings() {
    // One wording per edge, shared by both evaluators: the constructors
    // below are the single source for every history refusal text.
    use crate::pine::eval::{
        array_index_out_of_bounds, array_negative_index, history_negative_offset,
        undefined_variable,
    };
    assert_eq!(
        history_negative_offset(-2_i64).to_string(),
        "pine evaluation refused: negative history offset -2 is not a series read",
    );
    assert_eq!(
        array_index_out_of_bounds(5).to_string(),
        "pine evaluation refused: array index 5 out of bounds",
    );
    assert_eq!(
        array_negative_index(-1_i64).to_string(),
        "pine evaluation refused: negative array index -1",
    );
    assert_eq!(
        undefined_variable("nosuchvar").to_string(),
        "pine evaluation refused: undefined variable 'nosuchvar'",
    );
}

#[test]
fn fill_refusals_match_across_engines() {
    // The fill pairer's runtime edges refuse identically out of both
    // evaluators: a leg variable with no plot tag names the variable.
    // The loop resolves the tag through its link-time columns, the tree
    // through its plot-variable map — same reasons, one wording each.
    // The coverage assertion keeps the comparison honest: a fallback
    // would run the tree twice and agree vacuously.
    let modules = link_script("x = 1.0\nfill(x, plot(open, \"B\"))\nplot(close, \"P\")");
    let program = compile_case(&modules);
    let linked = link_case(&program, &modules).expect("fill shapes link");
    assert!(
        (linked.coverage() - 1.0).abs() < f64::EPSILON,
        "the loop actually runs; fallback would vacate the comparison"
    );
    let bars = ramp_bars(10);
    let tree = run_tree_walk_result(&modules, &bars).expect_err("tree refuses the untagged leg");
    let vm =
        run_vm(&linked, &bars, &VmConfig::headless()).expect_err("loop refuses the untagged leg");
    assert!(
        tree.to_string()
            .contains("fill names variable 'x' with no plot tag this run"),
        "tree names the shared string: {tree}"
    );
    assert_eq!(
        vm.to_string(),
        tree.to_string(),
        "loop matches the tree exactly"
    );
}

#[test]
fn untitled_capture_titles_match_across_engines() {
    // Captures without a static title fall back to `family:count` on
    // both engines: the tree resolves the missing title per bar, the
    // loop reads the link-time absence — same keys, one spelling.
    let modules = link_script("hline(50.0)\nplot(close, title=\"P\")");
    let program = compile_case(&modules);
    let linked = link_case(&program, &modules).expect("untitled shapes link");
    assert!(
        (linked.coverage() - 1.0).abs() < f64::EPSILON,
        "the loop actually runs; fallback would vacate the comparison"
    );
    let bars = ramp_bars(10);
    let tree = run_tree_walk_result(&modules, &bars).expect("tree evaluates");
    let vm = run_vm(&linked, &bars, &VmConfig::headless()).expect("loop evaluates");
    assert_eq!(
        format!("{:?}", tree.hlines),
        format!("{:?}", vm.hlines),
        "hline fallbacks agree (titles and sites)"
    );
    for (engine, outcome) in [("tree", &tree), ("loop", &vm)] {
        assert_eq!(
            outcome.hlines.len(),
            1,
            "{engine}: one capture for one site across all bars (once-per-site)"
        );
        assert_eq!(
            outcome.hlines[0].title, "hline:0",
            "{engine}: fallback counts first sights, never bars"
        );
        assert_eq!(
            outcome.hlines[0].values.len(),
            10,
            "{engine}: one value per bar"
        );
    }
}

#[test]
fn capture_pairing_constructors_name_the_shared_strings() {
    // One wording per pairing edge, shared by both evaluators: the
    // constructors below are the single source for the fill refusal
    // texts and the untitled-capture fallback.
    use crate::pine::eval::{
        capture_fallback_title, fill_plot_site_without_capture, fill_variable_without_plot_tag,
    };
    assert_eq!(capture_fallback_title("hline", 0), "hline:0");
    assert_eq!(capture_fallback_title("fill", 2), "fill:2");
    assert_eq!(
        fill_variable_without_plot_tag("x").to_string(),
        "pine evaluation refused: fill names variable 'x' with no plot tag this run",
    );
    assert_eq!(
        fill_plot_site_without_capture("plot@[]").to_string(),
        "pine evaluation refused: fill names a plot site with no capture this bar: plot@[]",
    );
}

#[test]
fn loop_bound_constructors_name_the_shared_strings() {
    // One wording per loop bound, shared by both evaluators: the tree
    // refuses through the same constructors the loop calls, so the
    // cold-path templates cannot drift apart.
    use crate::pine::eval::{
        call_depth_over_limit, for_in_absent, for_in_not_array, for_in_over_limit,
        for_range_over_limit, while_over_limit,
    };
    assert_eq!(
        for_range_over_limit(0, 20_000).to_string(),
        "pine evaluation refused: 'for' range 0..=20000 exceeds 10000 iterations",
    );
    assert_eq!(
        for_in_not_array(&crate::PineValue::Float(1.0)).to_string(),
        "pine evaluation refused: 'for ... in' iterates arrays, got 1",
    );
    assert_eq!(
        for_in_absent().to_string(),
        "pine evaluation refused: 'for ... in' iterates arrays, got na",
    );
    assert_eq!(
        for_in_over_limit(20_000).to_string(),
        "pine evaluation refused: 'for ... in' over 20000 elements exceeds 10000 iterations",
    );
    assert_eq!(
        while_over_limit().to_string(),
        "pine evaluation refused: 'while' exceeded 10000 iterations",
    );
    assert_eq!(
        call_depth_over_limit().to_string(),
        "pine evaluation refused: call depth exceeds 64",
    );
}
