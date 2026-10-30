#![cfg(feature = "bytecode")]
//! Trigger calibration for the bytecode track: three labeled triggers with
//! measured precision and recall, plus the control groups that keep them
//! honest. Pure-numeric sites must tier up and stay byte-identical;
//! mixed-type, version-skewed, tick-mode, and refusal-path controls must
//! never tier (any tier-up deopts on first contact). The calibration
//! constants are pinned here — changing them re-runs this suite.
//!
//! Test path note: this file rides the `pine` unit module, so the
//! runnable filter is `pine`, not the file stem.

use super::bytecode_diff_tests::{
    canonical_form, compile_case, fingerprint_result, link_case, link_script, ramp_bars,
    run_tree_walk_result, run_vm,
};
use crate::pine::bytecode_exec::{
    link_cache_key, run_linked_with_probe, SPLIT_BRAIN_COVERAGE, TIER_STABLE_BARS,
};
use crate::pine::eval::{MAX_CALL_DEPTH, MAX_LOOP_ITERATIONS};
use crate::pine::test_fixture as fixture;
use crate::pine::BytecodeMode;
use crate::{InputSet, LinkedModule, PineEngine, PineRuntimeConfig, RunOutcome};
use protopyne_types::Bar;

fn engine_config() -> PineRuntimeConfig {
    PineRuntimeConfig::new()
        .chart_symbol("SPY".to_owned())
        .chart_period("1".to_owned())
        .warmup_bars(0)
        .build()
}

fn run_tree(modules: &[LinkedModule], bars: &[Bar]) -> RunOutcome {
    let mut engine = PineEngine::new(engine_config());
    engine.set_bytecode_mode(BytecodeMode::TreeWalk);
    engine
        .run_linked(modules, bars, InputSet::new())
        .expect("calibration case evaluates")
}

/// Tree-walk run over a tape-injected engine (the request calibration's
/// oracle: no injection means no served request).
fn run_tree_injected(modules: &[LinkedModule], bars: &[Bar]) -> RunOutcome {
    let mut engine = PineEngine::new(engine_config());
    engine.set_bytecode_mode(BytecodeMode::TreeWalk);
    engine
        .inject_security("SPY", "D", fixture::daily_tape(40))
        .expect("security tape injects");
    engine
        .inject_lower_tf("SPY", "D", fixture::daily_tape(40))
        .expect("lower-tf tape injects");
    engine
        .run_linked(modules, bars, InputSet::new())
        .expect("injected calibration case evaluates")
}

fn plot_numbers(outcome: &RunOutcome) -> Vec<Option<f64>> {
    outcome.plots[0]
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

/// Version-skewed FastMAs triple: the fixture libUtil text under a skewed
/// declared version, warn-only linked exactly like the red-team proof.
fn skewed_fastmas_modules() -> Vec<LinkedModule> {
    let dir = fixture::fixture_root();
    let read = |rel: &str| std::fs::read_to_string(dir.join(rel)).expect("fixture reads");
    let skewed_text = read("libraries/_libUtil.pine").replacen("//@version=6", "//@version=5", 1);
    assert_ne!(
        skewed_text,
        read("libraries/_libUtil.pine"),
        "skew applied to the copy"
    );
    let util_source = crate::parse_pine_source("showmethegrail/libUtil", &skewed_text);
    assert_eq!(util_source.warnings.len(), 1, "skew warns exactly once");
    let util_program = crate::parse_program(&skewed_text).expect("skewed copy parses");
    let mas_text = read("libraries/_libMAs.pine");
    let mas_program = crate::parse_program(&mas_text).expect("libMAs parses");
    let fast_text = read("indicators/_FastMAs.pine");
    let fast_program = crate::parse_program(&fast_text).expect("FastMAs parses");
    let inputs = vec![
        crate::ModuleInput::new(
            crate::LibraryId {
                publisher: "showmethegrail".to_owned(),
                name: "libUtil".to_owned(),
                version: 5,
            },
            util_source,
            util_program,
        ),
        crate::ModuleInput::new(
            crate::LibraryId {
                publisher: "showmethegrail".to_owned(),
                name: "libMAs".to_owned(),
                version: 95,
            },
            crate::parse_pine_source("showmethegrail/libMAs", &mas_text),
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
    let graph = crate::resolve_imports(inputs.clone()).expect("skewed closure links");
    assert!(!graph.warnings().is_empty(), "skew warns on the graph");
    fixture::linked_modules(inputs)
}

#[test]
fn pinned_constants_hold() {
    // K and C are calibration constants, not beliefs: this test is the
    // approval ledger for changing them — any new value must re-run the
    // precision/recall measurements below before it can be trusted.
    assert_eq!(
        TIER_STABLE_BARS, 4,
        "tier-up needs four stable observations"
    );
    assert_eq!(
        SPLIT_BRAIN_COVERAGE, 1.0,
        "any fallback keeps the run tree-walk"
    );
    assert_eq!(
        MAX_LOOP_ITERATIONS, 10_000,
        "loop trip caps refuse past ten thousand"
    );
    assert_eq!(MAX_CALL_DEPTH, 64, "call depth refuses past sixty-four");
}

#[test]
fn pure_numeric_sites_tier_up_and_stay_correct() {
    // Must-fire cases: every script is float-only, writes its series every
    // bar, and reads user history — so eligible columns tier up, serve
    // float reads, and stay byte-identical to the tree-walk.
    let scripts = [
        "var x = 0.0\nx := x + close\ny = x[1]\nplot(y, title=\"Y\")",
        "v = close[3] - close[1]\nw = v[2]\nplot(w, title=\"W\")",
        "f(v) => v * 2.0 + 1.0\ny = f(close)\nz = y[1]\nplot(z, title=\"Z\")",
    ];
    let bars = ramp_bars(30);
    for script in scripts {
        let modules = link_script(script);
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).expect("must-fire case links");
        assert_eq!(linked.coverage(), 1.0, "{script}: fully compiled");
        let (got, probe) = run_linked_with_probe(
            &linked,
            &bars,
            InputSet::new(),
            &crate::pine::bytecode_exec::VmConfig::headless(),
            None,
        );
        let vm = got.expect("must-fire case runs");
        let tree = run_tree(&modules, &bars);
        assert_eq!(
            canonical_form(&vm),
            canonical_form(&tree),
            "{script}: tiered run stays byte-identical"
        );
        assert!(
            probe.pure_columns > 0,
            "{script}: float-only columns stay eligible"
        );
        assert_eq!(
            probe.tiered_columns, probe.pure_columns,
            "{script}: every eligible column tiers"
        );
        assert!(
            probe.float_reads > 0,
            "{script}: user history rides the float mirror"
        );
        assert_eq!(probe.deopts, 0, "{script}: no deopt on a pure run");
        crate::parity::assert_expr_parity(&plot_numbers(&vm), &plot_numbers(&tree), 1e-9, script)
            .expect("tiered batch matches within tolerance");
    }
}

#[test]
fn measurement_precision_is_one_and_recall_is_one() {
    // Precision: tiered-up sites that stayed byte-correct over all
    // tiered-up sites. Recall: eligible pure sites tiered-up over all
    // eligible. Both aggregate over the must-fire runs above; the bars
    // are 1.0 and 1.0 — any incorrect tier-up fails, any untiered
    // eligible site fails.
    let scripts = [
        "var x = 0.0\nx := x + close\ny = x[1]\nplot(y, title=\"Y\")",
        "v = close[3] - close[1]\nw = v[2]\nplot(w, title=\"W\")",
        "f(v) => v * 2.0 + 1.0\ny = f(close)\nz = y[1]\nplot(z, title=\"Z\")",
    ];
    let bars = ramp_bars(30);
    let mut tiered_runs = 0;
    let mut correct_tiered_runs = 0;
    let mut tiered_sites = 0;
    let mut eligible_sites = 0;
    for script in scripts {
        let modules = link_script(script);
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).expect("recall case links");
        let (got, probe) = run_linked_with_probe(
            &linked,
            &bars,
            InputSet::new(),
            &crate::pine::bytecode_exec::VmConfig::headless(),
            None,
        );
        let vm = got.expect("recall case runs");
        let tree = run_tree(&modules, &bars);
        let correct = canonical_form(&vm) == canonical_form(&tree);
        if probe.tiered_columns > 0 {
            tiered_runs += 1;
            if correct {
                correct_tiered_runs += 1;
            }
        }
        tiered_sites += probe.tiered_columns;
        eligible_sites += probe.pure_columns;
    }
    assert!(tiered_runs > 0, "must-fire runs tiered at least one site");
    let precision = f64::from(correct_tiered_runs) / f64::from(tiered_runs);
    assert_eq!(precision, 1.0, "any incorrect tier-up fails");
    assert!(eligible_sites > 0, "must-fire runs hold eligible sites");
    let recall = tiered_sites as f64 / eligible_sites as f64;
    assert_eq!(recall, 1.0, "every eligible site tiers");
}

#[test]
fn mixed_type_columns_never_tier() {
    // Must-NOT-fire control: one column alternates float and boolean
    // across bars, so it retires on first contact and history stays
    // scalar — still byte-identical, never tiered.
    let modules =
        link_script("x = bar_index % 2 == 0 ? close : close > open\nplot(close, title=\"C\")");
    let bars = ramp_bars(30);
    let program = compile_case(&modules);
    let linked = link_case(&program, &modules).expect("mixed case links");
    let (got, probe) = run_linked_with_probe(
        &linked,
        &bars,
        InputSet::new(),
        &crate::pine::bytecode_exec::VmConfig::headless(),
        None,
    );
    let vm = got.expect("mixed case runs");
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&run_tree(&modules, &bars)),
        "mixed run stays byte-identical"
    );
    assert_eq!(probe.tiered_columns, 0, "no site tiers on mixed types");
    assert_eq!(probe.float_reads, 0, "no float read on mixed types");
    assert!(probe.deopts > 0, "first non-numeric contact deopts");
}

#[test]
fn narrowing_value_deopts_on_first_contact() {
    // A site tiers while pure, then a late string write retires it: the
    // early float reads stay correct, later reads ride scalar, and the
    // whole run stays byte-identical.
    let modules = link_script(
        "var x = 0.0\nx := x + close\nif close > 105.0\n    x := \"hot\"\nplot(close, title=\"C\")",
    );
    let bars = ramp_bars(30);
    let program = compile_case(&modules);
    let linked = link_case(&program, &modules).expect("narrowing case links");
    let (got, probe) = run_linked_with_probe(
        &linked,
        &bars,
        InputSet::new(),
        &crate::pine::bytecode_exec::VmConfig::headless(),
        None,
    );
    let vm = got.expect("narrowing case runs");
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&run_tree(&modules, &bars)),
        "narrowed run stays byte-identical"
    );
    assert_eq!(probe.deopts, 1, "the poison write deopts exactly once");
}

#[test]
fn version_skewed_closure_stays_tree_walk() {
    // Must-NOT-fire control: the warn-only skewed linkage warns exactly
    // once on parse and on the graph, and whichever engine selection the
    // linker takes still agrees with the tree-walk — no site can tier up
    // into a divergent run there.
    let modules = skewed_fastmas_modules();
    let bars = ramp_bars(5);
    let program = compile_case(&modules);
    match link_case(&program, &modules) {
        Ok(vm) => {
            let config = crate::pine::bytecode_exec::VmConfig::headless();
            let got = run_vm(&vm, &bars, &config);
            let tree = run_tree_walk_result(&modules, &bars);
            assert_eq!(
                fingerprint_result(&got),
                fingerprint_result(&tree),
                "skewed closure agrees wherever it links"
            );
        }
        Err(crate::pine::bytecode_exec::LinkError::Fallback { .. }) => {}
        Err(other) => panic!("skew guard must fail on fallback: {other}"),
    }
    let mut tree_engine = PineEngine::new(engine_config());
    tree_engine.set_bytecode_mode(BytecodeMode::TreeWalk);
    let tree = tree_engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect("skewed closure stays green on tree-walk");
    assert_eq!(tree.evaluated, bars.len(), "skew evaluates every bar");
}

