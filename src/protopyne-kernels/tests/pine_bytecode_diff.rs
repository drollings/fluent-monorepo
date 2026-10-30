#![cfg(feature = "bytecode")]
//! Differential-test harness for the bytecode milestone track: a fixed
//! corpus of linked closures plus bars, a tree-walk runner, and a
//! canonical outcome hash. M0 proves the corpus green on the tree-walk;
//! M4 extends the same harness to assert byte-identity against the VM.
//!
//! Everything here composes proven loaders: fixture closures come from
//! `test_fixture::linked_fixture` (checked-in bytes under the canonical
//! server fixture tree), histories from the checked-in `tests/data/SPY_*`
//! trims (never the volatile live root), and hashing from
//! `common_core::hash::fnv1a64` (never a second hasher).
//!
//! Test path note: this file rides the `pine` unit module, so the
//! runnable filter is `pine`, not the file stem.

use crate::pine::test_fixture as fixture;
use crate::{CompiledProgram, InputSet, LinkedModule, PineEngine, PineRuntimeConfig, RunOutcome};
use protopyne_types::Bar;
use std::path::PathBuf;

/// One differential case: a linked closure plus the bars it runs over.
pub struct DiffCase {
    /// Stable case name (report labels, never code).
    pub name: &'static str,
    /// Linked closure as the engine consumes it.
    pub modules: Vec<LinkedModule>,
    /// Chart bars for the run.
    pub bars: Vec<Bar>,
}

