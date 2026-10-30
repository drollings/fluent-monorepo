#![cfg(feature = "bytecode")]
//! Red-team contract for the bytecode track: the adversarial corpus every
//! execution engine in this crate must satisfy. Each test pins one semantic
//! rule the tree-walk implements today — `na` discipline, history edges,
//! persistence identity, call-site identity, input sites, frame teardown,
//! loop/depth bounds, destructuring, shadowing, version skew, tick mode,
//! warmup, `max_bars`, and refusal strings — so the M4 VM has a fixed
//! target and M8 can close every risk with proof instead of review.
//!
//! All cases run the real interpreter on hermetic inputs; nothing here
//! mocks. Fixture bytes come from the canonical server fixture tree;
//! histories are synthetic ramps or the checked-in `tests/data` trims.
//!
//! Test path note: this file rides the `pine` unit module, so the
//! runnable filter is `pine`, not the file stem.

use super::bytecode_diff_tests::{link_script, ramp_bars};
use crate::{InputSet, KernelError, LinkedModule, PineEngine, PineRuntimeConfig, RunOutcome};
use protopyne_types::Bar;

fn run(modules: &[LinkedModule], bars: &[Bar]) -> RunOutcome {
    run_with_config(modules, bars, engine_config())
}

fn run_with_config(
    modules: &[LinkedModule],
    bars: &[Bar],
    config: PineRuntimeConfig,
) -> RunOutcome {
    PineEngine::new(config)
        .run_linked(modules, bars, InputSet::new())
        .expect("adversarial case evaluates")
}

fn run_err(modules: &[LinkedModule], bars: &[Bar]) -> String {
    PineEngine::new(PineRuntimeConfig::new().build())
        .run_linked(modules, bars, InputSet::new())
        .expect_err("adversarial case refuses")
        .to_string()
}

fn plot_values(outcome: &RunOutcome, index: usize) -> Vec<Option<f64>> {
    outcome.plots[index]
        .values
        .iter()
        .map(|cell| match cell {
            None => None,
            Some(crate::PineValue::Float(v)) => Some(*v),
            Some(crate::PineValue::Int(v)) => Some(*v as f64),
            Some(other) => panic!("plot holds a number, got {other:?}"),
        })
        .collect()
}

fn engine_config() -> PineRuntimeConfig {
    PineRuntimeConfig::new()
        .chart_symbol("SPY".to_owned())
        .chart_period("1".to_owned())
        .warmup_bars(0)
        .build()
}

#[test]
fn na_arithmetic_propagates_as_absence() {
    // `na` in arithmetic is absence out, never zero: every bar reads `na`.
    let bars = ramp_bars(10);
    let outcome = run(&link_script("v = na\nplot(v + 1.0, title=\"V\")"), &bars);
    assert!(
        plot_values(&outcome, 0).iter().all(Option::is_none),
        "na + 1.0 stays na on every bar"
    );
}

#[test]
fn offset_past_start_reads_na() {
    // `close[5]` on bars 0..4 reads before the window: `na`, not a wrap
    // and not a panic. Bars 5.. carry the lagged close.
    let bars = ramp_bars(8);
    let outcome = run(&link_script("plot(close[5], title=\"W\")"), &bars);
    let values = plot_values(&outcome, 0);
    assert_eq!(values.len(), 8);
    assert!(
        values[..5].iter().all(Option::is_none),
        "pre-window offsets read na: {values:?}"
    );
    let closes: Vec<f64> = (0..8).map(|i| 100.0 + f64::from(i) * 0.5).collect();
    for (bar, cell) in values.iter().enumerate().skip(5) {
        assert_eq!(
            *cell,
            Some(closes[bar - 5]),
            "bar {bar} reads the lagged close"
        );
    }
}