#[test]
fn tick_runs_stay_scalar() {
    // Must-NOT-fire control: tick emulation is unvalidated for tiering, so
    // it stays scalar by rule — still byte-identical, never tiered.
    let modules = link_script("var x = 0.0\nx := x + close\ny = x[1]\nplot(y, title=\"Y\")");
    let bars = ramp_bars(5);
    let stamps: Vec<i64> = bars.iter().map(|bar| bar.ts.timestamp_millis()).collect();
    let mut tree_engine = PineEngine::new(engine_config());
    tree_engine.set_bytecode_mode(BytecodeMode::TreeWalk);
    let tree = tree_engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("tick run evaluates");
    let program = compile_case(&modules);
    let linked = link_case(&program, &modules).expect("tick case links");
    let (got, probe) = run_linked_with_probe(
        &linked,
        &bars,
        InputSet::new(),
        &crate::pine::bytecode_exec::VmConfig::headless(),
        Some(&stamps),
    );
    let vm = got.expect("tick run evaluates on the loop");
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "tick runs agree bar for bar"
    );
    assert_eq!(probe.tiered_columns, 0, "no site tiers on tick runs");
    assert_eq!(probe.float_reads, 0, "no float read on tick runs");
}

#[test]
fn refusal_paths_match_exactly() {
    // Must-NOT-fire control: a run that refuses names the same reason on
    // both engines, and the aborted run tiers nothing.
    let modules = link_script("f(x) => f(x)\nplot(f(1.0), title=\"F\")");
    let bars = ramp_bars(10);
    let mut tree_engine = PineEngine::new(engine_config());
    tree_engine.set_bytecode_mode(BytecodeMode::TreeWalk);
    let tree = tree_engine.run_linked(&modules, &bars, InputSet::new());
    let program = compile_case(&modules);
    let linked = link_case(&program, &modules).expect("refusal case links");
    let (got, probe) = run_linked_with_probe(
        &linked,
        &bars,
        InputSet::new(),
        &crate::pine::bytecode_exec::VmConfig::headless(),
        None,
    );
    assert_eq!(
        fingerprint_result(&got),
        fingerprint_result(&tree),
        "refusal strings match exactly"
    );
    assert!(got.is_err(), "the run refuses");
    assert_eq!(probe.float_reads, 0, "aborted run serves no float read");
    assert_eq!(probe.tiered_columns, 0, "aborted run tiers nothing");
}

#[test]
fn split_brain_guard_keeps_partial_programs_tree_walk() {
    // T3 in one assertion: a program with unlowered constructs reports
    // coverage under the pinned bar and stays tree-walk, while a fully
    // compiled program reports full coverage and runs.
    let partial = link_script(
        "x = request.security(syminfo.tickerid, \"D\", [foo(close)])\nplot(x, title=\"X\")",
    );
    let program = compile_case(&partial);
    match link_case(&program, &partial) {
        Ok(_) => panic!("partial program must not link"),
        Err(crate::pine::bytecode_exec::LinkError::Fallback { .. }) => {}
        Err(other) => panic!("partial guard must fail on fallback: {other}"),
    }
    let clean = link_script("v = close[3] - close[1]\nplot(v, title=\"W\")");
    let clean_program = compile_case(&clean);
    let clean_linked = link_case(&clean_program, &clean).expect("clean case links");
    assert_eq!(
        clean_linked.coverage(),
        1.0,
        "fully compiled program reports full coverage"
    );
    assert!(
        clean_linked.coverage() >= SPLIT_BRAIN_COVERAGE,
        "clean program clears the guard"
    );
}

fn run_tree_result(
    modules: &[LinkedModule],
    bars: &[Bar],
) -> Result<RunOutcome, crate::KernelError> {
    let mut engine = PineEngine::new(engine_config());
    engine.set_bytecode_mode(BytecodeMode::TreeWalk);
    engine.run_linked(modules, bars, InputSet::new())
}

fn run_compiled_result(
    modules: &[LinkedModule],
    bars: &[Bar],
) -> Result<RunOutcome, crate::KernelError> {
    let mut engine = PineEngine::new(engine_config());
    engine.set_bytecode_mode(BytecodeMode::Compiled);
    engine.run_linked(modules, bars, InputSet::new())
}

fn run_compiled(modules: &[LinkedModule], bars: &[Bar]) -> RunOutcome {
    let mut engine = PineEngine::new(engine_config());
    engine.set_bytecode_mode(BytecodeMode::Compiled);
    engine
        .run_linked(modules, bars, InputSet::new())
        .expect("calibration case evaluates")
}

fn single_modules(lib: &str, driver: &str) -> Vec<LinkedModule> {
    vec![
        LinkedModule::new(
            "test/lib",
            crate::parse_program(&lib).expect("lib parses"),
            Default::default(),
        ),
        LinkedModule::new(
            "local/Driver",
            crate::parse_program(driver).expect("driver parses"),
            [("L".to_owned(), "test/lib".to_owned())]
                .into_iter()
                .collect(),
        ),
    ]
}

#[test]
fn shared_state_captures_and_fills_agree() {
    // One engine, same outcomes: shared globals, tree-side captures, and
    // fills all evaluate identically under either selection — the engine
    // choice never changes the outcome.
    let cases = [
        (
            "shared global",
            single_modules(
                "shared = 1.0\n\nexport get() =>\n    b = [1.0]\n    array.shift(b)\n    shared",
                "v = L.get() + shared + close\nplot(v, title=\"V\")",
            ),
        ),
        (
            "tree-side capture",
            single_modules(
                "export get() =>\n    b = [1.0]\n    array.shift(b)\n    plot(b, \"B\")\n    close",
                "v = L.get()\nplot(v, title=\"V\")",
            ),
        ),
        (
            "fill anywhere",
            single_modules(
                "export get() =>\n    b = [1.0]\n    array.shift(b)\n    close",
                "pa = plot(close, \"A\")\npb = plot(open, \"B\")\nfill(pa, pb)\nv = L.get()\nplot(v, title=\"V\")",
            ),
        ),
    ];
    let bars = ramp_bars(15);
    for (name, modules) in cases {
        assert_eq!(
            canonical_form(&run_compiled(&modules, &bars)),
            canonical_form(&run_tree(&modules, &bars)),
            "{name}: shared-state run agrees"
        );
    }
}

#[test]
fn whole_closure_runs_agree() {
    // The fixture closure links fallback-free end to end, so the compiled
    // selection runs it whole on the loop and agrees with the tree-walk;
    // the synthetic shape below agrees the same way.
    let bars = ramp_bars(15);
    let fastmas = fixture::linked_fixture("_FastMAs");
    assert_eq!(
        canonical_form(&run_compiled(&fastmas, &bars)),
        canonical_form(&run_tree(&fastmas, &bars)),
        "fixture whole run agrees"
    );
    let synthetic = single_modules(
        "export bump(x) =>\n    b = [1.0, 2.0]\n    array.shift(b)\n    x + array.get(b, 0)",
        "v = L.bump(close)\nplot(v, title=\"V\")",
    );
    assert_eq!(
        canonical_form(&run_compiled(&synthetic, &bars)),
        canonical_form(&run_tree(&synthetic, &bars)),
        "synthetic run agrees"
    );
}

#[test]
fn cross_module_calls_agree() {
    // A caller through two library hops evaluates identically under
    // either selection: names resolve through the same alias tables on
    // both engines.
    let lib_b = crate::parse_program("export double(x) => x * 2.0").expect("parses");
    let lib_a = crate::parse_program(&format!(
        "import test/libB/1 as B\n\nexport use(x) =>\n    B.double(x)"
    ))
    .expect("parses");
    let driver = crate::parse_program("v = A.use(close)\nplot(v, title=\"V\")").expect("parses");
    let modules = vec![
        LinkedModule::new("test/libB", lib_b, Default::default()),
        LinkedModule::new(
            "test/libA",
            lib_a,
            [("B".to_owned(), "test/libB".to_owned())]
                .into_iter()
                .collect(),
        ),
        LinkedModule::new(
            "local/Driver",
            driver,
            [("A".to_owned(), "test/libA".to_owned())]
                .into_iter()
                .collect(),
        ),
    ];
    let bars = ramp_bars(15);
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "chained calls agree"
    );
}

#[test]
fn method_numeric_columns_tier_up_and_stay_correct() {
    // Must-fire method-heavy case: pure-numeric work inside a lowered
    // method body tiers its columns and stays byte-identical. The tree
    // run is the contract first; the loop is measured against it.
    let script = "type Box2\n int n = 0\nmethod bump(Box2 this, int d) =>\n this.n := d\n this.n * 2.0 + 1.0\nw = Box2.new()\ny = w.bump(3)\nz = y[1]\nplot(z, title=\"Z\")";
    let bars = ramp_bars(30);
    let modules = link_script(script);
    let tree = run_tree(&modules, &bars);
    let program = compile_case(&modules);
    let linked = link_case(&program, &modules).expect("method case links");
    assert_eq!(linked.coverage(), 1.0, "method case fully compiles");
    let (got, probe) = run_linked_with_probe(
        &linked,
        &bars,
        InputSet::new(),
        &crate::pine::bytecode_exec::VmConfig::headless(),
        None,
    );
    let vm = got.expect("method case runs");
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "method run stays byte-identical"
    );
    assert!(probe.pure_columns > 0, "numeric columns stay eligible");
    assert_eq!(
        probe.tiered_columns, probe.pure_columns,
        "every eligible column tiers"
    );
    assert!(
        probe.float_reads > 0,
        "method history rides the float mirror"
    );
    assert_eq!(
        probe.deopts, 1,
        "only the instance column retires, on first contact"
    );
    crate::parity::assert_expr_parity(&plot_numbers(&vm), &plot_numbers(&tree), 1e-9, script)
        .expect("tiered method batch matches within tolerance");
}

#[test]
fn in_function_global_flows_tier_downstream_only() {
    // Must-fire in-function-global case: journaled caller-home writes
    // bypass the float mirror by construction (no tier, no deopt —
    // nothing to retire), while the downstream plain columns they feed
    // tier up and stay byte-identical.
    let script = "g = 0.0\nf() =>\n g := g + close\n g\ny = f()\nz = y[1]\nplot(z, title=\"Z\")";
    let bars = ramp_bars(30);
    let modules = link_script(script);
    let tree = run_tree(&modules, &bars);
    let program = compile_case(&modules);
    let linked = link_case(&program, &modules).expect("journal case links");
    assert_eq!(linked.coverage(), 1.0, "journal case fully compiles");
    let (got, probe) = run_linked_with_probe(
        &linked,
        &bars,
        InputSet::new(),
        &crate::pine::bytecode_exec::VmConfig::headless(),
        None,
    );
    let vm = got.expect("journal case runs");
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "journal run stays byte-identical"
    );
    assert!(probe.pure_columns > 0, "downstream columns stay eligible");
    assert_eq!(
        probe.tiered_columns, probe.pure_columns,
        "every eligible column tiers"
    );
    assert!(
        probe.float_reads > 0,
        "downstream history rides the float mirror"
    );
    assert_eq!(probe.deopts, 0, "journal writes never touch the mirror");
    crate::parity::assert_expr_parity(&plot_numbers(&vm), &plot_numbers(&tree), 1e-9, script)
        .expect("tiered journal batch matches within tolerance");
}