fn spy_bars() -> Vec<Bar> {
    // Checked-in trim (436 one-minute bars), same header contract as the
    // fixture harness: `time,open,high,low,close,Volume`.
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

pub fn ramp_bars(count: usize) -> Vec<Bar> {
    (0..count)
        .map(|i| {
            let close = 100.0 + i as f64 * 0.5;
            Bar::new()
                .ts(
                    chrono::DateTime::from_timestamp(1_700_000_000 + i as i64 * 60, 0)
                        .expect("bar time in range"),
                )
                .o(rust_decimal::Decimal::from_f64_retain(close - 0.25).expect("finite"))
                .h(rust_decimal::Decimal::from_f64_retain(close + 0.5).expect("finite"))
                .l(rust_decimal::Decimal::from_f64_retain(close - 0.5).expect("finite"))
                .c(rust_decimal::Decimal::from_f64_retain(close).expect("finite"))
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

/// One synthetic single-module script as a linked closure.
pub fn link_script(script: &str) -> Vec<LinkedModule> {
    let program = crate::parse_program(script).expect("synthetic script parses");
    vec![LinkedModule::new(
        "local/Driver",
        program,
        std::collections::HashMap::new(),
    )]
}

/// The fixed corpus: one fixture closure plus synthetic scripts covering
/// arithmetic, `na`, ternary, history offsets, `var` persistence, loops,
/// user functions, `ta.*`, and branching. Every case evaluates cleanly on
/// the tree-walk (proven below); refusal cases join the corpus in M1.
pub fn diff_corpus() -> Vec<DiffCase> {
    let ramp = ramp_bars(60);
    let scripts: &[(&str, &str)] = &[
        (
            "arith",
            "a = close + open * 2.0\nplot(a + 1.0, title=\"A\")",
        ),
        (
            "na_propagation",
            "v = close > open ? high : na\nplot(v, title=\"V\")",
        ),
        (
            "history_offsets",
            "v = close[3]\nw = close[0] - close[1]\nplot(v + w, title=\"W\")",
        ),
        (
            "var_persistence",
            "var x = 0.0\nx := x + 1.0\nplot(x, title=\"X\")",
        ),
        (
            "bounded_loop",
            "s = 0.0\nfor i = 0 to 4\n    s := s + close\nplot(s, title=\"S\")",
        ),
        (
            "user_function",
            "f(x) => x * 2.0 + 1.0\nplot(f(close), title=\"F\")",
        ),
        ("ta_bridge", "plot(ta.sma(close, 14), title=\"SMA\")"),
        (
            "branching",
            "v = 0.0\nif close > open\n    v := 1.0\nelse\n    v := -1.0\nplot(v, title=\"V\")",
        ),
        (
            "bool_ternary",
            "flag = close > open\nplot(flag ? 1.0 : 0.0, title=\"B\")",
        ),
        (
            "descending_loop",
            "s = 0\nfor i = 5 to 1\n    s := s + i\nplot(s, title=\"S\")",
        ),
        (
            "while_loop",
            "i = 0\nwhile i < 5\n    i := i + close\nplot(i, title=\"I\")",
        ),
        (
            "loop_break",
            "s = 0\nfor i = 0 to 10\n    if i > 2\n        break\n    s := s + i\nplot(s, title=\"S\")",
        ),
        (
            "nested_break",
            "s = 0\nfor i = 0 to 3\n    for j = 0 to 3\n        if j > 1\n            break\n        s := s + 1\nplot(s, title=\"S\")",
        ),
        (
            "switch_stmt",
            "v = 0.0\nswitch close > open\n    true => v := 1.0\n    false => v := -1.0\nplot(v, title=\"V\")",
        ),
        (
            "switch_value",
            "v = switch close\n    close => 1.0\n    open => -1.0\n    => 0.0\nplot(v, title=\"V\")",
        ),
        (
            "switch_nomatch",
            "v = switch 999.0\n    1.0 => 10.0\nplot(v, title=\"V\")",
        ),
        (
            "for_in_array",
            "t = 0.0\nfor x in [1.0, 2.0, 3.0]\n    t := t + x\nplot(t, title=\"T\")",
        ),
        (
            "array_index",
            "a = [10.0, 20.0, 30.0]\nv = a[1]\nplot(v, title=\"V\")",
        ),
        (
            "array_dynamic_index",
            "a = [10.0, 20.0, 30.0]\nv = a[close > open ? 1 : 2]\nplot(v, title=\"V\")",
        ),
        (
            "array_na_index",
            "a = [10.0, 20.0]\nv = a[na]\nplot(v, title=\"V\")",
        ),
        (
            "dynamic_leg_index",
            "v = close[bar_index % 5]\nplot(v, title=\"V\")",
        ),
        (
            "enum_member_compare",
            "enum E\n    A = \"a\"\n    B = \"b\"\nv = E.A == E.B ? 1.0 : 2.0\nplot(v, title=\"V\")",
        ),
        (
            "enum_switch",
            "enum S\n    A = \"a\"\n    B = \"b\"\nv = switch S.A\n    S.A => 1.0\n    S.B => 2.0\n    => 0.0\nplot(v, title=\"V\")",
        ),
        (
            "enum_input_default",
            "enum Dir\n    UP = 1\nv = input.enum(Dir.UP, \"D\")\nplot(close, title=\"C\")",
        ),
        (
            "bare_input_no_clobber",
            "x = 5.0\nv = input(x, \"K\")\nx := x + close\nplot(v + x, title=\"V\")",
        ),
        (
            "conditional_reassign_links",
            "x = 1\nif close > open\n    x := 2\nplot(x, title=\"X\")",
        ),
        (
            "array_builtin_read",
            "a = array.new_float(3, 1.5)\nv = array.get(a, 1) + array.size(a)\nplot(v, title=\"V\")",
        ),
        (
            "array_stmt_writeback",
            "a = array.new_float(3, 0.0)\narray.set(a, 1, 5.0)\narray.push(a, 7.0)\nv = array.get(a, 1) + array.get(a, 3)\nplot(v, title=\"V\")",
        ),
        (
            "array_value_no_writeback",
            "a = array.new_float(2, 1.0)\nb = array.push(a, 2.0)\nplot(array.size(b) + array.size(a), title=\"V\")",
        ),
        (
            "map_put_get",
            "m = map.new<string, int>()\nmap.put(m, \"k\", 42)\nv = map.get(m, \"k\")\nplot(v, title=\"V\")",
        ),
        (
            "matrix_set_get",
            "m = matrix.new(2, 2, 1.0)\nmatrix.set(m, 0, 1, 7.0)\nv = matrix.get(m, 0, 1)\nplot(v, title=\"V\")",
        ),
        (
            "user_call_collects_per_param",
            "f(a, b) =>\n    b\na = [1.0]\nc = [2.0]\nr = f(a, c)\nplot(array.get(a, 0) + array.get(c, 0) + array.get(r, 0), title=\"V\")",
        ),
        (
            "plotcandle",
            "plotcandle(open, high, low, close, title=\"C\", color=color.red)\nplot(close, \"P\")",
        ),
        (
            "barcolor",
            "barcolor(close > open ? color.red : na)\nplot(close, \"P\")",
        ),
        (
            "plotshape_hline",
            "plotshape(close > open, title=\"S\")\nhline(50.0, \"L\")\nplot(close, \"P\")",
        ),
        (
            "fill_inline",
            "fill(plot(close, \"A\"), plot(open, \"B\"))\nplot(close, \"P\")",
        ),
        (
            "fill_variable",
            "pa = plot(close, \"A\")\npb = plot(open, \"B\")\nfill(pa, pb)\nplot(close, \"P\")",
        ),
    ];
    let mut corpus = vec![DiffCase {
        name: "fastmas",
        modules: fixture::linked_fixture("_FastMAs"),
        bars: spy_bars(),
    }];
    for (name, script) in scripts {
        // `Box::leak` keeps the `&'static str` contract without a second
        // registry: names are report labels, never looked up.
        let name: &'static str = Box::leak(name.to_string().into_boxed_str());
        corpus.push(DiffCase {
            name,
            modules: link_script(script),
            bars: ramp.clone(),
        });
    }
    corpus
}

/// Run one case on the tree-walk as a `Result`: success carries the
/// outcome, refusal carries the exact engine error. The differential gate
/// compares these fingerprints, so refusal strings must match byte for byte.
pub fn run_tree_walk_result(
    modules: &[LinkedModule],
    bars: &[Bar],
) -> Result<RunOutcome, crate::KernelError> {
    let mut engine = PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("1".to_owned())
            .warmup_bars(0)
            .build(),
    );
    engine.set_bytecode_mode(crate::pine::BytecodeMode::TreeWalk);
    engine.run_linked(modules, bars, InputSet::new())
}

/// Run one case on the tree-walk engine: SPY scope, zero warmup, the same
/// evaluated-bar semantics every headless proof uses.
pub fn run_tree_walk(modules: &[LinkedModule], bars: &[Bar]) -> RunOutcome {
    run_tree_walk_result(modules, bars).expect("corpus case evaluates")
}

/// Canonical outcome rendering: every ordered capture via `Debug`, every
/// per-bar snapshot with keys sorted (the live `HashMap` order is
/// deliberately unspecified, so raw `Debug` is not a stable oracle).
/// The hash of this rendering is what M4 asserts byte-identical between
/// the tree-walk and the VM.
pub fn canonical_form(outcome: &RunOutcome) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "evaluated={}", outcome.evaluated);
    let _ = writeln!(
        out,
        "plots={:?}\nplotcandles={:?}\nbarcolors={:?}",
        outcome.plots, outcome.plotcandles, outcome.barcolors
    );
    let _ = writeln!(
        out,
        "fills={:?}\nplotshapes={:?}\nhlines={:?}",
        outcome.fills, outcome.plotshapes, outcome.hlines
    );
    let _ = writeln!(
        out,
        "draw_events={:?}\nstrategy_calls={:?}",
        outcome.draw_events, outcome.strategy_calls
    );
    for snapshot in &outcome.snapshots {
        let mut keys: Vec<&String> = snapshot.values.keys().collect();
        keys.sort();
        for key in keys {
            let _ = writeln!(
                out,
                "snapshot[{}].{key}={}",
                snapshot.bar,
                canonical_value(snapshot.values[key].as_ref())
            );
        }
    }
    out
}

/// One snapshot cell in canonical text: subtrees holding instances or
/// int-maps render key-sorted (shared hash maps never fork the
/// fingerprint); everything else renders exactly like its `Debug`, so
/// goldens are byte-identical where no instance appears. Recursion
/// covers instances nested in arrays and maps.
pub fn canonical_value(value: Option<&crate::PineValue>) -> String {
    use crate::PineValue;
    fn unordered(value: Option<&PineValue>) -> bool {
        match value {
            None
            | Some(
                PineValue::Float(_)
                | PineValue::Int(_)
                | PineValue::Bool(_)
                | PineValue::Str(_)
                | PineValue::ColorHandle(_)
                | PineValue::EnumHandle(_)
                | PineValue::Drawing(_),
            ) => false,
            Some(PineValue::Object(_) | PineValue::IntMap(_)) => true,
            Some(PineValue::Array(items)) => items.iter().any(|slot| unordered(slot.as_ref())),
            Some(PineValue::Matrix(grid)) => grid.cells.iter().any(|slot| unordered(slot.as_ref())),
            Some(PineValue::Map(entries)) => {
                entries.values().any(|(_, value)| unordered(value.as_ref()))
            }
        }
    }
    fn sorted(value: Option<&PineValue>) -> String {
        match value {
            None => "None".to_owned(),
            Some(PineValue::Array(items)) => {
                let rendered: Vec<String> =
                    items.iter().map(|slot| sorted(slot.as_ref())).collect();
                format!("Some(Array([{}]))", rendered.join(", "))
            }
            Some(PineValue::Matrix(grid)) => {
                let rendered: Vec<String> =
                    grid.cells.iter().map(|slot| sorted(slot.as_ref())).collect();
                format!(
                    "Some(Matrix(MatrixValue {{ rows: {}, cols: {}, cells: [{}] }}))",
                    grid.rows,
                    grid.cols,
                    rendered.join(", ")
                )
            }
            Some(PineValue::Map(entries)) => {
                let rendered: Vec<String> = entries
                    .iter()
                    .map(|(key, (held, stored))| {
                        format!("{key:?}: ({held:?}, {})", sorted(stored.as_ref()))
                    })
                    .collect();
                format!("Some(Map({{{}}}))", rendered.join(", "))
            }
            Some(PineValue::Object(instance)) => {
                let mut fields: Vec<&String> = instance.fields.keys().collect();
                fields.sort();
                let rendered: Vec<String> = fields
                    .iter()
                    .map(|name| {
                        let slot = instance
                            .fields
                            .get(name.as_str())
                            .and_then(|slot| slot.as_ref());
                        format!("{name:?}: {}", sorted(slot))
                    })
                    .collect();
                format!(
                    "Some(Object(UserInstance {{ type_name: {:?}, fields: FieldTable({{{}}}) }}))",
                    instance.type_name,
                    rendered.join(", ")
                )
            }
            Some(PineValue::IntMap(entries)) => {
                let mut pairs: Vec<(&i64, &i64)> = entries.iter().collect();
                pairs.sort_by_key(|(key, _)| *key);
                let rendered: Vec<String> = pairs
                    .iter()
                    .map(|(key, value)| format!("{key}: {value}"))
                    .collect();
                format!("Some(IntMap({{{}}}))", rendered.join(", "))
            }
            Some(other) => format!("{other:?}"),
        }
    }
    if unordered(value) {
        sorted(value)
    } else {
        format!("{value:?}")
    }
}

/// Canonical outcome hash: the canonical rendering through the shared
/// `fnv1a64` (test-only determinism check, never persisted).
pub fn canonical_hash(outcome: &RunOutcome) -> u64 {
    common_core::hash::fnv1a64(canonical_form(outcome).as_bytes())
}

#[test]
fn differential_corpus_runs_green_on_tree_walk() {
    // The M4 contract rests on this corpus: every case evaluates every
    // bar, publishes at least one plot, and re-runs to the identical hash
    // (rerun determinism is what makes the later byte-identity assertion
    // meaningful instead of vacuous).
    for case in diff_corpus() {
        let first = run_tree_walk(&case.modules, &case.bars);
        assert_eq!(
            first.evaluated,
            case.bars.len(),
            "{} evaluates every bar",
            case.name
        );
        assert!(!first.plots.is_empty(), "{} publishes plots", case.name);
        let second = run_tree_walk(&case.modules, &case.bars);
        assert_eq!(
            canonical_hash(&first),
            canonical_hash(&second),
            "{} reruns byte-identically",
            case.name
        );
    }
}

/// Compile every linked module of one case into a shared program.
/// Pure: the same inputs always produce the same bytecode.
pub fn compile_case(modules: &[LinkedModule]) -> CompiledProgram {
    use crate::pine::bytecode_compile::{
        compile_module, DeclRegistry, EnumRegistry, ExportRegistry,
    };
    let sources = modules
        .iter()
        .map(|module| (module.key.as_str(), &module.program, &module.aliases))
        .collect::<Vec<_>>();
    let enums = EnumRegistry::build(&sources);
    let decls = DeclRegistry::build(&sources);
    let exports = ExportRegistry::build(&sources);
    let mut program = CompiledProgram::new();
    for module in modules {
        program.insert(compile_module(
            &module.key,
            &module.program,
            &module.aliases,
            &enums,
            &decls,
            &exports,
        ));
    }
    program
}

/// Link one compiled case for the VM, in linked-module order.
pub fn link_case(
    program: &crate::pine::CompiledProgram,
    modules: &[LinkedModule],
) -> Result<crate::pine::bytecode_exec::VmProgram, crate::pine::bytecode_exec::LinkError> {
    let order: Vec<&str> = modules.iter().map(|module| module.key.as_str()).collect();
    crate::pine::bytecode_exec::link(program, &order)
}

/// Run one linked case on the VM with the headless config.
pub fn run_vm(
    program: &crate::pine::bytecode_exec::VmProgram,
    bars: &[Bar],
    config: &crate::pine::bytecode_exec::VmConfig,
) -> Result<RunOutcome, crate::KernelError> {
    crate::pine::bytecode_exec::run_linked(program, bars, InputSet::new(), config, None)
}

/// Structural fingerprint of either outcome: canonical form for runs,
/// the exact refusal string for failures. Byte-identity means equal
/// fingerprints, never equal Debug dumps (whose `HashMap` order is
/// unspecified).
pub fn fingerprint_result(result: &Result<RunOutcome, crate::KernelError>) -> String {
    match result {
        Ok(outcome) => canonical_form(outcome),
        Err(error) => format!("refused: {error}"),
    }
}

/// Byte-identity gate for one case: compile, link, and run on both
/// engines; a linked program must agree exactly, while a program the
/// linker refuses still runs tree-walk as sanity (its coverage lives in
/// the M0/M1 suites, never in a partial VM run).
pub fn assert_byte_identical(modules: &[LinkedModule], bars: &[Bar], name: &str) {
    let tree = run_tree_walk_result(modules, bars);
    let program = compile_case(modules);
    match link_case(&program, modules) {
        Ok(vm) => {
            let config = crate::pine::bytecode_exec::VmConfig::headless();
            let got = run_vm(&vm, bars, &config);
            assert_eq!(
                fingerprint_result(&got),
                fingerprint_result(&tree),
                "{name}: VM and tree-walk diverge"
            );
        }
        Err(crate::pine::bytecode_exec::LinkError::Fallback { .. }) => {
            // Whole-run fallback by design: the tree-walk run above is
            // the proof, and M8 owns the deferred constructs.
        }
        Err(other) => panic!("{name}: link must only fail on fallback: {other}"),
    }
}

#[test]
#[allow(clippy::float_cmp)]
fn capture_scripts_link_fully() {
    // Coverage gates are exact by correctness (split-brain equality),
    // never approximate: `== 1.0` below is the gate, not a measurement.
    // R5.1 proof: the visual-capture scripts contain no fallback, so
    // the byte-identity above exercises the VM loop rather than the
    // tree-walk fallback.
    for case in diff_corpus() {
        if !case.name.starts_with("plotcandle")
            && !case.name.starts_with("barcolor")
            && !case.name.starts_with("plotshape")
            && !case.name.starts_with("fill")
        {
            continue;
        }
        let program = compile_case(&case.modules);
        let linked = link_case(&program, &case.modules);
        assert!(
            linked.is_ok(),
            "{} links without fallback: {:?}",
            case.name,
            linked.err()
        );
        assert_eq!(
            linked.expect("linked above").coverage(),
            1.0,
            "{} is fully compiled",
            case.name
        );
    }
}

#[test]
fn vm_matches_tree_walk_where_linked() {
    // M4 differential gate: every M0 corpus case either runs byte-identical
    // on the VM or falls back wholesale to the tree-walk.
    for case in diff_corpus() {
        assert_byte_identical(&case.modules, &case.bars, case.name);
    }
}

#[test]
fn loop_refusals_match_exactly() {
    // Loops lower now, so their bound and trip refusals must read
    // byte-identically out of both engines — not merely fall back.
    let bars = ramp_bars(10);
    let scripts = [
        "for i = 0 to 20000\nplot(i, title=\"I\")",
        "while close > 0\n    x = na",
        "for x in 1.0\n    y = x",
    ];
    for script in scripts {
        let modules = link_script(script);
        let tree = run_tree_walk_result(&modules, &bars);
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).expect("loop shapes link");
        let config = crate::pine::bytecode_exec::VmConfig::headless();
        let got = run_vm(&linked, &bars, &config);
        assert_eq!(
            fingerprint_result(&got),
            fingerprint_result(&tree),
            "{script}: refusal strings match exactly"
        );
        assert!(got.is_err(), "{script}: the run refuses");
    }
}

