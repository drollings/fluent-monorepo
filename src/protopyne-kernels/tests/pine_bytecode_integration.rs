//! Engine selection behavior: the compiled loop rides behind an explicit
//! opt-in mode, defaults to the tree-walk, and falls back to it whenever
//! the program does not fully compile. Every test below runs the same
//! linked modules through both selections and pins byte-identical
//! outcomes — refusal strings included.
//!
//! Test path note: this file rides the `pine` unit module, so the
//! runnable filter is `pine`, not the file stem.

#![cfg(feature = "bytecode")]

use super::bytecode_diff_tests::{
    canonical_form, compile_case, diff_corpus, fingerprint_result, link_case, link_script,
    ramp_bars,
};
use crate::pine::test_fixture as fixture;
use crate::pine::BytecodeMode;
use crate::{InputSet, PineEngine, PineRuntimeConfig, RunOutcome};
use protopyne_types::Bar;
use std::path::PathBuf;

fn engine_config() -> PineRuntimeConfig {
    PineRuntimeConfig::new()
        .chart_symbol("SPY".to_owned())
        .chart_period("1".to_owned())
        .warmup_bars(0)
        .build()
}

fn run_with_result(
    mode: Option<BytecodeMode>,
    modules: &[crate::LinkedModule],
    bars: &[Bar],
) -> Result<RunOutcome, crate::KernelError> {
    let mut engine = PineEngine::new(engine_config());
    engine.set_bytecode_mode(mode.unwrap_or(BytecodeMode::TreeWalk));
    engine.run_linked(modules, bars, InputSet::new())
}

fn run_with(
    mode: Option<BytecodeMode>,
    modules: &[crate::LinkedModule],
    bars: &[Bar],
) -> RunOutcome {
    run_with_result(mode, modules, bars).expect("integration case evaluates")
}

fn full_spy_bars() -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/SPY_1M.csv");
    let content = std::fs::read_to_string(&path).expect("checked-in history loads");
    let mut lines = content.lines();
    assert_eq!(
        lines.next().expect("header present"),
        "time,open,high,low,close,Volume",
        "history header pinned"
    );
    lines
        .map(|line| {
            let mut cells = line.split(',');
            let secs: i64 = cells.next().expect("time").parse().expect("int time");
            let num =
                |cell: Option<&str>| cell.expect("leg").parse::<f64>().expect("parseable leg");
            Bar::new()
                .ts(chrono::DateTime::from_timestamp(secs, 0).expect("bar time in range"))
                .o(rust_decimal::Decimal::from_f64_retain(num(cells.next())).expect("finite open"))
                .h(rust_decimal::Decimal::from_f64_retain(num(cells.next())).expect("finite high"))
                .l(rust_decimal::Decimal::from_f64_retain(num(cells.next())).expect("finite low"))
                .c(
                    rust_decimal::Decimal::from_f64_retain(num(cells.next()))
                        .expect("finite close"),
                )
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
        .collect()
}

#[test]
fn compiled_mode_matches_tree_walk_on_numeric_corpus() {
    // Fully compiled scripts run the loop under the opt-in selection and
    // agree with the tree-walk bar for bar.
    let scripts = [
        "plot(close + open * 2.0 + 1.0, title=\"A\")",
        "v = close[3] - close[1]\nw = v[2]\nplot(w, title=\"W\")",
        "var x = 0.0\nx := x + close\ny = x[1]\nplot(y, title=\"Y\")",
        "f(v) => v * 2.0 + 1.0\nplot(f(close), title=\"F\")",
        "plot(ta.sma(close, 14), title=\"S\")",
        "v = 0.0\nif close > open\n    v := 1.0\nelse\n    v := -1.0\nplot(v, title=\"V\")",
        "a = input.int(14, \"A\")\nplot(a + close, title=\"S\")",
    ];
    let bars = ramp_bars(60);
    for script in scripts {
        let modules = link_script(script);
        let tree = run_with(None, &modules, &bars);
        let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            canonical_form(&vm),
            canonical_form(&tree),
            "{script}: selections agree"
        );
    }
}

#[test]
fn partial_programs_fall_back_to_tree_walk_in_compiled_mode() {
    // Unlowered constructs never enter the loop: the opt-in selection
    // runs them tree-walk, identical to the default.
    let scripts = [
        "a = [1.0, 2.0]\narray.shift(a)\nplot(close, title=\"C\")",
        "plot(close)",
        "x = request.security(syminfo.tickerid, \"D\", [foo(close)])\nplot(x, title=\"X\")",
    ];
    let bars = ramp_bars(20);
    for script in scripts {
        let modules = link_script(script);
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "{script}: fallback agrees"
        );
    }
}

#[test]
fn fresh_engines_default_to_tree_walk() {
    // A fresh engine behaves exactly like an explicit tree-walk selection:
    // opting in is the only way to reach the loop.
    let scripts = [
        "v = close[3] - close[1]\nplot(v, title=\"W\")",
        "s = 0.0\nfor i = 0 to 4\n    s := s + close\nplot(s, title=\"S\")",
    ];
    let bars = ramp_bars(30);
    for script in scripts {
        let modules = link_script(script);
        let fresh = run_with(None, &modules, &bars);
        let explicit = run_with(Some(BytecodeMode::TreeWalk), &modules, &bars);
        assert_eq!(
            canonical_form(&fresh),
            canonical_form(&explicit),
            "{script}: default is tree-walk"
        );
    }
}

#[test]
#[ignore = "soak: full-history byte-identity in compiled mode; the regular tier pins the same contract on 60-bar ramps"]
fn heavy_histories_agree_in_compiled_mode() {
    // Long histories stay identical under the opt-in selection: the full
    // checked-in minute trim plus a long synthetic ramp.
    let scripts = [
        "var x = 0.0\nx := x + close\ny = x[1]\nplot(y, title=\"Y\")",
        "plot(ta.sma(close, 14), title=\"S\")",
    ];
    let histories = [full_spy_bars(), ramp_bars(1500)];
    for script in scripts {
        let modules = link_script(script);
        for (index, bars) in histories.iter().enumerate() {
            let tree = run_with(None, &modules, bars);
            let vm = run_with(Some(BytecodeMode::Compiled), &modules, bars);
            assert_eq!(
                canonical_form(&vm),
                canonical_form(&tree),
                "{script} history {index}: long runs agree"
            );
        }
    }
}

#[test]
fn compiled_selection_matches_tree_walk_across_the_differential_corpus() {
    // The whole differential corpus through the engine seam (not the
    // direct loop entry): fully linked cases ride the loop, the rest
    // fall back, and both selections agree everywhere — success or
    // refusal string.
    let bars = ramp_bars(30);
    for case in diff_corpus() {
        let tree = run_with_result(None, &case.modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &case.modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "{}: selections agree",
            case.name
        );
    }
}

#[test]
fn compiled_selection_matches_tree_walk_on_tick_runs() {
    // Tick emulation runs through one discipline: the opt-in selection
    // serves the same millisecond `time` leg and the same outcomes.
    let scripts = [
        "plot(close + open * 2.0 + 1.0, title=\"A\")",
        "t = time\nplot(t, title=\"T\")",
    ];
    let bars = ramp_bars(10);
    let stamps: Vec<i64> = bars.iter().map(|bar| bar.ts.timestamp_millis()).collect();
    for script in scripts {
        let modules = link_script(script);
        let tree = run_ticks_with_result(None, &modules, &bars, &stamps);
        let vm = run_ticks_with_result(Some(BytecodeMode::Compiled), &modules, &bars, &stamps);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "{script}: tick runs agree"
        );
    }
}

#[test]
fn compiled_selection_matches_tree_walk_past_warmup() {
    // Warmup bars never evaluate under either selection: the run
    // reports the same evaluated count and the same surviving series.
    let config = PineRuntimeConfig::new()
        .chart_symbol("SPY".to_owned())
        .chart_period("1".to_owned())
        .warmup_bars(5)
        .build();
    let bars = ramp_bars(20);
    // Both scripts link, so the loop serves warmup under test — not the
    // fallback. History offsets stay relative to evaluated bars on both
    // sides (neither engine appends history for skipped bars).
    let scripts = [
        "var x = 0.0\nx := x + close\ny = x[1]\nplot(y, title=\"Y\")",
        "v = close[3] - close[1]\nplot(v, title=\"W\")",
    ];
    for script in scripts {
        let modules = link_script(script);
        let tree = run_with_config_result(None, &modules, &bars, config.clone());
        let vm = run_with_config_result(
            Some(BytecodeMode::Compiled),
            &modules,
            &bars,
            config.clone(),
        );
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "{script}: warmup runs agree"
        );
        assert_eq!(
            tree.expect("tree-walk evaluates").evaluated,
            15,
            "{script}: warmup bars never evaluate"
        );
    }
}

#[test]
fn compiled_selection_refuses_overfull_histories_identically() {
    // Over-capacity refuses fail-closed under either selection with the
    // same variant — the opt-in never truncates silently.
    let config = PineRuntimeConfig::new()
        .chart_symbol("SPY".to_owned())
        .chart_period("1".to_owned())
        .max_bars(10)
        .build();
    let bars = ramp_bars(20);
    let modules = link_script("v = close[3] - close[1]\nplot(v, title=\"W\")");
    let tree = run_with_config_result(None, &modules, &bars, config.clone())
        .expect_err("tree-walk refuses over-capacity");
    let vm = run_with_config_result(Some(BytecodeMode::Compiled), &modules, &bars, config)
        .expect_err("compiled refuses over-capacity");
    assert_eq!(
        tree.to_string(),
        vm.to_string(),
        "refusals read identically"
    );
    assert!(
        matches!(tree, crate::KernelError::TooManyBars { got: 20, max: 10 }),
        "the variant carries both counts: {tree:?}"
    );
}

fn run_ticks_with_result(
    mode: Option<BytecodeMode>,
    modules: &[crate::LinkedModule],
    bars: &[Bar],
    stamps: &[i64],
) -> Result<RunOutcome, crate::KernelError> {
    let mut engine = PineEngine::new(engine_config());
    if let Some(mode) = mode {
        engine.set_bytecode_mode(mode);
    }
    engine.run_linked_ticks(modules, bars, stamps, InputSet::new())
}

fn run_with_config_result(
    mode: Option<BytecodeMode>,
    modules: &[crate::LinkedModule],
    bars: &[Bar],
    config: PineRuntimeConfig,
) -> Result<RunOutcome, crate::KernelError> {
    let mut engine = PineEngine::new(config);
    engine.set_bytecode_mode(mode.unwrap_or(BytecodeMode::TreeWalk));
    engine.run_linked(modules, bars, InputSet::new())
}

/// One linked library plus driver with no tree-keeping marker: both
/// modules link clean, so the compiled selection runs the whole program
/// on the loop.
fn link_single(entry: &str, lib_body: &str) -> Vec<crate::LinkedModule> {
    let lib = crate::parse_program(lib_body).expect("lib parses");
    let driver = crate::parse_program(entry).expect("entry parses");
    vec![
        crate::LinkedModule::new("test/lib", lib, Default::default()),
        crate::LinkedModule::new(
            "local/Driver",
            driver,
            [("L".to_owned(), "test/lib".to_owned())]
                .into_iter()
                .collect(),
        ),
    ]
}

#[test]
fn cross_boundary_calls_match_tree_walk() {
    // Entry on the loop, library on the tree-walk: values cross per
    // call through the bridge with the tree-walk's own binding rules.
    let modules = link_single(
        "v = L.bump(close)\nplot(v, title=\"V\")",
        "export bump(x) =>\n    b = [1.0, 2.0]\n    array.shift(b)\n    x + array.get(b, 0)",
    );
    let bars = ramp_bars(30);
    let tree = run_with_result(None, &modules, &bars);
    let mixed = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&mixed),
        fingerprint_result(&tree),
        "mixed cross-boundary calls agree"
    );
    mixed.expect("mixed run evaluates");
}

#[test]
fn cross_boundary_write_backs_match_tree_walk() {
    // A collection parameter mutated inside the tree-walk callee writes
    // back to the loop caller's home, per parameter, exactly like a
    // loop-internal call would.
    let modules = link_single(
        "a = [1.0]\nr = L.push2(a)\nplot(array.get(a, 1) + array.get(r, 0), title=\"V\")",
        "export push2(arr) =>\n    tmp = [9.0]\n    array.shift(tmp)\n    array.push(arr, 2.0)",
    );
    let bars = ramp_bars(10);
    let tree = run_with_result(None, &modules, &bars);
    let mixed = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&mixed),
        fingerprint_result(&tree),
        "mixed write-backs agree"
    );
    let outcome = mixed.expect("mixed run evaluates");
    let plot = outcome
        .plots
        .iter()
        .find(|plot| plot.title == "V")
        .expect("plot recorded");
    assert!(
        plot.values.iter().all(|value| matches!(
            value,
            Some(crate::PineValue::Float(v)) if (*v - 3.0).abs() < 1e-9
        )),
        "pushed element visible through both homes: {plot:?}"
    );
}

#[test]
fn fastmas_fixture_matches_tree_walk() {
    // The fixture closure that motivated the split: the entry links
    // fully while the libraries stay tree-walk, so the compiled
    // selection runs mixed and must still read byte-identical.
    let modules = fixture::linked_fixture("_FastMAs");
    let bars = ramp_bars(60);
    let tree = run_with_result(None, &modules, &bars);
    let mixed = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&mixed),
        fingerprint_result(&tree),
        "_FastMAs mixed run agrees"
    );
    let outcome = mixed.expect("mixed run evaluates");
    assert_eq!(outcome.evaluated, 60, "every bar evaluates");
    assert!(!outcome.plots.is_empty(), "entry plots publish");
}

#[test]
fn nested_ta_calls_share_stacked_sites_across_engines() {
    // A `ta.*` call inside a user function stacks the caller sites
    // (`outer>inner>ta.sma@...`) on the loop exactly like the tree-walk
    // joins them per bar: the keyed kernels must observe the same site
    // in either engine, so recursive kernels stay in lockstep.
    let modules = link_script(
        "f(x) => ta.sma(x, 3)\ng(y) => f(y) + f(y * 2.0)\nv = g(close)\nplot(v, title=\"V\")",
    );
    let program = compile_case(&modules);
    let linked = link_case(&program, &modules).expect("nested calls link");
    assert!(
        (linked.coverage() - 1.0).abs() < f64::EPSILON,
        "the loop actually runs; fallback would vacate the comparison"
    );
    let bars = ramp_bars(30);
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "stacked sites agree across engines"
    );
}

#[test]
fn ta_site_identity_is_engine_independent() {
    // One logical call path keys one `ta` history: the two wrappers call
    // with different arguments, so a shared history would interleave the
    // streams and diverge from the tree — agreement plus distinct series
    // proves one key per path.
    let modules = link_script(
        "h(x) => ta.sma(x, 3)\no1(y) =>\n    z = g + y\n    h(z)\no2(y) =>\n    z = g + y * 2.0\n    h(z)\ng = 0.0\nif close < 0.0\n    probe = request.marker(close)\na = o1(close)\nb = o2(close)\nplot(a, title=\"A\")\nplot(b, title=\"B\")",
    );
    let bars = ramp_bars(30);
    let tree = run_with_result(None, &modules, &bars).expect("tree runs");
    let mut engine = PineEngine::new(engine_config());
    engine.set_bytecode_mode(BytecodeMode::Compiled);
    let compiled = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect("compiled runs");
    assert_eq!(
        fingerprint_result(&Ok(compiled.clone())),
        fingerprint_result(&Ok(tree.clone())),
        "one call path keys one history across engines"
    );
    let series = |outcome: &RunOutcome, title: &str| {
        outcome
            .plots
            .iter()
            .find(|plot| plot.title == title)
            .expect("plot captured")
            .values
            .clone()
    };
    assert_ne!(
        series(&compiled, "A"),
        series(&compiled, "B"),
        "the two call sites keep distinct histories"
    );
}

#[test]
fn a_call_site_never_changes_engines_mid_run() {
    // One run, one engine per call path: both helper calls evaluate on
    // every bar and the whole run agrees with the tree.
    let modules = link_script(
        "h(x) => ta.sma(x, 3)\no1(y) =>\n    z = g + y\n    h(z)\no2(y) =>\n    z = g + y * 2.0\n    h(z)\ng = 0.0\nif close < 0.0\n    probe = request.marker(close)\na = o1(close)\nb = o2(close)\nplot(a, title=\"A\")\nplot(b, title=\"B\")",
    );
    let bars = ramp_bars(30);
    let tree = run_with_result(None, &modules, &bars).expect("tree runs");
    let mut engine = PineEngine::new(engine_config());
    engine.set_bytecode_mode(BytecodeMode::Compiled);
    let compiled = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect("compiled runs");
    assert_eq!(
        fingerprint_result(&Ok(compiled)),
        fingerprint_result(&Ok(tree)),
        "the single-engine-per-path run agrees with the tree"
    );
}