#[test]
fn object_touching_columns_never_tier_while_numerics_do() {
    // Must-NOT-fire control for the object model: the instance column
    // retires on first contact (exactly one deopt — the take/put path
    // bypasses the mirror, so no second firing), while the pure-numeric
    // columns beside it tier up and stay byte-identical. This proves no
    // over-fire (objects never ride the mirror) and no under-fire
    // (neighbors still do).
    let script = "type Box2\n int n = 0\nmethod bump(Box2 this, int d) =>\n this.n := d\n this.n * 2.0 + 1.0\nw = Box2.new()\nw.n := 4\ny = w.bump(3)\nz = y[1]\nplot(z, title=\"Z\")";
    let bars = ramp_bars(30);
    let modules = link_script(script);
    let tree = run_tree(&modules, &bars);
    let program = compile_case(&modules);
    let linked = link_case(&program, &modules).expect("object case links");
    assert_eq!(linked.coverage(), 1.0, "object case fully compiles");
    let (got, probe) = run_linked_with_probe(
        &linked,
        &bars,
        InputSet::new(),
        &crate::pine::bytecode_exec::VmConfig::headless(),
        None,
    );
    let vm = got.expect("object case runs");
    assert_eq!(
        canonical_form(&vm),
        canonical_form(&tree),
        "object run stays byte-identical"
    );
    assert!(probe.pure_columns > 0, "numeric columns stay eligible");
    assert_eq!(
        probe.tiered_columns, probe.pure_columns,
        "every eligible column tiers; the instance column never does"
    );
    assert!(
        probe.float_reads > 0,
        "numeric history rides the float mirror"
    );
    assert_eq!(
        probe.deopts, 1,
        "the instance column retires exactly once, on first contact"
    );
    crate::parity::assert_expr_parity(&plot_numbers(&vm), &plot_numbers(&tree), 1e-9, script)
        .expect("tiered object-adjacent batch matches within tolerance");
}

fn loop_source() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/pine/bytecode_exec.rs"
    ))
    .expect("loop source is readable")
}

#[test]
fn only_pinned_thresholds_gate_the_loop() {
    // No-new-trigger audit: the loop file declares exactly two public
    // threshold constants (the tier gate and the split-brain guard) and
    // exactly two private capacity/layout constants (a fixed cache size
    // that is never calibrated, and a leg-count alias that is not a
    // threshold). Anything else numeric that gates behavior must arrive
    // through the four pinned imports below — a new literal here fails
    // this inventory.
    let source = loop_source();
    let mut public: Vec<(String, String)> = Vec::new();
    let mut private: Vec<(String, String)> = Vec::new();
    for line in source.lines() {
        let (target, rest) = if let Some(rest) = line.strip_prefix("pub const ") {
            (&mut public, rest)
        } else if let Some(rest) = line.strip_prefix("const ") {
            (&mut private, rest)
        } else {
            continue;
        };
        let name = rest.split(':').next().unwrap_or("").trim().to_owned();
        let value = rest
            .split('=')
            .nth(1)
            .unwrap_or("")
            .trim()
            .trim_end_matches(';')
            .trim()
            .to_owned();
        target.push((name, value));
    }
    assert_eq!(
        public,
        vec![
            ("TIER_STABLE_BARS".to_owned(), "4".to_owned()),
            ("SPLIT_BRAIN_COVERAGE".to_owned(), "1.0".to_owned()),
        ],
        "public thresholds are exactly the pinned pair"
    );
    assert_eq!(
        TIER_STABLE_BARS, 4,
        "tier-up needs four stable observations"
    );
    assert_eq!(
        SPLIT_BRAIN_COVERAGE, 1.0,
        "any fallback keeps the run tree-walk"
    );
    assert_eq!(
        private,
        vec![
            ("LINK_CACHE_CAPACITY".to_owned(), "64".to_owned()),
            (
                "LEG_COUNT_GLOBAL".to_owned(),
                "super::bytecode::LEG_COUNT as usize".to_owned()
            ),
        ],
        "private constants are fixed capacity and layout only, never gates"
    );
    assert_eq!(
        MAX_LOOP_ITERATIONS, 10_000,
        "loop trip caps refuse past ten thousand"
    );
    assert_eq!(MAX_CALL_DEPTH, 64, "call depth refuses past sixty-four");
    // Each pinned gate engages where the design says: tier stability,
    // link coverage, loop trip bounds, and call depth — and nowhere else
    // introduces a numeric comparison against a gate-shaped name.
    for gate in [
        ">= TIER_STABLE_BARS",
        "< SPLIT_BRAIN_COVERAGE",
        "MAX_LOOP_ITERATIONS as i128",
        "MAX_LOOP_ITERATIONS as i64",
        ">= MAX_CALL_DEPTH",
    ] {
        assert!(
            source.contains(gate),
            "pinned gate engages in the loop: {gate}"
        );
    }
}

#[test]
fn calibration_feeds_no_cache_artifact_or_default() {
    // The calibrated numbers must not leak into anything that persists
    // or defaults: the mixed-plan cache is content-keyed (same modules
    // in, same key out — calibration cannot steer it), the probe report
    // is dropped by the plain entry point (measurement never feeds back
    // into the outcome), and a fresh input store declares nothing (every
    // default flows from the script declaration, never from calibration).
    let left = link_script("v = close + 1.0\nplot(v, title=\"V\")");
    let left_again = link_script("v = close + 1.0\nplot(v, title=\"V\")");
    let right = link_script("v = close + 2.0\nplot(v, title=\"V\")");
    assert_eq!(
        link_cache_key(&left),
        link_cache_key(&left_again),
        "same modules key identically"
    );
    assert_ne!(
        link_cache_key(&left),
        link_cache_key(&right),
        "different modules key differently"
    );
    let bars = ramp_bars(15);
    let modules = link_script("var x = 0.0\nx := x + close\ny = x[1]\nplot(y, title=\"Y\")");
    let program = compile_case(&modules);
    let linked = link_case(&program, &modules).expect("probe case links");
    let config = crate::pine::bytecode_exec::VmConfig::headless();
    let plain =
        crate::pine::bytecode_exec::run_linked(&linked, &bars, InputSet::new(), &config, None)
            .expect("plain run evaluates");
    let (probed, _) = run_linked_with_probe(&linked, &bars, InputSet::new(), &config, None);
    let probed = probed.expect("probed run evaluates");
    assert_eq!(
        canonical_form(&plain),
        canonical_form(&probed),
        "measurement never feeds back into the outcome"
    );
    assert!(
        InputSet::new().get("undeclared").is_none(),
        "a fresh store declares nothing"
    );
}

#[test]
fn shared_object_global_agrees() {
    // A dotted take on a shared instance evaluates identically under
    // either selection.
    let bars = ramp_bars(15);
    let modules = single_modules(
        "export get() =>\n    b = [1.0]\n    array.shift(b)\n    w.n := w.n + 1\n    w.n",
        "type Box2\n    int n = 0\nw = Box2.new()\nv = L.get()\nplot(v + w.n, title=\"V\")",
    );
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "shared object run agrees"
    );
}

#[test]
fn object_side_capture_agrees() {
    // A library capture over a passed instance evaluates identically
    // under either selection.
    let bars = ramp_bars(15);
    let modules = single_modules(
        "export get(o) =>\n    plot(o.n, \"B\")\n    o.n",
        "type Box2\n    int n = 5\nw = Box2.new()\nv = L.get(w)\nplot(v, title=\"V\")",
    );
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "capture run agrees"
    );
}

#[test]
fn object_argument_reads_agree() {
    // An instance passed across modules reads field-identically on both
    // engines — construction loop-side, field read tree-side, same value.
    let bars = ramp_bars(15);
    let modules = single_modules(
        "export get(o) =>\n    b = [1.0]\n    array.shift(b)\n    o.n",
        "type Box2\n    int n = 5\nw = Box2.new()\nv = L.get(w)\nplot(v, title=\"V\")",
    );
    let tree = run_tree(&modules, &bars);
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&tree),
        "object argument run agrees"
    );
    assert!(
        tree.plots.iter().any(|plot| plot.title == "V"
            && plot.values.iter().all(|cell| matches!(
                cell,
                Some(crate::PineValue::Int(5) | crate::PineValue::Float(5.0))
            ))),
        "the bridged field reads back intact"
    );
}

#[test]
fn bridged_field_miss_refuses_identically() {
    // A field miss through a passed instance refuses with the same
    // wording on both engines, naming the same field.
    let bars = ramp_bars(5);
    let modules = single_modules(
        "export bad(o) =>\n    b = [1.0]\n    array.shift(b)\n    o.bogus",
        "type Box2\n    int n = 5\nw = Box2.new()\nv = L.bad(w)\nplot(v, title=\"V\")",
    );
    let mut tree_engine = PineEngine::new(engine_config());
    tree_engine.set_bytecode_mode(BytecodeMode::TreeWalk);
    let tree = tree_engine.run_linked(&modules, &bars, InputSet::new());
    let compiled = run_compiled_result(&modules, &bars);
    assert_eq!(
        fingerprint_result(&compiled),
        fingerprint_result(&tree),
        "field-miss refusals match"
    );
    assert!(tree.is_err(), "the tree refuses the missing field");
    assert!(
        tree.expect_err("refuses above")
            .to_string()
            .contains("unknown field 'o.bogus'"),
        "the refusal names the field"
    );
}

#[test]
fn object_callee_chain_agrees() {
    // A reader through two library hops evaluates identically under
    // either selection.
    let lib_b = crate::parse_program("export readn(o) => o.n").expect("parses");
    let lib_a = crate::parse_program(&format!(
        "import test/libB/1 as B\n\nexport use(o) =>\n    B.readn(o)"
    ))
    .expect("parses");
    let driver = crate::parse_program(
        "type Box2\n    int n = 7\nw = Box2.new()\nv = A.use(w)\nplot(v, title=\"V\")",
    )
    .expect("parses");
    let modules = vec![
        LinkedModule::new("test/libB", lib_b, Default::default()),
        LinkedModule::new(
            "test/libA",
            lib_a,
            [("B".to_owned(), "test/libB".to_owned())]
                .into_iter()
                .collect(),
        ),
        LinkedModule::new(
            "local/Driver",
            driver,
            [("A".to_owned(), "test/libA".to_owned())]
                .into_iter()
                .collect(),
        ),
    ];
    let bars = ramp_bars(15);
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "object callee chain agrees"
    );
}