#[test]
fn nested_break_counts_inner_iterations() {
    // Routing contract, not just agreement: the inner `break` fires at
    // j = 2, so each outer pass adds two and four passes total eight.
    // Both engines must produce this value, not merely agree with each
    // other.
    let bars = ramp_bars(5);
    let modules = link_script(
        "s = 0\nfor i = 0 to 3\n    for j = 0 to 3\n        if j > 1\n            break\n        s := s + 1\nplot(s, title=\"S\")",
    );
    let check = |outcome: &RunOutcome| {
        let last = outcome.snapshots.last().expect("snapshots");
        assert_eq!(
            format!("{:?}", last.values.get("s")),
            "Some(Some(Float(8.0)))",
            "inner break routes to the inner end"
        );
    };
    check(&run_tree_walk(&modules, &bars));
    let program = compile_case(&modules);
    let linked = link_case(&program, &modules).expect("nested loops link");
    let config = crate::pine::bytecode_exec::VmConfig::headless();
    check(&run_vm(&linked, &bars, &config).expect("VM runs"));
}

#[test]
fn switch_refusals_match_exactly() {
    // A bare arm with a non-bool pattern refuses naming the pattern,
    // identically out of both engines.
    let bars = ramp_bars(10);
    let scripts = ["switch\n    1 => x = 1\nplot(close, title=\"C\")"];
    for script in scripts {
        let modules = link_script(script);
        let tree = run_tree_walk_result(&modules, &bars);
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).expect("switch shapes link");
        let config = crate::pine::bytecode_exec::VmConfig::headless();
        let got = run_vm(&linked, &bars, &config);
        assert_eq!(
            fingerprint_result(&got),
            fingerprint_result(&tree),
            "{script}: refusal strings match exactly"
        );
        assert!(got.is_err(), "{script}: the run refuses");
    }
}