#[test]
fn capture_sites_agree_across_engines() {
    // Every capture family names its site through one shared spelling:
    // the tree recomputes it per bar while the loop reads the link-time
    // constant, and both must read byte-identical. Debug renderings carry
    // the private site keys, so equality here covers the spelling, not
    // just the values. The coverage assertion keeps the comparison
    // honest — a fallback would run the tree twice and agree vacuously.
    let modules = link_script(
        "pa = plot(close, \"A\")\nplotshape(close > open, title=\"S\")\nplotcandle(open, high, low, close, title=\"C\", color=color.red)\nbarcolor(close > open ? color.red : na)\nhline(50.0, \"L\")\npb = plot(open, \"B\")\nfill(pa, pb)",
    );
    let program = compile_case(&modules);
    let linked = link_case(&program, &modules).expect("capture shapes link");
    assert!(
        (linked.coverage() - 1.0).abs() < f64::EPSILON,
        "the loop actually runs; fallback would vacate the comparison"
    );
    let bars = ramp_bars(30);
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    for (family, left, right) in [
        (
            "plots",
            format!("{:?}", tree.plots),
            format!("{:?}", vm.plots),
        ),
        (
            "plotshapes",
            format!("{:?}", tree.plotshapes),
            format!("{:?}", vm.plotshapes),
        ),
        (
            "plotcandles",
            format!("{:?}", tree.plotcandles),
            format!("{:?}", vm.plotcandles),
        ),
        (
            "barcolors",
            format!("{:?}", tree.barcolors),
            format!("{:?}", vm.barcolors),
        ),
        (
            "hlines",
            format!("{:?}", tree.hlines),
            format!("{:?}", vm.hlines),
        ),
        (
            "fills",
            format!("{:?}", tree.fills),
            format!("{:?}", vm.fills),
        ),
    ] {
        assert_eq!(left, right, "{family} sites agree across engines");
    }
}

#[test]
fn cross_module_plot_sites_stay_separate() {
    // Capture rows key by resolved site text, never by const id: const
    // pools are per-module, so a library plot and a driver plot can share
    // one numeric id with different meanings. Merging them would append
    // both series into one capture.
    let lib =
        crate::parse_program("plot(close, title=\"LIB\")\nexport calc(x) =>\n    x * 2.0 + 1.0")
            .expect("lib parses");
    let driver =
        crate::parse_program("v = L.calc(close)\nplot(v, title=\"V\")").expect("driver parses");
    let modules = vec![
        crate::LinkedModule::new("test/lib", lib, Default::default()),
        crate::LinkedModule::new(
            "local/Driver",
            driver,
            [("L".to_owned(), "test/lib".to_owned())]
                .into_iter()
                .collect(),
        ),
    ];
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("cross-module plots link");
    let bars = ramp_bars(6);
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "cross-module plots agree"
    );
    let outcome = vm.expect("plots evaluate");
    assert_eq!(outcome.plots.len(), 2, "one capture per site: {outcome:?}");
    for plot in &outcome.plots {
        assert_eq!(
            plot.values.len(),
            bars.len(),
            "one value per bar in {:?}",
            plot.title
        );
    }
}

#[test]
fn tick_runs_match_tree_walk() {
    // Tick emulation runs mixed through the same discipline: the
    // loop-owned entry bridges per-tick calls into tree-walk libraries.
    let modules = link_single(
        "v = L.bump(close)\nplot(v, title=\"V\")",
        "export bump(x) =>\n    b = [1.0, 2.0]\n    array.shift(b)\n    x + array.get(b, 0)",
    );
    let bars = ramp_bars(10);
    let stamps: Vec<i64> = bars.iter().map(|bar| bar.ts.timestamp_millis()).collect();
    let tree = run_ticks_with_result(None, &modules, &bars, &stamps);
    let mixed = run_ticks_with_result(Some(BytecodeMode::Compiled), &modules, &bars, &stamps);
    assert_eq!(
        fingerprint_result(&mixed),
        fingerprint_result(&tree),
        "mixed tick runs agree"
    );
}

#[test]
fn session_time_reads_chart_period() {
    // A loop-owned module that calls `time("1")` must read the run's
    // chart period exactly like the tree-walk: mixed construction hands
    // the loop the same configuration the whole-run path does.
    let modules = link_single(
        "v = L.bump(close)\nplot(time(\"1\") + v, title=\"V\")",
        "export bump(x) =>\n    b = [1.0, 2.0]\n    array.shift(b)\n    x + array.get(b, 0)",
    );
    let bars = ramp_bars(10);
    let tree = run_with_result(None, &modules, &bars);
    let mixed = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&mixed),
        fingerprint_result(&tree),
        "mixed session time agrees"
    );
    let outcome = mixed.expect("mixed session time evaluates");
    let got = plotted_floats(&outcome, "V");
    // Bar stamps are seconds; `bump` adds the array tail (2.0) to close.
    let want: Vec<Option<f64>> = (0..10)
        .map(|bar| Some(1_700_000_000.0 + f64::from(bar) * 60.0 + 102.0 + f64::from(bar) * 0.5))
        .collect();
    assert_eq!(
        got, want,
        "the configured chart period serves time(): {outcome:?}"
    );
}

#[test]
fn wall_clock_parts_read_chart_period() {
    // `hour(time("1"))`/`minute(time("1"))` compose the session-time read
    // with the shared wall-part step, so the mixed partition must expose
    // the same chart period the tree-walk reads.
    let modules = link_single(
        "v = L.bump(close)\nplot(hour(time(\"1\")) * 100 + minute(time(\"1\")), title=\"V\")\nplot(v, title=\"W\")",
        "export bump(x) =>\n    b = [1.0, 2.0]\n    array.shift(b)\n    x + array.get(b, 0)",
    );
    let bars = ramp_bars(10);
    let tree = run_with_result(None, &modules, &bars);
    let mixed = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&mixed),
        fingerprint_result(&tree),
        "mixed wall-clock parts agree"
    );
    let outcome = mixed.expect("mixed wall-clock parts evaluate");
    let got = plotted_floats(&outcome, "V");
    // 1_700_000_000 is 2023-11-14 22:13:20 UTC; bars tick per minute.
    let want: Vec<Option<f64>> = (13..23)
        .map(|minute| Some(2200.0 + f64::from(minute)))
        .collect();
    assert_eq!(got, want, "UTC parts publish per bar: {outcome:?}");
}

#[test]
fn tick_time_matches_tree() {
    // Tick emulation measures session time in milliseconds: the mixed
    // The loop's time leg carries bar-open times exactly like the
    // tree-walk's, so `time("1")` agrees tick for tick.
    let modules = link_single(
        "v = L.bump(close)\nplot(time(\"1\") + v, title=\"V\")",
        "export bump(x) =>\n    b = [1.0, 2.0]\n    array.shift(b)\n    x + array.get(b, 0)",
    );
    let bars = ramp_bars(10);
    let stamps: Vec<i64> = bars.iter().map(|bar| bar.ts.timestamp_millis()).collect();
    let tree = run_ticks_with_result(None, &modules, &bars, &stamps);
    let mixed = run_ticks_with_result(Some(BytecodeMode::Compiled), &modules, &bars, &stamps);
    assert_eq!(
        fingerprint_result(&mixed),
        fingerprint_result(&tree),
        "mixed tick session time agrees"
    );
    let outcome = mixed.expect("mixed tick session time evaluates");
    let got = plotted_floats(&outcome, "V");
    let want: Vec<Option<f64>> = (0..10)
        .map(|bar| {
            Some((1_700_000_000_i64 + i64::from(bar) * 60) as f64 * 1_000.0 + 102.0 + f64::from(bar) * 0.5)
        })
        .collect();
    assert_eq!(got, want, "tick stamps are milliseconds: {outcome:?}");
}

#[test]
fn time_refusal_parity() {
    // A non-chart timeframe refuses with the tree-walk's exact wording:
    // the loop must name the configured chart period, never an empty one.
    let modules = link_single(
        "v = L.bump(close)\nplot(time(\"D\") + v, title=\"V\")",
        "export bump(x) =>\n    b = [1.0, 2.0]\n    array.shift(b)\n    x + array.get(b, 0)",
    );
    let bars = ramp_bars(5);
    let tree = run_with_result(None, &modules, &bars).expect_err("tree refuses off-period time");
    let mixed =
        run_with_result(Some(BytecodeMode::Compiled), &modules, &bars).expect_err("mixed refuses");
    assert_eq!(
        format!("{mixed}"),
        format!("{tree}"),
        "mixed refusal matches the tree-walk"
    );
}

#[test]
fn multi_assign_binds_items_in_source_order() {
    // `a = ..., b = ...` evaluates and binds left to right, and later
    // items see earlier bindings — the tree-walk's order on both engines.
    let bars = ramp_bars(10);
    for script in [
        "a = close, b = a * 2.0\nplot(a + b, title=\"V\")",
        "var x = 0.0\nx := x + 1.0, y = x * 10.0\nplot(x + y, title=\"V\")",
        "float p = 1.0, q = p + close\nplot(p + q, title=\"V\")",
    ] {
        let modules = link_script(script);
        compile_case(&modules);
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "multi-item bindings agree: {script}"
        );
    }
}

#[test]
fn destructure_binds_items_in_source_order() {
    // Tuple destructuring extracts in element order and stores each target
    // through the single-assignment rule; a later item never observes an
    // extraction that has not run yet.
    let bars = ramp_bars(10);
    for script in [
        "arr = [1.0, 2.0, 3.0]\n[a, b, c] = arr\nplot(a + b * c, title=\"V\")",
        "f() =>\n    [x, y] = [close, open]\n    x - y\nplot(f(), title=\"V\")",
    ] {
        let modules = link_script(script);
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "destructuring agrees: {script}"
        );
    }
}

#[test]
fn in_function_parameter_history_reads_its_own_past() {
    // `v[n]` on a parameter reads the past of its own call site: the loop
    // records each call's final slot value, keyed by call path, exactly
    // like the tree-walk's qualified frame history.
    let bars = ramp_bars(12);
    let modules = link_script("f(v) =>\n    v[1] + v * 2.0\nplot(f(close), title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("parameter history links");
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "parameter history agrees"
    );
    let outcome = vm.expect("parameter history evaluates");
    let got = plotted_floats(&outcome, "V");
    assert_eq!(got[0], None, "the first bar has no parameter past");
    // Bar 1 reads bar 0's parameter (100.0) and doubles its own (100.5).
    assert_eq!(
        got[1],
        Some(100.0 + 201.0),
        "the second bar reads bar 0's value"
    );
}

#[test]
fn in_function_local_history_reads_its_own_past() {
    // A body-bound local keeps its per-call past too, including through a
    // nested call that observes the same call site.
    let bars = ramp_bars(12);
    for script in [
        "f(v) =>\n    y = v * 2.0\n    y[1] + y\nplot(f(close), title=\"V\")",
        // Two call sites never share a past: each key carries its own path.
        "f(v) =>\n    y = v * 2.0\n    y[1] + y\na = f(close)\nb = f(open)\nplot(a + b, title=\"V\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("local history links");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "local history agrees: {script}"
        );
    }
}

#[test]
fn journaled_history_reads_after_the_write_match_tree() {
    // A journaled body's local history is per call: the write (through the
    // journaled mirror) and the later `y[1]` read address one slot, while
    // the caller-visible write still reverts on return.
    let bars = ramp_bars(12);
    let modules = link_script(
        "total = 0.0\nf(v) =>\n    y = v * 2.0\n    total := total + y\n    y[1] + y\nplot(f(close), title=\"V\")",
    );
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "journaled local history agrees"
    );
}

#[test]
fn read_before_binding_refuses_on_both() {
    // Reading a name's past before it is bound is the tree-walk's
    // undefined variable; the loop retains the fallback and refuses with
    // the same wording.
    let bars = ramp_bars(5);
    let modules = link_script(
        "total = 0.0\nf(v) =>\n    y = y[1] + v\n    total := total + y\n    y\nplot(f(close), title=\"V\")",
    );
    let tree = run_with_result(None, &modules, &bars).expect_err("tree refuses the forward read");
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars)
        .expect_err("compiled selection refuses the same read");
    assert_eq!(
        format!("{vm}"),
        format!("{tree}"),
        "read-before-binding refusals match"
    );
}

#[test]
fn in_function_shadowed_history_matches_tree() {
    // A parameter shadowing a global keeps its own per-call past; the
    // shared global's history stays readable from the top level.
    let bars = ramp_bars(12);
    let modules =
        link_script("x = 10.0\nf(x) =>\n    x[1] + x\nplot(f(close) + x[1], title=\"V\")");
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "shadowed history agrees"
    );
}

#[test]
fn forward_reference_refuses_on_both() {
    // A read before the declaration is an undefined variable, in the exact
    // tree-walk wording, on both selections.
    let modules = link_script("v = later + 1.0\nlater = 2.0\nplot(v, title=\"V\")");
    let bars = ramp_bars(5);
    let tree = run_with_result(None, &modules, &bars).expect_err("tree refuses forward read");
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars)
        .expect_err("compiled selection refuses the same read");
    assert_eq!(
        format!("{vm}"),
        format!("{tree}"),
        "forward references refuse identically"
    );
}

#[test]
fn conditional_declaration_reads_match_on_both() {
    // A name declared only under a branch reads the branch's binding when
    // it ran and absence otherwise; the compiled selection must agree.
    let bars = ramp_bars(12);
    let modules = link_script(
        "v = 0.0\nif close > 102.0\n    step = 1.0\n    v := step\nplot(v, title=\"V\")",
    );
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "conditional declarations agree"
    );
}

#[test]
fn stateful_ta_sites_share_across_engines() {
    // Identical top-level `ta.*` sites share one kernel history per run:
    // the tree-walk's merged namespace keys them by the same site string,
    // so a split run must observe the same advances, not one per engine.
    let modules = link_single(
        "v = ta.ema(close, 2)\nplot(v + L.libv(), title=\"V\")",
        "libs = ta.ema(close, 2)\nexport libv() =>\n    libs\nexport force() =>\n    a = [1.0]\n    array.shift(a)\n    0.0",
    );
    let bars = ramp_bars(20);
    let tree = run_with_result(None, &modules, &bars);
    let mixed = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&mixed),
        fingerprint_result(&tree),
        "identical ta sites observe one history across the split"
    );
    let outcome = mixed.expect("shared ta sites evaluate");
    let got = plotted_floats(&outcome, "V");
    assert!(
        got.iter().skip(2).any(Option::is_some),
        "the shared kernel produced values: {outcome:?}"
    );
}

#[test]
fn stateful_fixnan_sites_share_across_engines() {
    // `fixnan` memory is keyed by the argument shape, so two modules with
    // the same shape share one memory in a tree-only run. A split run must
    // replay from that same memory instead of growing a second one.
    let modules = link_single(
        "vhold(v) =>\n    fixnan(v)\nsig = vhold(bar_index % 3 == 0 ? close : na)\nplot(sig + L.libvalue(), title=\"V\")",
        "export hold(v) =>\n    fixnan(v)\nlibv = hold(bar_index % 2 == 0 ? na : 50.0)\nexport libvalue() =>\n    libv\nexport force() =>\n    buf = [1.0]\n    array.shift(buf)\n    0.0",
    );
    let bars = ramp_bars(20);
    let tree = run_with_result(None, &modules, &bars);
    let mixed = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&mixed),
        fingerprint_result(&tree),
        "same-shape fixnan sites share one memory across the split"
    );
}

#[test]
fn input_first_call_wins_across_engines() {
    // One input store per run: the first evaluation of a site declares it,
    // and every later evaluation — from either engine — reads that value.
    let modules = link_single(
        "n = input.int(3, \"N\")\nplot(n + L.libv(), title=\"V\")",
        "libn = input.int(3, \"N\")\nexport libv() =>\n    libn\nexport force() =>\n    a = [1.0]\n    array.shift(a)\n    0.0",
    );
    let bars = ramp_bars(5);
    let tree = run_with_result(None, &modules, &bars);
    let mixed = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&mixed),
        fingerprint_result(&tree),
        "one input store serves both engines"
    );
    let outcome = mixed.expect("shared inputs evaluate");
    let got = plotted_floats(&outcome, "V");
    assert!(
        got.iter().all(|value| *value == Some(6.0)),
        "both modules read the one declared value: {outcome:?}"
    );
}