#[test]
fn headless_strategy_shapes_fire_and_live_shapes_refuse() {
    // Must-fire corpus: every documented harness call and every single-dot
    // harness read links with full coverage and agrees with the tree-walk.
    // Must-refuse corpus: live-only broker dialogue never fabricates a
    // venue action — both selections refuse with the matrix wording.
    // Precision is the share of fired shapes that stayed correct; recall is
    // the share of headless shapes that fired. Both are 1.0 by construction
    // (one shared dispatcher), and this test measures them.
    let bars = ramp_bars(12);
    let call_shapes = [
        "strategy(\"S\", initial_capital=10000)\nstrategy.entry(\"L\", strategy.long)\nplot(close, title=\"V\")",
        "strategy(\"S\", initial_capital=10000)\nstrategy.entry(\"S\", strategy.short, qty=2)\nplot(close, title=\"V\")",
        "strategy(\"S\", initial_capital=10000)\nstrategy.exit(\"X\", from_entry=\"L\", profit=5, loss=3)\nplot(close, title=\"V\")",
        "strategy(\"S\", initial_capital=10000)\nstrategy.close(\"L\")\nplot(close, title=\"V\")",
        "strategy(\"S\", initial_capital=10000)\nstrategy.close_all(comment=\"done\")\nplot(close, title=\"V\")",
        "strategy(\"S\", initial_capital=10000)\nplot(strategy.closedtrades.size(), title=\"V\")",
        "strategy(\"S\", initial_capital=10000)\nplot(close, comment=strategy.closedtrades.exit_comment(0), title=\"V\")",
    ];
    let read_shapes = [
        "long",
        "short",
        "fixed",
        "initial_capital",
        "closedtrades",
        "opentrades",
        "wintrades",
        "losstrades",
        "position_size",
        "equity",
        "netprofit",
        "openprofit",
        "grossprofit",
        "grossloss",
        "position_avg_price",
        "max_drawdown",
    ];
    let mut must_fire = 0usize;
    let mut fired = 0usize;
    let mut fired_correct = 0usize;
    for script in call_shapes {
        must_fire += 1;
        let modules = link_script(script);
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).expect("headless call links");
        assert_eq!(linked.coverage(), 1.0, "{script}: fully compiled");
        let vm = run_compiled(&modules, &bars);
        let tree = run_tree_injected(&modules, &bars);
        fired += 1;
        if canonical_form(&vm) == canonical_form(&tree) {
            fired_correct += 1;
        }
        assert_eq!(
            canonical_form(&vm),
            canonical_form(&tree),
            "{script}: headless call agrees"
        );
    }
    for member in read_shapes {
        must_fire += 1;
        let script =
            format!("strategy(\"S\", initial_capital=1000)\nplot(strategy.{member}, title=\"V\")");
        let modules = link_script(&script);
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).expect("headless read links");
        assert_eq!(linked.coverage(), 1.0, "{member}: fully compiled");
        let vm = run_compiled(&modules, &bars);
        let tree = run_tree_injected(&modules, &bars);
        fired += 1;
        if canonical_form(&vm) == canonical_form(&tree) {
            fired_correct += 1;
        }
        assert_eq!(
            canonical_form(&vm),
            canonical_form(&tree),
            "{member}: headless read agrees"
        );
    }
    let live_shapes = [
        "strategy(\"S\", initial_capital=1000)\nstrategy.order(\"L\", strategy.long)\nplot(close, title=\"V\")",
        "strategy(\"S\", initial_capital=1000)\nstrategy.cancel(\"L\")\nplot(close, title=\"V\")",
        "strategy(\"S\", initial_capital=1000)\nstrategy.cancel_all()\nplot(close, title=\"V\")",
        "strategy(\"S\", initial_capital=1000)\nstrategy.risk.max_intraday_loss(1000)\nplot(close, title=\"V\")",
    ];
    for script in live_shapes {
        let modules = link_script(script);
        let mut tree_engine = PineEngine::new(engine_config());
        tree_engine.set_bytecode_mode(BytecodeMode::TreeWalk);
        let tree = tree_engine.run_linked(&modules, &bars, InputSet::new());
        let mut engine = PineEngine::new(engine_config());
        engine.set_bytecode_mode(BytecodeMode::Compiled);
        let vm = engine.run_linked(&modules, &bars, InputSet::new());
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "{script}: live dialogue refuses identically"
        );
        let refusal = tree
            .expect_err("live dialogue refuses headlessly")
            .to_string();
        assert!(
            refusal.contains("outside the harness matrix"),
            "{script}: the refusal names the matrix: {refusal}"
        );
    }
    assert_eq!(fired, must_fire, "recall: every headless shape fires");
    assert_eq!(
        fired_correct, must_fire,
        "precision: every fired shape stays byte-identical"
    );
}

#[test]
fn headless_drawing_and_alert_shapes_fire_without_delivery() {
    // Must-fire corpus: every recorded drawing transition and both alert
    // forms link with full coverage and agree with the tree-walk.
    // Must-NOT-fire control: nothing in a drawing/alert run reaches a
    // delivery path — the event sink is the only output, and an alert
    // records no row at all. Precision and recall are measured here.
    let bars = ramp_bars(12);
    let shapes = [
        "l = label.new(bar_index, close, \"L\")\nlabel.set_x(l, 5)\nv = label.get_x(l)\nplot(v, title=\"V\")",
        "b = box.new(bar_index, close, bar_index + 1, close + 1)\nbox.set_right(b, bar_index + 2)\nplot(box.get_right(b), title=\"V\")",
        "ln = line.new(bar_index, close, bar_index + 1, close + 1)\nline.set_xy1(ln, 1, 2)\nplot(line.get_x2(ln), title=\"V\")",
        "t = table.new(position.top_right, 2, 2)\ntable.cell(t, 0, 0, \"x\")\ntable.merge_cells(t, 0, 0, 1, 1)\nplot(close, title=\"V\")",
        "l = label.new(bar_index, close, \"L\")\nlabel.delete(l)\nplot(close, title=\"V\")",
        "alert(\"marker\", alert.freq_once_per_bar)\nplot(close, title=\"V\")",
        "alertcondition(close > open, title=\"T\")\nplot(close, title=\"V\")",
    ];
    let mut must_fire = 0usize;
    let mut fired = 0usize;
    let mut fired_correct = 0usize;
    for script in shapes {
        must_fire += 1;
        let modules = link_script(script);
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).expect("headless drawing shape links");
        assert_eq!(linked.coverage(), 1.0, "{script}: fully compiled");
        let vm = run_compiled(&modules, &bars);
        let tree = run_tree_injected(&modules, &bars);
        fired += 1;
        if canonical_form(&vm) == canonical_form(&tree) {
            fired_correct += 1;
        }
        assert_eq!(
            canonical_form(&vm),
            canonical_form(&tree),
            "{script}: headless drawing shape agrees"
        );
    }
    assert_eq!(fired, must_fire, "recall: every headless shape fires");
    assert_eq!(
        fired_correct, must_fire,
        "precision: every fired shape stays byte-identical"
    );

    // The alert control: arguments evaluate, nothing is recorded, and the
    // run has no delivery surface at all.
    let alert_module = link_script("alert(\"marker\")\nalertcondition(close > open, title=\"T\")");
    let vm = run_compiled(&alert_module, &bars);
    assert!(
        vm.draw_events.is_empty(),
        "an alert never records a drawing row: {vm:?}"
    );
    assert!(
        vm.strategy_calls.is_empty(),
        "an alert never records a broker call: {vm:?}"
    );
}

#[test]
fn injected_request_shapes_fire_and_absent_tapes_refuse() {
    // Must-fire corpus (injection present): every headless request shape the
    // tree serves links with full coverage and agrees with the tree-walk.
    // Must-refuse control (injection absent): the loop serves the injected
    // surface, so an unregistered pair refuses with the tree's exact
    // live-fetch wording instead of a second fetch path. Precision is the
    // share of fired shapes that stayed byte-identical; recall is the share
    // of headless shapes that fired. Both are 1.0 and measured here.
    let bars = ramp_bars(12);
    let shapes = [
        "probe = request.security(\"SPY\", \"D\", close, calc_bars_count=3)\nplot(close, title=\"V\")",
        "probe = request.security(\"SPY\", \"D\", [close, ta.sma(close, 3), ta.atr(3) * 2, (ta.rsi(close, 3) - 50) * 2], barmerge.gaps_off, barmerge.lookahead_off, calc_bars_count=5)\nplot(close, title=\"V\")",
        "probe = request.security_lower_tf(\"SPY\", \"D\", [time, open, high, low, close, volume], calc_bars_count=4)\nplot(close, title=\"V\")",
    ];
    let mut must_fire = 0usize;
    let mut fired = 0usize;
    let mut fired_correct = 0usize;
    for script in shapes {
        must_fire += 1;
        let modules = link_script(script);
        let program = compile_case(&modules);
        let mut engine = PineEngine::new(engine_config());
        engine.set_bytecode_mode(BytecodeMode::Compiled);
        engine
            .inject_security("SPY", "D", fixture::daily_tape(40))
            .expect("security tape injects");
        engine
            .inject_lower_tf("SPY", "D", fixture::daily_tape(40))
            .expect("lower-tf tape injects");
        let vm = engine
            .run_linked(&modules, &bars, InputSet::new())
            .expect("injected shape evaluates");
        let linked = link_case(&program, &modules).expect("injected shape links");
        assert_eq!(linked.coverage(), 1.0, "{script}: fully compiled");
        let tree = run_tree_injected(&modules, &bars);
        fired += 1;
        if canonical_form(&vm) == canonical_form(&tree) {
            fired_correct += 1;
        }
        assert_eq!(
            canonical_form(&vm),
            canonical_form(&tree),
            "{script}: injected shape agrees"
        );
    }
    assert_eq!(fired, must_fire, "recall: every headless shape fires");
    assert_eq!(
        fired_correct, must_fire,
        "precision: every fired shape stays byte-identical"
    );

    // Injection-absent control: same shapes without tapes, both engines
    // refusing with the same wording.
    for script in [
        "probe = request.security(\"SPY\", \"D\", close)\nplot(close, title=\"V\")",
        "probe = request.security_lower_tf(\"SPY\", \"D\", [close])\nplot(close, title=\"V\")",
    ] {
        let modules = link_script(script);
        let mut tree_engine = PineEngine::new(engine_config());
        tree_engine.set_bytecode_mode(BytecodeMode::TreeWalk);
        let tree = tree_engine.run_linked(&modules, &bars, InputSet::new());
        let mut engine = PineEngine::new(engine_config());
        engine.set_bytecode_mode(BytecodeMode::Compiled);
        let vm = engine.run_linked(&modules, &bars, InputSet::new());
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "{script}: absent tape refuses identically"
        );
        assert!(
            tree.expect_err("no tape refuses")
                .to_string()
                .contains("is refused headlessly"),
            "the refusal names the live fetch"
        );
    }
}

#[test]
fn dynamic_and_refusal_lowering_is_calibrated() {
    // Must-lower corpus: deterministic titles/inputs and deterministic
    // call-shape refusals all stay compiled and agree with the tree-walk
    // (values where the tree returns them, identical refusal text where it
    // refuses). Must-retain controls: shapes whose decision needs the
    // run-time object/capture model keep their typed fallback.
    let bars = ramp_bars(12);
    let must_lower = [
        "t = \"T\"\nplot(close, title=t)",
        "plot(close)",
        "v = input(close + 1.0)\nplot(v, title=\"V\")",
        "if close < 0.0\n    missing(1)\nplot(close, title=\"V\")",
        "missing(1)\nplot(close, title=\"V\")",
        "plot(na(close, close), title=\"V\")",
        "v = input()\nplot(v, title=\"V\")",
    ];
    let mut fired = 0usize;
    for script in must_lower {
        let modules = link_script(script);
        let program = compile_case(&modules);
        let linked = link_case(&program, &modules).expect("must-lower shape links");
        assert_eq!(linked.coverage(), 1.0, "{script}: fully compiled");
        fired += 1;
        let vm = run_compiled_result(&modules, &bars);
        let tree = run_tree_result(&modules, &bars);
        assert_eq!(
            fingerprint_result(&vm),
            fingerprint_result(&tree),
            "{script}: outcome agrees"
        );
    }
    assert_eq!(
        fired,
        must_lower.len(),
        "recall: every must-lower shape fires"
    );

    let must_retain = [
        "plotshape(close, title=close)",
        "fill(close, open)",
        "x = request.security(\"SPY\", \"D\", [call(close)])",
    ];
    for script in must_retain {
        let modules = link_script(script);
        let program = compile_case(&modules);
        assert!(
            matches!(
                link_case(&program, &modules),
                Err(crate::pine::bytecode_exec::LinkError::Fallback { .. })
            ),
            "must-retain shape keeps its fallback: {script}"
        );
    }
}

