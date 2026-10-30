//! Run-scoped builtin store behavior: one `ta.*` bridge and one `fixnan`
//! memory per run, bound lazily and keyed exactly like the engines key
//! them.
//!
//! Test path note: this file rides the `pine::builtins_state` unit module,
//! so the runnable filter is `pine::builtins_state`, not the file stem.

use super::*;
use crate::indicator::BarTick;

fn tick(close: f64) -> BarTick {
    BarTick {
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
    }
}

#[test]
fn ta_bridge_binds_once_and_keeps_kernel_state() {
    // The bridge is created on first use and reused afterwards: two calls
    // to the same site continue one kernel history instead of restarting.
    let mut builtins = RunBuiltins::new();
    let values = |value: f64| vec![Some(PineValue::Float(value)), Some(PineValue::Int(2))];
    let first = builtins
        .ta_mut()
        .expect("bridge binds")
        .call("ta.sma@2", "sma", &values(1.0), tick(1.0))
        .expect("first call evaluates");
    assert_eq!(first, None, "a two-bar mean has no value on its first bar");
    let second = builtins
        .ta_mut()
        .expect("bridge reuses the bound state")
        .call("ta.sma@2", "sma", &values(3.0), tick(3.0))
        .expect("second call evaluates");
    assert_eq!(
        second,
        Some(PineValue::Float(2.0)),
        "the second bar extends the first bar's history"
    );
}

#[test]
fn fixnan_memory_keeps_the_last_present_value_per_site() {
    // Per-site memory replays the last present value; a second site never
    // borrows the first site's history.
    let mut builtins = RunBuiltins::new();
    let site = "fixnan@Variable(\"a\")";
    assert_eq!(
        builtins
            .fixnan(site, &[None])
            .expect("empty memory reads absence"),
        None,
        "no present value yet"
    );
    assert_eq!(
        builtins
            .fixnan(site, &[Some(PineValue::Float(7.0))])
            .expect("present value stores"),
        Some(PineValue::Float(7.0))
    );
    assert_eq!(
        builtins.fixnan(site, &[None]).expect("gap replays"),
        Some(PineValue::Float(7.0)),
        "the gap replays the last present value"
    );
    assert_eq!(
        builtins
            .fixnan("fixnan@Variable(\"b\")", &[None])
            .expect("other site reads absence"),
        None,
        "another site keeps its own memory"
    );
    assert_eq!(
        builtins
            .fixnan(site, &[Some(PineValue::Int(9))])
            .expect("int stores"),
        Some(PineValue::Int(9))
    );
}

#[test]
fn fixnan_arity_refuses_through_the_shared_step() {
    // The store owns no arity rules of its own: the shared step refuses,
    // so both engines see the same wording.
    let mut builtins = RunBuiltins::new();
    let error = builtins
        .fixnan("fixnan@empty", &[])
        .expect_err("missing argument refuses");
    assert!(
        format!("{error}").contains("exactly one argument"),
        "arity refusal comes from the shared step: {error}"
    );
}

#[test]
fn stateful_builtins_have_one_owner() {
    // Ownership audit: the tree-walk state and the loop state must not
    // grow their own `ta` bridge, `fixnan` memory, or input registry —
    // those live in `RunBuiltins` (and, for inputs, the run context's one
    // store). A reintroduced twin fails here before it can diverge. The
    // tree side spans the facade, oracle, and shared leaves, so the audit
    // reads all three texts as one surface.
    let tree = concat!(
        include_str!("../src/pine/engine.rs"),
        include_str!("../src/pine/oracle/mod.rs"),
        include_str!("../src/pine/semantics.rs"),
    );
    for gone in ["ta_bridge", "fixnan_memory", "core_inputs"] {
        assert!(
            !tree.contains(gone),
            "tree-walk state still owns `{gone}`; it belongs to RunBuiltins"
        );
    }
    let loop_source = include_str!("../src/pine/bytecode_exec.rs");
    for gone in ["self.ta.", "self.fixnan", "self.inputs"] {
        assert!(
            !loop_source.contains(gone),
            "loop state still owns `{gone}`; it belongs to RunBuiltins"
        );
    }
    let owners = |source: &str| {
        source.matches("RunBuiltins::new()").count()
            + source.matches("RunBuiltins::with_requests(").count()
    };
    assert_eq!(
        owners(tree),
        1,
        "the whole run creates exactly one owner"
    );
    assert_eq!(
        owners(loop_source),
        2,
        "a VM-only run creates exactly one owner (with and without tapes)"
    );
}