#[test]
fn disjoint_stateful_sites_still_engage() {
    // Sharing the store must not veto a legal split: distinct `ta.*` sites
    // stay disjoint and the partition still engages.
    let modules = link_single(
        "v = ta.sma(open, 3)\nplot(v + L.value(), title=\"V\")",
        "libv = ta.sma(close, 2)\nexport value() =>\n    libv\nexport force() =>\n    a = [1.0]\n    array.shift(a)\n    0.0",
    );
    let bars = ramp_bars(20);
    let tree = run_with_result(None, &modules, &bars);
    let mixed = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&mixed),
        fingerprint_result(&tree),
        "disjoint sites agree across the split"
    );
}

#[test]
fn nested_ta_sites_keep_the_call_stack_prefix() {
    // A nested `ta.*` site is keyed by its stacked call path. The loop's
    // call-site string for a bridged call is the tree-walk's own
    // `module::function(args)` spelling, so both calls of one nested site
    // observe the same stacked prefix and the same kernel stride.
    let modules = link_single(
        "a = L.smooth(close)\nb = L.smooth(close)\nc = ta.sma(close, 2)\nplot(a + b + c, title=\"V\")",
        "export smooth(x) =>\n    ta.sma(x, 2)\nexport force() =>\n    buf = [1.0]\n    array.shift(buf)\n    0.0",
    );
    let bars = ramp_bars(20);
    let tree = run_with_result(None, &modules, &bars);
    let mixed = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&mixed),
        fingerprint_result(&tree),
        "nested ta sites keep the same stacked prefix"
    );
}

#[test]
fn chart_scope_reads_match_on_both_engines() {
    // Every deterministic chart-scope member lowers to a value read, not a
    // fallback: period, symbol, the harness constants, and the pure
    // timeframe predicates agree on both selections, at the top level and
    // inside functions and methods.
    let bars = ramp_bars(10);
    for script in [
        "p = timeframe.period\nplot(close, title=\"C\")",
        "id = syminfo.tickerid\nplot(close, title=\"C\")",
        "z = syminfo.timezone\nplot(close, title=\"C\")",
        "pv = syminfo.pointvalue\nmc = syminfo.mincontract\nplot(close, title=\"C\")",
        "i = timeframe.isintraday\nplot(close, title=\"C\")",
        "f() =>\n    timeframe.period\ng() =>\n    timeframe.isintraday\np = f()\ni = g()\nplot(close, title=\"C\")",
        "type Box\n int n = 1\nmethod period(Box this) =>\n    timeframe.period\nb = Box.new()\np = b.period()\nplot(close, title=\"C\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("chart-scope reads link");
        let tree = run_with(None, &modules, &bars);
        let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            canonical_form(&vm),
            canonical_form(&tree),
            "selections agree on chart scope: {script}"
        );
    }
    // The configured chart scope is what the reads observe.
    let modules =
        link_script("p = timeframe.period\nid = syminfo.tickerid\nplot(close, title=\"C\")");
    let config = PineRuntimeConfig::new()
        .chart_symbol("QQQ".to_owned())
        .chart_period("5".to_owned())
        .warmup_bars(0)
        .build();
    let vm = run_with_config_result(Some(BytecodeMode::Compiled), &modules, &bars, config)
        .expect("configured chart scope evaluates");
    let last = vm.snapshots.last().expect("snapshots");
    assert_eq!(
        last.values.get("p"),
        Some(&Some(crate::PineValue::Str("5".to_owned()))),
        "timeframe.period reads the configured period"
    );
    assert_eq!(
        last.values.get("id"),
        Some(&Some(crate::PineValue::Str("QQQ".to_owned()))),
        "syminfo.tickerid reads the configured symbol"
    );
    // Warmup bars never change chart scope: the first evaluated bar still
    // reads the same configuration.
    let config = PineRuntimeConfig::new()
        .chart_symbol("SPY".to_owned())
        .chart_period("1".to_owned())
        .warmup_bars(3)
        .build();
    let tree = run_with_config_result(None, &modules, &bars, config.clone()).expect("tree warmup");
    let vm = run_with_config_result(Some(BytecodeMode::Compiled), &modules, &bars, config)
        .expect("compiled warmup");
    assert_eq!(vm.evaluated, 7, "three warmup bars are skipped");
    assert_eq!(canonical_form(&vm), canonical_form(&tree), "warmup agrees");
    assert_eq!(
        vm.snapshots.first().map(|snapshot| snapshot.bar),
        Some(3),
        "the first evaluated bar is the post-warmup bar"
    );
}

#[test]
fn unknown_chart_member_refuses_on_both() {
    // Members the harness has no value for keep the tree-walk's exact
    // refusal: the loop retains the typed fallback instead of inventing a
    // value, so both selections name the same unknown name.
    let modules = link_script("x = timeframe.total\nplot(close, title=\"C\")");
    let program = compile_case(&modules);
    assert!(
        link_case(&program, &modules).is_err(),
        "an unknown member keeps its fallback"
    );
    let bars = ramp_bars(5);
    let tree = run_with_result(None, &modules, &bars).expect_err("tree refuses unknown member");
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars)
        .expect_err("compiled selection refuses the same member");
    assert_eq!(
        format!("{vm}"),
        format!("{tree}"),
        "unknown chart members refuse identically"
    );
}

#[test]
fn tick_mode_chart_scope_matches() {
    // Tick emulation changes the time leg, never the chart scope: period,
    // symbol, and the intraday predicate read the same configuration.
    let modules = link_script(
        "p = timeframe.period\ni = timeframe.isintraday\nid = syminfo.tickerid\nplot(close, title=\"C\")",
    );
    let bars = ramp_bars(10);
    let stamps: Vec<i64> = bars.iter().map(|bar| bar.ts.timestamp_millis()).collect();
    let tree = run_ticks_with_result(None, &modules, &bars, &stamps);
    let vm = run_ticks_with_result(Some(BytecodeMode::Compiled), &modules, &bars, &stamps);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "tick-mode chart scope agrees"
    );
    let vm = vm.expect("compiled tick run evaluates");
    let tree = tree.expect("tree tick run evaluates");
    assert_eq!(
        plotted_floats(&vm, "C"),
        plotted_floats(&tree, "C"),
        "tick legs agree bar for bar"
    );
}

#[test]
fn timeframe_change_matches_across_bars() {
    // `timeframe.change` is a pure adjacent-bar bucket comparison; the
    // loop serves it through the same shared step as the tree-walk and
    // agrees on every bucket boundary.
    let bars = ramp_bars(90);
    for script in [
        "c = timeframe.change(\"1\")\nplot(close, title=\"C\")",
        "c = timeframe.change(\"60\")\nplot(close, title=\"C\")",
        "c = timeframe.change(\"D\")\nplot(close, title=\"C\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("timeframe.change links");
        let tree = run_with(None, &modules, &bars);
        let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            canonical_form(&vm),
            canonical_form(&tree),
            "selections agree on timeframe.change: {script}"
        );
    }
    // The ramp opens at 22:13:20 UTC and ticks per minute, so the minute
    // bucket changes every bar and the hour bucket changes once, at 23:00
    // (bar 47). Both shapes pin the shared boundary rule.
    let series_of = |script: &str| -> Vec<Option<bool>> {
        let modules = link_script(script);
        let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
        vm.snapshots
            .iter()
            .map(|snapshot| match snapshot.values.get("c") {
                Some(Some(crate::PineValue::Bool(value))) => Some(*value),
                other => panic!("timeframe.change reads bool, got {other:?}"),
            })
            .collect()
    };
    let minutes = series_of("c = timeframe.change(\"1\")\nplot(close, title=\"C\")");
    assert_eq!(minutes[0], Some(false), "the first bar has no prior bucket");
    assert!(
        minutes.iter().skip(1).all(|value| *value == Some(true)),
        "every later minute opens a new minute bucket: {minutes:?}"
    );
    let hours = series_of("c = timeframe.change(\"60\")\nplot(close, title=\"C\")");
    assert_eq!(hours[0], Some(false), "the first bar has no prior bucket");
    assert_eq!(hours[47], Some(true), "23:00 UTC opens a new hour bucket");
    assert!(
        hours
            .iter()
            .enumerate()
            .all(|(index, value)| index == 47 || *value == Some(false)),
        "the hour bucket changes exactly once: {hours:?}"
    );
}

#[test]
fn downgraded_library_calls_stay_identical() {
    // Chained library calls evaluate identically under either selection,
    // including collection shapes inside the middle hop.
    let lib_b = crate::parse_program("export double(x) => x * 2.0").expect("parses");
    let lib_a = crate::parse_program(
        "import test/libB/1 as B\n\nexport use(x) =>\n    b = [1.0]\n    array.shift(b)\n    B.double(x)",
    )
    .expect("parses");
    let driver = crate::parse_program("v = A.use(close)\nplot(v, title=\"V\")").expect("parses");
    let modules = vec![
        crate::LinkedModule::new("test/libB", lib_b, Default::default()),
        crate::LinkedModule::new(
            "test/libA",
            lib_a,
            [("B".to_owned(), "test/libB".to_owned())]
                .into_iter()
                .collect(),
        ),
        crate::LinkedModule::new(
            "local/Driver",
            driver,
            [("A".to_owned(), "test/libA".to_owned())]
                .into_iter()
                .collect(),
        ),
    ];
    let bars = ramp_bars(15);
    let tree = run_with_result(None, &modules, &bars);
    let mixed = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&mixed),
        fingerprint_result(&tree),
        "chained calls agree"
    );
    mixed.expect("chained run evaluates");
}

#[test]
fn shared_global_reads_stay_identical() {
    // A plain global read across modules evaluates identically under
    // either selection.
    let modules = link_single(
        "v = L.get() + shared + close\nplot(v, title=\"V\")",
        "shared = 1.0\nexport get() =>\n    shared",
    );
    let bars = ramp_bars(15);
    let tree = run_with_result(None, &modules, &bars);
    let mixed = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&mixed),
        fingerprint_result(&tree),
        "shared global run agrees"
    );
}

#[test]
fn multi_assign_lowes_to_sequential_stores() {
    // Multi-target assignment is a pure sequential desugar: each item
    // runs its single-assignment rule in order (`=` declares and
    // stores, `:=` rebinds a declared name), so the loop emits the same
    // stores the tree executes. Byte-identical outcomes with fully
    // linked modules; the coverage assertion fails while the family
    // falls back.
    for script in [
        "a = 1.0, b = 2.0\nplot(a + b, title=\"S\")",
        "o = 0.0\nh = 0.0\no := open, h = high\nplot(o + h, title=\"S\")",
        "a = close, b = close[1]\nplot(a + nz(b), title=\"S\")",
        "f(x) =>\n    a = x + 1, b = x + 2\n    a + b\nplot(f(close), title=\"F\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).expect("multi-assign shapes link");
        assert!(
            (linked.coverage() - 1.0).abs() < f64::EPSILON,
            "{script}: the loop actually runs; fallback would vacate the comparison"
        );
        let bars = ramp_bars(30);
        let tree = run_with(None, &modules, &bars);
        let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            canonical_form(&vm),
            canonical_form(&tree),
            "{script}: sequential stores agree across engines"
        );
    }
}

#[test]
fn multi_assign_undeclared_rebind_refuses_on_both() {
    // The family's characteristic refusal: `:=` on an undeclared name
    // refuses through the single-rebind rule on the tree, and the loop
    // keeps the whole statement on the tree via the same named reason —
    // identical strings either way.
    let modules = link_script("q := 1.0, w = 2.0\nplot(w, title=\"W\")");
    let bars = ramp_bars(5);
    let tree = run_with_result(None, &modules, &bars).expect_err("tree refuses the rebind");
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars)
        .expect_err("compiled refuses the rebind");
    assert!(
        tree.to_string()
            .contains("':=' targets undeclared variable 'q'"),
        "tree names the reason: {tree}"
    );
    assert_eq!(
        vm.to_string(),
        tree.to_string(),
        "compiled matches the tree exactly"
    );
}

#[test]
fn aliased_reads_resolve_through_export_table() {
    // Alias-qualified reads lower at link time to the same shared column
    // the bare name uses: the target exports the name, the spellings are
    // unambiguous, and both selections agree bar for bar.
    let lib = crate::parse_program("export const float THRESH = 10.0\n").expect("lib parses");
    let driver =
        crate::parse_program("v = L.THRESH + close\nplot(v, title=\"V\")").expect("parses");
    let modules = vec![
        crate::LinkedModule::new("test/lib", lib, Default::default()),
        crate::LinkedModule::new(
            "local/Driver",
            driver,
            [("L".to_owned(), "test/lib".to_owned())]
                .into_iter()
                .collect(),
        ),
    ];
    let bars = ramp_bars(10);
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "aliased reads agree"
    );
    let program = compile_case(&modules);
    assert!(
        link_case(&program, &modules).is_ok(),
        "aliased reads link without fallback"
    );
    let last = vm.snapshots.last().expect("snapshots");
    assert!(
        last.values.contains_key("v"),
        "the aliased read binds: {last:?}"
    );
}

#[test]
fn unexported_and_unknown_dotted_reads_stay_tree_walk() {
    // Alias reads resolve through the target's namespace exactly like the
    // tree-walk (which applies no export gate), so an unexported spelling
    // links and both selections read the shared column identically.
    // Unknown members keep their fallback: the tree refuses naming the
    // bare spelling.
    let lib =
        crate::parse_program("export const float SHOWN = 1.0\nhidden = 2.0\n").expect("lib parses");
    let driver =
        crate::parse_program("v = L.hidden + close\nplot(v, title=\"V\")").expect("parses");
    let modules = vec![
        crate::LinkedModule::new("test/lib", lib, Default::default()),
        crate::LinkedModule::new(
            "local/Driver",
            driver,
            [("L".to_owned(), "test/lib".to_owned())]
                .into_iter()
                .collect(),
        ),
    ];
    let bars = ramp_bars(5);
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "unexported reads agree"
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("unexported reads link through the alias rule");
    let outcome = tree.expect("the shared column serves the unexported spelling");
    let want: Vec<Option<f64>> = bars
        .iter()
        .map(|bar| Some(protopyne_types::bar_leg_as_f64(bar.c) + 2.0))
        .collect();
    assert_eq!(
        outcome.plots[0]
            .values
            .iter()
            .map(|cell| match cell {
                Some(crate::PineValue::Float(v)) => Some(*v),
                other => panic!("numeric close plus hidden, got {other:?}"),
            })
            .collect::<Vec<_>>(),
        want,
        "every bar reads close plus the unexported 2.0"
    );

    // Unknown member: the tree refuses naming the bare spelling, the loop
    // keeps the fallback and refuses identically.
    let unknown = link_script("x = E.BOGUS\nplot(x, title=\"X\")");
    let tree = run_with_result(None, &unknown, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &unknown, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "unknown members agree"
    );
    assert!(tree.is_err(), "the tree refuses the unknown member");
    let reason = tree.expect_err("refuses above").to_string();
    assert!(
        reason.contains("BOGUS"),
        "the refusal names the member: {reason}"
    );

    // Ambiguous multi-dot spellings never resolve statically.
    let ambiguous = link_script("v = a.b.c\nplot(v, title=\"V\")");
    let tree = run_with_result(None, &ambiguous, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &ambiguous, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "ambiguous spellings agree through fallback"
    );
}

#[test]
fn dotted_call_homes_write_back_like_tree() {
    // Dotted write-back homes persist through the shared column exactly
    // like the tree's assign path, except `this.*` which stays tree-only
    // until the object model lowers.
    let modules = link_script(
        "f(x) =>\n    y = array.get(x, 0)\n    y\na.b = array.new_float(1, 1.0)\nf(a.b)\nplot(array.size(a.b), title=\"V\")",
    );
    let bars = ramp_bars(5);
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "dotted homes agree"
    );

    // `this` rebinding refuses identically on both selections.
    let this_home = link_script("f(x) => x\nf(this)\nplot(close, title=\"C\")");
    let tree = run_with_result(None, &this_home, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &this_home, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "`this` homes agree"
    );
}