/// One linked library plus a driver: the library top level carries a
/// capture alongside the function under test, so the closed-over value
/// and the call result travel together.
fn single_pair(lib_body: &str, driver: &str) -> Vec<LinkedModule> {
    vec![
        LinkedModule::new(
            "test/lib",
            crate::parse_program(&format!("plot(close, title=\"LIB\")\n{lib_body}"))
                .expect("lib parses"),
            Default::default(),
        ),
        LinkedModule::new(
            "local/Driver",
            crate::parse_program(driver).expect("driver parses"),
            [("L".to_owned(), "test/lib".to_owned())]
                .into_iter()
                .collect(),
        ),
    ]
}

#[test]
fn library_function_shapes_agree() {
    // Every library-function shape evaluates identically under either
    // selection: self-contained or not, the engines agree value for
    // value, and refusals name the same reason on both.
    let bars = ramp_bars(12);
    let must_fire = [
        (
            "arith",
            "export calc(x) =>\n    x * 2.0 + 1.0",
            "v = L.calc(close)\nplot(v, title=\"V\")",
        ),
        (
            "array",
            "export calc(x) =>\n    b = [x]\n    array.push(b, x + 1.0)\n    array.get(b, 1)",
            "v = L.calc(close)\nplot(v, title=\"V\")",
        ),
        (
            "indirect",
            "export double(x) =>\n    x * 2.0\nexport calc(x) =>\n    double(x) + 1.0",
            "v = L.calc(close)\nplot(v, title=\"V\")",
        ),
        (
            "recursive",
            "export fib(n) =>\n    n < 2 ? n : fib(n - 1) + fib(n - 2)",
            "v = L.fib(5)\nplot(v, title=\"V\")",
        ),
        (
            "ta",
            "export smooth(x) =>\n    ta.sma(x, 3)",
            "v = L.smooth(close)\nplot(v, title=\"V\")",
        ),
    ];
    let mut fired = 0usize;
    let mut fired_correct = 0usize;
    for (name, lib, driver) in must_fire {
        let modules = single_pair(lib, driver);
        let tree = run_tree(&modules, &bars);
        let vm = run_compiled(&modules, &bars);
        assert_eq!(
            canonical_form(&vm),
            canonical_form(&tree),
            "{name}: library function agrees"
        );
        fired += 1;
        fired_correct += 1;
    }
    assert_eq!(
        fired,
        must_fire.len(),
        "recall: every must-fire shape fires"
    );
    assert_eq!(
        fired_correct,
        must_fire.len(),
        "precision: every fired shape stays byte-identical"
    );

    for (name, lib) in [
        (
            "module-global",
            "shared = 2.0\nexport calc(x) =>\n    x * shared",
        ),
        (
            "var-cell",
            "export calc(x) =>\n    var t = 0.0\n    t := t + x\n    t",
        ),
        ("capture", "export calc(x) =>\n    plot(x, \"P\")\n    x"),
        (
            "request",
            "export calc(x) =>\n    request.security(\"SPY\", \"D\", close)\n    x",
        ),
        (
            "calls-impure",
            "shared = 3.0\nexport base(x) =>\n    x + shared\nexport calc(x) =>\n    base(x)",
        ),
    ] {
        let modules = single_pair(lib, "v = L.calc(close)\nplot(v, title=\"V\")");
        assert_eq!(
            fingerprint_result(&run_compiled_result(&modules, &bars)),
            fingerprint_result(&run_tree_result(&modules, &bars)),
            "{name}: stateful function still agrees"
        );
    }
}

#[test]
fn runtime_error_message_matches_tree() {
    // A live `runtime.error` refuses with the tree-walk's exact wording on
    // the loop: a dynamic string (including `str.tostring` composition), a
    // non-string value with its prefix, bare (`na`) with the bare wording.
    // The program links with full coverage first, so the agreement below
    // proves the loop raised it, never the fallback tree-walk.
    let bars = ramp_bars(10);
    for (name, script) in [
        (
            "dynamic",
            "if close > 0.0\n    runtime.error(\"hot at \" + str.tostring(close))\nplot(close, title=\"C\")",
        ),
        (
            "non-string",
            "if close > 0.0\n    runtime.error(close)\nplot(close, title=\"C\")",
        ),
        (
            "bare",
            "if close > 0.0\n    runtime.error()\nplot(close, title=\"C\")",
        ),
        (
            "absent",
            "if close > 0.0\n    runtime.error(na)\nplot(close, title=\"C\")",
        ),
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("runtime.error links with full coverage");
        assert_eq!(
            fingerprint_result(&run_compiled_result(&modules, &bars)),
            fingerprint_result(&run_tree_result(&modules, &bars)),
            "{name}: refusal wording agrees"
        );
        assert!(
            run_compiled_result(&modules, &bars).is_err(),
            "{name}: the run refuses"
        );
    }
    let dynamic = link_script(
        "if close > 0.0\n    runtime.error(\"hot at \" + str.tostring(close))\nplot(close, title=\"C\")",
    );
    let wording = run_tree_result(&dynamic, &bars)
        .expect_err("tree refuses")
        .to_string();
    assert!(
        wording.contains("hot at 100"),
        "the dynamic text carries the composed value: {wording}"
    );
}

#[test]
fn runtime_error_is_only_live_when_reached() {
    // A `runtime.error` in a branch that never runs stays silent: the run
    // evaluates clean and agrees bar for bar, with full coverage throughout.
    let bars = ramp_bars(10);
    let modules =
        link_script("if close < 0.0\n    runtime.error(\"dead marker\")\nplot(close, title=\"C\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("dead-branch error links with full coverage");
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "dead-branch error stays silent"
    );
}

#[test]
fn computed_defaults_evaluate_in_caller_scope() {
    // An omitted default evaluates at the call site in the caller's scope,
    // in declaration order: a default referencing a caller local sees the
    // caller's value, never a callee binding. Full-coverage links prove the
    // loop (not the fallback tree-walk) binds them.
    let bars = ramp_bars(10);
    for (name, script, want) in [
        (
            "caller local",
            "f(x, y=a + 1.0) =>\n    x + y\na = 10.0\nv = f(1.0)\nplot(v, title=\"V\")",
            12.0,
        ),
        (
            "later default sees caller, not sibling",
            "x = 100.0\nf(x, y=x * 2.0) =>\n    x + y\nv = f(1.0)\nplot(v, title=\"V\")",
            201.0,
        ),
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("computed defaults link with full coverage");
        assert_eq!(
            canonical_form(&run_compiled(&modules, &bars)),
            canonical_form(&run_tree(&modules, &bars)),
            "{name}: caller-scope defaults agree"
        );
        let tree = run_tree(&modules, &bars);
        assert!(
            tree.plots
                .iter()
                .any(|plot| plot.values.iter().all(|cell| matches!(
                    cell,
                    Some(crate::PineValue::Float(v)) if (*v - want).abs() < 1e-9
                ))),
            "{name}: the default saw the caller scope, want {want}"
        );
    }
}

#[test]
fn default_only_and_named_argument_mix() {
    // Named, positional, and defaulted arguments mix in one call shape:
    // defaults fill only what the call leaves open, positionally after.
    let bars = ramp_bars(10);
    let modules = link_script(
        "base = 100.0\nf(a, b=2.0, c=base + 1.0) =>\n    a + b + c\nv1 = f(1.0)\nv2 = f(1.0, 3.0)\nv3 = f(1.0, c=10.0)\nv4 = f(a=1.0, b=2.0)\nplot(v1 + v2 + v3 + v4, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("mixed defaults link with full coverage");
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "mixed defaults agree"
    );
    let tree = run_tree(&modules, &bars);
    assert!(
        tree.plots
            .iter()
            .any(|plot| plot.values.iter().all(|cell| matches!(
                cell,
                Some(crate::PineValue::Float(v)) if (*v - 326.0).abs() < 1e-9
            ))),
        "v1=104, v2=105, v3=13, v4=104 sum to 326"
    );
}

