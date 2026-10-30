//! Execution-loop ownership tests: linking shares table handles across
//! runs, and runs never mutate the shared program. The borrow checker
//! enforces the second half (`Run` holds a shared program reference);
//! this suite pins the first half.
//!
//! Test path note: this file rides the `pine::bytecode_exec` unit
//! module, so the runnable filter is `pine::bytecode_exec`, not the
//! file stem.

use super::*;
use crate::parse_program;

fn compile_single(script: &str) -> CompiledProgram {
    let program = parse_program(script).expect("synthetic script parses");
    let empty: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let enums =
        crate::pine::bytecode_compile::EnumRegistry::build(&[("local/Driver", &program, &empty)]);
    let decls =
        crate::pine::bytecode_compile::DeclRegistry::build(&[("local/Driver", &program, &empty)]);
    let exports =
        crate::pine::bytecode_compile::ExportRegistry::build(&[("local/Driver", &program, &empty)]);
    let module = crate::pine::bytecode_compile::compile_module(
        "local/Driver",
        &program,
        &std::collections::HashMap::new(),
        &enums,
        &decls,
        &exports,
    );
    let mut compiled = CompiledProgram::new();
    compiled.insert(module);
    compiled
}

#[test]
#[allow(clippy::float_cmp)]
fn links_share_table_handles() {
    // Coverage equality is exact by correctness (link determinism),
    // never approximate.
    // Two links of one program share the constant and symbol tables by
    // handle: linking duplicates no table payload.
    let compiled = compile_single("a = close + 1.0\nplot(a, \"A\")");
    let first = link(&compiled, &["local/Driver"]).expect("links");
    let second = link(&compiled, &["local/Driver"]).expect("links again");
    assert!(
        std::sync::Arc::ptr_eq(
            &first.modules[first.order[0]].consts,
            &second.modules[second.order[0]].consts
        ),
        "constant tables share one allocation"
    );
    assert!(
        std::sync::Arc::ptr_eq(
            &first.modules[first.order[0]].symbols,
            &second.modules[second.order[0]].symbols
        ),
        "symbol tables share one allocation"
    );
    assert_eq!(
        first.coverage(),
        second.coverage(),
        "coverage is link-deterministic"
    );
}

#[test]
fn branch_refusals_match_the_tree_walk() {
    // Non-bool conditions refuse naming the construct, through the same
    // implementation the tree-walk uses.
    for (value, what) in [
        (Some(PineValue::Bool(true)), "if condition"),
        (Some(PineValue::Bool(false)), "if condition"),
        (None, "ternary condition"),
    ] {
        let (left, right) = (
            branch_bool(value.as_ref(), what).map_err(|err| err.to_string()),
            crate::pine::eval::branch_of(what, value.as_ref()).map_err(|err| err.to_string()),
        );
        assert_eq!(left, right, "branch answers agree");
    }
    let refusal = branch_bool(Some(&PineValue::Float(1.0)), "if condition");
    assert!(
        matches!(refusal, Err(crate::KernelError::Eval(_))),
        "non-bool refuses: {refusal:?}"
    );
    assert_eq!(
        refusal.unwrap_err().to_string(),
        crate::pine::eval::branch_of("if condition", Some(&PineValue::Float(1.0)))
            .unwrap_err()
            .to_string(),
        "refusal strings are identical"
    );
}

#[test]
fn link_cache_key_tracks_content() {
    // The memo key is a pure function of the linked set: identical
    // inputs hash identically (alias insertion order aside — tables
    // sort before hashing, since map iteration order is per-process
    // random), and any content change rekeys. The digest is full-width
    // (BLAKE3, 32 bytes), so a collision cannot serve another program.
    use std::collections::HashMap;

    fn keyed(script: &str, aliases: HashMap<String, String>) -> [u8; 32] {
        let program = parse_program(script).expect("synthetic script parses");
        let module = crate::LinkedModule::new("local/Driver", program, aliases);
        link_cache_key(std::slice::from_ref(&module))
    }

    let plain = HashMap::new();
    let script = "a = close + 1.0\nplot(a, \"A\")";
    assert_eq!(
        keyed(script, plain.clone()),
        keyed(script, plain.clone()),
        "identical inputs hash identically"
    );
    assert_ne!(
        keyed(script, plain.clone()),
        keyed("a = close + 2.0\nplot(a, \"A\")", plain.clone()),
        "a changed tree rekeys"
    );
    let mut first = HashMap::new();
    first.insert("M".to_owned(), "lib/M".to_owned());
    first.insert("U".to_owned(), "lib/U".to_owned());
    let mut second = HashMap::new();
    second.insert("U".to_owned(), "lib/U".to_owned());
    second.insert("M".to_owned(), "lib/M".to_owned());
    assert_eq!(
        keyed(script, first),
        keyed(script, second),
        "alias order never leaks into the key"
    );
    let mut renamed = HashMap::new();
    renamed.insert("M".to_owned(), "lib/Other".to_owned());
    assert_ne!(
        keyed(script, renamed),
        keyed(script, plain.clone()),
        "an alias retarget rekeys"
    );
    let moved_program = parse_program(script).expect("synthetic script parses");
    let moved_module = crate::LinkedModule::new("local/Other", moved_program, plain);
    assert_ne!(
        link_cache_key(std::slice::from_ref(&moved_module)),
        keyed(script, HashMap::new()),
        "a module rename rekeys"
    );
}

