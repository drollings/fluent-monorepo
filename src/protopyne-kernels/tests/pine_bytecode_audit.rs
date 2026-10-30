//! Enum-categorization audit for the bytecode compiler: every reserved
//! name set the compiler categorizes, pinned as behavior so the
//! enum-first representation holds the same boundaries the string
//! matches used to hold.
//!
//! Test path note: this file rides the `pine::bytecode_compile` unit
//! module, so the runnable filter is `pine::bytecode_compile`, not the
//! file stem.

use super::*;
use crate::parse_program;

fn compile_script(script: &str) -> CompiledModule {
    let program = parse_program(script).expect("synthetic script parses");
    let enums =
        EnumRegistry::build(&[("local/Driver", &program, &std::collections::HashMap::new())]);
    let decls =
        DeclRegistry::build(&[("local/Driver", &program, &std::collections::HashMap::new())]);
    let exports =
        ExportRegistry::build(&[("local/Driver", &program, &std::collections::HashMap::new())]);
    compile_module(
        "local/Driver",
        &program,
        &std::collections::HashMap::new(),
        &enums,
        &decls,
        &exports,
    )
}

#[test]
fn builtin_namespaces_categorize_by_enum() {
    // The delegated namespace set: lowering treats exactly these as
    // builtin calls and everything else as user/alias/unknown.
    for (name, namespace) in [
        ("ta", BuiltinNamespace::Ta),
        ("input", BuiltinNamespace::Input),
        ("math", BuiltinNamespace::Math),
        ("str", BuiltinNamespace::Str),
        ("color", BuiltinNamespace::Color),
        ("timeframe", BuiltinNamespace::Timeframe),
        ("array", BuiltinNamespace::Array),
        ("matrix", BuiltinNamespace::Matrix),
        ("map", BuiltinNamespace::Map),
        ("strategy", BuiltinNamespace::Strategy),
    ] {
        assert_eq!(
            BuiltinNamespace::parse(name),
            Some(namespace),
            "{name} delegates"
        );
    }
    for name in ["request", ""] {
        assert_eq!(BuiltinNamespace::parse(name), None, "{name} falls back");
    }
}

#[test]
fn capture_names_categorize_by_enum() {
    // The plot capture families never lower to value position on their own.
    // Broker dialogue and alerts are builtin namespaces now, so they are no
    // capture callees; their reserved `CaptureKind` tags stay encoded for
    // the representations that still round-trip them.
    for (name, kind) in [
        ("fill", CaptureKind::Fill),
        ("plotshape", CaptureKind::PlotShape),
        ("plotcandle", CaptureKind::PlotCandle),
        ("barcolor", CaptureKind::BarColor),
        ("hline", CaptureKind::Hline),
    ] {
        assert_eq!(CaptureKind::parse(name), Some(kind), "{name} is a capture");
    }
    for name in [
        "plot",
        "ta",
        "input",
        "nz",
        "na",
        "strategy",
        "alert",
        "alertcondition",
        "",
    ] {
        assert_eq!(CaptureKind::parse(name), None, "{name} is not a capture");
    }
    for (reserved, spelling) in [
        (CaptureKind::Strategy, "strategy"),
        (CaptureKind::Alert, "alert"),
        (CaptureKind::AlertCondition, "alertcondition"),
    ] {
        assert_eq!(
            reserved.as_str(),
            spelling,
            "the reserved tag keeps its spelling"
        );
    }
}

#[test]
fn leg_names_map_to_reserved_kinds() {
    // All ten chart legs (chart plus derived) resolve to reserved kinds.
    for (name, kind) in [
        ("open", LegKind::Open),
        ("high", LegKind::High),
        ("low", LegKind::Low),
        ("close", LegKind::Close),
        ("volume", LegKind::Volume),
        ("time", LegKind::Time),
        ("hl2", LegKind::Hl2),
        ("hlc3", LegKind::Hlc3),
        ("ohlc4", LegKind::Ohlc4),
        ("wcl", LegKind::Wcl),
    ] {
        assert_eq!(leg_slot(name), Some(kind), "{name} is reserved");
        assert_eq!(kind.index(), LegKind::parse(name).expect("parses").index());
    }
    assert_eq!(leg_slot("ohlc3"), None, "no such leg");
    assert_eq!(leg_slot("not_a_leg"), None, "open names stay open");
}

