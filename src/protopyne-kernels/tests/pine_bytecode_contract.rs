#![cfg(feature = "bytecode")]
//! Fixed-corpus outcome contract: the tree-walk and the compiled selection
//! reproduce one checked-in canonical rendering per case, the shared refusal
//! wordings are inventoried, the engine-selection call sites are listed, and
//! both selections agree over the fixtures plus the data root.
//!
//! Test path note: this file rides the `pine` unit module, so the
//! runnable filter is `pine`, not the file stem.

use super::bytecode_diff_tests::{fingerprint_result, link_script, ramp_bars};
use crate::pine::test_fixture as fixture;
use crate::pine::BytecodeMode;
use crate::{InputSet, LinkedModule, PineEngine, PineRuntimeConfig, RunOutcome};
use protopyne_types::Bar;
use std::collections::BTreeMap;

fn engine_config() -> PineRuntimeConfig {
    PineRuntimeConfig::new()
        .chart_symbol("SPY".to_owned())
        .chart_period("1".to_owned())
        .warmup_bars(0)
        .build()
}

struct ContractCase {
    name: &'static str,
    modules: Vec<LinkedModule>,
    bars: Vec<Bar>,
    inject_security: bool,
}

fn contract_corpus() -> Vec<ContractCase> {
    vec![
        ContractCase {
            name: "fixture_fastmas",
            modules: fixture::linked_fixture("_FastMAs"),
            bars: ramp_bars(20),
            inject_security: false,
        },
        ContractCase {
            name: "fixture_integration_lite",
            modules: fixture::linked_fixture("_IntegrationLite"),
            bars: ramp_bars(20),
            inject_security: false,
        },
        ContractCase {
            name: "numeric",
            modules: link_script("a = close + open * 2.0\nplot(a + 1.0, title=\"A\")"),
            bars: ramp_bars(20),
            inject_security: false,
        },
        ContractCase {
            name: "strategy",
            modules: link_script("strategy.entry(\"E\", strategy.long)\nplot(close, title=\"V\")"),
            bars: ramp_bars(10),
            inject_security: false,
        },
        ContractCase {
            name: "drawing",
            modules: link_script("l = line.new(bar_index, low, bar_index, high)\nplot(close, title=\"V\")"),
            bars: ramp_bars(10),
            inject_security: false,
        },
        ContractCase {
            name: "injected_request",
            modules: link_script(
                "probe = request.security(\"SPY\", \"D\", [close, ta.sma(close, 3)], barmerge.gaps_off, barmerge.lookahead_off, calc_bars_count=10)\nplot(close, title=\"V\")",
            ),
            bars: ramp_bars(10),
            inject_security: true,
        },
    ]
}

fn run_case(
    case: &ContractCase,
    mode: Option<BytecodeMode>,
) -> Result<RunOutcome, crate::KernelError> {
    let mut engine = PineEngine::new(engine_config());
    engine.set_bytecode_mode(mode.unwrap_or(BytecodeMode::TreeWalk));
    if case.inject_security {
        engine
            .inject_security("SPY", "D", fixture::daily_tape(40))
            .expect("contract security tape injects");
    }
    engine.run_linked(&case.modules, &case.bars, InputSet::new())
}

fn contract_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/bytecode_contract.json")
}

fn refusal_inventory_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/refusal_inventory.json")
}

fn stored_contract() -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(contract_path()).expect("contract snapshot loads");
    let value: serde_json::Value = serde_json::from_str(&text).expect("contract snapshot parses");
    value
        .get("cases")
        .expect("contract snapshot has cases")
        .as_object()
        .expect("cases is a map")
        .iter()
        .map(|(name, form)| {
            (
                name.clone(),
                form.as_str().expect("case form is a string").to_owned(),
            )
        })
        .collect()
}