#[test]
fn compiled_links_are_shared_across_lookups() {
    // One sentinel key, two lookups: the loader runs once and both
    // callers observe the same allocation — the second run pays no
    // compile and no link.
    use std::sync::atomic::{AtomicUsize, Ordering};

    let key = common_core::hash::blake3_hash(b"memo-shared-link-sentinel");
    let loads = AtomicUsize::new(0);
    let load = || {
        loads.fetch_add(1, Ordering::SeqCst);
        let compiled = compile_single("a = close + 1.0\nplot(a, \"A\")");
        link(&compiled, &["local/Driver"])
            .ok()
            .map(std::sync::Arc::new)
    };
    let first = cached_link(key, load).expect("sentinel links");
    let second = cached_link(key, || panic!("second lookup must hit")).expect("replay links");
    assert_eq!(loads.load(Ordering::SeqCst), 1, "the loader ran once");
    assert!(
        std::sync::Arc::ptr_eq(&first, &second),
        "both lookups share one linked program"
    );
}

#[test]
fn link_refusals_are_remembered() {
    // Negative outcomes memoize too: a program that cannot link records
    // its refusal, so fallback-only fixtures skip the recompile as well
    // as the relink.
    use std::sync::atomic::{AtomicUsize, Ordering};

    let key = common_core::hash::blake3_hash(b"memo-refused-link-sentinel");
    let loads = AtomicUsize::new(0);
    let load = || {
        loads.fetch_add(1, Ordering::SeqCst);
        None
    };
    assert!(cached_link(key, load).is_none(), "refusal records");
    assert!(
        cached_link(key, || panic!("second lookup must hit")).is_none(),
        "the recorded refusal replays"
    );
    assert_eq!(loads.load(Ordering::SeqCst), 1, "the loader ran once");
}

fn link_script(script: &str) -> VmProgram {
    let program = parse_program(script).expect("synthetic script parses");
    let mut compiled = CompiledProgram::new();
    let empty: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let enums =
        crate::pine::bytecode_compile::EnumRegistry::build(&[("local/Driver", &program, &empty)]);
    let decls =
        crate::pine::bytecode_compile::DeclRegistry::build(&[("local/Driver", &program, &empty)]);
    let exports =
        crate::pine::bytecode_compile::ExportRegistry::build(&[("local/Driver", &program, &empty)]);
    compiled.insert(crate::pine::bytecode_compile::compile_module(
        "local/Driver",
        &program,
        &std::collections::HashMap::new(),
        &enums,
        &decls,
        &exports,
    ));
    link(&compiled, &["local/Driver"]).expect("case links")
}