#[test]
fn varip_resets_each_bar_while_var_persists() {
    // Persistence identity across the bar advance (`VarStore::next_bar`
    // drops intrabar cells, keeps `var` cells): `varip` seeds once, then
    // reads `na` on later bars; `var` accumulates the running close sum.
    let bars = ramp_bars(10);
    let outcome = run(
        &link_script(
            "varip x = 0.0\nx := x + close\nvar y = 0.0\ny := y + close\nplot(x, title=\"X\")\nplot(y, title=\"Y\")",
        ),
        &bars,
    );
    let (x, y) = (plot_values(&outcome, 0), plot_values(&outcome, 1));
    assert_eq!(x[0], Some(100.0), "varip seeds on the first bar");
    assert!(
        x[1..].iter().all(Option::is_none),
        "intrabar cells drop at the bar advance: {x:?}"
    );
    let mut running = 0.0;
    for (bar, cell) in y.iter().enumerate() {
        running += 100.0 + bar as f64 * 0.5;
        assert!(
            cell.is_some_and(|v| (v - running).abs() < 1e-9),
            "bar {bar} accumulates under var: {cell:?} vs {running}"
        );
    }
}

#[test]
fn call_site_identity_splits_ta_state() {
    // The same function text called from two sites keeps two bridge
    // states: a shared-state bug would collapse both legs onto one
    // length. Lengths 5 and 10 differ on the ramp once warm.
    let bars = ramp_bars(30);
    let outcome = run(
        &link_script(
            "sma_wrap(x, len) => ta.sma(x, len)\na = sma_wrap(close, 5)\nb = sma_wrap(close, 10)\nplot(a, title=\"A\")\nplot(b, title=\"B\")",
        ),
        &bars,
    );
    let (a, b) = (plot_values(&outcome, 0), plot_values(&outcome, 1));
    assert!(
        a[29].is_some() && b[29].is_some(),
        "both legs warm by the last bar"
    );
    assert_ne!(a[29], b[29], "sites keep separate states: {a:?} vs {b:?}");
}

#[test]
fn input_sites_keep_separate_defaults() {
    // First-call-wins is per textual site: two `input.int` calls with
    // different defaults answer their own default every bar.
    let bars = ramp_bars(5);
    let outcome = run(
        &link_script(
            "a = input.int(14, \"A\")\nb = input.int(99, \"B\")\nplot(a + b, title=\"S\")",
        ),
        &bars,
    );
    assert!(
        plot_values(&outcome, 0)
            .iter()
            .all(|cell| *cell == Some(113.0)),
        "sites keep 14 and 99 apart"
    );
}

#[test]
fn nested_calls_tear_down_frames_cleanly() {
    // Receiver shadowing plus nested calls: each frame restores the
    // caller's bindings, so the closed form holds on every bar.
    let bars = ramp_bars(10);
    let outcome = run(
        &link_script(
            "inner(v) => v * 2.0\nouter(v) => inner(v) + inner(v + 1.0)\nplot(outer(close), title=\"O\")",
        ),
        &bars,
    );
    for (bar, cell) in plot_values(&outcome, 0).iter().enumerate() {
        let close = 100.0 + bar as f64 * 0.5;
        assert!(
            cell.is_some_and(|v| (v - (4.0 * close + 2.0)).abs() < 1e-9),
            "bar {bar} tears down cleanly: {cell:?}"
        );
    }
}

#[test]
fn break_exits_only_the_inner_loop() {
    // `break` at `i == 3` stops the accumulation at three increments on
    // every bar — the loop bound never leaks across bars.
    let bars = ramp_bars(5);
    let outcome = run(
        &link_script(
            "s = 0.0\nfor i = 0 to 10\n    if i == 3\n        break\n    s := s + 1.0\nplot(s, title=\"S\")",
        ),
        &bars,
    );
    assert!(
        plot_values(&outcome, 0)
            .iter()
            .all(|cell| *cell == Some(3.0)),
        "break stops at three every bar"
    );
}

#[test]
fn for_range_bound_refuses_by_name() {
    // The `MAX_LOOP_ITERATIONS` bound is fail-closed with its count: a
    // range past it refuses instead of running long.
    let bars = ramp_bars(3);
    let reason = run_err(&link_script("for i = 0 to 20000\n    x = 1"), &bars);
    assert!(
        reason.contains("20000") || reason.contains("exceeds"),
        "the bound names itself: {reason}"
    );
}