#[test]
fn destructured_arrays_bind_elementwise_and_link() {
    // A tuple unpack evaluates its value once, checks the arity, then
    // binds each element left to right: both selections publish the
    // elementwise sums.
    let bars = ramp_bars(10);
    let modules = link_script("v = close\n[a, b] = [v, v * 2.0]\nplot(a + b, title=\"S\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("tuple unpacks link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on unpacked sums"
    );
    for (bar, cell) in plotted_floats(&vm, "S").iter().enumerate() {
        let want = 3.0 * (100.0 + bar as f64 * 0.5);
        assert!(
            cell.is_some_and(|v| (v - want).abs() < 1e-9),
            "bar {bar} sums both elements: {cell:?} vs {want}"
        );
    }

    // Unpacking inside a function binds frame locals, discarded on
    // return exactly like the tree-walk's journal teardown.
    let nested = link_script("f() =>\n    [a, b] = [1.0, 2.0]\n    a + b\nplot(f(), title=\"F\")");
    let program = compile_case(&nested);
    link_case(&program, &nested).expect("in-function unpacks link");
    let tree = run_with(None, &nested, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &nested, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on in-function unpacks"
    );
    assert!(
        plotted_floats(&vm, "F")
            .iter()
            .all(|cell| *cell == Some(3.0)),
        "every bar sums the unpacked pair"
    );
}

#[test]
fn destructured_arity_and_kind_refusals_match() {
    // Arity, absence, and kind mismatches refuse with the tree's exact
    // wording out of the loop: the run links, then refuses at the site.
    let bars = ramp_bars(5);
    for script in [
        "[a, b] = [1.0]\nplot(a, title=\"A\")",
        "[a, b] = na\nplot(a, title=\"A\")",
        "[a] = 1.0\nplot(a, title=\"A\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules)
            .unwrap_or_else(|err| panic!("{script}: unpack refusals link: {err:?}"));
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "{script}: unpack refusals match"
        );
        assert!(tree.is_err(), "{script}: the tree refuses");
        let reason = tree.expect_err("refuses above").to_string();
        assert!(
            reason.contains("destructuring"),
            "{script}: the refusal names the unpack: {reason}"
        );
    }
}

#[test]
fn typed_multi_declarations_initialize_in_order_and_link() {
    // Comma-separated typed items declare plus initialize left to right;
    // an absent initializer seeds absence, exactly like the tree-walk.
    let bars = ramp_bars(10);
    let modules = link_script("float x = 1.0, y = x + 1.0\nplot(y, title=\"Y\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("multi-declarations link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on declared items"
    );
    assert!(
        plotted_floats(&vm, "Y")
            .iter()
            .all(|cell| *cell == Some(2.0)),
        "every bar reads the second item"
    );

    // Absent initializers seed absence.
    let seeded =
        link_script("float p = 2.0, q\nplot(p, title=\"P\")\nplot(na(q) ? 1.0 : 0.0, title=\"Q\")");
    let program = compile_case(&seeded);
    link_case(&program, &seeded).expect("absent initializers link");
    let tree = run_with(None, &seeded, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &seeded, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on seeded absence"
    );
    assert!(
        plotted_floats(&vm, "Q")
            .iter()
            .all(|cell| *cell == Some(1.0)),
        "the valueless item reads absent on every bar"
    );

    // Items inside functions bind frame locals.
    let body =
        link_script("f() =>\n    float x = 1.0, y = x + 1.0\n    x + y\nplot(f(), title=\"F\")");
    let program = compile_case(&body);
    link_case(&program, &body).expect("in-function declarations link");
    let tree = run_with(None, &body, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &body, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on in-function declarations"
    );
    assert!(
        plotted_floats(&vm, "F")
            .iter()
            .all(|cell| *cell == Some(3.0)),
        "every bar sums the declared pair"
    );

    // The first failing item reports: later items never bind.
    let failing = link_script("float a = nosuchfn(1.0), b = 2.0\nplot(b, title=\"B\")");
    let tree = run_with_result(None, &failing, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &failing, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "failing items refuse identically"
    );
    assert!(tree.is_err(), "the unknown callee refuses");
}

#[test]
fn unpacked_and_declared_items_leave_no_tuple_fallback() {
    // The scripts above must carry no tuple-unpack marker: only that
    // family shrinks when these shapes lower.
    use crate::pine::bytecode::{FallbackReason, Instruction};
    let scripts = [
        "v = close\n[a, b] = [v, v * 2.0]\nplot(a + b, title=\"S\")",
        "f() =>\n    [a, b] = [1.0, 2.0]\n    a + b\nplot(f(), title=\"F\")",
        "float x = 1.0, y = x + 1.0\nplot(y, title=\"Y\")",
        "float p = 2.0, q\nplot(p, title=\"P\")\nplot(na(q) ? 1.0 : 0.0, title=\"Q\")",
        "f() =>\n    float x = 1.0, y = x + 1.0\n    x + y\nplot(f(), title=\"F\")",
    ];
    for script in scripts {
        let modules = link_script(script);
        let program = compile_case(&modules);
        for module in &modules {
            let compiled = program
                .get(&module.key)
                .unwrap_or_else(|| panic!("{script}: module compiled"));
            for block in &compiled.blocks {
                for instr in &block.instrs {
                    assert!(
                        !matches!(
                            instr,
                            Instruction::Fallback {
                                reason: FallbackReason::TupleUnpack,
                                ..
                            }
                        ),
                        "{script}: no tuple fallback remains"
                    );
                }
            }
        }
        link_case(&program, &modules)
            .unwrap_or_else(|err| panic!("{script}: links without fallback: {err:?}"));
    }
}

fn fingerprinted_strategy_calls(
    outcome: &Result<RunOutcome, crate::KernelError>,
) -> Vec<crate::pine::eval::StrategyCall> {
    outcome
        .as_ref()
        .expect("strategy run evaluates")
        .strategy_calls
        .clone()
}

fn plotted_floats(outcome: &RunOutcome, title: &str) -> Vec<Option<f64>> {
    outcome
        .plots
        .iter()
        .find(|plot| plot.title == title)
        .unwrap_or_else(|| panic!("plot {title} recorded"))
        .values
        .iter()
        .map(|cell| match cell {
            None => None,
            Some(crate::PineValue::Float(v)) => Some(*v),
            Some(crate::PineValue::Int(v)) => Some(*v as f64),
            Some(other) => panic!("plot {title} holds a number, got {other:?}"),
        })
        .collect()
}

#[test]
fn lagged_parenthesized_sums_agree_and_link() {
    // History of a parenthesized sum reads the lagged value of the sum:
    // the sum evaluates once per bar and the offset reads back through
    // it, so both selections publish identical lagged values.
    let bars = ramp_bars(10);
    let lagged = link_script("v = (close + open)[1]\nplot(v, title=\"V\")");
    {
        let modules = &lagged;
        let program = compile_case(modules);
        link_case(&program, modules).expect("parenthesized histories link");
        let tree = run_with(None, modules, &bars);
        let vm = run_with(Some(BytecodeMode::Compiled), modules, &bars);
        assert_eq!(
            canonical_form(&vm),
            canonical_form(&tree),
            "selections agree on lagged sums"
        );
    }
    let values = plotted_floats(&run_with(None, &lagged, &bars), "V");
    assert_eq!(values.len(), 10);
    assert!(values[0].is_none(), "no bar precedes the first: {values:?}");
    for (bar, cell) in values.iter().enumerate().skip(1) {
        let prior = 100.0 + (bar - 1) as f64 * 0.5;
        let want = prior + (prior - 0.25);
        assert!(
            cell.is_some_and(|v| (v - want).abs() < 1e-9),
            "bar {bar} reads the lagged sum: {cell:?} vs {want}"
        );
    }

    // A zero offset reads the current bar through the same path.
    let current = link_script("v = (close * 2.0)[0]\nplot(v, title=\"V\")");
    let program = compile_case(&current);
    link_case(&program, &current).expect("zero-offset parenthesized links");
    let tree = run_with(None, &current, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &current, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on zero-offset sums"
    );
    for (bar, cell) in plotted_floats(&vm, "V").iter().enumerate() {
        let want = 2.0 * (100.0 + bar as f64 * 0.5);
        assert!(
            cell.is_some_and(|v| (v - want).abs() < 1e-9),
            "bar {bar} reads the current doubled close: {cell:?} vs {want}"
        );
    }

    // Offsets compose: the inner named offset feeds the outer sum offset.
    let nested = link_script("v = (close[1] + 1.0)[1]\nplot(v, title=\"V\")");
    let program = compile_case(&nested);
    link_case(&program, &nested).expect("nested offsets link");
    let tree = run_with(None, &nested, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &nested, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on nested offsets"
    );
    for (bar, cell) in plotted_floats(&vm, "V").iter().enumerate() {
        let want = if bar < 2 {
            None
        } else {
            Some(100.0 + (bar - 2) as f64 * 0.5 + 1.0)
        };
        assert_eq!(
            *cell, want,
            "bar {bar} reads twice-lagged close plus one: {cell:?} vs {want:?}"
        );
    }
}

#[test]
fn parenthesized_histories_lower_without_computed_fallback() {
    // The scripts above must carry no computed-series fallback marker:
    // only that family shrinks when parenthesized histories lower.
    use crate::pine::bytecode::{FallbackReason, Instruction};
    let scripts = [
        "v = (close + open)[1]\nplot(v, title=\"V\")",
        "v = (close * 2.0)[0]\nplot(v, title=\"V\")",
        "v = (close[1] + 1.0)[1]\nplot(v, title=\"V\")",
    ];
    for script in scripts {
        let modules = link_script(script);
        let program = compile_case(&modules);
        for module in &modules {
            let compiled = program
                .get(&module.key)
                .unwrap_or_else(|| panic!("{script}: module compiled"));
            for block in &compiled.blocks {
                for instr in &block.instrs {
                    assert!(
                        !matches!(
                            instr,
                            Instruction::Fallback {
                                reason: FallbackReason::ComputedSeriesHistory,
                                ..
                            }
                        ),
                        "{script}: no computed-series fallback remains"
                    );
                }
            }
        }
        link_case(&program, &modules)
            .unwrap_or_else(|err| panic!("{script}: links without fallback: {err:?}"));
    }
}

#[test]
fn compound_history_negative_offset_refuses_on_both() {
    // A negative offset through brackets refuses with the shared
    // history-edge wording on both selections.
    let bars = ramp_bars(5);
    let modules = link_script("v = (close + open)[-1]\nplot(v, title=\"V\")");
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "compound negative offsets agree"
    );
}

#[test]
fn cross_module_history_ordering_links_or_falls_back() {
    // A reader ordered before its writer links through the loop's
    // declaredness rule and both selections refuse naming the unreadable
    // name; a writer ordered first lets the reader link and both publish
    // the lagged values.
    let bars = ramp_bars(5);
    let writer = crate::parse_program("x = 5.0").expect("writer parses");
    let reader = crate::parse_program("y = x[1]\nplot(y, title=\"Y\")").expect("reader parses");
    let unreadable = vec![
        crate::LinkedModule::new("test/reader", reader.clone(), Default::default()),
        crate::LinkedModule::new("test/writer", writer.clone(), Default::default()),
    ];
    let program = compile_case(&unreadable);
    link_case(&program, &unreadable).expect("reader-before-writer links through the rule");
    let tree = run_with_result(None, &unreadable, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &unreadable, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "unreadable histories refuse identically"
    );
    assert!(tree.is_err(), "the tree refuses the unordered read");
    assert!(
        tree.expect_err("refuses above").to_string().contains('x'),
        "the refusal names the unreadable series"
    );

    let ordered = vec![
        crate::LinkedModule::new("test/writer", writer, Default::default()),
        crate::LinkedModule::new("test/reader", reader, Default::default()),
    ];
    let program = compile_case(&ordered);
    link_case(&program, &ordered).expect("writer-before-reader links");
    let tree = run_with(None, &ordered, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &ordered, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "ordered histories agree"
    );
    assert_eq!(
        plotted_floats(&vm, "Y"),
        vec![None, Some(5.0), Some(5.0), Some(5.0), Some(5.0)],
        "the lagged constant reads na then the value"
    );

    // The same ordering rule covers plain reads and rebinding: the
    // reader links through the declaredness rule and both selections
    // refuse naming the same name.
    let plain_reader = crate::parse_program("y = x\nplot(y, title=\"Y\")").expect("reader parses");
    let plain_writer = crate::parse_program("x = 5.0").expect("writer parses");
    let plain_unordered = vec![
        crate::LinkedModule::new("test/reader", plain_reader.clone(), Default::default()),
        crate::LinkedModule::new("test/writer", plain_writer.clone(), Default::default()),
    ];
    let program = compile_case(&plain_unordered);
    link_case(&program, &plain_unordered).expect("unordered plain reads link through the rule");
    let tree = run_with_result(None, &plain_unordered, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &plain_unordered, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "unordered plain reads refuse identically"
    );
    assert!(tree.is_err(), "the tree refuses the unordered read");
    assert!(
        tree.expect_err("refuses above").to_string().contains('x'),
        "the refusal names the unreadable series"
    );
    let rebind_reader =
        crate::parse_program("x := 2.0\nplot(x, title=\"X\")").expect("reader parses");
    let rebind_unordered = vec![
        crate::LinkedModule::new("test/reader", rebind_reader, Default::default()),
        crate::LinkedModule::new("test/writer", plain_writer, Default::default()),
    ];
    let program = compile_case(&rebind_unordered);
    link_case(&program, &rebind_unordered).expect("unordered rebinds link through the rule");
    let tree = run_with_result(None, &rebind_unordered, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &rebind_unordered, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "unordered rebinds refuse identically"
    );
}

#[test]
fn nested_calls_observe_caller_frame_writes() {
    // A callee writing a caller-visible global is observed by a nested
    // call made after the write: the write flows to the shared column
    // and reverts on return, so both selections publish the nested value.
    let bars = ramp_bars(10);
    let modules = link_script("g = 0.0\nh() => g\nf() =>\n g = 1.0\n h()\nplot(f(), title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("global-mutating calls link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on nested observation"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|cell| *cell == Some(1.0)),
        "every bar publishes the nested read"
    );
}

#[test]
fn reassigned_globals_link_and_agree() {
    // Rebinding a declared global inside a function lowers instead of
    // falling back: the rebinding applies for the rest of the call and
    // reverts on return, identically on both selections.
    let bars = ramp_bars(10);
    let modules = link_script("g = 0.0\nf() =>\n g := g + 1.0\n g\nplot(f(), title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("in-function rebinds link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on rebinding"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|cell| *cell == Some(1.0)),
        "every bar publishes the rebound value"
    );
}

#[test]
fn shadowing_reverts_after_return() {
    // A shadowing write is visible to nested calls made after it, but
    // the caller home is restored when the call returns: later reads
    // see the pre-call value on both selections.
    let bars = ramp_bars(10);
    let modules = link_script(
        "g = 0.0\nh() => g + 100.0\nf(x) =>\n g = x\n h() + g\nplot(f(1.0) + f(2.0) + g, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("shadowing calls link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on shadow-and-revert"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|cell| *cell == Some(206.0)),
        "revert restores the pre-call home: {vm:?}"
    );
}

#[test]
fn in_function_counters_revert_like_tree() {
    // Loop counters inside functions bind fresh per call and vanish on
    // return: snapshots never carry the counter on either selection.
    let bars = ramp_bars(10);
    let modules =
        link_script("f() =>\n s = 0.0\n for i = 0 to 2\n  s := i\n s\nplot(f(), title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("in-function loops link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on in-function loops"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|cell| *cell == Some(2.0)),
        "every bar publishes the final counter"
    );
}

#[test]
fn pure_functions_emit_no_journal_writes() {
    // Escape analysis is static per function: a body that never assigns
    // outside its own frame keeps the existing fast path (no journal
    // instructions), while a global-mutating body journals.
    use crate::pine::bytecode::Instruction;
    let pure = link_script("f(x) => x * 2.0 + 1.0\nplot(f(close), title=\"F\")");
    let program = compile_case(&pure);
    for module in &pure {
        let compiled = program
            .get(&module.key)
            .unwrap_or_else(|| panic!("pure module compiled"));
        for block in &compiled.blocks {
            assert!(
                !block
                    .instrs
                    .iter()
                    .any(|instr| matches!(instr, Instruction::JournalWrite { .. })),
                "pure bodies keep the fast path"
            );
        }
    }
    let mutating = link_script("g = 0.0\nf() =>\n g = 1.0\n g\nplot(f(), title=\"V\")");
    let program = compile_case(&mutating);
    link_case(&program, &mutating).expect("mutating calls link");
    let mut saw_journal = false;
    for module in &mutating {
        let compiled = program
            .get(&module.key)
            .unwrap_or_else(|| panic!("mutating module compiled"));
        for block in &compiled.blocks {
            saw_journal |= block
                .instrs
                .iter()
                .any(|instr| matches!(instr, Instruction::JournalWrite { .. }));
        }
    }
    assert!(saw_journal, "mutating bodies journal their writes");
}