#[test]
fn array_writeback_values_match() {
    // The write-back contract, pinned by value (not just agreement):
    // statement `set`/`push` persist, value-position `push` does not.
    let bars = ramp_bars(5);
    let cases = [
        ("array_stmt_writeback", crate::PineValue::Float(12.0)),
        ("array_value_no_writeback", crate::PineValue::Int(5)),
        ("array_builtin_read", crate::PineValue::Float(4.5)),
    ];
    for (name, want) in cases {
        let case = diff_corpus()
            .into_iter()
            .find(|case| case.name == name)
            .unwrap_or_else(|| panic!("{name} in corpus"));
        let check = |outcome: &RunOutcome| {
            let plot = outcome
                .plots
                .iter()
                .find(|plot| plot.title == "V")
                .expect("V plot recorded");
            assert_eq!(
                plot.values.last().cloned().unwrap_or(None),
                Some(want.clone()),
                "{name} computes {want}"
            );
        };
        check(&run_tree_walk(&case.modules, &bars));
        let program = compile_case(&case.modules);
        let linked = link_case(&program, &case.modules).expect("array cases link");
        let config = crate::pine::bytecode_exec::VmConfig::headless();
        check(&run_vm(&linked, &bars, &config).expect("VM runs"));
    }
}

#[test]
fn array_refusals_match_exactly() {
    // Indexing contracts, byte-identical out of both engines: bounds,
    // negativity, non-arrays, and absent indices.
    let bars = ramp_bars(10);
    // These link and refuse identically inside the VM.
    let linked_cases = [
        "a = [1.0]\nplot(a[5], title=\"V\")",
        "a = [1.0]\nplot(a[-1], title=\"V\")",
    ];
    for script in linked_cases {
        let modules = link_script(script);
        let tree = run_tree_walk_result(&modules, &bars);
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).expect("array shapes link");
        let config = crate::pine::bytecode_exec::VmConfig::headless();
        let got = run_vm(&linked, &bars, &config);
        assert_eq!(
            fingerprint_result(&got),
            fingerprint_result(&tree),
            "{script}: refusal strings match exactly"
        );
        assert!(got.is_err(), "{script}: the run refuses");
    }
    // A series offset over a constant now evaluates on both engines:
    // the value spills once per bar and the offset reads it back, so
    // the loop links and both selections publish the constant.
    let modules = link_script("plot(1[0], title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("constant offsets link");
    let tree = run_tree_walk_result(&modules, &bars).expect("tree evaluates");
    let config = crate::pine::bytecode_exec::VmConfig::headless();
    let linked = link_case(&program, &modules).expect("links above");
    let got = run_vm(&linked, &bars, &config).expect("loop evaluates");
    assert_eq!(
        fingerprint_result(&Ok(got)),
        fingerprint_result(&Ok(tree)),
        "constant offsets agree"
    );
}