#[test]
fn recursion_depth_refuses_by_name() {
    // The language forbids recursion: the depth bound turns a cycle into
    // a reason naming the bound.
    let bars = ramp_bars(3);
    let reason = run_err(&link_script("f(x) => f(x)\nplot(f(1.0))"), &bars);
    assert!(
        reason.contains("64"),
        "the depth bound names itself: {reason}"
    );
}

#[test]
fn destructured_tuple_binds_positionally() {
    // Multi-output unpacking binds in order: the closed sum holds.
    let bars = ramp_bars(5);
    let outcome = run(
        &link_script("f() => [1.0, 2.0]\n[a, b] = f()\nplot(a + b, title=\"S\")"),
        &bars,
    );
    assert!(
        plot_values(&outcome, 0)
            .iter()
            .all(|cell| *cell == Some(3.0)),
        "tuple unpacks positionally"
    );
}

#[test]
fn params_shadow_outer_bindings() {
    // A parameter shadows the same-named outer variable for the call.
    let bars = ramp_bars(5);
    let outcome = run(
        &link_script("x = 10.0\nf(x) => x + 1.0\nplot(f(1.0), title=\"F\")"),
        &bars,
    );
    assert!(
        plot_values(&outcome, 0)
            .iter()
            .all(|cell| *cell == Some(2.0)),
        "the parameter shadows the outer binding"
    );
}

#[test]
fn version_skewed_fixture_links_and_runs() {
    // Warn-only skew: a fixture library under a skewed declared version
    // still links (with warnings) and evaluates identically.
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../protopyne-server/tests/data/pine_closure");
    let read = |rel: &str| std::fs::read_to_string(dir.join(rel)).expect("fixture reads");
    let skewed = read("libraries/_libUtil.pine").replacen("//@version=6", "//@version=5", 1);
    assert_ne!(skewed, read("libraries/_libUtil.pine"));
    let source = crate::parse_pine_source("showmethegrail/libUtil", &skewed);
    assert_eq!(source.warnings.len(), 1, "skew warns exactly once");
    let program = crate::parse_program(&skewed).expect("skewed copy parses");
    let mas_source = read("libraries/_libMAs.pine");
    let mas_program = crate::parse_program(&mas_source).expect("libMAs parses");
    let fast_text = read("indicators/_FastMAs.pine");
    let fast_program = crate::parse_program(&fast_text).expect("FastMAs parses");
    let inputs = vec![
        crate::ModuleInput::new(
            crate::LibraryId {
                publisher: "showmethegrail".to_owned(),
                name: "libUtil".to_owned(),
                version: 5,
            },
            source,
            program,
        ),
        crate::ModuleInput::new(
            crate::LibraryId {
                publisher: "showmethegrail".to_owned(),
                name: "libMAs".to_owned(),
                version: 95,
            },
            crate::parse_pine_source("showmethegrail/libMAs", &mas_source),
            mas_program,
        ),
        crate::ModuleInput::new(
            crate::LibraryId {
                publisher: "local".to_owned(),
                name: "_FastMAs".to_owned(),
                version: 6,
            },
            crate::parse_pine_source("indicators/_FastMAs.pine", &fast_text),
            fast_program,
        ),
    ];
    let graph = crate::resolve_imports(inputs).expect("skewed closure links");
    assert!(!graph.warnings().is_empty(), "skew warns on the graph");
    let modules: Vec<crate::LinkedModule> = graph
        .order()
        .iter()
        .map(|id| {
            let entry = graph.get(id).expect("ordered module resolves");
            crate::LinkedModule::new(
                &id.to_string(),
                entry.program.clone(),
                graph
                    .alias_table(id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(alias, target)| (alias, target.to_string()))
                    .collect(),
            )
        })
        .collect();
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/SPY_1M.csv");
    let content = std::fs::read_to_string(&path).expect("checked-in history loads");
    let bars: Vec<Bar> = content
        .lines()
        .skip(1)
        .map(|line| {
            let mut cells = line.split(',');
            let secs: i64 = cells.next().expect("time").parse().expect("int time");
            let num =
                |cell: Option<&str>| cell.expect("leg").parse::<f64>().expect("parseable leg");
            Bar::new()
                .ts(chrono::DateTime::from_timestamp(secs, 0).expect("bar time in range"))
                .o(rust_decimal::Decimal::from_f64_retain(num(cells.next())).expect("finite"))
                .h(rust_decimal::Decimal::from_f64_retain(num(cells.next())).expect("finite"))
                .l(rust_decimal::Decimal::from_f64_retain(num(cells.next())).expect("finite"))
                .c(rust_decimal::Decimal::from_f64_retain(num(cells.next())).expect("finite"))
                .volume(
                    cells
                        .next()
                        .expect("volume")
                        .parse::<u64>()
                        .expect("int volume"),
                )
                .maybe_adj(None)
                .build()
        })
        .collect();
    let outcome = run(&modules, &bars);
    assert_eq!(outcome.evaluated, bars.len(), "skewed closure evaluates");
}