#[test]
fn undeclared_rebind_in_function_refuses_on_both() {
    // Rebinding a name bound nowhere refuses with the tree's exact
    // wording out of the loop: the journal resolves homes at run time
    // and refuses unresolvable ones identically.
    let bars = ramp_bars(5);
    let modules = link_script("f() =>\n q := 1.0\n q\nplot(f(), title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("refusing calls still link");
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "undeclared rebinds refuse identically"
    );
    assert!(tree.is_err(), "the tree refuses the undeclared rebind");
    assert!(
        tree.expect_err("refuses above")
            .to_string()
            .contains("':=' targets undeclared variable 'q'"),
        "the refusal names the variable"
    );
}

#[test]
fn in_function_histories_of_written_names_stay_on_tree() {
    // Per-call qualified history with the bare column behind it: history
    // reads of names a journaled body writes lower to the checked
    // local-history read (qualified past, bare column when the key never
    // recorded) instead of staying tree-walk — both selections agree with
    // full coverage throughout.
    let bars = ramp_bars(10);
    for script in [
        "g = 0.0\nf() =>\n g = 1.0\n g[1]\nplot(f(), title=\"V\")",
        "g = 0.0\nf(j) =>\n g = 1.0\n g[j]\nplot(f(1), title=\"V\")",
        "g = 0.0\nf(g) =>\n g[1]\nplot(f(1.0), title=\"V\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules)
            .unwrap_or_else(|err| panic!("{script}: links with full coverage: {err:?}"));
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "in-function written-name histories agree: {script}"
        );
    }
}

#[test]
fn in_function_plot_values_pair_with_fill() {
    // A `plot()` value bound inside a function tags the global site
    // while the value itself stays frame-local: an outer `fill()`
    // pairing through that variable agrees on both selections.
    let bars = ramp_bars(10);
    let modules = link_script(
        "f() =>\n p = plot(close, title=\"P\")\n fill(p, plot(close, title=\"Q\"))\n close\nplot(f(), title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("in-function plot tags link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on in-function plot pairing"
    );
    assert!(!vm.fills.is_empty(), "the pairing emits fills: {vm:?}");
}

#[test]
fn cleared_plot_tags_refuse_fill_on_both() {
    // Rebinding a plot-tagged name to a plain value clears the tag, so a
    // later `fill()` through it refuses with the tree's exact wording
    // out of the loop.
    let bars = ramp_bars(5);
    let modules = link_script(
        "f() =>\n p = plot(close, title=\"P\")\n p = 1.0\n fill(p, plot(close, title=\"Q\"))\n close\nplot(f(), title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("tag clearing links");
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "cleared tags refuse identically"
    );
    assert!(tree.is_err(), "the tree refuses the untagged fill");
}

#[test]
fn in_function_var_accumulates_across_bars() {
    // An in-function `var` cell persists per instance across bars: the
    // counter climbs 1, 2, 3 on both selections.
    let bars = ramp_bars(10);
    let modules = link_script("f() =>\n var c = 0\n c := c + 1\n c\nplot(f(), title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("in-function vars link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on var accumulation"
    );
    let got = plotted_floats(&vm, "V");
    let want: Vec<Option<f64>> = (1..=10).map(|n| Some(f64::from(n))).collect();
    assert_eq!(got, want, "the cell climbs every bar: {vm:?}");
}

#[test]
fn in_function_varip_resets_each_bar() {
    // An in-function `varip` cell persists within a bar but drops at the
    // advance: the same call site twice in one bar shares the cell (one
    // plus two), while later bars read absence — identically on both
    // selections.
    let bars = ramp_bars(10);
    let modules = link_script(
        "f() =>\n varip c = 0\n c := c + 1\n c\ns = 0\nfor i = 0 to 1\n s := f()\nplot(s, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("in-function varips link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on varip reset"
    );
    let got = plotted_floats(&vm, "V");
    let mut want = vec![Some(2.0)];
    want.extend(std::iter::repeat_n(None, 9));
    assert_eq!(got, want, "within-bar shares, cross-bar resets: {vm:?}");
}

#[test]
fn recursive_var_cells_stay_per_activation() {
    // Recursion gets fresh cells per activation: three nested instances
    // each add ten on the first bar, and each instance keeps accumulating
    // across bars — thirty, sixty, ninety on both selections. Shared
    // cells would already publish sixty on the first bar.
    let bars = ramp_bars(10);
    let modules = link_script(
        "f(n) =>\n var c = 0\n c := c + 10\n c + (n <= 0 ? 0 : f(n - 1))\nplot(f(2), title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("recursive vars link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on recursion"
    );
    let got = plotted_floats(&vm, "V");
    let want: Vec<Option<f64>> = (1..=10).map(|n| Some(30.0 * f64::from(n))).collect();
    assert_eq!(got, want, "cells persist per instance: {vm:?}");
}

#[test]
fn unseeded_var_reads_refuse_on_both() {
    // Reading an in-function `var` before its declaration executes
    // refuses with the tree's exact wording out of the loop.
    let bars = ramp_bars(5);
    let modules = link_script("f() =>\n q\n var q = 1\n q\nplot(f(), title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("unseeded reads link");
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "unseeded reads refuse identically"
    );
    assert!(tree.is_err(), "the tree refuses the unseeded read");
}

#[test]
fn object_shapes_still_run_tree_walk_on_both() {
    // Handles ride the loop by shared handle; construction and method
    // dispatch lower now, while dotted writes keep their fallback — so
    // linked shapes publish identical outcomes and the retained shape
    // still agrees through the tree-walk.
    let bars = ramp_bars(10);
    for script in [
        "type Counter\n int n = 0\nmethod get(Counter this) =>\n this.n\ngot = Counter.new().get()\nplot(got, title=\"V\")",
        "type Box2\n int n = 0\nmethod bump(Box2 this, int d) =>\n this.n += d\n this.n\nw = Box2.new()\na = w.bump(5)\nplot(a, title=\"V\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("object calls link");
        let tree = run_with(None, &modules, &bars);
        let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            canonical_form(&vm),
            canonical_form(&tree),
            "object calls agree: {script}"
        );
    }
    // Single-dot field writes and reads lower through the shared home
    // model: the instance leaves its home, mutates, and returns.
    let modules =
        link_script("type Wrap\n int n = 0\nw = Wrap.new()\nw.n := 3\nplot(w.n, title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("dotted writes link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "dotted writes agree"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|value| *value == Some(3.0)),
        "the written field publishes: {vm:?}"
    );
}

#[test]
fn field_reads_on_plain_values_refuse_on_both() {
    // Parenthesized receivers lower through the shared table: a
    // non-object receiver refuses with the tree-walk's exact wording
    // out of the loop, and absence refuses on its own.
    let bars = ramp_bars(5);
    for (script, wording) in [
        ("plot((1).n, title=\"V\")", "needs an object receiver"),
        ("plot((close).n, title=\"V\")", "needs an object receiver"),
        ("f() =>\n na\nplot((f()).n, title=\"V\")", "read on na"),
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("field reads link");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "field refusals match: {script}"
        );
        assert!(tree.is_err(), "the tree refuses: {script}");
        assert!(
            tree.expect_err("refuses above")
                .to_string()
                .contains(wording),
            "the refusal names the rule: {script}"
        );
    }
}

#[test]
fn object_field_reads_agree_across_modules() {
    // Field reads lower in the library (no fallback there) while
    // construction keeps the driver on the tree-walk: both selections
    // agree, values and refusals alike.
    let lib = crate::parse_program(
        "export id(o) => o\nexport get(o) =>\n (id(o)).n\nexport deep(o) =>\n ((id(o)).box).n\nexport missing(o) =>\n (id(o)).bogus",
    )
    .expect("lib parses");
    let driver = crate::parse_program(
        "type Box2\n int n = 5\nw = Box2.new()\nv = L.get(w)\nplot(v, title=\"V\")",
    )
    .expect("driver parses");
    let modules = vec![
        crate::LinkedModule::new("test/lib", lib, Default::default()),
        crate::LinkedModule::new(
            "local/Driver",
            driver,
            [("L".to_owned(), "test/lib".to_owned())]
                .into_iter()
                .collect(),
        ),
    ];
    let bars = ramp_bars(10);
    let program = compile_case(&modules);
    let lib_compiled = program.get("test/lib").expect("library compiles");
    assert!(
        !lib_compiled.blocks.iter().any(|block| block
            .instrs
            .iter()
            .any(|instr| matches!(instr, crate::pine::bytecode::Instruction::Fallback { .. }))),
        "field reads lower without fallback"
    );
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "object field reads agree"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|cell| *cell == Some(5.0)),
        "every bar publishes the field: {vm:?}"
    );
    // Nested reads pair through two tables, and a missing field refuses
    // naming both the field and its type — identically on both engines.
    let nested_lib = "export id(o) => o\nexport deep(o) =>\n ((id(o)).box).n\nexport missing(o) =>\n (id(o)).bogus";
    for (stmt, wording, expect_ok) in [
        (
            "v = L.deep(w)\nplot(v, title=\"V\")",
            "nested fields agree",
            true,
        ),
        (
            "v = L.missing(w)\nplot(v, title=\"V\")",
            "missing fields refuse identically",
            false,
        ),
    ] {
        let nested = crate::parse_program(&format!(
            "type Box2\n int n = 5\ntype Wrap\n Box2 box = na\nw = Wrap.new()\nw.box := Box2.new()\n{stmt}"
        ))
        .expect("driver parses");
        let modules = vec![
            crate::LinkedModule::new(
                "test/lib",
                crate::parse_program(nested_lib).expect("lib parses"),
                Default::default(),
            ),
            crate::LinkedModule::new(
                "local/Driver",
                nested,
                [("L".to_owned(), "test/lib".to_owned())]
                    .into_iter()
                    .collect(),
            ),
        ];
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "{wording}"
        );
        if expect_ok {
            let vm = vm.expect("nested reads evaluate");
            assert!(
                plotted_floats(&vm, "V")
                    .iter()
                    .all(|cell| *cell == Some(5.0)),
                "every bar publishes the nested field: {vm:?}"
            );
        } else {
            assert!(tree.is_err(), "the tree refuses the missing field");
            assert!(
                tree.expect_err("refuses above")
                    .to_string()
                    .contains("bogus"),
                "the refusal names the field"
            );
        }
    }
    let tree = run_with_result(None, &modules, &bars);
    assert!(tree.is_ok(), "the tree runs field reads");
}

#[test]
fn dotted_field_writes_agree() {
    // A dotted rebinding takes the instance out of its home, mutates
    // the field, and writes the copy back — identically on both
    // selections, with reads pairing through the shared table.
    let bars = ramp_bars(10);
    let modules = link_script(
        "type Box2\n int n = 0\nid(o) => o\nw = Box2.new()\nw.n := 3\nplot((id(w)).n, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("dotted writes link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on dotted writes"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|cell| *cell == Some(3.0)),
        "every bar publishes the written field: {vm:?}"
    );
}

#[test]
fn alias_field_writes_agree() {
    // Receiver-alias writes hit the live slot in place: the mutation is
    // visible to nested reads without any take, on both selections.
    let bars = ramp_bars(10);
    let modules = link_script(
        "type Box2\n int n = 0\nmethod touch(Box2 level, int d) =>\n level.n := d\n level.n\nw = Box2.new()\nplot(w.touch(7), title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("alias writes link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on alias writes"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|cell| *cell == Some(7.0)),
        "every bar publishes the alias write: {vm:?}"
    );
}

#[test]
fn unknown_fields_on_write_refuse_on_both() {
    // Writing a missing field refuses naming the full path after
    // restoring the instance — out of the loop with the tree-walk's
    // exact wording.
    let bars = ramp_bars(5);
    for script in [
        "type Box2\n int n = 0\nw = Box2.new()\nw.bogus := 1\nplot(0, title=\"V\")",
        "type Box2\n int n = 0\nmethod touch(Box2 level) =>\n level.bogus := 1\n level.n\nw = Box2.new()\nplot(w.touch(), title=\"V\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("refusing writes link");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "unknown fields refuse identically: {script}"
        );
        assert!(tree.is_err(), "the tree refuses: {script}");
        assert!(
            tree.expect_err("refuses above")
                .to_string()
                .contains("bogus"),
            "the refusal names the field: {script}"
        );
    }
}

#[test]
fn nested_field_receivers_match_on_both_engines() {
    // A method called through a nested field object (`o.box.bump()`) takes
    // the inner instance out of its root home, runs the body, and writes
    // the mutated chain back — one home model, both engines.
    let bars = ramp_bars(10);
    let modules = link_script(
        "type Inner\n int n = 1\nmethod bump(Inner this) =>\n this.n := this.n + 1\n this.n\ntype Outer\n Inner box = na\no = Outer.new()\no.box := Inner.new()\nplot(o.box.bump(), title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("nested receivers link");
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "nested field receivers agree"
    );
    vm.expect("nested field receiver evaluates");
}

#[test]
fn map_entry_write_back_matches_once() {
    // A variable bound from `map.get` aliases the entry: a method call or
    // field write through the variable persists into the map exactly once.
    let bars = ramp_bars(10);
    let modules = link_script(
        "type Box2\n int n = 1\nmethod bump(Box2 this) =>\n this.n := this.n + 1\n this.n\nm = map.new<int, Box2>()\nmap.put(m, 1, Box2.new())\nheld = map.get(m, 1)\nfirst = held.bump()\nplot(first + map.get(m, 1).n, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("map-entry write-backs link");
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "map-entry write-backs agree"
    );
    let outcome = tree.expect("tree map-entry write-back evaluates");
    assert_eq!(
        plotted_floats(&outcome, "V").first().copied(),
        Some(Some(4.0)),
        "the call value and the stored entry both observe one bump: {outcome:?}"
    );
}

#[test]
fn take_miss_restores_the_home_before_refusing() {
    // A nested path that holds no object restores the root before the
    // refusal: both engines name the same method and the same wording.
    let bars = ramp_bars(5);
    let modules = link_script(
        "type Inner\n int n = 1\ntype Outer\n Inner box = na\no = Outer.new()\nplot(o.box.missing(), title=\"V\")",
    );
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "nested take misses agree"
    );
    assert!(tree.is_err(), "the tree refuses the missing receiver");
}

#[test]
fn object_alias_shadowing_prefers_the_live_slot() {
    // A receiver alias shadows a same-named global inside the body: the
    // live slot is written, the global stands untouched.
    let bars = ramp_bars(10);
    let modules = link_script(
        "type Box2\n int n = 1\ng = Box2.new()\nf(g) =>\n g.n := 9\n g.n\nplot(f(Box2.new()) + g.n, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("alias shadowing links");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "alias shadowing agrees"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|value| *value == Some(10.0)),
        "the live slot wins and the global is untouched: {vm:?}"
    );
}

#[test]
fn object_handle_shared_across_calls() {
    // Object values ride by handle until the first mutation separates
    // them: two bindings of one instance share the resident table, and a
    // write through one binding copies-on-write, exactly like the shared
    // `FieldTable` contract. Both engines pin that shape.
    let bars = ramp_bars(10);
    let modules = link_script(
        "type Box2\n int n = 1\nid(o) => o\nw = Box2.new()\nv = id(w)\nv.n := 5\nplot(v.n * 10 + w.n, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("shared handles link");
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "shared object handles agree"
    );
    let vm = vm.expect("shared handle evaluates");
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|value| *value == Some(51.0)),
        "the written copy and the untouched binding both read: {vm:?}"
    );
}

#[test]
fn nested_field_paths_keep_their_fallback() {
    // Multi-dot write paths keep the tree-walk's exact refusal through
    // the whole-module fallback on both selections.
    let bars = ramp_bars(5);
    let modules = link_script(
        "type Box2\n int n = 0\ntype Wrap\n Box2 box = na\nw = Wrap.new()\nw.box := Box2.new()\nw.box.n := 3\nplot(0, title=\"V\")",
    );
    let program = compile_case(&modules);
    assert!(
        matches!(
            link_case(&program, &modules),
            Err(crate::pine::bytecode_exec::LinkError::Fallback { .. })
        ),
        "nested writes keep their fallback"
    );
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "nested writes agree through fallback"
    );
}

#[test]
fn shadowing_globals_beat_in_function_vars() {
    // A shared global shadows an in-function `var` of the same name: the
    // tree-walk serves its single namespace first, so both selections
    // publish the global on every bar.
    let bars = ramp_bars(10);
    let modules = link_script("g = 0.0\nf() =>\n var g = 1\n g\nplot(f(), title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("shadowed vars link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on shadowing"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|cell| *cell == Some(0.0)),
        "the global wins: {vm:?}"
    );
}