#[test]
fn enum_refusals_match_exactly() {
    // Unregistered members refuse in the tree-walk with the
    // undefined-variable string; use-before-declaration lowers to the
    // checked read instead of falling back, and both selections refuse
    // identically when the read executes before registration.
    let bars = ramp_bars(10);
    for script in [
        "x = E.B\nplot(x, title=\"X\")",
        "enum E\n    A = \"a\"\nx = E.B\nplot(x, title=\"X\")",
    ] {
        let modules = link_script(script);
        let tree = run_tree_walk_result(&modules, &bars);
        let reason = tree
            .as_ref()
            .expect_err("{script}: the tree-walk refuses")
            .to_string();
        assert!(
            reason.contains("E."),
            "{script}: the refusal names the dotted read: {reason}"
        );
        let program = compile_case(&modules);
        assert!(
            matches!(
                link_case(&program, &modules),
                Err(crate::pine::bytecode_exec::LinkError::Fallback { .. })
            ),
            "{script}: the link guard keeps the program tree-walk"
        );
    }
    // Use-before-declaration lowers to the checked read instead of
    // falling back: the read executes before registration, so both
    // selections refuse naming the spelling.
    let script = "x = E.A\nenum E\n    A = \"a\"\nplot(x, title=\"X\")";
    let modules = link_script(script);
    let program = compile_case(&modules);
    let vm = link_case(&program, &modules)
        .unwrap_or_else(|err| panic!("{script}: links with full coverage: {err:?}"));
    let tree = run_tree_walk_result(&modules, &bars);
    let reason = tree
        .as_ref()
        .expect_err("{script}: the tree-walk refuses")
        .to_string();
    assert!(
        reason.contains("E.A"),
        "{script}: the refusal names the dotted read: {reason}"
    );
    let config = crate::pine::bytecode_exec::VmConfig::headless();
    let compiled = run_vm(&vm, &bars, &config);
    assert_eq!(
        fingerprint_result(&compiled),
        fingerprint_result(&tree),
        "{script}: the loop refuses identically"
    );
}