#[test]
fn contract_golden_outcomes_reproduce() {
    // The fixed corpus reproduces one checked-in canonical rendering per
    // case on both selections: the snapshot records the canonical form, so
    // a failure names the bar and key that drifted.
    if std::env::var("UPDATE_CONTRACT_GOLDEN").as_deref() == Ok("1") {
        let mut cases = BTreeMap::new();
        for case in contract_corpus() {
            let outcome = run_case(&case, None);
            cases.insert(case.name.to_owned(), fingerprint_result(&outcome));
        }
        let snapshot = serde_json::json!({ "cases": cases });
        std::fs::write(
            contract_path(),
            serde_json::to_string_pretty(&snapshot).expect("snapshot renders") + "\n",
        )
        .expect("snapshot writes");
        return;
    }
    let stored = stored_contract();
    assert_eq!(
        stored.len(),
        contract_corpus().len(),
        "the snapshot covers every corpus case"
    );
    for case in contract_corpus() {
        let expected = stored
            .get(case.name)
            .unwrap_or_else(|| panic!("snapshot covers {}", case.name));
        let tree = run_case(&case, None);
        let compiled = run_case(&case, Some(BytecodeMode::Compiled));
        assert_eq!(
            fingerprint_result(&tree),
            fingerprint_result(&compiled),
            "{}: selections diverge",
            case.name
        );
        let tree_form = fingerprint_result(&tree);
        assert_eq!(&tree_form, expected, "{}: canonical form drifted", case.name);
    }
}

struct RefusalProbe {
    id: &'static str,
    script: &'static str,
    contains: &'static str,
}

fn refusal_probes() -> Vec<RefusalProbe> {
    vec![
        RefusalProbe {
            id: "unknown_function",
            script: "plot(nosuchfn(close), title=\"V\")",
            contains: "unknown function 'nosuchfn'",
        },
        RefusalProbe {
            id: "plot_missing_series",
            script: "plot()",
            contains: "'plot' takes a series to draw",
        },
        RefusalProbe {
            id: "na_arity",
            script: "v = na(1, 2)\nplot(v, title=\"V\")",
            contains: "'na' takes zero or one argument",
        },
        RefusalProbe {
            id: "nz_arity",
            script: "v = nz(close, 1, 2)\nplot(v, title=\"V\")",
            contains: "'nz' takes one or two arguments",
        },
        RefusalProbe {
            id: "input_missing_default",
            script: "v = input()\nplot(close, title=\"V\")",
            contains: "'input' takes a default value",
        },
        RefusalProbe {
            id: "call_depth_over_limit",
            script: "f(x) => f(x)\nplot(f(close), title=\"V\")",
            contains: "call depth exceeds 64",
        },
        RefusalProbe {
            id: "live_request",
            script: "v = request.security(\"SPY\", \"D\", close)\nplot(v, title=\"V\")",
            contains: "is refused headlessly",
        },
        RefusalProbe {
            id: "array_index_out_of_bounds",
            script: "a = [1.0, 2.0]\nv = a[5]\nplot(v, title=\"V\")",
            contains: "array index 5 out of bounds",
        },
        RefusalProbe {
            id: "undefined_variable",
            script: "plot(nosuchvar, title=\"V\")",
            contains: "undefined variable 'nosuchvar'",
        },
        RefusalProbe {
            id: "reassign_undeclared",
            script: "x := 1\nplot(x, title=\"V\")",
            contains: "':=' targets undeclared variable 'x'",
        },
        RefusalProbe {
            id: "destructure_arity_mismatch",
            script: "[a, b] = [1.0]\nplot(a, title=\"V\")",
            contains: "destructuring arity 1 does not match 2 targets",
        },
        RefusalProbe {
            id: "for_range_over_limit",
            script: "for i = 0 to 20000\nplot(i, title=\"I\")",
            contains: "'for' range 0..=20000 exceeds 10000 iterations",
        },
    ]
}