#[test]
fn tick_run_needs_one_stamp_per_bar() {
    // Tick mode is length-checked: a stamp count mismatch refuses naming
    // both counts, and a aligned run evaluates every bar.
    let bars = ramp_bars(5);
    let modules = link_script("plot(close, title=\"C\")");
    let stamps: Vec<i64> = bars.iter().map(|bar| bar.ts.timestamp_millis()).collect();
    let err = PineEngine::new(engine_config())
        .run_linked_ticks(&modules, &bars, &stamps[..3], InputSet::new())
        .expect_err("stamp mismatch refuses")
        .to_string();
    assert!(
        err.contains('3') && err.contains('5'),
        "the refusal names both counts: {err}"
    );
    let outcome = PineEngine::new(engine_config())
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("aligned tick run evaluates");
    assert_eq!(outcome.evaluated, 5, "every bar evaluates in tick mode");
}

#[test]
fn warmup_bars_advance_unseen() {
    // Warmup is enforced, not advisory: the first five bars never reach
    // evaluation, so `evaluated` excludes them and `bar_index` opens at 5.
    let bars = ramp_bars(20);
    let config = PineRuntimeConfig::new()
        .chart_symbol("SPY".to_owned())
        .chart_period("1".to_owned())
        .warmup_bars(5)
        .build();
    let outcome = run_with_config(&link_script("plot(bar_index, title=\"B\")"), &bars, config);
    assert_eq!(outcome.evaluated, 15, "warmup bars never evaluate");
    let values = plot_values(&outcome, 0);
    assert_eq!(values[0], Some(5.0), "`bar_index` opens past warmup");
}

#[test]
fn max_bars_refuses_instead_of_truncating() {
    // Overflow is fail-closed: more bars than `max_bars` refuses with the
    // `TooManyBars` variant, never a silent prefix.
    let bars = ramp_bars(20);
    let config = PineRuntimeConfig::new()
        .chart_symbol("SPY".to_owned())
        .chart_period("1".to_owned())
        .max_bars(10)
        .build();
    let err = PineEngine::new(config)
        .run_linked(&link_script("plot(close)"), &bars, InputSet::new())
        .expect_err("overflow refuses");
    assert!(
        matches!(err, KernelError::TooManyBars { got: 20, max: 10 }),
        "the variant carries both counts: {err:?}"
    );
}

#[test]
fn unknown_function_refuses_by_name() {
    let bars = ramp_bars(3);
    let reason = run_err(&link_script("plot(nope(close))"), &bars);
    assert!(
        reason.contains("unknown function 'nope'"),
        "the refusal names the function: {reason}"
    );
}

#[test]
fn unknown_ta_method_refuses_by_name() {
    let bars = ramp_bars(20);
    let reason = run_err(&link_script("plot(ta.nope(close, 14))"), &bars);
    assert!(
        reason.contains("ta.nope"),
        "the refusal names the method: {reason}"
    );
}