#[test]
fn backward_calls_keep_enum_uses_tree_walk() {
    // Soundness boundary for registered enum reads: a function reached
    // from a module ordered before the enum's declaration observes an
    // unregistered table, so the checked read refuses with the
    // tree-walk's exact wording instead of serving a handle. The modules
    // below are deliberately not in dependency order; both selections
    // refuse naming the same spelling.
    let caller = crate::parse_program("v = L.f()\nplot(v, title=\"V\")").expect("parses");
    let decl = crate::parse_program("export enum E\n    A = \"a\"").expect("parses");
    let use_ = crate::parse_program("export f() => E.A").expect("parses");
    let modules = vec![
        LinkedModule::new(
            "caller",
            caller,
            [("L".to_owned(), "use_".to_owned())].into_iter().collect(),
        ),
        LinkedModule::new("decl", decl, std::collections::HashMap::new()),
        LinkedModule::new("use_", use_, std::collections::HashMap::new()),
    ];
    let bars = ramp_bars(10);
    let program = compile_case(&modules);
    let vm = link_case(&program, &modules).expect("backward calls link through the checked read");
    let tree = run_tree_walk_result(&modules, &bars);
    let reason = tree
        .as_ref()
        .expect_err("the tree-walk refuses")
        .to_string();
    assert!(
        reason.contains("E.A"),
        "the refusal names the unregistered read: {reason}"
    );
    let compiled = run_vm(
        &vm,
        &bars,
        &crate::pine::bytecode_exec::VmConfig::headless(),
    );
    assert_eq!(
        fingerprint_result(&compiled),
        fingerprint_result(&tree),
        "the loop refuses identically"
    );
}

