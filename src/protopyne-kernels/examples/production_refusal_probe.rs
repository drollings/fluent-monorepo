//! Production-shape refusal probe (build-matrix leg, not a unit test).
//!
//! This example runs in whatever feature shape invokes it and asserts that
//! shape's contract, failing loudly otherwise:
//! - without the `oracle` feature (production): an unlinkable program
//!   refuses with the typed load-time diagnostic under the compiled
//!   selection, and any run under the retired tree-walk selection refuses
//!   with the retired-engine diagnostic. A linkable program still runs.
//! - with the `oracle` feature: both selections keep today's behavior —
//!   the tree answers either way, script-level refusals stay byte-identical
//!   across selections, and neither typed diagnostic appears.
//!
//! The matrix invokes both shapes; `cargo test` only checks compilation.

use protopyne_kernels::{
    BytecodeMode, InputSet, KernelError, LinkedModule, PineEngine, PineRuntimeConfig,
};
use std::collections::HashMap;

fn engine_compiled() -> PineEngine {
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine.set_bytecode_mode(BytecodeMode::Compiled);
    engine
}

/// Tree-walk engine for the oracle shape only: the variant does not
/// exist in production builds, so neither does this helper.
#[cfg(feature = "oracle")]
/// Tree-walk engine for the oracle shape only: the variant does not
/// exist in production builds, so neither does this helper.
fn engine_tree() -> PineEngine {
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine.set_bytecode_mode(BytecodeMode::TreeWalk);
    engine
}

fn linked_modules(script: &str) -> Vec<LinkedModule> {
    let program = protopyne_kernels::parse_program(script).expect("probe script parses");
    vec![LinkedModule::new("local", program, HashMap::new())]
}

fn bars() -> Vec<protopyne_types::Bar> {
    (0..3)
        .map(|i| {
            let close = 100.0 + i as f64;
            protopyne_types::Bar::new()
                .ts(chrono::DateTime::from_timestamp(1_700_000_000 + i * 60, 0)
                    .expect("bar time in range"))
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

fn main() {
    // Programs no engine may link: live dialogue calls the linker
    // never serves.
    let unlinked = [(
        "live dialogue",
        linked_modules("x = request.marker(close)\nplot(x, title=\"V\")"),
    )];
    // Lowered live-fetch calls link and refuse at run time with the
    // script-level wording on both selections.
    let runtime_refusals = [
        (
            "live fetch",
            linked_modules(
                "v = request.security(syminfo.tickerid, \"D\", close)\nplot(v, title=\"V\")",
            ),
        ),
        (
            "unserved lower-tf",
            linked_modules(
                "v = request.security_lower_tf(syminfo.tickerid, \"D\", [close])\nplot(v, title=\"V\")",
            ),
        ),
    ];
    // Plain arithmetic: links clean everywhere.
    let linked = linked_modules("v = 1.0 + 2.0\nplot(v, title=\"V\")");
    let bars = bars();

    // The shipping default is the compiled loop in every shape: a fresh
    // engine with no selection call must run the linkable program.
    assert_eq!(
        BytecodeMode::default(),
        BytecodeMode::Compiled,
        "a fresh engine selects the loop"
    );
    let defaulted = PineEngine::new(PineRuntimeConfig::new().build()).run_linked(
        &linked,
        &bars,
        InputSet::new(),
    );
    let explicitly_compiled = engine_compiled().run_linked(&linked, &bars, InputSet::new());
    match (&defaulted, &explicitly_compiled) {
        (Ok(first), Ok(second)) => assert_eq!(
            first.evaluated, second.evaluated,
            "a fresh engine runs the loop"
        ),
        _ => panic!("a fresh engine must run the linkable program: {defaulted:?}"),
    }
    // Selection behavior is pinned per shape below: full answers with
    // the oracle, typed refusals without it. The retired selection is
    // unnameable in production (totality scan), so only the oracle
    // shape names both engines.

    #[cfg(feature = "oracle")]
    {
        for (name, modules) in unlinked.iter().chain(runtime_refusals.iter()) {
            let compiled = engine_compiled().run_linked(modules, &bars, InputSet::new());
            let tree = engine_tree().run_linked(modules, &bars, InputSet::new());
            let (compiled_text, tree_text) = match (&compiled, &tree) {
                (Err(first), Err(second)) => (first.to_string(), second.to_string()),
                _ => {
                    panic!("oracle build must answer {name} on the tree: {compiled:?} vs {tree:?}")
                }
            };
            assert_eq!(
                compiled_text, tree_text,
                "oracle build keeps one wording across selections for {name}"
            );
            assert!(
                !matches!(
                    compiled,
                    Err(KernelError::Uncompilable { .. } | KernelError::EngineRetired { .. })
                ),
                "oracle build never raises the typed diagnostics for {name}: {compiled:?}"
            );
        }
        let compiled_ok = engine_compiled().run_linked(&linked, &bars, InputSet::new());
        let tree_ok = engine_tree().run_linked(&linked, &bars, InputSet::new());
        assert!(
            compiled_ok.is_ok() && tree_ok.is_ok(),
            "oracle build runs the linkable program on both selections"
        );
        eprintln!("oracle build keeps both engines");
    }
    #[cfg(not(feature = "oracle"))]
    {
        for (name, modules) in &unlinked {
            match engine_compiled().run_linked(modules, &bars, InputSet::new()) {
                Err(KernelError::Uncompilable { module, construct }) => {
                    assert!(!module.is_empty() && !construct.is_empty());
                    eprintln!("production refuses {name}: {module}: {construct}");
                }
                other => {
                    panic!("production must refuse {name} with Uncompilable: {other:?}")
                }
            }
        }
        // Lowered live-fetch calls link and refuse at run time with the
        // script-level wording — identical to the tree's, never typed.
        for (name, modules) in &runtime_refusals {
            match engine_compiled().run_linked(&modules, &bars, InputSet::new()) {
                Err(KernelError::Eval(message)) => {
                    assert!(message.contains("live request"));
                    eprintln!("production refuses {name} at run time: {message}");
                }
                other => {
                    panic!("production must refuse {name} with the script wording: {other:?}")
                }
            }
        }
        // The retired selection is unnameable here by construction
        // (the variant is oracle-gated): totality is pinned by the
        // single-pathway scan, not by a run-time refusal.
        assert!(
            engine_compiled()
                .run_linked(&linked, &bars, InputSet::new())
                .is_ok(),
            "production still runs the linkable program"
        );
        eprintln!("production refuses with typed diagnostics");
    }
}