#[test]
fn construction_and_field_reads_agree() {
    // A constructed instance flows through a call and back: the field
    // read pairs through the shared table on both selections.
    let bars = ramp_bars(10);
    let modules = link_script(
        "type Box2\n int n = 5\nid(o) => o\nw = Box2.new()\nplot((id(w)).n, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("construction links");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on construction"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|cell| *cell == Some(5.0)),
        "every bar publishes the field: {vm:?}"
    );
}

#[test]
fn method_calls_agree() {
    // Variable-held calls take the instance, run the lowered body with
    // `this` bound, and write the mutated copy back; rvalue calls drop
    // the mutation and keep only the return — identically on both.
    let bars = ramp_bars(10);
    let modules = link_script(
        "type Box2\n int n = 0\nmethod bump(Box2 this, int d) =>\n this.n := d\n this.n\nw = Box2.new()\na = w.bump(5)\nb = (Box2.new()).bump(4)\nplot(a + b, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("method calls link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on method calls"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|cell| *cell == Some(9.0)),
        "write-back plus rvalue sum: {vm:?}"
    );
}

#[test]
fn unknown_methods_refuse_on_both() {
    // Dispatch on a missing method refuses naming the type and the
    // method, out of the loop exactly like the tree-walk.
    let bars = ramp_bars(5);
    for script in [
        "type Box2\n int n = 0\nw = Box2.new()\nplot(w.bogus(), title=\"V\")",
        "type Box2\n int n = 0\nplot((Box2.new()).bogus(), title=\"V\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("unknown methods link");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "unknown methods refuse identically: {script}"
        );
        assert!(tree.is_err(), "the tree refuses: {script}");
        assert!(
            tree.expect_err("refuses above")
                .to_string()
                .contains("Box2.bogus"),
            "the refusal names type and method: {script}"
        );
    }
}

#[test]
fn this_outside_methods_refuses_on_both() {
    // `this` with no enclosing method refuses on both selections.
    let bars = ramp_bars(5);
    let modules = link_script("plot(this, title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("bare this links");
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "bare this refuses identically"
    );
    assert!(tree.is_err(), "the tree refuses bare this");
    assert!(
        tree.expect_err("refuses above")
            .to_string()
            .contains("only bound inside a method"),
        "the refusal names the rule"
    );
}

#[test]
fn nested_method_calls_agree() {
    // A method calling its sibling through `this` observes the live
    // receiver: both increments persist in order on both selections.
    let bars = ramp_bars(10);
    let modules = link_script(
        "type Box2\n int n = 0\nmethod add(Box2 this, int d) =>\n this.n := this.n + d\n this.n\nmethod bump2(Box2 this, int d) =>\n this.add(d)\n this.add(d * 10)\nw = Box2.new()\nplot(w.bump2(1), title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("nested methods link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on nesting"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|cell| *cell == Some(11.0)),
        "one plus ten persists in order: {vm:?}"
    );
}

#[test]
fn method_dispatch_follows_receiver_type_per_bar() {
    // Same method name, two receiver types, chosen per bar: the runtime
    // type test dispatches each bar to its own body on both selections.
    let bars = ramp_bars(10);
    let modules = link_script(
        "type A\n int n = 1\nmethod val(A this) =>\n this.n\ntype B\n int n = 2\nmethod val(B this) =>\n this.n * 10\nmk(flag) =>\n r = A.new()\n if flag\n  r := B.new()\n r\nplot((mk(bar_index % 2 == 0)).val(), title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("typed dispatch links");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on typed dispatch"
    );
    let got = plotted_floats(&vm, "V");
    let want: Vec<Option<f64>> = (0..10)
        .map(|bar| Some(if bar % 2 == 0 { 20.0 } else { 1.0 }))
        .collect();
    assert_eq!(got, want, "each bar dispatches its type: {vm:?}");
}

#[test]
fn session_time_reads_bar_time_on_both() {
    // `time()` of the chart period is a pure read of the bar time leg:
    // both selections publish the same millisecond stamps.
    let bars = ramp_bars(10);
    for script in [
        "plot(time(\"1\"), title=\"V\")",
        "plot(time(\"1\", \"0930-1600\"), title=\"V\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("session time links");
        let tree = run_with(None, &modules, &bars);
        let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            canonical_form(&vm),
            canonical_form(&tree),
            "selections agree on session time: {script}"
        );
    }
    let modules = link_script("plot(time(\"1\"), title=\"V\")");
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    let got = plotted_floats(&vm, "V");
    let want: Vec<Option<f64>> = (0..10)
        .map(|bar| Some((1_700_000_000 + i64::from(bar) * 60) as f64))
        .collect();
    assert_eq!(got, want, "every bar publishes its stamp: {vm:?}");
}

#[test]
fn wall_clock_parts_agree_on_both() {
    // `hour()`/`minute()` are pure arithmetic on the stamp: both
    // selections agree in UTC and in the mapped Eastern zone.
    let bars = ramp_bars(10);
    for script in [
        "plot(hour(time) * 100 + minute(time), title=\"V\")",
        "plot(hour(time, \"America/New_York\") * 100 + minute(time, \"America/New_York\"), title=\"V\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("wall-clock parts link");
        let tree = run_with(None, &modules, &bars);
        let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            canonical_form(&vm),
            canonical_form(&tree),
            "selections agree on wall-clock parts: {script}"
        );
    }
    // 1_700_000_000 is 2023-11-14 22:13:20 UTC; bars tick per minute.
    let modules = link_script("plot(hour(time) * 100 + minute(time), title=\"V\")");
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    let got = plotted_floats(&vm, "V");
    let want: Vec<Option<f64>> = (13..23)
        .map(|minute| Some(2200.0 + f64::from(minute)))
        .collect();
    assert_eq!(got, want, "UTC parts publish per bar: {vm:?}");
}

#[test]
fn fixnan_carries_last_present_value() {
    // `fixnan()` replays the last present value at its own call site:
    // both selections fill the same gaps from the same memory.
    let bars = ramp_bars(10);
    let modules =
        link_script("v = bar_index > 2 ? close : na\nf = fixnan(v)\nplot(f, title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("fixnan links");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on fixnan replay"
    );
    let got = plotted_floats(&vm, "V");
    let mut want = vec![None, None, None];
    want.extend((3..10).map(|bar| Some(100.0 + f64::from(bar) * 0.5)));
    assert_eq!(got, want, "gaps fill from the last present bar: {vm:?}");
}

#[test]
fn fixnan_sites_split_by_argument_shape() {
    // Per-site memory keys on the argument shape: two call sites with
    // different arguments keep separate memories on both selections.
    let bars = ramp_bars(10);
    let modules = link_script(
        "a = fixnan(bar_index > 2 ? close : na)\nb = fixnan(bar_index > 5 ? open : na)\nplot(a + b, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("split sites link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on split sites"
    );
    let got = plotted_floats(&vm, "V");
    let mut want = vec![None, None, None, None, None, None];
    want.extend((6..10).map(|bar| Some(200.0 + f64::from(bar) - 0.25)));
    assert_eq!(got, want, "each site replays its own past: {vm:?}");
}

#[test]
fn session_time_refusals_match_on_both() {
    // Session-time guards refuse naming the rule out of the loop with
    // the tree-walk's exact wording.
    let bars = ramp_bars(5);
    for (script, wording) in [
        (
            "plot(time(\"D\"), title=\"V\")",
            "supports only the chart period",
        ),
        (
            "plot(time(), title=\"V\")",
            "takes a timeframe and an optional session",
        ),
        ("plot(time(1), title=\"V\")", "takes a timeframe string"),
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("time guards link");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "time refusals match: {script}"
        );
        assert!(tree.is_err(), "the tree refuses: {script}");
        assert!(
            tree.expect_err("refuses above")
                .to_string()
                .contains(wording),
            "the refusal names the rule: {script}"
        );
    }
}

#[test]
fn wall_clock_refusals_match_on_both() {
    // Wall-clock guards refuse naming the row out of the loop with the
    // tree-walk's exact wording.
    let bars = ramp_bars(5);
    for (script, wording) in [
        ("plot(hour(\"x\"), title=\"V\")", "takes a timestamp"),
        (
            "plot(hour(time, \"Mars/Olympus\"), title=\"V\")",
            "not mapped headlessly",
        ),
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("wall-clock guards link");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "wall-clock refusals match: {script}"
        );
        assert!(tree.is_err(), "the tree refuses: {script}");
        assert!(
            tree.expect_err("refuses above")
                .to_string()
                .contains(wording),
            "the refusal names the rule: {script}"
        );
    }
}

#[test]
fn fixnan_arity_refuses_on_both() {
    // `fixnan` takes exactly one argument on either engine.
    let bars = ramp_bars(5);
    for script in [
        "plot(fixnan(), title=\"V\")",
        "plot(fixnan(close, open), title=\"V\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("fixnan arity links");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "fixnan arity matches: {script}"
        );
        assert!(tree.is_err(), "the tree refuses: {script}");
        assert!(
            tree.expect_err("refuses above")
                .to_string()
                .contains("takes exactly one argument"),
            "the refusal names the arity: {script}"
        );
    }
}

#[test]
fn broker_dialogue_lowers_to_the_shared_capture() {
    // Broker calls observe without a venue: the loop records into the run's
    // shared strategy sink, so both selections run the same dialogue and
    // agree exactly (records included).
    let bars = ramp_bars(10);
    for script in [
        "strategy.entry(\"E\", strategy.long)\nplot(close, title=\"V\")",
        "q = strategy.position_size\nplot(close + q, title=\"V\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("broker dialogue links");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "broker dialogue agrees: {script}"
        );
        assert!(tree.is_ok(), "the tree runs broker dialogue: {script}");
    }
}

#[test]
fn live_fetches_refuse_on_both() {
    // Live-fetch namespaces refuse headlessly without injections: the
    // loop serves the injected surface and refuses with the tree-walk's
    // exact wording when no tape is registered.
    let bars = ramp_bars(5);
    for script in [
        "v = request.security(syminfo.tickerid, \"D\", close)\nplot(v, title=\"V\")",
        "v = request.security_lower_tf(syminfo.tickerid, \"1\", [time, close])\nplot(close, title=\"V\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("live-fetch shapes link");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "live fetches refuse identically: {script}"
        );
        assert!(tree.is_err(), "the tree refuses headlessly: {script}");
        assert!(
            tree.expect_err("refuses above")
                .to_string()
                .contains("refused headlessly"),
            "the refusal names the headless rule: {script}"
        );
    }
}

#[test]
fn drawings_lower_to_the_shared_registry() {
    // Drawing lifetimes ride the run's shared registry: creation, mutation,
    // reads, and deletion lower, and both selections replay one event
    // stream.
    let bars = ramp_bars(10);
    for script in [
        "l = line.new(bar_index, low, bar_index, high)\nplot(close, title=\"V\")",
        "b = box.new(bar_index, low, bar_index, high)\nplot(close, title=\"V\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("drawings link");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "drawings agree: {script}"
        );
    }
}

#[test]
fn dynamic_plot_titles_match_tree() {
    // A title is an expression: the loop evaluates it per bar and names the
    // capture at first sight, exactly like the tree-walk. A non-string (and
    // a missing title) falls back to the run-time `plot:{n}` default.
    let bars = ramp_bars(10);
    for (script, title) in [
        ("t = \"T\"\nplot(close, title=t)", "T"),
        ("plot(close, \"Pos\")", "Pos"),
        ("plot(close, \"Pos\", title=\"Named\")", "Named"),
        ("plot(close)", "plot:0"),
        ("t = 5\nplot(close, title=t)", "plot:0"),
        (
            "t = close > open ? \"Up\" : \"Down\"\nplot(close, title=t)",
            "Up",
        ),
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("plot titles link");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "plot title agrees: {script}"
        );
        let vm = vm.expect("compiled plot run evaluates");
        assert_eq!(
            vm.plots.first().expect("one plot capture").title,
            title,
            "the capture title is the evaluated value: {script}"
        );
    }
}

#[test]
fn computed_input_defaults_match_first_call_wins() {
    // A computed default names its input from its run-time value and then
    // reads the one first-call-wins store: later bars observe the declared
    // value, never the live expression. Both selections share that store.
    let bars = ramp_bars(10);
    for script in [
        "v = input(close + 1.0)\nplot(v, title=\"V\")",
        "v = input(close > open)\nplot(close, title=\"V\")",
        "a = input(close + 1.0)\nb = input(close + 1.0)\nplot(a + b, title=\"V\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("computed input links");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "computed input agrees: {script}"
        );
    }
    let modules = link_script("v = input(close + 1.0)\nplot(v, title=\"V\")");
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    let plotted = plotted_floats(&vm, "V");
    assert_eq!(
        plotted.first().copied(),
        Some(Some(101.0)),
        "the first call declares the bar-zero value: {vm:?}"
    );
    assert!(
        plotted.iter().all(|value| *value == Some(101.0)),
        "later bars read the declared value, never the live series: {vm:?}"
    );
}

#[test]
fn input_sites_split_by_name_and_args() {
    // Input identity is the call site (textual arguments): two syntactically
    // different computed defaults declare twice, one repeated spelling
    // declares once and reads one value.
    let bars = ramp_bars(8);
    let split =
        link_script("a = input(close + 1.0)\nb = input(close + 2.0)\nplot(a - b, title=\"V\")");
    let vm = run_with(Some(BytecodeMode::Compiled), &split, &bars);
    let tree = run_with(None, &split, &bars);
    assert_eq!(canonical_form(&vm), canonical_form(&tree), "sites agree");
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|value| *value == Some(-1.0)),
        "two sites keep their own defaults: {vm:?}"
    );

    let same = link_script("f() => input(close + 1.0)\na = f()\nb = f()\nplot(a - b, title=\"V\")");
    let vm = run_with(Some(BytecodeMode::Compiled), &same, &bars);
    let tree = run_with(None, &same, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "repeated spelling agrees"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|value| *value == Some(0.0)),
        "one site reads one declared value: {vm:?}"
    );
}

#[test]
fn runtime_refusals_match_tree_timing() {
    // Deterministic refusals are instructions now: they fire only when
    // execution reaches them, so a dead branch stays silent and a live one
    // refuses with the tree's exact message at the same point.
    let bars = ramp_bars(5);
    for (script, refusal) in [
        ("if close < 0.0\n foo(1)\nplot(close, title=\"V\")", None),
        (
            "foo(1)\nplot(close, title=\"V\")",
            Some("unknown function 'foo'"),
        ),
        ("if close < 0.0\n na(1, 2)\nplot(close, title=\"V\")", None),
        (
            "na(1, 2)\nplot(close, title=\"V\")",
            Some("'na' takes zero or one argument"),
        ),
        (
            "nz(1, 2, 3)\nplot(close, title=\"V\")",
            Some("'nz' takes one or two arguments"),
        ),
        ("plot()", Some("'plot' takes a series to draw")),
        (
            "v = input()\nplot(v, title=\"V\")",
            Some("'input' takes a default value"),
        ),
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("deterministic refusal links");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "refusal timing agrees: {script}"
        );
        match refusal {
            Some(text) => {
                let error = tree.expect_err("the tree refuses above").to_string();
                assert!(error.contains(text), "the refusal names {text:?}: {error}");
            }
            None => {
                tree.expect("the dead branch stays silent");
            }
        }
    }
}

#[test]
#[allow(clippy::float_cmp)]
fn unknown_callee_refuses_only_when_executed() {
    // Coverage gates are exact by correctness (split-brain equality),
    // never approximate: `== 1.0` below is the gate, not a measurement.
    // The unknown callee no longer evicts the module: the compiled loop
    // carries the refusal and runs normally while the branch never fires.
    let bars = ramp_bars(6);
    let modules =
        link_script("if close < 0.0\n missingFunction(1)\nplot(close * 2.0, title=\"V\")");
    let program = compile_case(&modules);
    let linked = link_case(&program, &modules).expect("unknown callee links");
    assert_eq!(linked.coverage(), 1.0, "the module stays compiled");
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    let tree = run_with(None, &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "dead branch agrees"
    );
}

#[test]
fn remaining_builtin_statement_forms_match() {
    // Statement-position builtins that only exist for their effect: every
    // form evaluates its arguments, drops the value, and agrees.
    let bars = ramp_bars(8);
    for script in [
        "a = array.new_float(3, 1.0)\narray.push(a, 2.0)\narray.shift(a)\nplot(array.size(a), title=\"V\")",
        "m = map.new<int, int>()\nmap.put(m, 1, 2)\nmap.remove(m, 1)\nplot(map.size(m), title=\"V\")",
        "str.tostring(close)\nmath.max(close, open)\nplot(close, title=\"V\")",
        "rem = close % 3\nplot(rem, title=\"V\")",
    ] {
        let modules = link_script(script);
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "statement form agrees: {script}"
        );
    }
}