#[test]
fn conditional_decl_refusals_match_exactly() {
    // A use that the tree-walk would refuse on an unexecuted branch
    // lowers to the loop's declaredness rule instead of falling back:
    // the program links with full coverage and both engines refuse
    // naming the same unbound name.
    let bars = ramp_bars(10);
    for script in [
        "if close < open\n    x = 1\ny = x\nplot(y, title=\"Y\")",
        "if close < open\n    x = 1\nx := 2\nplot(x, title=\"X\")",
        "x = x[1] + 1\nplot(x, title=\"X\")",
        "for i = i to 5\n    x = i\nplot(x, title=\"X\")",
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        let vm = link_case(&program, &modules)
            .unwrap_or_else(|err| panic!("{script}: links with full coverage: {err:?}"));
        let tree = run_tree_walk_result(&modules, &bars);
        let reason = tree
            .as_ref()
            .expect_err("{script}: the tree-walk refuses")
            .to_string();
        assert!(
            reason.contains('x') || reason.contains('i'),
            "{script}: the refusal names the unbound read: {reason}"
        );
        let config = crate::pine::bytecode_exec::VmConfig::headless();
        let compiled = run_vm(&vm, &bars, &config);
        assert_eq!(
            fingerprint_result(&compiled),
            fingerprint_result(&tree),
            "{script}: the loop refuses identically"
        );
    }
}

#[test]
fn descending_and_break_loops_link_fully() {
    // The new loop shapes contain no fallback: the differential gate
    // above exercises the VM loop rather than the tree-walk fallback.
    for name in [
        "descending_loop",
        "while_loop",
        "loop_break",
        "nested_break",
        "switch_stmt",
        "switch_value",
        "switch_nomatch",
        "for_in_array",
        "array_index",
        "array_dynamic_index",
        "array_na_index",
        "dynamic_leg_index",
        "enum_member_compare",
        "enum_switch",
        "enum_input_default",
        "bare_input_no_clobber",
        "conditional_reassign_links",
        "array_builtin_read",
        "array_stmt_writeback",
        "array_value_no_writeback",
        "map_put_get",
        "matrix_set_get",
        "user_call_collects_per_param",
    ] {
        let case = diff_corpus()
            .into_iter()
            .find(|case| case.name == name)
            .expect("loop case in corpus");
        let program = compile_case(&case.modules);
        let linked = link_case(&program, &case.modules);
        assert!(
            linked.is_ok(),
            "{name} links without fallback: {:?}",
            linked.err()
        );
    }
}

#[test]
fn fallback_emissions_repeat_per_module_across_fixtures_and_corpus() {
    // Per-module fallback tallies stay identical on rerun for the same
    // inputs: the same fixture closures and the same differential
    // scripts must compile to the same emission counts every time, so
    // later lowerings can name only their own family delta.
    use crate::pine::bytecode::Instruction;
    use crate::pine::bytecode_compile::{
        compile_module, DeclRegistry, EnumRegistry, ExportRegistry,
    };
    use std::collections::BTreeMap;

    fn emissions(modules: &[LinkedModule]) -> BTreeMap<(String, String), usize> {
        let sources = modules
            .iter()
            .map(|module| (module.key.as_str(), &module.program, &module.aliases))
            .collect::<Vec<_>>();
        let enums = EnumRegistry::build(&sources);
        let decls = DeclRegistry::build(&sources);
        let exports = ExportRegistry::build(&sources);
        let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
        for module in modules {
            let compiled = compile_module(
                &module.key,
                &module.program,
                &module.aliases,
                &enums,
                &decls,
                &exports,
            );
            for block in &compiled.blocks {
                for instr in &block.instrs {
                    if let Instruction::Fallback { reason, .. } = instr {
                        *counts
                            .entry((module.key.clone(), reason.family().to_owned()))
                            .or_default() += 1;
                    }
                }
            }
        }
        counts
    }

    for stem in ["_FastMAs", "_IntegrationLite"] {
        let modules = fixture::linked_fixture(stem);
        let first = emissions(&modules);
        let second = emissions(&modules);
        assert_eq!(
            first, second,
            "{stem}: same inputs must tally identically on rerun"
        );
        let total: usize = first.values().sum();
        eprintln!(
            "{stem}: {total} fallback emissions across {} modules",
            modules.len()
        );
        for ((module, kind), count) in &first {
            eprintln!("{stem} {module} {kind}: {count}");
        }
        assert!(
            !modules.is_empty(),
            "{stem}: the closure under census is non-empty"
        );
    }

    for case in diff_corpus() {
        let first = emissions(&case.modules);
        let second = emissions(&case.modules);
        assert_eq!(
            first, second,
            "{}: same inputs must tally identically on rerun",
            case.name
        );
        let total: usize = first.values().sum();
        eprintln!("corpus {}: {total} fallback emissions", case.name);
    }
}