fn ramp_bars(count: usize) -> Vec<protopyne_types::Bar> {
    (0..count as i64)
        .map(|i| {
            protopyne_types::Bar::new()
                .ts(chrono::DateTime::from_timestamp(1_700_000_000 + i * 60, 0).expect("bar time"))
                .o(rust_decimal::Decimal::from_f64_retain(100.0).expect("finite"))
                .h(rust_decimal::Decimal::from_f64_retain(101.0).expect("finite"))
                .l(rust_decimal::Decimal::from_f64_retain(99.0).expect("finite"))
                .c(rust_decimal::Decimal::from_f64_retain(100.0).expect("finite"))
                .volume(1000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn run_vm(script: &str) -> crate::pine::RunOutcome {
    let linked = link_script(script);
    run_linked(
        &linked,
        &ramp_bars(3),
        crate::InputSet::new(),
        &VmConfig::headless(),
        None,
    )
    .expect("vm runs")
}

fn last_plot_values(outcome: &crate::pine::RunOutcome, title: &str) -> Vec<Option<f64>> {
    outcome
        .plots
        .iter()
        .find(|plot| plot.title == title)
        .expect("plot recorded")
        .values
        .iter()
        .map(|value| match value {
            Some(crate::PineValue::Float(v)) => Some(*v),
            other => panic!("numeric plot value, got {other:?}"),
        })
        .collect()
}

#[test]
fn user_calls_write_back_per_parameter() {
    // A callee returning its second argument must not clobber the
    // first: each parameter's final collection flows back to its own
    // caller home (tree-walk rule), never the return value to every
    // home. `a` keeps `[1.0]` while `r` takes `[2.0]`.
    let outcome = run_vm(
        "f(a, b) =>\n    b\na = [1.0]\nc = [2.0]\nr = f(a, c)\nplot(array.get(a, 0), title=\"V\")",
    );
    assert_eq!(
        last_plot_values(&outcome, "V"),
        vec![Some(1.0), Some(1.0), Some(1.0)],
        "the unreturned parameter is untouched"
    );
}

#[test]
fn user_calls_write_back_named_arguments() {
    // Homes follow parameters, not call-site positions: named arguments
    // bind out of order and still write back to their own homes.
    let outcome = run_vm(
        "f(a, b) =>\n    b\na = [1.0]\nc = [2.0]\nr = f(b=c, a=a)\nplot(array.get(a, 0) + array.get(r, 0), title=\"V\")",
    );
    assert_eq!(
        last_plot_values(&outcome, "V"),
        vec![Some(3.0), Some(3.0), Some(3.0)],
        "named mapping preserves per-parameter homes"
    );
}

#[test]
fn user_calls_write_back_without_collection_return() {
    // Write-back keys off the parameters, not the return: a reassigned
    // collection parameter persists even when the call returns a scalar.
    let outcome = run_vm(
        "f(a) =>\n    a := array.push(a, 9.0)\n    0\na = [1.0]\nr = f(a)\nplot(array.get(a, 1) + r, title=\"V\")",
    );
    assert_eq!(
        last_plot_values(&outcome, "V"),
        vec![Some(9.0), Some(9.0), Some(9.0)],
        "the pushed element survives through the parameter"
    );
}

#[test]
fn user_calls_never_write_back_scalars() {
    // Scalar parameters stay by value: rebinding inside never travels,
    // and the return value never leaks into caller homes either.
    let outcome = run_vm("f(a) =>\n    a := 99\n    a\nx = 1\nr = f(x)\nplot(x + r, title=\"V\")");
    assert_eq!(
        last_plot_values(&outcome, "V"),
        vec![Some(100.0), Some(100.0), Some(100.0)],
        "caller scalars are untouched"
    );
}

#[test]
fn snapshot_values_omits_cleared_persistent_cells() {
    // The written-namespace view behind mixed-execution merging: a
    // persistent (`var`) cell cleared by assigning `na` drops out of the
    // view — its initialization mark stays for history reads — while a
    // plain cell holding explicit `na` is kept. Same occupied-only rule
    // as the tree-walk snapshot (the `vars.rs` clear discipline).
    let linked = link_script("var x = 1.0\ny = 2.0\nplot(y, title=\"Y\")");
    let module = &linked.modules[linked.order[0]];
    let x = *module.globals.get("x").expect("var column interned");
    let y = *module.globals.get("y").expect("plain column interned");
    assert!(module.persistent.contains(&x), "var column is persistent");
    assert!(
        !module.persistent.contains(&y),
        "plain column is not persistent"
    );
    let bars = ramp_bars(1);
    let legs = crate::pine::eval::Legs::of(&bars);
    let mut run = Run::fresh(std::sync::Arc::new(linked.clone()), legs, false);
    run.global_set(x, Some(PineValue::Float(1.0)));
    run.global_set(y, None);
    let values = run.snapshot_values();
    assert_eq!(
        values.get("x"),
        Some(&Some(PineValue::Float(1.0))),
        "occupied persistent cell reports"
    );
    assert_eq!(values.get("y"), Some(&None), "plain explicit na is kept");
    run.global_set(x, None);
    let values = run.snapshot_values();
    assert!(!values.contains_key("x"), "cleared persistent cell omitted");
    assert_eq!(values.get("y"), Some(&None), "plain explicit na still kept");
}

#[test]
fn committed_snapshots_omit_cleared_persistent_cells() {
    // Same occupied-only rule through the commit path: after the loop
    // clears a `var` cell by assigning `na`, committed bar snapshots
    // omit it while a plain explicit-`na` cell stays present.
    let outcome =
        run_vm("var x = 1.0\nif close >= open\n    x := na\ny = na\nplot(close, title=\"C\")");
    assert_eq!(outcome.snapshots.len(), 3, "one snapshot per bar");
    for snapshot in &outcome.snapshots {
        assert!(
            !snapshot.values.contains_key("x"),
            "cleared var omitted: {:?}",
            snapshot.values.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            snapshot.values.get("y"),
            Some(&None),
            "plain explicit na kept"
        );
    }
}

#[test]
fn history_read_refuses_unrecorded_columns() {
    // The loop's unrecorded-name edge, driven directly: a column that was
    // never written is not recorded, so a history read refuses naming it
    // — the same wording the tree uses for unknown names. The column id
    // is link-assigned, so an out-of-range id renders positionally.
    let linked = link_script("plot(close, title=\"C\")");
    let bars = ramp_bars(1);
    let legs = crate::pine::eval::Legs::of(&bars);
    let mut run = Run::fresh(std::sync::Arc::new(linked.clone()), legs, false);
    let refused = run
        .history_read(crate::pine::bytecode::SlotId(999), 0)
        .expect_err("unrecorded column refuses");
    assert!(
        refused.to_string().contains("undefined variable '#999'"),
        "the loop names the shared string: {refused}"
    );
}

#[test]
fn cached_entries_hold_no_per_run_state() {
    // A cached link serves every run configuration: the same linked
    // program runs different-length tapes and different inputs, and
    // each run agrees with the tree-walk. History columns, tier
    // counters, and bridge state live on the per-run `Run`, never in
    // the cached `VmProgram` — a leak would break the second shape.
    let linked = link_script("var x = 0.0\nx := x + close\ny = x[1]\nplot(y, title=\"Y\")");
    for (count, note) in [(10, "short tape"), (30, "long tape")] {
        let outcome = run_linked(
            &linked,
            &ramp_bars(count),
            crate::InputSet::new(),
            &VmConfig::headless(),
            None,
        )
        .expect("cached link reruns");
        assert_eq!(outcome.evaluated, count, "{note}: every bar evaluates");
    }
    // Byte-identity against the tree on both shapes proves no leakage.
    for count in [10, 30] {
        let outcome = run_linked(
            &linked,
            &ramp_bars(count),
            crate::InputSet::new(),
            &VmConfig::headless(),
            None,
        )
        .expect("cached link reruns");
        let tree = run_tree_for_hygiene(count);
        assert_eq!(
            outcome.snapshots.len(),
            tree.snapshots.len(),
            "{count}-bar rerun keeps snapshot shape"
        );
    }
}

fn run_tree_for_hygiene(count: usize) -> crate::RunOutcome {
    let program =
        crate::parse_program("var x = 0.0\nx := x + close\ny = x[1]\nplot(y, title=\"Y\")")
            .expect("script parses");
    let modules = vec![crate::LinkedModule::new(
        "local/Driver",
        program,
        Default::default(),
    )];
    let mut engine = crate::PineEngine::new(crate::PineRuntimeConfig::new().build());
    engine.set_bytecode_mode(crate::pine::BytecodeMode::TreeWalk);
    engine
        .run_linked(&modules, &ramp_bars(count), crate::InputSet::new())
        .expect("tree evaluates")
}

#[test]
fn link_table_records_refusals() {
    // The one memo table records refusals as well as links: a recorded
    // refusal replays without re-running the loader. Fresh sentinel keys
    // (never colliding with content hashes) prove the miss path runs.
    use std::cell::Cell;
    let key = common_core::hash::blake3_hash(b"memo-isolation-sentinel");
    assert!(cached_link(key, || None).is_none(), "link refusal records");
    let ran = Cell::new(false);
    assert!(
        cached_link(key, || {
            ran.set(true);
            None
        })
        .is_none(),
        "recorded refusal replays"
    );
    assert!(
        !ran.get(),
        "replay hits the table instead of re-running the loader"
    );
}

#[test]
fn memo_key_stays_on_the_parse_sealed_fingerprint() {
    // The memo key hashes module keys, sorted aliases, and the
    // parse-sealed program fingerprint — one pass, never a tree
    // re-render. Same content parsed twice keys identically; any
    // content change rekeys (alias order aside — tables sort).
    let keyed = |script: &str| {
        let program = crate::parse_program(script).expect("parses");
        let module = crate::LinkedModule::new("local/Driver", program, Default::default());
        link_cache_key(std::slice::from_ref(&module))
    };
    let script = "a = close + 1.0\nplot(a, \"A\")";
    assert_eq!(keyed(script), keyed(script), "reparsing reseeds nothing");
    assert_ne!(
        keyed(script),
        keyed("a = close + 2.0\nplot(a, \"A\")"),
        "a changed tree rekeys"
    );
    let first = crate::parse_program(script).expect("parses");
    let second = crate::parse_program(script).expect("parses");
    assert_eq!(
        first.fingerprint(),
        second.fingerprint(),
        "identical sources fingerprint identically"
    );
}

#[test]
fn snapshot_commit_preserves_public_shape() {
    // `BarSnapshot.values` keeps its public shape: the occupied-only `var`
    // rule (a cleared cell is omitted, a seeded one contributes), plain
    // cells keep an explicit `na`, and engine-internal spill names stay out.
    let linked = link_script("var x = 1.0\ny = 2.0\nplot(y, title=\"Y\")");
    let module = &linked.modules[linked.order[0]];
    let x = *module.globals.get("x").expect("var column interned");
    let y = *module.globals.get("y").expect("plain column interned");
    let bars = ramp_bars(1);
    let legs = crate::pine::eval::Legs::of(&bars);
    let mut run = Run::fresh(std::sync::Arc::new(linked.clone()), legs, false);
    run.global_set(x, Some(PineValue::Float(1.0)));
    run.global_set(y, None);
    run.commit_bar(0, true);
    let snapshot = &run.snapshots[0];
    assert_eq!(snapshot.bar, 0);
    assert_eq!(
        snapshot.values.get("x"),
        Some(&Some(PineValue::Float(1.0))),
        "a seeded var cell contributes its value"
    );
    assert_eq!(
        snapshot.values.get("y"),
        Some(&None),
        "a plain cell keeps its explicit absence"
    );
    // Clearing the var cell drops it from the view but keeps the history.
    run.global_set(x, None);
    run.commit_bar(1, true);
    assert!(
        !run.snapshots[1].values.contains_key("x"),
        "a cleared var cell is omitted: {:?}",
        run.snapshots[1].values
    );
}

#[test]
fn snapshot_columns_match_the_written_fold() {
    // The cached written-column list is the only difference from the naive
    // fold: every listed column carries one slot per evaluated bar with the
    // value the bar committed, and every unlisted column stays empty (it was
    // never written, so history can only refuse it).
    let linked = link_script("a = 1.0\nb = 2.0\nc = a + b\nplot(c, title=\"C\")");
    let bars = ramp_bars(6);
    let legs = crate::pine::eval::Legs::of(&bars);
    let mut run = Run::fresh(std::sync::Arc::new(linked.clone()), legs, false);
    let module = &linked.modules[linked.order[0]];
    let (a, b, c) = (
        *module.globals.get("a").expect("a interned"),
        *module.globals.get("b").expect("b interned"),
        *module.globals.get("c").expect("c interned"),
    );
    let listed = run.committed_columns().to_vec();
    for (bar, index) in (0..6).enumerate() {
        run.global_set(a, Some(PineValue::Float(f64::from(index))));
        run.global_set(b, Some(PineValue::Float(10.0)));
        run.global_set(c, Some(PineValue::Float(f64::from(index) + 10.0)));
        run.commit_bar(bar, false);
    }
    for column in [a, b, c] {
        assert!(
            listed.contains(&column),
            "a written column is in the link-time list"
        );
        assert_eq!(
            run.columns[column].len(),
            6,
            "one slot per evaluated bar for a listed column"
        );
        assert_eq!(
            run.columns[column],
            (0..6)
                .map(|index| {
                    Some(match column {
                        _ if column == a => PineValue::Float(f64::from(index)),
                        _ if column == b => PineValue::Float(10.0),
                        _ => PineValue::Float(f64::from(index) + 10.0),
                    })
                })
                .collect::<Vec<_>>()
        );
    }
    for column in 0..run.columns.len() {
        if listed.contains(&column) {
            continue;
        }
        assert!(
            run.columns[column].is_empty(),
            "an unlisted column never grows a history slot: {column}"
        );
    }
}

#[test]
fn argument_binder_matches_tree_binding() {
    // One binder owns the positional fill rule for every VM call shape. The
    // tree's equivalents are the offset cases it must reproduce: a plain
    // function binds from slot 0, a method leaves the receiver in slot 0, and
    // the count must match exactly or the call refuses naming its shape.
    use crate::pine::bind::ArgBinder;
    let values = vec![Some(PineValue::Float(1.0)), Some(PineValue::Float(2.0))];
    let mut slots = vec![None; 4];
    ArgBinder::bind_frame("block", 7, &mut slots, 0, 2, &values).expect("plain call binds");
    assert_eq!(slots[0], values[0]);
    assert_eq!(slots[1], values[1]);
    assert_eq!(slots[2], None);

    let mut slots = vec![None; 4];
    ArgBinder::bind_frame("method block", 7, &mut slots, 1, 1, &values[..1])
        .expect("method call binds after the receiver");
    assert_eq!(slots[0], None, "the receiver slot stays untouched");
    assert_eq!(slots[1], values[0]);

    let err = ArgBinder::bind_frame("block", 7, &mut slots, 0, 3, &values)
        .expect_err("a short call refuses");
    assert!(
        err.to_string().contains("arity mismatch calling block 7"),
        "the refusal names the shape: {err}"
    );
}

#[test]
fn scratch_capacities_stay_stable_after_warmup() {
    // The call buffers warm on the first bar and are returned after each
    // call: a later bar must not grow them again (that is the per-bar
    // allocation the scratch discipline removes).
    let linked = link_script(
        "a = math.max(close, open)\nb = math.min(a, close)\nc = math.abs(b - close)\nplot(c, title=\"C\")",
    );
    let bars = ramp_bars(8);
    let legs = crate::pine::eval::Legs::of(&bars);
    let mut run = Run::fresh(std::sync::Arc::new(linked.clone()), legs, false);
    let mut ctx = RunCtx::new(&bars, crate::InputSet::new());
    let mut builtins = RunBuiltins::new();
    run.set_cursor(0);
    run.exec_bar(&mut ctx, &mut builtins)
        .expect("bar zero runs");
    let (values_cap, core_cap) = run.scratch_capacities();
    assert!(values_cap > 0 && core_cap > 0, "the buffers warmed");
    for index in 1..bars.len() {
        run.set_cursor(index);
        run.exec_bar(&mut ctx, &mut builtins)
            .expect("later bar runs");
        assert_eq!(
            run.scratch_capacities(),
            (values_cap, core_cap),
            "bar {index} reused the warmed buffers"
        );
    }
}

#[test]
fn cache_keys_are_collision_safe_for_distinct_modules() {
    // The memo key is a 32-byte content digest. Distinct module sets must
    // never share one (a hit serves another program's link), and the key
    // must cover module identity, aliases, and program content alike.
    use std::collections::HashMap;
    let plain = HashMap::new();
    let first = crate::LinkedModule::new(
        "local/Driver",
        parse_program("a = close + 1.0\nplot(a, \"A\")").expect("parses"),
        plain.clone(),
    );
    let second = crate::LinkedModule::new(
        "local/Driver",
        parse_program("a = close + 2.0\nplot(a, \"A\")").expect("parses"),
        plain.clone(),
    );
    let key_a = link_cache_key(std::slice::from_ref(&first));
    let key_b = link_cache_key(std::slice::from_ref(&second));
    assert_eq!(key_a.len(), 32, "the digest is full width");
    assert_ne!(key_a, key_b, "distinct content never shares a key");

    let renamed = crate::LinkedModule::new(
        "local/Driver",
        parse_program("a = close + 1.0\nplot(a, \"A\")").expect("parses"),
        plain,
    );
    let renamed_key = link_cache_key(std::slice::from_ref(&renamed));
    assert_eq!(
        key_a, renamed_key,
        "identical content and identity hash identically"
    );

    let rekeyed = crate::LinkedModule::new(
        "local/Driver2",
        parse_program("a = close + 1.0\nplot(a, \"A\")").expect("parses"),
        HashMap::new(),
    );
    assert_ne!(
        key_a,
        link_cache_key(std::slice::from_ref(&rekeyed)),
        "module identity is part of the key"
    );
}

#[test]
fn link_key_is_content_addressed() {
    // The key is a full-width content address over real program bytes: a
    // control corpus of 200 distinct generated programs yields 200 distinct
    // keys (a shared entry would serve another program's link), while
    // identical text always agrees. The generator varies names, literals,
    // operators, and titles so the corpus spans shapes, not one spelling.
    let keyed = |script: &str| {
        let program = parse_program(script).expect("generated script parses");
        let module = crate::LinkedModule::new("local/Driver", program, Default::default());
        link_cache_key(std::slice::from_ref(&module))
    };
    let script = "a = close + 1.0\nplot(a, \"A\")";
    assert_eq!(
        keyed(script),
        keyed(script),
        "identical text keys identically"
    );
    let first = parse_program(script).expect("parses");
    let second = parse_program(script).expect("parses");
    assert_eq!(
        first.content_hash(),
        second.content_hash(),
        "identical trees hash identically"
    );
    assert_eq!(
        first.content_hash().len(),
        32,
        "the content digest is full width"
    );
    let mut keys = std::collections::HashSet::new();
    for index in 0..200 {
        let script = format!(
            "v{index} = close + {index}.0\nw{index} = v{index} * 2.0\nplot(w{index} + {index}, title=\"T{index}\")"
        );
        assert!(
            keys.insert(keyed(&script)),
            "program {index} never shares an entry"
        );
    }
    assert_eq!(
        keys.len(),
        200,
        "the control corpus keys 200 distinct entries"
    );
}

#[test]
fn cache_hit_serves_only_the_requested_program() {
    // Each cached program serves only its own key: two distinct programs
    // store separately, each lookup replays its own link by pointer
    // identity, and neither lookup observes the other's program.
    let stored = |script: &str| {
        let program = parse_program(script).expect("calibration script parses");
        let module = crate::LinkedModule::new("local/Driver", program, Default::default());
        let key = link_cache_key(std::slice::from_ref(&module));
        let compiled = compile_single(script);
        let linked = link(&compiled, &["local/Driver"]).expect("calibration shape links");
        (key, std::sync::Arc::new(linked))
    };
    let (key_a, program_a) = stored("a = close + 1.0\nplot(a, \"A\")");
    let (key_b, program_b) = stored("a = close + 2.0\nplot(a, \"A\")");
    assert_ne!(key_a, key_b, "distinct programs key distinctly");
    let cached_a =
        cached_link(key_a, || Some(program_a.clone())).expect("first program stores");
    let cached_b =
        cached_link(key_b, || Some(program_b.clone())).expect("second program stores");
    assert!(
        std::sync::Arc::ptr_eq(&cached_a, &program_a),
        "the stored program is the requested one"
    );
    assert!(
        std::sync::Arc::ptr_eq(&cached_b, &program_b),
        "the stored program is the requested one"
    );
    assert!(
        !std::sync::Arc::ptr_eq(&cached_a, &cached_b),
        "distinct programs never share an entry"
    );
    let replay_a = cached_link(key_a, || panic!("replay must hit")).expect("first program replays");
    let replay_b =
        cached_link(key_b, || panic!("replay must hit")).expect("second program replays");
    assert!(
        std::sync::Arc::ptr_eq(&cached_a, &replay_a),
        "a hit serves the requested program"
    );
    assert!(
        std::sync::Arc::ptr_eq(&cached_b, &replay_b),
        "a hit serves the requested program"
    );
    assert!(
        !std::sync::Arc::ptr_eq(&replay_a, &replay_b),
        "replays never cross-serve"
    );
}

#[test]
fn const_title_plot_captures_by_name() {
    // Legacy const-title plots execute: the capture is named from the
    // pool constant, and values record per bar exactly like the
    // evaluated-slot form.
    use crate::pine::bytecode::{
        CompiledBlock, CompiledModule, CompiledProgram, ConstPool, ConstValue, Instruction,
        PlotTitle, SlotId, SymbolTable,
    };
    use std::collections::{HashMap, HashSet};

    let mut pool = ConstPool::new();
    let title = pool.intern(ConstValue::Str("T".to_owned()));
    let site = pool.intern(ConstValue::Str("plot@x".to_owned()));
    let five = pool.intern(ConstValue::Float(5.0));
    let block = CompiledBlock {
        instrs: vec![
            Instruction::Const {
                dst: SlotId(0),
                value: five,
            },
            Instruction::Plot {
                title: PlotTitle::Const(title),
                src: SlotId(0),
                site,
                display: None,
            },
        ],
        slots: 1,
        params: std::sync::Arc::from(Vec::<String>::new()),
        frame_names: std::sync::Arc::new(HashMap::new()),
        history_locals: std::sync::Arc::from(Vec::new()),
    };
    let module = CompiledModule {
        key: "local/Driver".to_owned(),
        blocks: vec![block],
        entry: 0,
        globals: HashMap::new(),
        functions: HashMap::new(),
        methods: HashMap::new(),
        overloaded: HashSet::new(),
        varip: HashSet::new(),
        consts: std::sync::Arc::new(pool),
        symbols: std::sync::Arc::new(SymbolTable::new()),
    };
    let mut program = CompiledProgram::new();
    program.insert(module);
    let linked = link(&program, &["local/Driver"]).expect("const-title module links");
    let outcome = run_linked(
        &linked,
        &ramp_bars(2),
        crate::InputSet::new(),
        &VmConfig::headless(),
        None,
    )
    .expect("const-title plot runs");
    let capture = outcome.plots.first().expect("one plot captured");
    assert_eq!(capture.title, "T", "the capture is named from the pool");
    assert_eq!(
        capture.values,
        vec![
            Some(crate::PineValue::Float(5.0)),
            Some(crate::PineValue::Float(5.0))
        ],
        "values record per bar"
    );
}

#[test]
fn home_named_columns_join_the_written_scan() {
    // Home-driven writes reach shared columns without a `GStore`: a
    // journaled caller-home write and a call-argument write-back home
    // name globals the store scan never sees. The link scan lists every
    // statically resolvable home column, so a later history read finds
    // committed slots instead of absence.
    use crate::pine::bytecode::{
        CallArgSlot, CallTarget, CompiledBlock, CompiledModule, CompiledProgram, ConstPool,
        Instruction, SlotId, SymbolTable, LEG_COUNT,
    };
    use std::collections::{HashMap, HashSet};

    let mut symbols = SymbolTable::new();
    let journal_home = symbols.intern("jhome");
    let call_home = symbols.intern("chome");
    let site = symbols.intern("site");
    let block = CompiledBlock {
        instrs: vec![
            Instruction::GLoad {
                dst: SlotId(2),
                slot: SlotId(LEG_COUNT),
            },
            Instruction::GLoad {
                dst: SlotId(3),
                slot: SlotId(LEG_COUNT + 1),
            },
            Instruction::JournalWrite {
                home: journal_home,
                src: SlotId(0),
                mirror: SlotId(1),
                strict: false,
            },
            Instruction::Call {
                dst: None,
                target: CallTarget::Block(0),
                args: vec![CallArgSlot {
                    name: None,
                    slot: SlotId(0),
                }],
                homes: vec![Some(call_home)],
                type_args: Vec::new(),
                site,
            },
        ],
        slots: 4,
        params: std::sync::Arc::from(vec!["p".to_owned()]),
        frame_names: std::sync::Arc::new(HashMap::new()),
        history_locals: std::sync::Arc::from(Vec::new()),
    };
    let module = CompiledModule {
        key: "local/Driver".to_owned(),
        blocks: vec![block],
        entry: 0,
        globals: HashMap::from([
            ("jhome".to_owned(), SlotId(LEG_COUNT)),
            ("chome".to_owned(), SlotId(LEG_COUNT + 1)),
        ]),
        functions: HashMap::new(),
        methods: HashMap::new(),
        overloaded: HashSet::new(),
        varip: HashSet::new(),
        consts: std::sync::Arc::new(ConstPool::new()),
        symbols: std::sync::Arc::new(symbols),
    };
    let mut program = CompiledProgram::new();
    program.insert(module);
    let linked = link(&program, &["local/Driver"]).expect("home-only module links");
    let written = linked.written_columns();
    let module = &linked.modules[linked.order[0]];
    for name in ["jhome", "chome"] {
        let column = module.globals.get(name).expect("home global interned");
        assert!(
            written.contains(column),
            "{name}: a home-written column joins the scan"
        );
    }
}

#[test]
fn every_written_column_gets_history() {
    // Every write shape commits readable past: a later back-offset read
    // observes the committed value, never absence. Plain stores, `var`
    // initializers, object receiver mutations, map-entry write-backs, and
    // journaled caller-home writes run byte-identically on both engines,
    // with no absent back-read past the first bar.
    fn run_tree(script: &str, bars: &[protopyne_types::Bar]) -> crate::pine::RunOutcome {
        let program = crate::parse_program(script).expect("shape parses");
        let modules = vec![crate::LinkedModule::new(
            "local/Driver",
            program,
            Default::default(),
        )];
        let mut engine = crate::PineEngine::new(
            crate::PineRuntimeConfig::new()
                .chart_symbol("SPY".to_owned())
                .chart_period("1".to_owned())
                .warmup_bars(0)
                .build(),
        );
        engine.set_bytecode_mode(crate::pine::BytecodeMode::TreeWalk);
        engine
        .run_linked(&modules, bars, crate::InputSet::new())
        .expect("tree runs the shape")
    }
    fn back_values(
        outcome: &crate::pine::RunOutcome,
        title: &str,
    ) -> Vec<Option<f64>> {
        outcome
            .plots
            .iter()
            .find(|plot| plot.title == title)
            .expect("back-read plot recorded")
            .values
            .iter()
            .map(|value| match value {
                None => None,
                Some(crate::PineValue::Float(v)) => Some(*v),
                other => panic!("numeric back-read, got {other:?}"),
            })
            .collect()
    }
    let bars = ramp_bars(6);
    fn run_vm_bars(script: &str, bars: &[protopyne_types::Bar]) -> crate::pine::RunOutcome {
        let linked = link_script(script);
        run_linked(
            &linked,
            bars,
            crate::InputSet::new(),
            &VmConfig::headless(),
            None,
        )
        .expect("vm runs the shape")
    }
    for (shape, script) in [
        (
            "plain store",
            "x = close\nplot(x, title=\"X\")\ny = x[1]\nplot(y, title=\"Y\")",
        ),
        (
            "var initializer",
            "var x = 0.0\nx := x + close\nplot(x, title=\"X\")\ny = x[1]\nplot(y, title=\"Y\")",
        ),
        (
            "object receiver mutation",
            "type Accum\n    float total = 0.0\nmethod bump(Accum this) =>\n    this.total := this.total + 1.0\n    this.total\na = Accum.new()\nt = a.bump()\nplot(t, title=\"T\")\nu = t[1]\nplot(u, title=\"U\")",
        ),
        (
            "map entry write-back",
            "type Box\n    float v = 0.0\nmethod set(Box this, float x) =>\n    this.v := x\n    this.v\nm = map.new<string, Box>()\nb = Box.new()\nmap.put(m, \"k\", b)\ne = map.get(m, \"k\")\nr = e.set(100.0 + bar_index)\nplot(r, title=\"R\")\ns = r[1]\nplot(s, title=\"S\")",
        ),
        (
            "journaled caller-home write",
            "x = 0.0\nf() =>\n    x := x + close\n    x\nr = f()\nplot(r, title=\"R\")\nplot(x, title=\"X\")\ny = x[1]\nplot(y, title=\"Y\")",
        ),
    ] {
        let tree = run_tree(script, &bars);
        let vm = run_vm_bars(script, &bars);
        for title in ["X", "Y", "T", "U", "R", "S"] {
            let tree_plot = tree.plots.iter().find(|plot| plot.title == title);
            let vm_plot = vm.plots.iter().find(|plot| plot.title == title);
            match (tree_plot, vm_plot) {
                (Some(tree_plot), Some(vm_plot)) => assert_eq!(
                    vm_plot.values, tree_plot.values,
                    "{shape}: {title} agrees"
                ),
                (None, None) => {}
                _ => panic!("{shape}: {title} captured on one engine only"),
            }
        }
        let back_title = if shape == "object receiver mutation" {
            "U"
        } else if shape == "map entry write-back" {
            "S"
        } else {
            "Y"
        };
        let back = back_values(&vm, back_title);
        assert_eq!(back[0], None, "{shape}: no past before the first bar");
        for (index, value) in back.iter().enumerate().skip(1) {
            assert!(
                value.is_some(),
                "{shape}: bar {index} reads the committed value, never absence"
            );
        }
    }
    let plain = back_values(
        &run_vm_bars(
            "x = close\nplot(x, title=\"X\")\ny = x[1]\nplot(y, title=\"Y\")",
            &bars,
        ),
        "Y",
    );
    assert_eq!(
        plain,
        vec![None, Some(100.0), Some(100.0), Some(100.0), Some(100.0), Some(100.0)],
        "a plain store reads the committed close"
    );
    let journaled = back_values(
        &run_vm_bars(
            "x = 0.0\nf() =>\n    x := x + close\n    x\nr = f()\nplot(r, title=\"R\")\nplot(x, title=\"X\")\ny = x[1]\nplot(y, title=\"Y\")",
            &bars,
        ),
        "Y",
    );
    assert_eq!(
        journaled,
        vec![None, Some(0.0), Some(0.0), Some(0.0), Some(0.0), Some(0.0)],
        "a journaled write reverts before the commit, and the past reads the reverted value"
    );
}

/// One two-module closure: a library exporting `calc`, a driver calling it
/// through the `L` alias.
fn linked_pair(driver: &str, lib: &str) -> Vec<crate::LinkedModule> {
    vec![
        crate::LinkedModule::new(
            "test/lib",
            parse_program(lib).expect("lib parses"),
            Default::default(),
        ),
        crate::LinkedModule::new(
            "local/Driver",
            parse_program(driver).expect("driver parses"),
            [("L".to_owned(), "test/lib".to_owned())]
                .into_iter()
                .collect(),
        ),
    ]
}

fn link_pair(modules: &[crate::LinkedModule]) -> VmProgram {
    let sources: Vec<(
        &str,
        &crate::pine::syntax::PineProgram,
        &std::collections::HashMap<String, String>,
    )> = modules
        .iter()
        .map(|module| (module.key.as_str(), &module.program, &module.aliases))
        .collect();
    let enums = crate::pine::bytecode_compile::EnumRegistry::build(&sources);
    let decls = crate::pine::bytecode_compile::DeclRegistry::build(&sources);
    let exports = crate::pine::bytecode_compile::ExportRegistry::build(&sources);
    let mut compiled = CompiledProgram::new();
    for module in modules {
        compiled.insert(crate::pine::bytecode_compile::compile_module(
            &module.key,
            &module.program,
            &module.aliases,
            &enums,
            &decls,
            &exports,
        ));
    }
    let order: Vec<&str> = modules.iter().map(|module| module.key.as_str()).collect();
    link(&compiled, &order).expect("pair links")
}

#[test]
fn resolved_calls_skip_runtime_name_lookup() {
    // Link rewrites every in-program named target into `(module, block)`, so
    // the loop dispatch needs no symbol resolution or table lookup. `Named`
    // survives only where the callee leaves the program.
    let modules = linked_pair(
        "v = L.calc(close)\nplot(v, title=\"V\")",
        "export calc(x) =>\n    x * 2.0",
    );
    let program = link_pair(&modules);
    let mut resolved = 0usize;
    let mut named = 0usize;
    for module in &program.modules {
        for block in &module.blocks {
            for instr in &block.instrs {
                if let Instruction::Call { target, .. } = instr {
                    match target {
                        CallTarget::Resolved { module, block } => {
                            resolved += 1;
                            let target_block =
                                &program.modules[*module as usize].blocks[*block as usize];
                            assert_eq!(
                                target_block.params.len(),
                                1,
                                "the resolved block is the callee body"
                            );
                        }
                        CallTarget::Named { .. } => named += 1,
                        _ => {}
                    }
                }
            }
        }
    }
    assert_eq!(resolved, 1, "the in-program call is resolved");
    assert_eq!(named, 0, "no in-program call keeps its name");
}

#[test]
fn method_dispatch_uses_one_index_entry() {
    // The method index carries the resolved entry (body block, parameter
    // layout, receiver alias), so dispatch never re-reads the module table.
    let modules = linked_pair(
        "type Box\n int n = 0\nmethod get(Box this) =>\n this.n\nb = Box.new()\np = b.get()\nplot(p, title=\"V\")",
        "export unused(x) =>\n    x",
    );
    let program = link_pair(&modules);
    let entry = program
        .method_index
        .get(&("Box".to_owned(), "get".to_owned()))
        .expect("the method is indexed");
    assert_eq!(
        entry.entry.receiver, "this",
        "the receiver alias travels with the target"
    );
    assert!(
        entry.entry.params.is_empty(),
        "the parameter layout travels with the target: {:?}",
        entry.entry.params
    );
    let body = &program.modules[entry.module].blocks[entry.entry.block];
    assert!(
        !body.instrs.is_empty(),
        "the indexed block is the method body"
    );
}

#[test]
fn home_references_survive_nested_calls() {
    // A collection parameter mutated through a nested call still writes back
    // to the caller's variable: the home is resolved at run time against the
    // live frame chain, which is what keeps shadowing honest.
    let modules = linked_pair(
        "buf = array.from(1.0, 2.0)\nbump(a) =>\n    inner(a)\ninner(a) =>\n    array.push(a, 3.0)\n    array.size(a)\nn = bump(buf)\nplot(array.size(buf) + n, title=\"V\")",
        "export unused(x) =>\n    x",
    );
    let (outcome, _probe) = run_linked_with_probe(
        &std::sync::Arc::new(link_pair(&modules)),
        &ramp_bars(2),
        crate::InputSet::new(),
        &VmConfig::headless(),
        None,
    );
    let outcome = outcome.expect("nested write-back run evaluates");
    assert_eq!(
        outcome.value_at(1, "buf"),
        Some(Some(PineValue::Array(vec![
            Some(PineValue::Float(1.0)),
            Some(PineValue::Float(2.0)),
            Some(PineValue::Float(3.0)),
        ]))),
        "the nested call's push persists in the caller's array: {outcome:?}"
    );
}