#[test]
fn missing_input_default_refuses_on_both() {
    // A bare `input()` with no default refuses on either engine with
    // the tree-walk's exact wording.
    let bars = ramp_bars(5);
    let modules = link_script("v = input()\nplot(v, title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("missing input default links");
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "missing input defaults refuse identically"
    );
    assert!(tree.is_err(), "the tree refuses the missing default");
}

#[test]
fn capture_shapes_refuse_on_both() {
    // A misshapen capture refuses at run time on either engine: the
    // loop keeps the typed fallback, so both selections run the
    // tree-walk and refuse with the same string.
    let bars = ramp_bars(5);
    let modules = link_script("fill(close, open)\nplot(close, title=\"V\")");
    let program = compile_case(&modules);
    assert!(
        matches!(
            link_case(&program, &modules),
            Err(crate::pine::bytecode_exec::LinkError::Fallback { .. })
        ),
        "capture shapes keep their fallback"
    );
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "capture shapes refuse identically"
    );
    assert!(tree.is_err(), "the tree refuses the misshapen capture");
}

#[test]
fn statement_collection_mutations_lower_and_persist() {
    // Statement-position `array.shift`/`map.remove` persist the shrunken
    // collection through the one write-back path while the call's own
    // value is dropped: the loop links, and both selections agree on the
    // mutated binding and every later read of it.
    let bars = ramp_bars(10);
    for script in [
        "a = array.from(1.0, 2.0, 3.0)\narray.shift(a)\nplot(array.size(a) + array.get(a, 0), title=\"V\")",
        "m = map.new<int, int>()\nmap.put(m, 1, 7)\nmap.remove(m, 1)\nplot(map.size(m), title=\"V\")",
        "f(a) =>\n    array.shift(a)\n    array.size(a)\nb = array.from(1.0, 2.0, 3.0)\nplot(f(b) + array.size(b), title=\"V\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("statement mutations link");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "statement mutations agree: {script}"
        );
        vm.expect("compiled statement mutation evaluates");
    }
}

#[test]
fn statement_shift_writes_back_once() {
    // The shrunken array persists exactly once: the loop's binding sees
    // two elements after one shift, and the head is dropped rather than
    // stored back over the array.
    let bars = ramp_bars(10);
    let modules = link_script(
        "a = array.from(1.0, 2.0, 3.0)\narray.shift(a)\nplot(array.size(a), title=\"S\")",
    );
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert!(
        plotted_floats(&vm, "S")
            .iter()
            .all(|value| *value == Some(2.0)),
        "one shift leaves two elements: {vm:?}"
    );
    let tree = run_with(None, &modules, &bars);
    assert_eq!(
        plotted_floats(&vm, "S"),
        plotted_floats(&tree, "S"),
        "one write-back on both engines"
    );
}

#[test]
fn alert_dialogue_evaluates_and_drops() {
    // Alerts evaluate and drop without a delivery path: both forms lower,
    // and both selections agree.
    let bars = ramp_bars(10);
    for script in [
        "alert(\"fired\", alert.freq_once_per_bar)\nplot(close, title=\"V\")",
        "x = alert(\"fired\", alert.freq_once_per_bar)\nplot(x, title=\"V\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("alert dialogue links");
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "alert dialogue agrees: {script}"
        );
        assert!(tree.is_ok(), "the tree runs alert dialogue: {script}");
    }
}

#[test]
fn shared_input_and_timeframe_bridges_agree() {
    // `input.*` (single first-call-wins store) and `timeframe.*` already
    // compose the shared bridges: both selections agree, values and
    // refusals alike.
    let bars = ramp_bars(10);
    let modules = link_script(
        "n = input.int(5, \"Length\")\ns = timeframe.in_seconds(\"D\")\nplot(close + n + s, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("shared bridges link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "selections agree on shared bridges"
    );
    let got = plotted_floats(&vm, "V");
    let want: Vec<Option<f64>> = (0..10)
        .map(|bar| Some(86505.0 + f64::from(bar) * 0.5))
        .collect();
    assert_eq!(got, want, "defaults and periods compose: {vm:?}");
}

#[test]
fn shared_refusal_wordings_match_on_both_engines() {
    // Every shared refusal constructor names its string identically out
    // of both engines: linking shapes refuse at run time, fallback
    // shapes through the tree-walk — either way byte-identical,
    // refusal strings included.
    struct Case {
        script: &'static str,
        wording: &'static str,
        links: bool,
    }
    let bars = ramp_bars(5);
    for case in [
        Case {
            script: "plot(close[-1], title=\"V\")",
            wording: "negative history offset -1",
            links: true,
        },
        Case {
            script: "v = array.new_float(3)\nplot(v[5], title=\"V\")",
            wording: "array index 5 out of bounds",
            links: true,
        },
        Case {
            script: "v = array.new_float(3)\ni = 0 - 1\nplot(v[i], title=\"V\")",
            wording: "negative array index -1",
            links: true,
        },
        Case {
            script: "f() =>\n q\n var q = 1\n q\nplot(f(), title=\"V\")",
            wording: "undefined variable 'q'",
            links: true,
        },
        Case {
            script: "[a, b] = array.new_float(1)\nplot(a, title=\"V\")",
            wording: "destructuring arity 1 does not match 2 targets",
            links: true,
        },
        Case {
            script: "[a] = 1.0\nplot(a, title=\"V\")",
            wording: "destructuring binds array values, got 1",
            links: true,
        },
        Case {
            script: "f() =>\n q := 1.0\n q\nplot(f(), title=\"V\")",
            wording: "':=' targets undeclared variable 'q'",
            links: true,
        },
        Case {
            script: "v = 1.0\nfill(v, plot(close, title=\"Q\"))\nplot(close, title=\"V\")",
            wording: "no plot tag",
            links: true,
        },
        Case {
            script: "plot((1).n, title=\"V\")",
            wording: "needs an object receiver",
            links: true,
        },
        Case {
            script: "f() =>\n na\nplot((f()).n, title=\"V\")",
            wording: "read on na",
            links: true,
        },
        Case {
            script: "type Box2\n int n = 0\nw = Box2.new()\nw.bogus := 1\nplot(0, title=\"V\")",
            wording: "unknown field 'w.bogus'",
            links: true,
        },
        Case {
            script: "type Box2\n int n = 0\nw = Box2.new()\nplot(w.bogus(), title=\"V\")",
            wording: "unknown method 'Box2.bogus'",
            links: true,
        },
        Case {
            script: "plot((1).bump(), title=\"V\")",
            wording: "needs an object receiver",
            links: true,
        },
        Case {
            script: "f() =>\n na\nplot((f()).bump(), title=\"V\")",
            wording: "called on na",
            links: true,
        },
        Case {
            script: "type Box2\n int n = 0\nmethod bump(Box2 this, int d) =>\n this.n\na = Box2.new()\nplot(a.bump(), title=\"V\")",
            wording: "missing argument 'd'",
            links: true,
        },
        Case {
            script: "type Box2\n int n = 0\nmethod bump(Box2 this, int d) =>\n this.n\na = Box2.new()\nplot(a.bump(1, 2), title=\"V\")",
            wording: "too many positional arguments",
            links: true,
        },
        Case {
            script: "type Box2\n int n = 0\nmethod bump(Box2 this, int d) =>\n this.n\na = Box2.new()\nplot(a.bump(1, x=2), title=\"V\")",
            wording: "unknown argument 'x'",
            links: true,
        },
        Case {
            script: "type Box2\n int n = 0\nmethod m() =>\n 1\nw = Box2.new()\nplot(w.m(), title=\"V\")",
            wording: "needs a typed receiver",
            links: false,
        },
        Case {
            script: "plot(this, title=\"V\")",
            wording: "only bound inside a method",
            links: true,
        },
        Case {
            script: "type Box2\n int n = 0\nmethod m(Box2 this) =>\n this.bogus\nw = Box2.new()\nplot(w.m(), title=\"V\")",
            wording: "unknown field 'this.bogus'",
            links: true,
        },
        Case {
            script: "type Box2\n int n = 0\nmethod m(Box2 this) =>\n this := 1\n this.n\nw = Box2.new()\nplot(w.m(), title=\"V\")",
            wording: "cannot rebind 'this'",
            links: false,
        },
        Case {
            script: "plot(foo(1.0), title=\"V\")",
            wording: "unknown function 'foo'",
            links: true,
        },
        Case {
            script: "f(x) => f(x)\nplot(f(1), title=\"V\")",
            wording: "call depth exceeds",
            links: true,
        },
        Case {
            script: "plot(na(close, close), title=\"V\")",
            wording: "takes zero or one argument",
            links: true,
        },
    ] {
        let modules = link_script(case.script);
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).is_ok();
        assert_eq!(
            linked, case.links,
            "linkage pins the shape: {}",
            case.script
        );
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "refusals match: {}",
            case.script
        );
        assert!(tree.is_err(), "the tree refuses: {}", case.script);
        assert!(
            tree.expect_err("refuses above")
                .to_string()
                .contains(case.wording),
            "the refusal names its reason: {}",
            case.script
        );
    }
}

#[test]
fn headless_strategy_events_match_tree_order() {
    // The harness records the broker dialogue as ordered observations:
    // declaration capital first, then every entry/exit/close with the bar
    // that committed it. The loop shares one sink with the tree-walk, so
    // the event stream is byte-identical in either selection.
    let bars = ramp_bars(10);
    let modules = link_script(
        "strategy(\"S\", initial_capital=100000)\nstrategy.entry(\"L\", strategy.long)\nif close > open\n strategy.exit(\"X\", from_entry=\"L\", profit=10)\nif bar_index == 3\n strategy.close_all()\nplot(close, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("headless strategy events link");
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprinted_strategy_calls(&vm),
        fingerprinted_strategy_calls(&tree),
        "strategy events agree"
    );
    let vm = vm.expect("compiled strategy run evaluates");
    let tree = tree.expect("tree strategy run evaluates");
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "full outcomes agree"
    );
    let first = vm.strategy_calls.first().expect("the entry is recorded");
    assert_eq!(first.bar, 0, "the entry commits on the first bar");
    assert_eq!(first.kind, "entry");
    assert_eq!(first.id, "L");
    assert_eq!(first.direction.as_deref(), Some("long"));
    assert!(
        vm.strategy_calls
            .iter()
            .any(|call| call.kind == "close_all" && call.bar == 3),
        "the conditional close commits on its own bar: {:?}",
        vm.strategy_calls
    );
}

#[test]
fn strategy_position_reads_match_after_entries() {
    // The harness models an empty book: counts read zero, realized money
    // reads zero, price reads `na`, and equity reads the declared capital.
    // Both selections answer from the one shared strategy state.
    let bars = ramp_bars(10);
    let modules = link_script(
        "strategy(\"S\", initial_capital=50000)\nstrategy.entry(\"L\", strategy.long, qty=2)\nplot(strategy.position_size + strategy.equity + strategy.closedtrades + strategy.opentrades + strategy.netprofit + strategy.grossprofit, title=\"V\")\nplot(strategy.position_avg_price, title=\"A\")\nplot(strategy.closedtrades.size(), title=\"C\")\nplot(strategy.initial_capital, title=\"K\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("strategy reads link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "strategy reads agree"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|value| *value == Some(50_000.0)),
        "empty-book counts and equity sum to the declared capital: {vm:?}"
    );
    assert!(
        plotted_floats(&vm, "A").iter().all(Option::is_none),
        "a price read without a position is na: {vm:?}"
    );
    assert!(
        plotted_floats(&vm, "C")
            .iter()
            .all(|value| *value == Some(0.0)),
        "closed-trade size reads zero: {vm:?}"
    );
    assert!(
        plotted_floats(&vm, "K")
            .iter()
            .all(|value| *value == Some(50_000.0)),
        "the declaration capital reads back: {vm:?}"
    );
}

#[test]
fn strategy_calls_never_reach_a_venue() {
    // A strategy call is an observation, never an order: the loop records
    // the same event the tree records and the book stays empty afterwards.
    // Nothing in the run can submit, fill, or track a position.
    let bars = ramp_bars(10);
    let modules = link_script(
        "strategy(\"S\", initial_capital=10000)\nstrategy.entry(\"L\", strategy.long, qty=1)\nplot(strategy.position_size, title=\"P\")\nplot(strategy.opentrades, title=\"O\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("observation-only calls link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "observation streams agree"
    );
    for call in &vm.strategy_calls {
        assert!(
            matches!(call.kind, "entry" | "exit" | "close" | "close_all"),
            "only broker dialogue is recorded, got {}",
            call.kind
        );
    }
    assert!(
        plotted_floats(&vm, "P")
            .iter()
            .all(|value| *value == Some(0.0)),
        "no position is ever tracked: {vm:?}"
    );
    assert!(
        plotted_floats(&vm, "O")
            .iter()
            .all(|value| *value == Some(0.0)),
        "no trade is ever opened: {vm:?}"
    );
}

#[test]
fn live_strategy_dialogue_refuses_explicitly_on_both() {
    // Live-only broker dialogue (`strategy.order`) is outside the headless
    // harness matrix. The loop refuses with the tree-walk's exact message
    // and records no event — it never guesses at a venue submission.
    let bars = ramp_bars(5);
    let modules = link_script(
        "strategy(\"S\", initial_capital=1000)\nstrategy.order(\"L\", strategy.long)\nplot(close, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("live dialogue links to an explicit refusal");
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "live dialogue refuses identically"
    );
    let refusal = tree
        .expect_err("the tree refuses live-only dialogue")
        .to_string();
    assert!(
        refusal.contains("strategy.order is outside the harness matrix"),
        "the refusal names the matrix: {refusal}"
    );
}

#[test]
fn strategy_initial_capital_reads_before_declaration_refuses() {
    // Reading the declaration's capital before it runs refuses naming the
    // read; once the declaration executes, the same read answers. Both
    // selections share one state, so both edges agree exactly.
    let bars = ramp_bars(5);
    let early = link_script(
        "early() =>\n strategy.equity\nv = early()\nstrategy(\"S\", initial_capital=1000)\nplot(v, title=\"V\")",
    );
    let tree = run_with_result(None, &early, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &early, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "pre-declaration reads agree"
    );
    let refusal = tree
        .expect_err("the tree refuses the pre-declaration read")
        .to_string();
    assert!(
        refusal.contains("strategy.equity read before any strategy declaration ran"),
        "the refusal names the read: {refusal}"
    );

    let declared = link_script(
        "strategy(\"S\", initial_capital=1234.5)\nplot(strategy.initial_capital, title=\"C\")",
    );
    let program = compile_case(&declared);
    link_case(&program, &declared).expect("declared capital links");
    let tree = run_with(None, &declared, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &declared, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "declared capital agrees"
    );
    assert!(
        plotted_floats(&vm, "C")
            .iter()
            .all(|value| *value == Some(1234.5)),
        "the declared capital reads back: {vm:?}"
    );
}