#[test]
fn closeout_baseline_pins_coverage_and_ledger() {
    // Closeout regression guard: fallback totals per fixture never grow
    // past the measured baseline, and the fully-linked module count
    // never drops below it. Improvement (fewer emissions, more linked
    // modules) passes; silent coverage loss fails. Baselines are the
    // re-measured closeout numbers, not beliefs.
    use crate::pine::bytecode::{CompiledProgram, Instruction};
    use crate::pine::bytecode_compile::{
        compile_module, DeclRegistry, EnumRegistry, ExportRegistry,
    };

    fn compiled(modules: &[LinkedModule]) -> CompiledProgram {
        let sources = modules
            .iter()
            .map(|module| (module.key.as_str(), &module.program, &module.aliases))
            .collect::<Vec<_>>();
        let enums = EnumRegistry::build(&sources);
        let decls = DeclRegistry::build(&sources);
        let exports = ExportRegistry::build(&sources);
        let mut program = CompiledProgram::new();
        for module in modules {
            program.insert(compile_module(
                &module.key,
                &module.program,
                &module.aliases,
                &enums,
                &decls,
                &exports,
            ));
        }
        program
    }

    for (stem, emission_ceiling, linked_floor) in
        [("_FastMAs", 78, 1), ("_IntegrationLite", 205, 0)]
    {
        let modules = fixture::linked_fixture(stem);
        assert!(
            !modules.is_empty(),
            "{stem}: the closure under pin is non-empty"
        );
        let program = compiled(&modules);
        let mut total = 0;
        let mut linked = 0;
        for module in &modules {
            let compiled = program.get(&module.key).expect("fixture compiles");
            let fallbacks = compiled
                .blocks
                .iter()
                .flat_map(|block| block.instrs.iter())
                .filter(|instr| matches!(instr, Instruction::Fallback { .. }))
                .count();
            total += fallbacks;
            if fallbacks == 0 {
                linked += 1;
            }
        }
        eprintln!(
            "{stem}: {total} emissions across {} modules, {linked} fully linked",
            modules.len()
        );
        assert!(
            total <= emission_ceiling,
            "{stem}: emissions grew past the closeout baseline ({total} > {emission_ceiling})"
        );
        assert!(
            linked >= linked_floor,
            "{stem}: fully-linked modules dropped below the closeout floor ({linked} < {linked_floor})"
        );
    }
}

#[test]
fn both_evaluators_satisfy_the_differential_corpus() {
    // One seam, two implementations: every differential case runs through
    // `&dyn PineEvaluator` once per engine, and both fingerprints agree —
    // so the trait carries the whole corpus, not just the easy cases.
    use crate::pine::oracle::OracleEngine;
    use crate::pine::PineEvaluator;
    use crate::BytecodeMode;
    fn config() -> PineRuntimeConfig {
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("1".to_owned())
            .warmup_bars(0)
            .build()
    }
    fn run_through(
        evaluator: &mut dyn PineEvaluator,
        case: &DiffCase,
    ) -> Result<RunOutcome, crate::KernelError> {
        evaluator.run_linked(&case.modules, &case.bars, InputSet::new())
    }
    let mut checked = 0;
    for case in diff_corpus() {
        let mut compiled = PineEngine::new(config());
        compiled.set_bytecode_mode(BytecodeMode::Compiled);
        let mut oracle = OracleEngine::new(config());
        assert_eq!(
            fingerprint_result(&run_through(&mut compiled, &case)),
            fingerprint_result(&run_through(&mut oracle, &case)),
            "{}: evaluators diverge through the shared seam",
            case.name
        );
        checked += 1;
    }
    assert!(checked > 0, "the corpus is non-empty through the seam");
}