/// M4 adversarial coverage: every case whose closure links clean runs
/// byte-identical on the VM, including the recursion refusal (depth
/// bound) and the clock-shaped configs (warmup, ticks, over-capacity).
/// Fallback cases stay tree-walk-only by design (M8 owns them).
#[test]
fn vm_matches_tree_walk_on_clean_adversarial_subset() {
    use super::bytecode_diff_tests::{assert_byte_identical, compile_case, link_case, run_vm};
    // Straight-line numeric cases plus the recursion refusal: all link
    // clean, so the VM must agree with the tree-walk byte for byte —
    // values and refusal strings alike.
    let scripts = [
        ("na_arithmetic", "v = na\nplot(v + 1.0, title=\"V\")"),
        (
            "offset_edges",
            "plot(close[3] - close[0], title=\"W\")",
        ),
        (
            "varip_var",
            "varip x = 0.0\nx := x + close\nvar y = 0.0\ny := y + close\nplot(x + y, title=\"S\")",
        ),
        (
            "call_site_identity",
            "sma_wrap(x, len) => ta.sma(x, len)\na = sma_wrap(close, 5)\nb = sma_wrap(close, 10)\nplot(a - b, title=\"D\")",
        ),
        (
            "input_sites",
            "a = input.int(14, \"A\")\nb = input.int(99, \"B\")\nplot(a + b, title=\"S\")",
        ),
        (
            "nested_calls",
            "inner(v) => v * 2.0\nouter(v) => inner(v) + inner(v + 1.0)\nplot(outer(close), title=\"O\")",
        ),
        (
            "shadowing",
            "x = 10.0\nf(x) => x + 1.0\nplot(f(1.0), title=\"F\")",
        ),
        ("recursion_refuses", "f(x) => f(x)\nplot(f(1.0), title=\"F\")"),
    ];
    for (name, script) in scripts {
        let modules = link_script(script);
        let bars = ramp_bars(30);
        assert_byte_identical(&modules, &bars, name);
    }
    // Warmup skips early bars identically on both engines.
    {
        let modules = link_script("plot(bar_index, title=\"B\")");
        let bars = ramp_bars(20);
        let config = crate::pine::bytecode_exec::VmConfig {
            symbol: "SPY".to_owned(),
            period: "1".to_owned(),
            warmup: 5,
            max_bars: 10_000,
        };
        let tree = PineEngine::new(
            PineRuntimeConfig::new()
                .chart_symbol("SPY".to_owned())
                .chart_period("1".to_owned())
                .warmup_bars(5)
                .build(),
        )
        .run_linked(&modules, &bars, InputSet::new());
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).expect("warmup case links");
        let got = run_vm(&linked, &bars, &config);
        assert_eq!(
            super::bytecode_diff_tests::fingerprint_result(&got),
            super::bytecode_diff_tests::fingerprint_result(&tree),
            "warmup skips identically"
        );
    }
    // Over-capacity refuses identically without running.
    {
        let modules = link_script("plot(close, title=\"C\")");
        let bars = ramp_bars(20);
        let config = crate::pine::bytecode_exec::VmConfig {
            symbol: "SPY".to_owned(),
            period: "1".to_owned(),
            warmup: 0,
            max_bars: 10,
        };
        let tree = PineEngine::new(
            PineRuntimeConfig::new()
                .chart_symbol("SPY".to_owned())
                .chart_period("1".to_owned())
                .max_bars(10)
                .build(),
        )
        .run_linked(&modules, &bars, InputSet::new());
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).expect("trivial case links");
        let got = run_vm(&linked, &bars, &config);
        assert_eq!(
            super::bytecode_diff_tests::fingerprint_result(&got),
            super::bytecode_diff_tests::fingerprint_result(&tree),
            "over-capacity refuses identically"
        );
    }
    // Tick mode: length-checked alike, millis legs alike.
    {
        let modules = link_script("plot(close, title=\"C\")");
        let bars = ramp_bars(5);
        let stamps: Vec<i64> = bars.iter().map(|bar| bar.ts.timestamp_millis()).collect();
        let tree = PineEngine::new(
            PineRuntimeConfig::new()
                .chart_symbol("SPY".to_owned())
                .chart_period("1".to_owned())
                .warmup_bars(0)
                .build(),
        )
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("tick run evaluates");
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).expect("tick case links");
        let config = crate::pine::bytecode_exec::VmConfig::headless();
        let got = crate::pine::bytecode_exec::run_linked(
            &linked,
            &bars,
            InputSet::new(),
            &config,
            Some(&stamps),
        )
        .expect("VM tick run evaluates");
        assert_eq!(
            super::bytecode_diff_tests::canonical_form(&got),
            super::bytecode_diff_tests::canonical_form(&tree),
            "tick runs agree"
        );
    }
}