#[test]
fn drawing_events_match_tree_order() {
    // Drawing calls are recorded data: creation mints an object and emits a
    // `Created` row, setters/cell/merge emit `Updated` rows, and deletion
    // emits `Deleted`. One shared registry serves both engines, so the rows
    // — and their bar numbers — are byte-identical.
    let bars = ramp_bars(10);
    let modules = link_script(
        "l = label.new(bar_index, close, \"L\", color=color.red)\nlabel.set_x(l, bar_index + 1)\nb = box.new(bar_index, close, bar_index + 1, close + 1)\nbox.set_right(b, bar_index + 2)\nt = table.new(position.top_right, 2, 2)\ntable.cell(t, 0, 0, \"x\")\ntable.merge_cells(t, 0, 0, 1, 1)\nln = line.new(bar_index, close, bar_index + 1, close + 1)\nline.set_xy1(ln, bar_index + 2, close + 2)\nplot(close, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("drawing calls link");
    let tree = run_with_result(None, &modules, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "drawing events agree"
    );
    let vm = vm.expect("drawing run evaluates");
    let kinds: Vec<&str> = vm
        .draw_events
        .iter()
        .map(|event| match event.kind {
            crate::pine::eval::DrawingEventKind::Created => "created",
            crate::pine::eval::DrawingEventKind::Updated => "updated",
            crate::pine::eval::DrawingEventKind::Deleted => "deleted",
        })
        .collect();
    let per_bar = [
        "created", "updated", "created", "updated", "created", "updated", "updated", "created",
        "updated",
    ];
    assert_eq!(
        kinds.len(),
        per_bar.len() * bars.len(),
        "every bar replays the same block of rows: {:?}",
        vm.draw_events
    );
    for block in kinds.chunks(per_bar.len()) {
        assert_eq!(
            block, per_bar,
            "every creation and mutation records one row: {:?}",
            vm.draw_events
        );
    }
    assert_eq!(
        vm.draw_events.first().expect("the label row").key,
        "label#0",
        "handle identity starts at the first minted object"
    );
}

#[test]
fn drawing_handles_survive_updates_and_deletes() {
    // A handle names one live object: setters mutate the stored properties
    // (getters read them back), and deletion retires the object so every
    // later call refuses identically on both selections.
    let bars = ramp_bars(10);
    let modules = link_script(
        "l = label.new(bar_index, close, \"L\")\nlabel.set_x(l, 7)\nv = label.get_x(l)\nplot(v, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("handle updates link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "getter sees the setter's value"
    );
    assert!(
        plotted_floats(&vm, "V")
            .iter()
            .all(|value| *value == Some(7.0)),
        "the written property reads back: {vm:?}"
    );

    let dead = link_script(
        "l = label.new(bar_index, close, \"L\")\nlabel.delete(l)\nplot(label.get_x(l), title=\"V\")",
    );
    let tree = run_with_result(None, &dead, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &dead, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "deleted handles refuse identically"
    );
    assert!(
        tree.expect_err("the tree refuses the deleted handle")
            .to_string()
            .contains("unknown label object 'label#0'"),
        "the refusal names the retired object"
    );
}

#[test]
fn drawing_refusal_strings_match() {
    // Every drawing refusal edge is one shared wording: arity, non-handle
    // first argument, wrong namespace, and unknown method.
    let bars = ramp_bars(5);
    for (script, wording) in [
        (
            "l = label.new(bar_index, close, \"L\")\nlabel.set_x(l)\nplot(close, title=\"V\")",
            "'label.set_x' takes 1 values (0 given)",
        ),
        (
            "label.set_x(5, 1)\nplot(close, title=\"V\")",
            "'label.set_x' takes a label object, got 5",
        ),
        (
            "l = label.new(bar_index, close, \"L\")\nbox.get_right(l)\nplot(close, title=\"V\")",
            "'box.get_right' takes a box object, got label#0",
        ),
        (
            "l = label.new(bar_index, close, \"L\")\nlabel.bogus(l)\nplot(close, title=\"V\")",
            "unknown method 'label.bogus'",
        ),
    ] {
        let modules = link_script(script);
        let tree = run_with_result(None, &modules, &bars);
        let vm = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "drawing refusal agrees: {script}"
        );
        let refusal = tree.expect_err("the tree refuses above").to_string();
        assert!(
            refusal.contains(wording),
            "the refusal names the edge: {refusal}"
        );
    }
}

#[test]
fn headless_alerts_evaluate_and_never_deliver() {
    // Alerts cannot fire headlessly: every argument still evaluates (its
    // refusals stand), and nothing is recorded or delivered. The loop
    // matches the tree-walk exactly — no capture vector exists for alerts,
    // and live delivery is the server's out-of-band path, never the VM.
    let bars = ramp_bars(5);
    let modules = link_script(
        "alert(\"marker\", alert.freq_once_per_bar)\nalertcondition(close > open, title=\"T\")\nx = alertcondition(close > open, title=\"X\")\nplot(x, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("headless alerts link");
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "alert evaluation agrees"
    );
    assert!(
        vm.draw_events.is_empty() && vm.strategy_calls.is_empty(),
        "alerts record nothing: {vm:?}"
    );
    assert!(
        plotted_floats(&vm, "V").iter().all(Option::is_none),
        "alertcondition evaluates to na: {vm:?}"
    );
}

/// Run linked modules with the given injections under one selection.
fn run_injected(
    mode: Option<BytecodeMode>,
    modules: &[crate::LinkedModule],
    bars: &[Bar],
    security: bool,
    ltf: bool,
) -> Result<RunOutcome, crate::KernelError> {
    let mut engine = PineEngine::new(engine_config());
    if let Some(mode) = mode {
        engine.set_bytecode_mode(mode);
    }
    if security {
        engine
            .inject_security("SPY", "D", fixture::daily_tape(40))
            .expect("daily security injects");
    }
    if ltf {
        engine
            .inject_lower_tf("SPY", "D", fixture::daily_tape(40))
            .expect("daily lower-tf injects");
    }
    engine.run_linked(modules, bars, InputSet::new())
}

#[test]
fn injected_security_matches_tree() {
    // The harness serves `request.security` from the injected tape: bare
    // legs, kernel-backed `ta.*`, and scalar operators read the confirmed
    // tail. Both selections produce the identical arrays.
    let bars = ramp_bars(10);
    let modules = link_script(
        "probe = request.security(\"SPY\", \"D\", [close, ta.sma(close, 3), ta.atr(3), ta.cci(close, 3) * 3, (ta.rsi(close, 3) - 50) * 2], barmerge.gaps_off, barmerge.lookahead_off, calc_bars_count=10)\nplot(close, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("injected security links");
    let tree = run_injected(None, &modules, &bars, true, false);
    let vm = run_injected(Some(BytecodeMode::Compiled), &modules, &bars, true, false);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "injected security agrees"
    );
    let vm = vm.expect("compiled security run evaluates");
    let tree = tree.expect("tree security run evaluates");
    let probe = |outcome: &RunOutcome, bar: usize| match outcome.value_at(bar, "probe") {
        Some(Some(crate::PineValue::Array(values))) => values.clone(),
        other => panic!("expected the fetch array on bar {bar}, got {other:?}"),
    };
    let last = bars.len() - 1;
    assert_eq!(probe(&vm, last), probe(&tree, last), "values agree");
    assert!(
        probe(&vm, last)[0]
            .as_ref()
            .and_then(crate::PineValue::as_float)
            .is_some(),
        "the served close is a number: {:?}",
        probe(&vm, last)
    );
}

#[test]
fn injected_lower_tf_matches_tree() {
    // `request.security_lower_tf` serves raw injected columns as arrays of
    // arrays: the same window, the same values, in either selection.
    let bars = ramp_bars(10);
    let modules = link_script(
        "probe = request.security_lower_tf(\"SPY\", \"D\", [time, open, high, low, close, volume], calc_bars_count=5)\nplot(close, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("injected lower-tf links");
    let tree = run_injected(None, &modules, &bars, false, true);
    let vm = run_injected(Some(BytecodeMode::Compiled), &modules, &bars, false, true);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "injected lower-tf agrees"
    );
    let vm = vm.expect("compiled lower-tf run evaluates");
    match vm.value_at(bars.len() - 1, "probe") {
        Some(Some(crate::PineValue::Array(columns))) => {
            assert_eq!(columns.len(), 6, "one array per requested leg");
            match &columns[4] {
                Some(crate::PineValue::Array(values)) => {
                    assert_eq!(values.len(), 5, "calc_bars_count trims the tail");
                }
                other => panic!("close column is an array, got {other:?}"),
            }
        }
        other => panic!("expected six column arrays, got {other:?}"),
    }
}

#[test]
fn security_without_injection_refuses_on_both() {
    // No injection means no fetch: both selections refuse with the exact
    // live-fetch wording, and neither guesses a value.
    let bars = ramp_bars(5);
    for script in [
        "probe = request.security(\"SPY\", \"D\", close)\nplot(close, title=\"V\")",
        "probe = request.security_lower_tf(\"SPY\", \"D\", [close], calc_bars_count=5)\nplot(close, title=\"V\")",
    ] {
        let modules = link_script(script);
        let tree = run_injected(None, &modules, &bars, false, false);
        let vm = run_injected(Some(BytecodeMode::Compiled), &modules, &bars, false, false);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "no-injection refusal agrees: {script}"
        );
        let refusal = tree
            .expect_err("the tree refuses without an injection")
            .to_string();
        assert!(
            refusal.contains("is refused headlessly; inject multi-timeframe bars"),
            "the refusal names the live fetch: {refusal}"
        );
    }
}

#[test]
fn security_tick_and_warmup_match() {
    // Tick runs and warmup skip read the same injected tape: the served
    // window is decided by the bar instant and the same evaluated bars.
    let bars = ramp_bars(12);
    let ticks: Vec<i64> = bars.iter().map(|bar| bar.ts.timestamp_millis()).collect();
    let modules = link_script(
        "probe = request.security(\"SPY\", \"D\", [close, ta.sma(close, 2)], calc_bars_count=4)\nplot(close, title=\"V\")",
    );
    let run = |mode: Option<BytecodeMode>| {
        let mut engine = PineEngine::new(
            PineRuntimeConfig::new()
                .chart_symbol("SPY".to_owned())
                .chart_period("1".to_owned())
                .warmup_bars(3)
                .build(),
        );
        if let Some(mode) = mode {
            engine.set_bytecode_mode(mode);
        }
        engine
            .inject_security("SPY", "D", fixture::daily_tape(40))
            .expect("daily security injects");
        engine
            .run_linked_ticks(&modules, &bars, &ticks, InputSet::new())
            .expect("tick security run evaluates")
    };
    let tree = run(None);
    let vm = run(Some(BytecodeMode::Compiled));
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "tick/warmup security agrees"
    );
    assert_eq!(vm.evaluated, bars.len() - 3, "warmup bars stay skipped");
}

#[test]
fn security_calc_bars_count_matches() {
    // `calc_bars_count` trims the served tail identically; a non-positive
    // count refuses with the tree's wording.
    let bars = ramp_bars(8);
    for (script, refuses) in [
        (
            "probe = request.security(\"SPY\", \"D\", close, barmerge.gaps_off, barmerge.lookahead_off, calc_bars_count=1)\nplot(close, title=\"V\")",
            false,
        ),
        (
            "probe = request.security(\"SPY\", \"D\", close, calc_bars_count=0)\nplot(close, title=\"V\")",
            true,
        ),
        (
            "probe = request.security_lower_tf(\"SPY\", \"D\", [close], calc_bars_count=0)\nplot(close, title=\"V\")",
            true,
        ),
    ] {
        let modules = link_script(script);
        let tree = run_injected(None, &modules, &bars, true, true);
        let vm = run_injected(Some(BytecodeMode::Compiled), &modules, &bars, true, true);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "calc_bars_count agrees: {script}"
        );
        assert_eq!(
            tree.is_err(),
            refuses,
            "the refusal expectation holds: {script}"
        );
    }
}

/// One library module whose top level stays on the tree-walk (it captures)
/// plus a driver, so function-level slots are the only route to the VM.
fn linked_pair(lib_body: &str, driver: &str) -> Vec<crate::LinkedModule> {
    vec![
        crate::LinkedModule::new(
            "test/lib",
            crate::parse_program(&format!("plot(close, title=\"LIB\")\n{lib_body}"))
                .expect("lib parses"),
            Default::default(),
        ),
        crate::LinkedModule::new(
            "local/Driver",
            crate::parse_program(driver).expect("driver parses"),
            [("L".to_owned(), "test/lib".to_owned())]
                .into_iter()
                .collect(),
        ),
    ]
}

#[test]
fn pure_library_function_matches_tree() {
    // A self-contained library function evaluates identically under
    // either selection, value for value.
    let modules = linked_pair(
        "export scaled(x) =>\n    x * 2.0 + 1.0",
        "v = L.scaled(close)\nplot(v, title=\"V\")",
    );
    let bars = ramp_bars(12);
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(canonical_form(&vm), canonical_form(&tree), "values agree");
}

#[test]
fn library_function_writebacks_match_tree() {
    // A collection parameter mutated in a library function writes back to
    // the caller's variable through the same per-parameter rule on both
    // engines.
    let modules = linked_pair(
        "export grow(a, v) =>\n    array.push(a, v)\n    array.size(a)",
        "buf = array.from(1.0, 2.0)\nn = L.grow(buf, 9.0)\nplot(array.size(buf) + n, title=\"V\")",
    );
    let bars = ramp_bars(8);
    let tree = run_with(None, &modules, &bars);
    let vm = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "write-backs agree"
    );
    assert_eq!(
        vm.value_at(bars.len() - 1, "buf"),
        Some(Some(crate::PineValue::Array(vec![
            Some(crate::PineValue::Float(1.0)),
            Some(crate::PineValue::Float(2.0)),
            Some(crate::PineValue::Float(9.0)),
        ]))),
        "the caller's array carries the pushed value: {vm:?}"
    );
}

#[test]
fn recursive_pure_function_keeps_depth_bound() {
    // Recursive library functions report the same depth refusal on both
    // engines, and terminating recursion agrees value for value.
    let modules = linked_pair(
        "export fib(n) =>\n    n < 2 ? n : fib(n - 1) + fib(n - 2)",
        "v = L.fib(6)\nplot(v, title=\"V\")",
    );
    let bars = ramp_bars(5);
    let tree = run_with(None, &modules, &bars);
    let mixed = run_with(Some(BytecodeMode::Compiled), &modules, &bars);
    assert_eq!(
        canonical_form(&mixed),
        canonical_form(&tree),
        "values agree"
    );
    assert!(
        plotted_floats(&mixed, "V")
            .iter()
            .all(|value| *value == Some(8.0)),
        "fib(6) is 8: {mixed:?}"
    );

    let runaway = linked_pair(
        "export loop(x) =>\n    loop(x)",
        "v = L.loop(1.0)\nplot(v, title=\"V\")",
    );
    let tree = run_with_result(None, &runaway, &bars);
    let vm = run_with_result(Some(BytecodeMode::Compiled), &runaway, &bars);
    assert_eq!(
        fingerprint_result(&vm),
        fingerprint_result(&tree),
        "depth refusals agree"
    );
    assert!(
        tree.expect_err("unbounded recursion refuses")
            .to_string()
            .contains("call depth exceeds"),
        "the refusal names the bound"
    );
}

#[test]
fn function_reading_module_global_matches_tree() {
    // A library function reading a module global evaluates identically
    // under either selection.
    let modules = linked_pair(
        "shared = 2.0\nexport scaled(x) =>\n    x * shared",
        "v = L.scaled(close)\nplot(v, title=\"V\")",
    );
    let bars = ramp_bars(6);
    assert_eq!(
        canonical_form(&run_with(Some(BytecodeMode::Compiled), &modules, &bars)),
        canonical_form(&run_with(None, &modules, &bars)),
        "the tree-bound function still agrees"
    );
}

#[test]
fn function_with_var_cell_matches_tree() {
    // A `var` cell inside a library function accumulates identically on
    // both engines.
    let modules = linked_pair(
        "export acc(x) =>\n    var total = 0.0\n    total := total + x\n    total",
        "v = L.acc(close)\nplot(v, title=\"V\")",
    );
    let bars = ramp_bars(6);
    assert_eq!(
        canonical_form(&run_with(Some(BytecodeMode::Compiled), &modules, &bars)),
        canonical_form(&run_with(None, &modules, &bars)),
        "var-cell accumulation agrees"
    );
}

#[test]
fn function_with_effect_matches_tree() {
    // Captures, alerts, and drawings inside a library function body
    // evaluate identically under either selection.
    for body in [
        "export draw(x) =>\n    plot(x, \"P\")\n    x",
        "export draw(x) =>\n    alert(\"fired\")\n    x",
        "export draw(x) =>\n    l = label.new(bar_index, x, \"L\")\n    x",
    ] {
        let modules = linked_pair(body, "v = L.draw(close)\nplot(v, title=\"V\")");
        let bars = ramp_bars(6);
        assert_eq!(
            canonical_form(&run_with(Some(BytecodeMode::Compiled), &modules, &bars)),
            canonical_form(&run_with(None, &modules, &bars)),
            "effectful body agrees: {body}"
        );
    }
}

#[test]
fn whole_fixture_closures_stay_byte_identical() {
    // Gate: every checked-in fixture closure must read exactly the same in
    // the compiled selection as on the tree.
    for name in ["_FastMAs", "_IntegrationLite"] {
        let modules = fixture::linked_fixture(name);
        let bars = ramp_bars(60);
        let tree = run_with_result(None, &modules, &bars);
        let mixed = run_with_result(Some(BytecodeMode::Compiled), &modules, &bars);
        assert_eq!(
            fingerprint_result(&mixed),
            fingerprint_result(&tree),
            "{name}: compiled selection is byte-identical"
        );
        // A closure that needs injected tapes refuses identically in both
        // selections, which the fingerprint already proves; one that
        // evaluates must cover every bar.
        match mixed {
            Ok(outcome) => assert_eq!(outcome.evaluated, 60, "{name}: every bar evaluates"),
            Err(refusal) => assert!(
                refusal.to_string().contains("request."),
                "{name}: an evaluation refusal names the external boundary: {refusal}"
            ),
        }
    }
}