#[test]
fn identical_call_sites_share_the_same_key_shape() {
    // Two textually identical builtin calls in one module produce equal
    // interned site ids today (one table entry per unique shape).
    let module = compile_script("a = ta.sma(close, 14)\nb = ta.sma(close, 14)\nplot(a + b, \"S\")");
    let mut sites = Vec::new();
    for block in &module.blocks {
        for instr in &block.instrs {
            if let Instruction::Call { site, .. } = instr {
                sites.push(*site);
            }
        }
    }
    assert!(sites.len() >= 2, "both calls lower: {sites:?}");
    assert_eq!(
        sites[0], sites[1],
        "identical calls share one interned site"
    );
    let text = module.symbols.resolve(sites[0]).expect("site resolves");
    assert!(
        text.starts_with("ta.sma@"),
        "namespace qualifies the key: {text:?}"
    );
}

#[test]
fn unlowerable_constructs_carry_typed_reasons() {
    // A bounded loop has no lowering today: the module still compiles,
    // carrying a typed fallback reason instead of refusing.
    let module = compile_script(
        "x = request.security(syminfo.tickerid, \"D\", [foo(close)])\nplot(x, \"X\")",
    );
    let reasons: Vec<FallbackReason> = module
        .blocks
        .iter()
        .flat_map(|block| block.instrs.iter())
        .filter_map(|instr| match instr {
            Instruction::Fallback { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !reasons.is_empty(),
        "the loop falls back with a typed reason"
    );
    assert!(
        reasons
            .iter()
            .any(|reason| matches!(reason, FallbackReason::OutsideBuiltin { .. })),
        "the namespace names its family: {reasons:?}"
    );
    for reason in &reasons {
        assert!(
            !reason.display(&module.symbols).is_empty(),
            "every reason renders"
        );
    }
}

#[test]
fn hot_loop_avoids_per_bar_allocation_paths() {
    // Guardrail: the steady-state loop must not reintroduce the paths
    // the enum/pool work removed. Fails if anyone restores a per-bar or
    // per-step allocation in the execution loop.
    let path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pine/bytecode_exec.rs");
    let source = std::fs::read_to_string(&path).expect("execution loop source loads");
    for retired in [
        "instrs[self.pc].clone()",
        "slots: vec![None; slots]",
        "iter().position(|plot|",
        "iter().position(|module_|",
    ] {
        assert!(
            !source.contains(retired),
            "retired allocation path is back: {retired}"
        );
    }
    for load_bearing in ["frame_take", "capture_index", "module_index"] {
        assert!(
            source.contains(load_bearing),
            "pooled/indexed path missing: {load_bearing}"
        );
    }
}

#[test]
fn ta_path_builds_no_site_string_per_call() {
    // Guardrail: the stacked `ta` site must never rejoin the caller
    // stack per call. The prefix is maintained at call boundaries and
    // the final site composes into a reused buffer, so the steady-state
    // loop allocates nothing per `ta` call. Fails if anyone restores a
    // per-call join or format on the `ta` path.
    let path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pine/bytecode_exec.rs");
    let source = std::fs::read_to_string(&path).expect("execution loop source loads");
    for retired in ["call_stack.join", "{}>{site}"] {
        assert!(
            !source.contains(retired),
            "retired per-call site path is back: {retired}"
        );
    }
    for load_bearing in ["stack_prefix", "site_scratch"] {
        assert!(
            source.contains(load_bearing),
            "incremental site path missing: {load_bearing}"
        );
    }
    // The bridge keys every per-site table by the call-site text: the
    // hit path must borrow without allocating, materializing the owned
    // key only for a first-seen site.
    let bridge_path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pine/builtins_ta.rs");
    let bridge = std::fs::read_to_string(&bridge_path).expect("bridge source loads");
    for retired in [
        "entry(site.to_owned())",
        "\n                self.cum_sums.insert(site.to_owned(), running)",
    ] {
        assert!(
            !bridge.contains(retired),
            "retired per-call key path is back: {retired}"
        );
    }
    for load_bearing in ["contains_key(site)", "insert(site.to_owned()"] {
        assert!(
            bridge.contains(load_bearing),
            "borrowed-key bridge path missing: {load_bearing}"
        );
    }
}