#[test]
fn default_that_refuses_refuses_at_the_call_site() {
    // A refusing default expression refuses at the call site with the
    // tree-walk's exact wording on both engines.
    let bars = ramp_bars(10);
    let modules = link_script("f(x, y=missing(1)) =>\n    x\nv = f(1.0)\nplot(v, title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("refusing defaults link with full coverage");
    let tree = run_tree_result(&modules, &bars);
    let compiled = run_compiled_result(&modules, &bars);
    assert_eq!(
        fingerprint_result(&compiled),
        fingerprint_result(&tree),
        "refusing defaults agree"
    );
    assert!(tree.is_err(), "the tree refuses the default");
}

#[test]
fn method_computed_defaults_evaluate_in_caller_scope() {
    // A method call leaving a computed default open evaluates it at the
    // call site in the caller's scope and passes it by name: the
    // single-candidate signature is unambiguous, so the dispatch target
    // cannot matter.
    let bars = ramp_bars(10);
    let modules = link_script(
        "base = 10\ntype Accum\n    int total = 0\nmethod bump(Accum this, int d, int step = base) =>\n    this.total := d + step\n    this.total\nw = Accum.new()\ny = w.bump(3)\nplot(y, title=\"Y\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("method defaults link with full coverage");
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "method defaults agree"
    );
    let tree = run_tree(&modules, &bars);
    let thirteen = |cell: &Option<crate::PineValue>| match cell {
        Some(crate::PineValue::Int(v)) => *v == 13,
        Some(crate::PineValue::Float(v)) => (*v - 13.0).abs() < 1e-9,
        _ => false,
    };
    assert!(
        tree.plots
            .iter()
            .any(|plot| plot.values.iter().all(thirteen)),
        "the default saw the caller global (3 + 10)"
    );
}

#[test]
fn plain_read_of_unassigned_name_refuses() {
    // A plain read of a name bound nowhere refuses with the tree-walk's
    // exact wording on the loop: the program links with full coverage
    // first, so the agreement below proves the loop raised it, never the
    // fallback tree-walk.
    let bars = ramp_bars(10);
    let modules = link_script("v = nosuchvar\nplot(v, title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("unassigned reads link with full coverage");
    let tree = run_tree_result(&modules, &bars);
    let compiled = run_compiled_result(&modules, &bars);
    assert_eq!(
        fingerprint_result(&compiled),
        fingerprint_result(&tree),
        "unassigned reads refuse identically"
    );
    let reason = tree
        .expect_err("the tree refuses the unassigned read")
        .to_string();
    assert!(
        reason.contains("undefined variable 'nosuchvar'"),
        "the refusal names the variable: {reason}"
    );
}

#[test]
fn declared_but_unassigned_reads_na() {
    // A declared persistent cell with no value reads as absence on both
    // engines: the declaration alone never refuses, only truly unbound
    // names do.
    let bars = ramp_bars(10);
    let modules = link_script("var x = na\nv = x\nplot(v, title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("declared reads link with full coverage");
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "declared-but-unassigned reads agree"
    );
    assert!(
        run_tree(&modules, &bars)
            .plots
            .iter()
            .any(|plot| plot.values.iter().all(Option::is_none)),
        "the declared cell reads as absence"
    );
}

#[test]
fn history_read_of_unwritten_name_refuses() {
    // History over a name bound nowhere refuses with the tree-walk's
    // exact wording on the loop, with full coverage throughout.
    let bars = ramp_bars(10);
    let modules = link_script("v = nosuchvar[1]\nplot(v, title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("unwritten histories link with full coverage");
    let tree = run_tree_result(&modules, &bars);
    let compiled = run_compiled_result(&modules, &bars);
    assert_eq!(
        fingerprint_result(&compiled),
        fingerprint_result(&tree),
        "unwritten histories refuse identically"
    );
    let reason = tree
        .expect_err("the tree refuses the unwritten history")
        .to_string();
    assert!(
        reason.contains("undefined variable 'nosuchvar'"),
        "the refusal names the variable: {reason}"
    );
}

#[test]
fn stray_reassign_of_undeclared_name_refuses() {
    // `:=` on a name bound nowhere refuses with the tree-walk's exact
    // wording on the loop, with full coverage throughout: the checked
    // store fires only when reached, so a dead-branch rebind stays
    // silent on both engines.
    let bars = ramp_bars(10);
    let modules = link_script("q := 1.0\nplot(close, title=\"C\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("stray rebinds link with full coverage");
    let tree = run_tree_result(&modules, &bars);
    let compiled = run_compiled_result(&modules, &bars);
    assert_eq!(
        fingerprint_result(&compiled),
        fingerprint_result(&tree),
        "stray rebinds refuse identically"
    );
    let reason = tree
        .expect_err("the tree refuses the stray rebind")
        .to_string();
    assert!(
        reason.contains("':=' targets undeclared variable 'q'"),
        "the refusal names the target: {reason}"
    );
    let silent = link_script("if close < 0.0\n    q := 1.0\nplot(close, title=\"C\")");
    let program = compile_case(&silent);
    link_case(&program, &silent).expect("dead-branch rebinds link with full coverage");
    assert_eq!(
        canonical_form(&run_compiled(&silent, &bars)),
        canonical_form(&run_tree(&silent, &bars)),
        "dead-branch rebinds stay silent"
    );
}

#[test]
fn dotted_argument_writeback_matches_tree() {
    // Dotted call-argument homes lower through the shared home model:
    // a nested field, a map-entry alias, and a `this.field` spelling
    // all write back exactly like the tree-walk. Full-coverage links
    // prove the loop (not the fallback tree-walk) serves them.
    let bars = ramp_bars(10);
    for (name, script, want) in [
        (
            "nested field",
            "type Box\n    float v = 0.0\nid(x) => x\nb = Box.new()\nb.v := 3.0\ny = id(b.v)\nplot(y, title=\"V\")",
            3.0,
        ),
        (
            "this field",
            "type Counter\n    float n = 0.0\npeek(x) => x * 10.0\nmethod go(Counter this) =>\n    v = peek(this.n)\n    this.n := v\n    this.n\nc = Counter.new()\nc.n := 1.0\ny = c.go()\nplot(y, title=\"V\")",
            10.0,
        ),
        (
            "map-entry alias",
            "m = map.new<string, float>()\nmap.put(m, \"k\", 1.0)\nh = map.get(m, \"k\")\nid(x) => x\ny = id(h)\nplot(y + map.get(m, \"k\"), title=\"V\")",
            2.0,
        ),
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("dotted homes link with full coverage");
        assert_eq!(
            canonical_form(&run_compiled(&modules, &bars)),
            canonical_form(&run_tree(&modules, &bars)),
            "{name}: dotted homes agree"
        );
        let tree = run_tree(&modules, &bars);
        assert!(
            tree.plots.iter().any(|plot| plot.values.iter().all(|cell| matches!(
                cell,
                Some(crate::PineValue::Float(v)) if (*v - want).abs() < 1e-9
            ))),
            "{name}: want {want}"
        );
    }
}

#[test]
fn dotted_home_refusal_matches_tree() {
    // A dotted home over a missing field or a non-object root refuses
    // with the tree-walk's exact wording on the loop, with full coverage
    // throughout.
    let bars = ramp_bars(10);
    for (name, script, want) in [
        (
            "missing field",
            "type Box\n    float v = 0.0\nid(x) => x\nb = Box.new()\ny = id(b.missing)\nplot(y, title=\"V\")",
            "unknown field 'b.missing'",
        ),
        (
            "non-object root",
            "id(x) => x\nn = 5.0\ny = id(n.v)\nplot(y, title=\"V\")",
            "undefined variable 'n.v'",
        ),
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("refusing homes link with full coverage");
        let tree = run_tree_result(&modules, &bars);
        let compiled = run_compiled_result(&modules, &bars);
        assert_eq!(
            fingerprint_result(&compiled),
            fingerprint_result(&tree),
            "{name}: refusing homes agree"
        );
        let reason = tree.expect_err("the tree refuses").to_string();
        assert!(
            reason.contains(want),
            "{name}: the refusal names the path: {reason}"
        );
    }
}

#[test]
fn in_function_enum_member_resolves_like_tree() {
    // An enum from a strictly earlier module resolves inside a later
    // module's body to the same identity handle the tree-walk serves:
    // topological link order guarantees the declaration executed before
    // any call. Shadowing the enum name with a local object changes
    // nothing — the tree checks the registry before the object, and so
    // does the loop. Full-coverage links prove loop execution.
    let bars = ramp_bars(10);
    let enum_mod =
        crate::parse_program("export enum E\n    A = \"a\"\n    B = \"b\"").expect("parses");
    let user_mod = crate::parse_program(
        "type Box\n    float v = 0.0\nexport pick() =>\n    E.A == E.A ? 10.0 : 20.0\nexport shadow() =>\n    E = Box.new()\n    E.A == E.A ? 10.0 : 20.0",
    )
    .expect("parses");
    let driver = crate::parse_program(
        "import test/user/1 as U\nv = U.pick() + U.shadow()\nplot(v, title=\"V\")",
    )
    .expect("parses");
    let modules = vec![
        crate::LinkedModule::new("test/enum", enum_mod, Default::default()),
        crate::LinkedModule::new("test/user", user_mod, Default::default()),
        crate::LinkedModule::new(
            "local/Driver",
            driver,
            [("U".to_owned(), "test/user".to_owned())]
                .into_iter()
                .collect(),
        ),
    ];
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("in-function enums link with full coverage");
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "in-function enums agree"
    );
    let tree = run_tree(&modules, &bars);
    assert!(
        tree.plots
            .iter()
            .any(|plot| plot.values.iter().all(|cell| matches!(
                cell,
                Some(crate::PineValue::Float(v)) if (*v - 20.0).abs() < 1e-9
            ))),
        "both calls resolve the handle (10 + 10)"
    );
}

#[test]
fn barstate_flags_match_tree_in_closed_and_tick_runs() {
    // Per-bar flags serve from the run cursor on the loop, in closed-bar
    // and tick runs alike: newcomers, confirmations, history, realtime,
    // first, and last all agree bar for bar.
    let script = "v = (barstate.isfirst ? 1.0 : 0.0) + (barstate.islast ? 10.0 : 0.0) + (barstate.isnew ? 100.0 : 0.0) + (barstate.isconfirmed ? 1000.0 : 0.0) + (barstate.ishistory ? 10000.0 : 0.0) + (barstate.isrealtime ? 100000.0 : 0.0)\nplot(v, title=\"V\")";
    let bars = ramp_bars(10);
    let modules = link_script(script);
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("barstate flags link with full coverage");
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "closed-bar flags agree"
    );
    let closed = run_tree(&modules, &bars);
    let first = closed.plots[0].values.first().cloned().unwrap_or(None);
    let last = closed.plots[0].values.last().cloned().unwrap_or(None);
    assert_eq!(
        first,
        Some(crate::PineValue::Float(11101.0)),
        "bar zero flags"
    );
    assert_eq!(
        last,
        Some(crate::PineValue::Float(11110.0)),
        "last-bar flags"
    );
    let stamps: Vec<i64> = bars.iter().map(|bar| bar.ts.timestamp_millis()).collect();
    let mut tree_engine = PineEngine::new(engine_config());
    tree_engine.set_bytecode_mode(BytecodeMode::TreeWalk);
    let tree_ticks = tree_engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("tree tick run evaluates");
    let mut compiled_engine = PineEngine::new(engine_config());
    compiled_engine.set_bytecode_mode(BytecodeMode::Compiled);
    let compiled_ticks = compiled_engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("compiled tick run evaluates");
    assert_eq!(
        canonical_form(&compiled_ticks),
        canonical_form(&tree_ticks),
        "tick-run flags agree"
    );
    let tick_first = tree_ticks.plots[0].values.first().cloned().unwrap_or(None);
    assert_eq!(
        tick_first,
        Some(crate::PineValue::Float(101101.0)),
        "tick bar zero flags"
    );
}

#[test]
fn timenow_matches_tree_in_closed_and_tick_runs() {
    // The chart clock serves on the loop in closed-bar and tick runs
    // alike: time-leg floats for closed bars, integer tick stamps for
    // ticks — exactly like the tree-walk. A user binding shadows the
    // clock on both engines, so the shadowed spelling keeps its value.
    let bars = ramp_bars(5);
    let modules = link_script("plot(timenow, title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("timenow links with full coverage");
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "closed-bar clock agrees"
    );
    let closed = run_tree(&modules, &bars);
    let first = closed.plots[0].values.first().cloned().unwrap_or(None);
    assert!(
        matches!(first, Some(crate::PineValue::Float(_))),
        "closed bars serve time-leg floats: {first:?}"
    );
    let stamps: Vec<i64> = bars.iter().map(|bar| bar.ts.timestamp_millis()).collect();
    let mut tree_engine = PineEngine::new(engine_config());
    tree_engine.set_bytecode_mode(BytecodeMode::TreeWalk);
    let tree_ticks = tree_engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("tree tick run evaluates");
    let mut compiled_engine = PineEngine::new(engine_config());
    compiled_engine.set_bytecode_mode(BytecodeMode::Compiled);
    let compiled_ticks = compiled_engine
        .run_linked_ticks(&modules, &bars, &stamps, InputSet::new())
        .expect("compiled tick run evaluates");
    assert_eq!(
        canonical_form(&compiled_ticks),
        canonical_form(&tree_ticks),
        "tick clock agrees"
    );
    let tick_first = tree_ticks.plots[0].values.first().cloned().unwrap_or(None);
    assert_eq!(
        tick_first,
        Some(crate::PineValue::Int(stamps[0])),
        "ticks serve integer stamps"
    );
    let shadowed = link_script("timenow = 7.0\nplot(timenow, title=\"V\")");
    let shadow_program = compile_case(&shadowed);
    link_case(&shadow_program, &shadowed).expect("shadowed clock links");
    assert_eq!(
        canonical_form(&run_compiled(&shadowed, &bars)),
        canonical_form(&run_tree(&shadowed, &bars)),
        "shadowed clock agrees"
    );
    let shadowed_outcome = run_tree(&shadowed, &bars);
    assert!(
        shadowed_outcome.plots[0]
            .values
            .iter()
            .all(|cell| *cell == Some(crate::PineValue::Float(7.0))),
        "the binding shadows the clock: {:?}",
        shadowed_outcome.plots[0].values
    );
}

#[test]
fn computed_receiver_method_call_matches_tree() {
    // A method call on a computed receiver (a call return, not a held
    // variable) takes the receiver into a temporary and dispatches
    // through the shared home model; a field read off the same shape
    // serves through the shared table. Full-coverage links prove loop
    // execution on both shapes.
    let bars = ramp_bars(10);
    for (name, script, want) in [
        (
            "call receiver",
            "type Box2\n    int n = 1\nmethod bump(Box2 this) =>\n    this.n := this.n + 1\n    this.n\nmake() => Box2.new()\ny = make().bump()\nplot(y, title=\"V\")",
            2.0,
        ),
        (
            "call receiver field",
            "type Box2\n    int n = 1\nmake() => Box2.new()\ny = make().n + 10.0\nplot(y, title=\"V\")",
            11.0,
        ),
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("computed receivers link with full coverage");
        assert_eq!(
            canonical_form(&run_compiled(&modules, &bars)),
            canonical_form(&run_tree(&modules, &bars)),
            "{name}: computed receivers agree"
        );
        let tree = run_tree(&modules, &bars);
        assert!(
            tree.plots.iter().any(|plot| plot.values.iter().all(|cell| matches!(
                cell,
                Some(crate::PineValue::Float(v)) if (*v - want).abs() < 1e-9
            ))),
            "{name}: want {want}"
        );
    }
}

#[test]
fn library_fetch_call_matches_tree() {
    // A held-receiver library-shaped fetch (`bars.fetch()` in the
    // fixture) takes the instance out of its home, dispatches on its
    // runtime type, and writes the mutated copy back — mirroring the
    // tree-walk's take/put lifecycle on a synthetic analog.
    let bars = ramp_bars(10);
    let modules = link_script(
        "type T\n    int n = 0\nmethod fetch(T this) =>\n    this.n := this.n + 1\n    this.n\nvar t = T.new()\ny = t.fetch()\nplot(y, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("fetch calls link with full coverage");
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "fetch calls agree"
    );
    let tree = run_tree(&modules, &bars);
    let got: Vec<Option<f64>> = tree.plots[0]
        .values
        .iter()
        .map(|cell| match cell {
            Some(crate::PineValue::Float(v)) => Some(*v),
            Some(crate::PineValue::Int(v)) => Some(*v as f64),
            other => panic!("numeric fetch count, got {other:?}"),
        })
        .collect();
    assert_eq!(
        got,
        (1..=10).map(|v| Some(f64::from(v))).collect::<Vec<_>>(),
        "the fetch mutates through its home, once per bar"
    );
}

#[test]
fn alias_constructed_value_calls_match_tree() {
    // Cross-module construction (`ALIAS.Type.new()`) builds through the
    // exporting module's field table, and chained calls on the fresh
    // rvalue dispatch through the shared method index — both selections
    // observe the same object.
    let bars = ramp_bars(10);
    let lib = crate::parse_program("export type Widget\n    int n = 1\n").expect("parses");
    let user =
        crate::parse_program("import test/lib/1 as A\nexport run() =>\n    A.Widget.new().n")
            .expect("parses");
    let driver = crate::parse_program("import test/user/1 as U\nv = U.run()\nplot(v, title=\"V\")")
        .expect("parses");
    let modules = vec![
        crate::LinkedModule::new("test/lib", lib, Default::default()),
        crate::LinkedModule::new(
            "test/user",
            user,
            [("A".to_owned(), "test/lib".to_owned())]
                .into_iter()
                .collect(),
        ),
        crate::LinkedModule::new(
            "local/Driver",
            driver,
            [("U".to_owned(), "test/user".to_owned())]
                .into_iter()
                .collect(),
        ),
    ];
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("alias construction links with full coverage");
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "alias construction agrees"
    );
    let tree = run_tree(&modules, &bars);
    assert!(
        tree.plots
            .iter()
            .any(|plot| plot.values.iter().all(|cell| matches!(
                cell,
                Some(crate::PineValue::Float(v)) if (*v - 1.0).abs() < 1e-9
            ))),
        "the fresh value carries its default"
    );
}

#[test]
fn this_statement_calls_match_tree() {
    // A `this._helper(...)` call in statement position dispatches against
    // the innermost live receiver and writes the mutated copy back in
    // place — exactly like the value-position twin.
    let bars = ramp_bars(10);
    let modules = link_script(
        "type B\n    int n = 1\nmethod helper(B this, int d) =>\n    this.n := d\n    this.n\nmethod go(B this) =>\n    this.helper(5)\n    this.n\nw = B.new()\ny = w.go()\nplot(y, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("this statement calls link with full coverage");
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "this statement calls agree"
    );
    let tree = run_tree(&modules, &bars);
    assert!(
        tree.plots
            .iter()
            .any(|plot| plot.values.iter().all(|cell| matches!(
                cell,
                Some(crate::PineValue::Float(v)) if (*v - 5.0).abs() < 1e-9
            ))),
        "the helper mutation persists"
    );
}

#[test]
fn nested_this_home_refusal_matches_tree() {
    // A multi-dot `this` argument home lowers (its value read refuses the
    // unknown dotted field before any write-back could run), so both
    // engines refuse naming the same path with full coverage throughout.
    let bars = ramp_bars(10);
    let modules = link_script(
        "type H\n    array<float> xs = array.new_float(0)\nmethod go(H this) =>\n    array.push(this.xs, 1.0)\n    array.clear(this.xs.a)\n    0.0\nw = H.new()\ny = w.go()\nplot(y, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("nested this homes link with full coverage");
    let tree = run_tree_result(&modules, &bars);
    let compiled = run_compiled_result(&modules, &bars);
    assert_eq!(
        fingerprint_result(&compiled),
        fingerprint_result(&tree),
        "nested this homes refuse identically"
    );
    let reason = tree.expect_err("the tree refuses").to_string();
    assert!(
        reason.contains("xs.a"),
        "the refusal names the dotted path: {reason}"
    );
}

#[test]
fn persistent_shadowing_writes_stay_tree_walk() {
    // Two call paths plain-bind one top-level persistent cell: the
    // tree-walk persists those writes to the shared cell (last-wins
    // across paths) while the loop's journal reverts them on return —
    // so the shape keeps its fallback and both selections agree through
    // the tree-walk. Pinning the divergence keeps the veto honest.
    let bars = ramp_bars(10);
    let modules = link_script(
        "var x = 0\nf() =>\n    x = 10\n    x\ng() =>\n    x = 20\n    x\na = f()\nb = g()\nc = x\nplot(a + b + c, title=\"V\")",
    );
    let program = compile_case(&modules);
    assert!(
        matches!(
            link_case(&program, &modules),
            Err(crate::pine::bytecode_exec::LinkError::Fallback { .. })
        ),
        "persistent shadowing writes keep their fallback"
    );
    let tree = run_tree_result(&modules, &bars);
    let compiled = run_compiled_result(&modules, &bars);
    assert_eq!(
        fingerprint_result(&compiled),
        fingerprint_result(&tree),
        "selections agree via fallback"
    );
    let outcome = tree.expect("tree serves the shared cell");
    assert!(
        outcome
            .plots
            .iter()
            .any(|plot| plot.values.iter().all(|cell| matches!(
                cell,
                Some(crate::PineValue::Float(v)) if (*v - 50.0).abs() < 1e-9
            ))),
        "shared last-wins sums to fifty every bar"
    );
}

#[test]
fn debug_fill_divergence() {
    let modules = single_modules(
        "export get() =>\n    b = [1.0]\n    array.shift(b)\n    close",
        "pa = plot(close, \"A\")\npb = plot(open, \"B\")\nfill(pa, pb)\nv = L.get()\nplot(v, title=\"V\")",
    );
    let bars = ramp_bars(3);
    let tree = run_tree(&modules, &bars);
    let vm = run_compiled(&modules, &bars);
    let a = canonical_form(&vm);
    let b = canonical_form(&tree);
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        if x != y {
            eprintln!("DIFF line {i}\n vm: {x}\ntree: {y}");
        }
    }
}

#[test]
fn triggers_and_classifiers_are_pinned() {
    // The calibration milestone's ledger: every gate is registered with the
    // kind of decision it may make, the pinned constants are unchanged, and
    // no confidence gate appears among the correctness classifiers.
    use crate::pine::trigger::{trigger_kind, TriggerKind};
    assert_eq!(TIER_STABLE_BARS, 4, "the tier-up warmup is pinned");
    assert_eq!(SPLIT_BRAIN_COVERAGE, 1.0, "a partial program never splits");
    assert_eq!(MAX_CALL_DEPTH, 64, "the call bound is pinned");
    assert_eq!(MAX_LOOP_ITERATIONS, 10_000, "the loop bound is pinned");
    for (name, kind) in crate::pine::trigger::TRIGGERS {
        assert_eq!(trigger_kind(name), Some(*kind), "{name} is registered once");
    }
    for correctness in [
        "split_brain_coverage",
        "max_call_depth",
        "max_loop_iterations",
        "headless_effect",
    ] {
        assert_eq!(
            trigger_kind(correctness),
            Some(TriggerKind::Correctness),
            "{correctness} decides correctness"
        );
    }
    assert_eq!(
        trigger_kind("tier_stable_bars"),
        Some(TriggerKind::Confidence),
        "the only representation gate is confidence"
    );
    assert_eq!(
        trigger_kind("unknown-gate"),
        None,
        "unregistered gates do not exist"
    );
    let confidence = crate::pine::trigger::TRIGGERS
        .iter()
        .filter(|(_, kind)| *kind == TriggerKind::Confidence)
        .count();
    assert_eq!(
        confidence, 1,
        "exactly one gate may trade speed for representation"
    );
}

#[test]
fn tier_constants_are_pinned() {
    // Source-text pins for every gate constant: the value and its type
    // at the declaration site, so a moved, retyped, or retuned gate
    // fails here before its axis can drift. The registry test pins the
    // kinds; this pins the declarations.
    for (stem, source, declaration) in [
        (
            "tier-up warmup",
            include_str!("../src/pine/bytecode_exec.rs"),
            "pub const TIER_STABLE_BARS: u32 = 4;",
        ),
        (
            "link coverage",
            include_str!("../src/pine/bytecode_exec.rs"),
            "pub const SPLIT_BRAIN_COVERAGE: f64 = 1.0;",
        ),
        (
            "call bound",
            include_str!("../src/pine/engine.rs"),
            "pub const MAX_CALL_DEPTH: usize = 64;",
        ),
        (
            "loop bound",
            include_str!("../src/pine/engine.rs"),
            "pub const MAX_LOOP_ITERATIONS: usize = 10_000;",
        ),
    ] {
        assert!(
            source.contains(declaration),
            "{stem} declaration moved or retuned: {declaration}"
        );
    }
    assert_eq!(TIER_STABLE_BARS, 4, "the tier-up warmup reads pinned");
    assert_eq!(SPLIT_BRAIN_COVERAGE, 1.0, "a partial program never splits");
}

#[test]
fn bare_true_range_matches_tree() {
    // Legacy bare `tr` reads true range at the cursor (high-low on the
    // first bar, the full three-way max after): a user binding shadows
    // the builtin on both engines, so the shadowed spelling keeps its
    // value.
    let bars = ramp_bars(5);
    let modules = link_script("plot(tr, title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("bare tr links with full coverage");
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "true range agrees"
    );
    let outcome = run_tree(&modules, &bars);
    let first = outcome.plots[0].values.first().cloned().unwrap_or(None);
    assert!(
        matches!(first, Some(crate::PineValue::Float(_))),
        "bar zero serves high-low: {first:?}"
    );
    let shadowed = link_script("tr = 7.0\nplot(tr, title=\"V\")");
    let shadow_program = compile_case(&shadowed);
    link_case(&shadow_program, &shadowed).expect("shadowed tr links");
    assert_eq!(
        canonical_form(&run_compiled(&shadowed, &bars)),
        canonical_form(&run_tree(&shadowed, &bars)),
        "shadowed tr agrees"
    );
    let shadowed_outcome = run_tree(&shadowed, &bars);
    assert!(
        shadowed_outcome.plots[0]
            .values
            .iter()
            .all(|cell| *cell == Some(crate::PineValue::Float(7.0))),
        "the binding shadows the builtin: {:?}",
        shadowed_outcome.plots[0].values
    );
}

#[test]
fn legacy_bare_ta_names_route_to_the_bridge() {
    // Pre-namespace scripts call `sma(...)` bare: an unclaimed name serves
    // through the shared `ta` namespace with the identical site key a
    // `ta.`-qualified call builds, so both spellings key one history. A
    // user definition shadows the bridge on both engines.
    let bars = ramp_bars(10);
    let modules = link_script("a = sma(close, 3)\nb = ta.sma(close, 3)\nplot(a, title=\"A\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("bare ta names link with full coverage");
    let tree = run_tree(&modules, &bars);
    let compiled = run_compiled(&modules, &bars);
    assert_eq!(
        canonical_form(&compiled),
        canonical_form(&tree),
        "bare and qualified spellings agree"
    );
    let first = tree.plots[0].values.first().cloned().unwrap_or(None);
    assert!(
        matches!(first, Some(crate::PineValue::Float(_)) | None),
        "the average serves a number or warming absence: {first:?}"
    );
    let shadowed = link_script("sma(a, b) => a - b\nv = sma(10.0, 3.0)\nplot(v, title=\"V\")");
    let shadow_program = compile_case(&shadowed);
    link_case(&shadow_program, &shadowed).expect("shadowed name links");
    assert_eq!(
        canonical_form(&run_compiled(&shadowed, &bars)),
        canonical_form(&run_tree(&shadowed, &bars)),
        "a user definition shadows the bridge"
    );
    let shadowed_outcome = run_tree(&shadowed, &bars);
    assert!(
        shadowed_outcome.plots[0]
            .values
            .iter()
            .all(|cell| *cell == Some(crate::PineValue::Float(7.0))),
        "the user body decides: {:?}",
        shadowed_outcome.plots[0].values
    );
}

#[test]
fn legacy_bare_math_names_route_to_the_bridge() {
    // Pre-namespace scripts call `pow(...)` bare: an unclaimed name serves
    // through the shared `math` namespace with the identical site key a
    // `math.`-qualified call builds. A user definition shadows the bridge
    // on both engines.
    let bars = ramp_bars(5);
    let modules = link_script("a = pow(2.0, 8.0)\nb = math.pow(2.0, 8.0)\nplot(a, title=\"A\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("bare math names link with full coverage");
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "bare and qualified spellings agree"
    );
    let outcome = run_tree(&modules, &bars);
    assert!(
        outcome.plots[0]
            .values
            .iter()
            .all(|cell| *cell == Some(crate::PineValue::Float(256.0))),
        "two to the eighth: {:?}",
        outcome.plots[0].values
    );
    let shadowed = link_script("pow(a, b) => a + b\nv = pow(10.0, 3.0)\nplot(v, title=\"V\")");
    let shadow_program = compile_case(&shadowed);
    link_case(&shadow_program, &shadowed).expect("shadowed name links");
    assert_eq!(
        canonical_form(&run_compiled(&shadowed, &bars)),
        canonical_form(&run_tree(&shadowed, &bars)),
        "a user definition shadows the bridge"
    );
}

#[test]
fn legacy_bare_tostring_routes_to_the_bridge() {
    // Pre-namespace scripts call `tostring(...)` bare: an unclaimed name
    // serves through the shared `str` namespace with the identical site
    // key a `str.`-qualified call builds.
    let bars = ramp_bars(3);
    let modules = link_script("a = tostring(close)\nb = str.tostring(close)\nplot(close, title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("bare tostring links with full coverage");
    assert_eq!(
        canonical_form(&run_compiled(&modules, &bars)),
        canonical_form(&run_tree(&modules, &bars)),
        "bare and qualified spellings agree"
    );
}

#[test]
fn construction_binds_field_arguments_positionally_and_by_name() {
    // `Type.new(...)` fills fields in declaration order positionally and
    // by name; unfilled fields evaluate their defaults. Extra positionals
    // and unknown names refuse identically on both engines.
    let bars = ramp_bars(5);
    let modules = link_script(
        "type Pair\n    float first\n    float second = 99.0\np = Pair.new(1.0, 2.0)\nq = Pair.new(10.0)\nr = Pair.new(second=7.0, first=3.0)\nplot(p.first + p.second + q.first + q.second + r.first + r.second, title=\"V\")",
    );
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("field arguments link with full coverage");
    let tree = run_tree(&modules, &bars);
    let compiled = run_compiled(&modules, &bars);
    // Object field maps hash in insertion order, so the full canonical
    // form (snapshot `Debug`) is order-unstable for instances on both
    // engines: compare evaluated counts plus plot values, which carry the
    // constructed outcomes.
    assert_eq!(compiled.evaluated, tree.evaluated);
    assert_eq!(compiled.plots.len(), tree.plots.len());
    for (got, want) in compiled.plots.iter().zip(tree.plots.iter()) {
        assert_eq!(got.values, want.values, "constructed fields agree");
    }
    let outcome = tree;
    assert!(
        outcome.plots[0]
            .values
            .iter()
            .all(|cell| *cell == Some(crate::PineValue::Float(122.0))),
        "1+2, 10+99, 3+7: {:?}",
        outcome.plots[0].values
    );
    for (name, script) in [
        ("extra positional", "type Pair\n    float first\np = Pair.new(1.0, 2.0)\nplot(p.first, title=\"V\")"),
        ("unknown name", "type Pair\n    float first\np = Pair.new(nope=1.0)\nplot(p.first, title=\"V\")"),
    ] {
        let bad = link_script(script);
        let tree = run_tree_result(&bad, &bars).expect_err("tree refuses");
        let compiled = run_compiled_result(&bad, &bars).expect_err("loop refuses");
        assert_eq!(
            tree.to_string(),
            compiled.to_string(),
            "{name}: one wording across engines"
        );
    }
}

#[test]
fn collection_receivers_dispatch_through_object_chains() {
    // Method calls on collections held anywhere (a variable, an object
    // field, a parameter's field) dispatch to the collection bridge on
    // both engines: reads serve, statement-position mutators persist
    // through the holder, and value-position calls leave the holder
    // untouched — exactly like the namespace spelling.
    let bars = ramp_bars(5);
    for (name, script, want) in [
        (
            "single-level read",
            "a = array.from(1.0, 2.0)\nv = a.size()\nplot(v, title=\"V\")",
            2.0,
        ),
        (
            "single-level mutating statement",
            "a = array.from(1.0, 2.0)\na.push(3.0)\nv = a.size()\nplot(v, title=\"V\")",
            3.0,
        ),
        (
            "value-position call leaves the holder",
            "a = array.from(1.0)\nv = a.push(2.0)\nw = a.size()\nplot(w, title=\"V\")",
            1.0,
        ),
        (
            "shift statement persists the rest",
            "a = array.from(1.0, 2.0, 3.0)\na.shift()\nv = a.size()\nplot(v, title=\"V\")",
            2.0,
        ),
        (
            "shift value yields the head",
            "a = array.from(5.0, 6.0)\nv = a.shift()\nplot(v, title=\"V\")",
            5.0,
        ),
        (
            "pop statement persists the rest",
            "a = array.from(1.0, 2.0, 3.0)\na.pop()\nv = a.size()\nplot(v, title=\"V\")",
            2.0,
        ),
        (
            "unshift statement grows the holder",
            "a = array.from(1.0, 2.0)\na.unshift(0.0)\nv = a.size()\nplot(v, title=\"V\")",
            3.0,
        ),
        (
            "nested field read",
            "type Box2\n    int[] items = array.from(1.0)\nw = Box2.new()\nw.items.push(7.0)\nv = w.items.size()\nplot(v, title=\"V\")",
            2.0,
        ),
        (
            "parameter field mutates in the call",
            "type Box2\n    int[] items = array.from(1.0)\npack(Box2 b) =>\n    b.items.push(7.0)\n    b.items.size()\nw = Box2.new()\nv = pack(w)\nplot(v, title=\"V\")",
            2.0,
        ),
        (
            "receiver field mutates through the alias",
            "type Box2\n    int[] items = array.from(1.0)\nmethod grow(Box2 b, float v) =>\n    b.items.push(v)\n    b.items.size()\nw = Box2.new()\ns = w.grow(5.0)\nplot(s, title=\"V\")",
            2.0,
        ),
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("collection receivers link with full coverage");
        let tree = run_tree(&modules, &bars);
        let compiled = run_compiled(&modules, &bars);
        assert_eq!(
            canonical_form(&compiled),
            canonical_form(&tree),
            "{name}: both engines agree"
        );
        assert!(
            tree.plots[0]
                .values
                .iter()
                .all(|cell| cell.clone().map(|value| match value {
                    crate::PineValue::Float(number) => number,
                    crate::PineValue::Int(number) => number as f64,
                    _ => f64::NAN,
                }) == Some(want)),
            "{name}: want {want}: {:?}",
            tree.plots[0].values
        );
    }
}

#[test]
fn for_by_strides_the_counter_on_both_engines() {
    // `for i = 0 to 6 by 2` visits 0, 2, 4, 6; a descending range strides
    // down; a non-positive stride refuses naming the rule — identically
    // on both engines.
    let bars = ramp_bars(3);
    for (name, script, want) in [
        ("ascending stride", "t = 0\nfor i = 0 to 6 by 2\n    t := t + i\nplot(t, title=\"V\")", 12.0),
        ("descending stride", "t = 0\nfor i = 10 to 4 by 3\n    t := t + i\nplot(t, title=\"V\")", 21.0),
    ] {
        let modules = link_script(script);
        let program = compile_case(&modules);
        link_case(&program, &modules).expect("strided loops link with full coverage");
        let tree = run_tree(&modules, &bars);
        let compiled = run_compiled(&modules, &bars);
        assert_eq!(
            canonical_form(&compiled),
            canonical_form(&tree),
            "{name}: both engines agree"
        );
        assert!(
            tree.plots[0].values.iter().all(|cell| *cell
                == Some(crate::PineValue::Float(want))),
            "{name}: want {want}: {:?}",
            tree.plots[0].values
        );
    }
    for script in [
        "for i = 0 to 6 by 0\n    x = i",
        "for i = 0 to 6 by -2\n    x = i",
    ] {
        let modules = link_script(script);
        let tree = run_tree_result(&modules, &bars).expect_err("tree refuses");
        let compiled = run_compiled_result(&modules, &bars).expect_err("loop refuses");
        assert_eq!(
            tree.to_string(),
            compiled.to_string(),
            "one wording across engines: {script}"
        );
        assert!(
            tree.to_string().contains("by"),
            "the refusal names the rule: {tree}"
        );
    }
}

#[test]
fn bare_timeframe_in_seconds_reads_the_chart_period() {
    // `timeframe.in_seconds()` with no arguments reports the chart
    // period in seconds (here the one-minute harness period): the same
    // shared resolver serves both engines, so config and value agree.
    let bars = ramp_bars(3);
    let modules = link_script("x = timeframe.in_seconds()\nplot(x, title=\"V\")");
    let program = compile_case(&modules);
    link_case(&program, &modules).expect("bare in_seconds links with full coverage");
    let tree = run_tree(&modules, &bars);
    let compiled = run_compiled(&modules, &bars);
    assert_eq!(
        canonical_form(&compiled),
        canonical_form(&tree),
        "both engines agree"
    );
    assert!(
        tree.plots[0]
            .values
            .iter()
            .all(|cell| *cell == Some(crate::PineValue::Int(60))),
        "one minute is sixty seconds: {:?}",
        tree.plots[0].values
    );
}