fn run_refusal(script: &str) -> String {
    let modules = link_script(script);
    let bars = ramp_bars(5);
    let mut engine = PineEngine::new(engine_config());
    match engine.run_linked(&modules, &bars, InputSet::new()) {
        Ok(_) => panic!("probe evaluates, expected a refusal: {script}"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn refusal_inventory_is_complete() {
    // Every shared refusal wording is produced by exactly one probe, and
    // the full refusal strings match the checked-in inventory.
    if std::env::var("UPDATE_CONTRACT_GOLDEN").as_deref() == Ok("1") {
        let mut inventory = BTreeMap::new();
        for probe in refusal_probes() {
            inventory.insert(probe.id.to_owned(), run_refusal(probe.script));
        }
        let snapshot = serde_json::json!({ "refusals": inventory });
        std::fs::write(
            refusal_inventory_path(),
            serde_json::to_string_pretty(&snapshot).expect("inventory renders") + "\n",
        )
        .expect("inventory writes");
        return;
    }
    let text =
        std::fs::read_to_string(refusal_inventory_path()).expect("refusal inventory loads");
    let value: serde_json::Value =
        serde_json::from_str(&text).expect("refusal inventory parses");
    let stored: BTreeMap<String, String> = value
        .get("refusals")
        .expect("inventory has refusals")
        .as_object()
        .expect("refusals is a map")
        .iter()
        .map(|(id, refusal)| {
            (
                id.clone(),
                refusal.as_str().expect("refusal is a string").to_owned(),
            )
        })
        .collect();
    let probes = refusal_probes();
    assert_eq!(
        stored.len(),
        probes.len(),
        "the inventory covers every probe"
    );
    let mut refusals = Vec::new();
    for probe in &probes {
        let refusal = run_refusal(probe.script);
        assert!(
            refusal.contains(probe.contains),
            "{}: refusal {refusal:?} names {}",
            probe.id,
            probe.contains
        );
        let expected = stored
            .get(probe.id)
            .unwrap_or_else(|| panic!("inventory covers {}", probe.id));
        assert_eq!(&refusal, expected, "{}: refusal wording drifted", probe.id);
        refusals.push(refusal);
    }
    for probe in &probes {
        let owners = refusals
            .iter()
            .filter(|refusal| refusal.contains(probe.contains))
            .count();
        assert_eq!(
            owners, 1,
            "{}: wording {:?} appears in {owners} probes, want exactly one",
            probe.id, probe.contains
        );
    }
}

#[test]
fn engine_selection_sites_are_known() {
    // The engine selection is explicit and listed: the one production
    // facade plus the named suites and bench. A new call site fails here
    // until it joins the list.
    let facade = include_str!("../../../src/protopyne-strategy/src/signals_pine.rs");
    assert_eq!(
        facade.matches("set_bytecode_mode").count(),
        1,
        "the production facade holds the one production call site"
    );
    for (stem, source) in [
        (
            "pine_bytecode_integration",
            include_str!("pine_bytecode_integration.rs"),
        ),
        (
            "pine_bytecode_calibration",
            include_str!("pine_bytecode_calibration.rs"),
        ),
        (
            "pine_bytecode_closeout",
            include_str!("pine_bytecode_closeout.rs"),
        ),
        (
            "pine_bytecode_contract",
            include_str!("pine_bytecode_contract.rs"),
        ),
        ("pine_eval_bench", include_str!("../benches/pine_eval.rs")),
    ] {
        assert!(
            source.contains("set_bytecode_mode"),
            "{stem}: listed suite holds its call sites"
        );
    }
    for (stem, source) in [
        (
            "pine_indicator",
            include_str!("../../../src/protopyne-egui/src/pine_indicator.rs"),
        ),
        (
            "server_service",
            include_str!("../../../src/protopyne-server/src/service.rs"),
        ),
    ] {
        assert!(
            !source.contains("set_bytecode_mode"),
            "{stem}: production path keeps the default selection"
        );
    }
}

#[test]
fn production_code_never_names_the_oracle_engine() {
    // The tree engine is a test collaborator, never a production
    // dependency: no shipping file may name it. A production use fails
    // here until the dependency is removed.
    for (stem, source) in [
        (
            "strategy_facade",
            include_str!("../../../src/protopyne-strategy/src/signals_pine.rs"),
        ),
        (
            "pine_indicator",
            include_str!("../../../src/protopyne-egui/src/pine_indicator.rs"),
        ),
        (
            "server_service",
            include_str!("../../../src/protopyne-server/src/service.rs"),
        ),
        ("kernels_lib", include_str!("../src/lib.rs")),
        ("pine_surface", include_str!("../src/pine/mod.rs")),
        ("pine_facade", include_str!("../src/pine/engine.rs")),
        ("pine_leaves", include_str!("../src/pine/semantics.rs")),
    ] {
        assert!(
            !source.contains("OracleEngine"),
            "{stem}: production code names the tree engine"
        );
    }
}

#[test]
fn compiled_and_tree_agree_over_corpus_and_data_root() {
    // Parity target over the fixed corpus, every hermetic fixture, and
    // every data-root closure that links: both selections report the same
    // fingerprint, so the golden above pins the value and this test pins
    // the agreement.
    let mut checked = 0;
    for case in contract_corpus() {
        let tree = run_case(&case, None);
        let compiled = run_case(&case, Some(BytecodeMode::Compiled));
        assert_eq!(
            fingerprint_result(&compiled),
            fingerprint_result(&tree),
            "{}: selections diverge",
            case.name
        );
        checked += 1;
    }
    for outcome in fixture::fixture_outcomes() {
        let (stem, closure) = match outcome {
            crate::ClosureOutcome::Linked(closure) => (closure.stem.clone(), closure),
            crate::ClosureOutcome::Refused { .. } => continue,
        };
        let Ok(inputs) = crate::closure_inputs(&closure) else {
            continue;
        };
        let modules = fixture::linked_modules(inputs);
        let bars = ramp_bars(10);
        let mut tree_engine = PineEngine::new(engine_config());
        tree_engine.set_bytecode_mode(BytecodeMode::TreeWalk);
        let tree = tree_engine.run_linked(&modules, &bars, InputSet::new());
        let mut compiled_engine = PineEngine::new(engine_config());
        compiled_engine.set_bytecode_mode(BytecodeMode::Compiled);
        let compiled = compiled_engine.run_linked(&modules, &bars, InputSet::new());
        assert_eq!(
            fingerprint_result(&compiled),
            fingerprint_result(&tree),
            "{stem}: fixture selections diverge"
        );
        assert_eq!(
            fingerprint_result(&compiled),
            fingerprint_result(&tree),
            "{stem}: fixture selections diverge"
        );
        checked += 1;
    }
    let root = fixture::data_root();
    if root.exists() {
        let (indicators, libraries) =
            crate::manifest_files::load_pine_sources(&root).expect("data root loads");
        for outcome in crate::discover_closures(&indicators, &libraries) {
            let (stem, closure) = match outcome {
                crate::ClosureOutcome::Linked(closure) => (closure.stem.clone(), closure),
                crate::ClosureOutcome::Refused { .. } => continue,
            };
            let Ok(inputs) = crate::closure_inputs(&closure) else {
                continue;
            };
            let modules = fixture::linked_modules(inputs);
            let bars = ramp_bars(10);
            let mut tree_engine = PineEngine::new(engine_config());
            tree_engine.set_bytecode_mode(BytecodeMode::TreeWalk);
            let tree = tree_engine.run_linked(&modules, &bars, InputSet::new());
            let mut compiled_engine = PineEngine::new(engine_config());
            compiled_engine.set_bytecode_mode(BytecodeMode::Compiled);
            let compiled = compiled_engine.run_linked(&modules, &bars, InputSet::new());
            assert_eq!(
                fingerprint_result(&compiled),
                fingerprint_result(&tree),
                "{stem}: data-root selections diverge"
            );
            checked += 1;
        }
    }
    assert!(checked >= 6, "the parity target covers the corpus");
}

#[test]
fn production_default_is_compiled() {
    // The shipping default is the compiled loop: a fresh engine with no
    // selection call runs linkable programs and agrees fingerprint for
    // fingerprint with an explicitly compiled engine.
    assert_eq!(
        BytecodeMode::default(),
        BytecodeMode::Compiled,
        "a fresh engine selects the loop"
    );
    let modules = link_script("v = 1.0 + 2.0\nplot(v, title=\"V\")");
    let bars = ramp_bars(5);
    let defaulted = PineEngine::new(engine_config()).run_linked(&modules, &bars, InputSet::new());
    let mut explicit = PineEngine::new(engine_config());
    explicit.set_bytecode_mode(BytecodeMode::Compiled);
    let compiled = explicit.run_linked(&modules, &bars, InputSet::new());
    assert_eq!(
        fingerprint_result(&defaulted),
        fingerprint_result(&compiled),
        "a fresh engine runs the loop"
    );
    defaulted.expect("a fresh engine evaluates");
}
