//! Compiler tests: the M0 corpus and the fixture closures compile,
//! focused lowerings assert exact instruction shapes, unlowerable
//! constructs fall back with named reasons, and fixture coverage is
//! measured (never gated).
//!
//! Test path note: this file rides the `pine::bytecode_compile` unit
//! module, so the runnable filter is `pine::bytecode_compile`, not the
//! file stem.

use super::*;
use crate::parse_program;
use crate::pine::test_fixture as fixture;
use std::collections::BTreeMap;

fn compile_script(script: &str) -> CompiledModule {
    let program = parse_program(script).expect("synthetic script parses");
    compile_script_of(&program)
}

/// Compile one parsed script against its own top-level tables, mirroring
/// what the engine sees for a single-module run.
fn compile_script_of(program: &crate::pine::syntax::PineProgram) -> CompiledModule {
    let empty: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let enums = EnumRegistry::build(&[("local/Driver", program, &empty)]);
    let decls = DeclRegistry::build(&[("local/Driver", program, &empty)]);
    let exports = ExportRegistry::build(&[("local/Driver", program, &empty)]);
    compile_module(
        "local/Driver",
        program,
        &std::collections::HashMap::new(),
        &enums,
        &decls,
        &exports,
    )
}

/// Compile one script against its own top-level enums, mirroring what
/// the engine sees for a single-module run.
fn compile_script_with_enums(script: &str) -> CompiledModule {
    compile_script(script)
}

fn entry(module: &CompiledModule) -> &CompiledBlock {
    &module.blocks[module.entry]
}

fn has_fallback(block: &CompiledBlock) -> bool {
    block
        .instrs
        .iter()
        .any(|instr| matches!(instr, Instruction::Fallback { .. }))
}

fn has_enum_read(block: &CompiledBlock) -> bool {
    block
        .instrs
        .iter()
        .any(|instr| matches!(instr, Instruction::EnumRead { .. }))
}

fn fallback_reasons(module: &CompiledModule, block: &CompiledBlock) -> Vec<String> {
    block
        .instrs
        .iter()
        .filter_map(|instr| match instr {
            Instruction::Fallback { reason, .. } => Some(reason.display(&module.symbols)),
            _ => None,
        })
        .collect()
}

#[test]
fn straight_line_numeric_code_compiles_clean() {
    // `plot(close + 1.0, "A")`: builtin leg, const, binop, plot — no
    // fallback, jump targets patched in range.
    let module = compile_script("plot(close + 1.0, \"A\")");
    let block = entry(&module);
    assert!(
        !has_fallback(block),
        "straight-line code lowers fully: {:?}",
        block.instrs
    );
    assert!(
        block.instrs.iter().any(|instr| matches!(
            instr,
            Instruction::Builtin {
                var: BuiltinVarKind::Close,
                ..
            }
        )),
        "the close leg loads: {:?}",
        block.instrs
    );
    assert!(
        block
            .instrs
            .iter()
            .any(|instr| matches!(instr, Instruction::BinOp { op: BinOp::Add, .. })),
        "addition lowers: {:?}",
        block.instrs
    );
    assert!(
        block
            .instrs
            .iter()
            .any(|instr| matches!(instr, Instruction::Plot { .. })),
        "the plot emits: {:?}",
        block.instrs
    );
    for (index, instr) in block.instrs.iter().enumerate() {
        match instr {
            Instruction::Jump { target }
            | Instruction::JumpIfFalse { target, .. }
            | Instruction::JumpIfNa { target, .. } => {
                assert!(
                    *target < block.instrs.len(),
                    "jump {index} lands in the block"
                );
            }
            Instruction::Call {
                target: CallTarget::Block(target),
                ..
            } => {
                assert!(
                    *target < module.blocks.len(),
                    "call {index} lands in a block"
                );
            }
            _ => {}
        }
    }
}

#[test]
fn ternary_compiles_to_na_aware_jumps() {
    // `cond ? a : b` with a possibly-absent condition: the na-jump lands
    // past both arms (absence reads absence, never the else value).
    let module = compile_script("v = close > open ? high : low\nplot(v, \"V\")");
    let block = entry(&module);
    assert!(!has_fallback(block), "ternary lowers: {:?}", block.instrs);
    let na_jumps: Vec<usize> = block
        .instrs
        .iter()
        .enumerate()
        .filter_map(|(index, instr)| match instr {
            Instruction::JumpIfNa { .. } => Some(index),
            _ => None,
        })
        .collect();
    assert_eq!(na_jumps.len(), 1, "one na-guard: {:?}", block.instrs);
    assert!(
        block
            .instrs
            .iter()
            .any(|instr| matches!(instr, Instruction::JumpIfFalse { .. })),
        "one false-branch: {:?}",
        block.instrs
    );
}

#[test]
fn na_literal_writes_nothing() {
    // NOTE: untitled plots fall back (the `plot:N` default titles count
    // captures at run time), so the probe carries a title.
    // `plot(na)`: the source temp is never written; slots reset per bar,
    // so absence is structural, never a stored zero.
    let module = compile_script("plot(na, \"N\")");
    let block = entry(&module);
    assert!(!has_fallback(block), "na lowers: {:?}", block.instrs);
    let plot_src = block.instrs.iter().find_map(|instr| match instr {
        Instruction::Plot { src, .. } => Some(*src),
        _ => None,
    });
    assert!(plot_src.is_some(), "the plot emits");
    assert!(
        !block.instrs.iter().any(|instr| match instr {
            Instruction::GStore { slot, .. } => Some(*slot) == plot_src,
            Instruction::Copy { dst, .. }
            | Instruction::BinOp { dst, .. }
            | Instruction::UnOp { dst, .. }
            | Instruction::Builtin { dst, .. }
            | Instruction::Const { dst, .. }
            | Instruction::History { dst, .. } => Some(*dst) == plot_src,
            _ => false,
        }),
        "the na temp is never written: {:?}",
        block.instrs
    );
}

#[test]
fn history_offsets_lower_to_indexed_loads() {
    let module = compile_script("plot(close[3] - open[0], \"W\")");
    let block = entry(&module);
    assert!(!has_fallback(block), "offsets lower: {:?}", block.instrs);
    let mut offsets: Vec<i32> = block
        .instrs
        .iter()
        .filter_map(|instr| match instr {
            Instruction::History { offset, .. } => Some(*offset),
            _ => None,
        })
        .collect();
    offsets.sort_unstable();
    assert_eq!(
        offsets,
        vec![0, 3],
        "both offsets lower: {:?}",
        block.instrs
    );
}

#[test]
fn var_and_varip_mark_persistence() {
    let module = compile_script("var x = 1.0\nvarip y = 2.0\nplot(x + y, \"S\")");
    assert!(!has_fallback(&module.blocks[module.entry]));
    assert_eq!(module.globals.len(), 2, "both names allocate columns");
    let x = module.globals["x"];
    let y = module.globals["y"];
    assert_ne!(x, y, "distinct columns");
    assert!(
        module.varip.contains(&y) && !module.varip.contains(&x),
        "only the varip column resets per bar"
    );
}

#[test]
fn user_functions_compile_to_blocks() {
    // `f(close)` calls block 1 with one positional arg; the site names
    // the caller, the callee, and the textual call. The straight-line
    // body lowers fully — params resolve to frame slots, never globals.
    let module = compile_script("f(v) => v * 2.0\nplot(f(close), \"F\")");
    assert_eq!(module.blocks.len(), 2, "entry plus one body");
    let body = &module.blocks[1];
    assert_eq!(
        body.params.as_ref(),
        ["v".to_owned()],
        "params ride the frame head"
    );
    assert!(
        !has_fallback(body),
        "a straight-line body lowers fully: {:?}",
        body.instrs
    );
    let entry_block = entry(&module);
    assert!(!has_fallback(entry_block));
    let call = entry_block.instrs.iter().find_map(|instr| match instr {
        Instruction::Call {
            target: CallTarget::Block(index),
            site,
            args,
            ..
        } => Some((
            *index,
            module.symbols.resolve(*site).unwrap_or("?").to_owned(),
            args.len(),
        )),
        _ => None,
    });
    assert_eq!(
        call,
        Some((
            1,
            "local/Driver::f@[CallArg { name: None, value: BuiltinVariable(Close) }]".to_owned(),
            1
        )),
        "intra-module call carries block, site, arity"
    );
}

#[test]
fn bridge_calls_carry_precomputed_sites() {
    let module = compile_script("plot(ta.sma(close, 14), \"S\")\na = input.int(7, \"L\")");
    let block = entry(&module);
    assert!(!has_fallback(block), "bridges lower: {:?}", block.instrs);
    let mut saw_ta = false;
    let mut saw_input = false;
    for instr in &block.instrs {
        if let Instruction::Call {
            target: CallTarget::Builtin { namespace, method },
            site,
            ..
        } = instr
        {
            let method_name = module.symbols.resolve(*method).unwrap_or("?");
            let site_text = module.symbols.resolve(*site).unwrap_or("?");
            if *namespace == BuiltinNamespace::Ta && method_name == "sma" {
                assert!(
                    site_text.starts_with("ta.sma@"),
                    "ta site names the call: {site_text}"
                );
                saw_ta = true;
            }
            if *namespace == BuiltinNamespace::Input && method_name == "int" {
                assert!(
                    site_text.starts_with("input.int@"),
                    "input site names the call: {site_text}"
                );
                saw_input = true;
            }
        }
    }
    assert!(
        saw_ta && saw_input,
        "both bridges lower: {:?}",
        block.instrs
    );
}

#[test]
fn display_and_color_consts_fold_at_compile_time() {
    // `display.all` / `color.red` resolve through the same pure table the
    // tree-walk uses — no per-bar hashing, no runtime tables.
    let module = compile_script("plot(close, display=display.all, color=color.red, title=\"C\")");
    let block = entry(&module);
    assert!(
        !has_fallback(block),
        "display/color consts fold: {:?}",
        block.instrs
    );
}

#[test]
fn function_locals_lower_to_frame_slots() {
    // A plain declaration inside a function body binds a frame local
    // (copied, discarded on return) instead of falling back; a plot
    // right-hand side tags the run-global site while the value stays
    // frame-local, so the body links fully there too.
    let module = compile_script("f(v) =>\n    y = v * 2.0\n    y + 1.0\nplot(f(close), \"F\")");
    let body = &module.blocks[1];
    assert!(!has_fallback(body), "locals lower fully: {:?}", body.instrs);
    let plotted = compile_script("f(v) =>\n    y = plot(v, \"P\")\n    y\nplot(f(close), \"F\")");
    assert!(
        !plotted.blocks.iter().any(has_fallback),
        "a plot-bound local links with its tag: {:?}",
        plotted
            .blocks
            .iter()
            .flat_map(|block| &block.instrs)
            .collect::<Vec<_>>()
    );
    assert!(
        plotted
            .blocks
            .iter()
            .flat_map(|block| &block.instrs)
            .any(|instr| matches!(instr, crate::pine::bytecode::Instruction::PlotTag { .. })),
        "the tag travels on its own instruction"
    );
}

#[test]
fn fixture_modules_report_fully_lowered_count() {
    // Closeout ledger: how many fixture modules link with zero fallbacks
    // (candidates for per-module execution) vs need the tree-walk.
    for stem in ["_FastMAs", "_IntegrationLite"] {
        let modules = fixture::linked_fixture(stem);
        let (clean, total) = count_fully_lowered(&modules);
        eprintln!("{stem}: {clean}/{total} modules fully lowered");
    }
}

/// Count fully-lowered modules in one fixture closure with the
/// engine-mirroring registries: per-module execution sees the same
/// cross-module enums and declarations as a full run, so the count
/// reports what the split-brain guard would actually admit.
fn count_fully_lowered(modules: &[crate::LinkedModule]) -> (usize, usize) {
    // Engine-mirroring registry: per-module execution sees the same
    // cross-module enums as a full run, so the count reports what
    // the split-brain guard would actually admit.
    let sources = modules
        .iter()
        .map(|module| (module.key.as_str(), &module.program, &module.aliases))
        .collect::<Vec<_>>();
    let enums = EnumRegistry::build(&sources);
    let decls = DeclRegistry::build(&sources);
    let exports = ExportRegistry::build(&sources);
    let mut clean = 0;
    let mut total = 0;
    for module in modules {
        total += 1;
        let compiled = compile_module(
            &module.key,
            &module.program,
            &module.aliases,
            &enums,
            &decls,
            &exports,
        );
        if !compiled.blocks.iter().any(has_fallback) {
            clean += 1;
        }
    }
    (clean, total)
}

/// One row of the per-function fallback ledger: how many times each
/// reason family fired in one compiled block. Keyed by
/// `(module, block)` where `block` names the owning function or method
/// (or `<entry>` for the module top level), so a census can attribute
/// an emission to the function that fell back, not just its module.
type FunctionFallbackLedger = BTreeMap<(String, String), BTreeMap<&'static str, (usize, String)>>;

/// Per-function fallback ledger over one linked closure: compile every
/// module with the engine-mirroring registries, then tally each
/// `Fallback` instruction by `(module, owning block)` and reason family.
/// Blocks are named through the module's own `functions`/`methods`
/// tables, so a ledger row never depends on block ordering.
fn function_fallback_ledger(modules: &[crate::LinkedModule]) -> FunctionFallbackLedger {
    let sources = modules
        .iter()
        .map(|module| (module.key.as_str(), &module.program, &module.aliases))
        .collect::<Vec<_>>();
    let enums = EnumRegistry::build(&sources);
    let decls = DeclRegistry::build(&sources);
    let exports = ExportRegistry::build(&sources);
    let mut ledger: FunctionFallbackLedger = BTreeMap::new();
    for module in modules {
        let compiled = compile_module(
            &module.key,
            &module.program,
            &module.aliases,
            &enums,
            &decls,
            &exports,
        );
        // Block index → owning name. Methods and functions share the
        // block table; a block reached by both spellings reports the
        // first name in sorted order so the ledger stays deterministic.
        let mut names: Vec<Option<String>> = vec![None; compiled.blocks.len()];
        for (name, block) in &compiled.functions {
            let slot = &mut names[*block];
            if slot.as_ref().is_none_or(|known| *name < *known) {
                *slot = Some(name.clone());
            }
        }
        for ((type_name, method), entry) in &compiled.methods {
            let slot = &mut names[entry.block];
            let spelling = format!("{type_name}.{method}");
            if slot.as_ref().is_none_or(|known| spelling < *known) {
                *slot = Some(spelling);
            }
        }
        names[compiled.entry] = Some("<entry>".to_owned());
        for (block_index, block) in compiled.blocks.iter().enumerate() {
            let name = names[block_index]
                .clone()
                .unwrap_or_else(|| format!("#{block_index}"));
            for instr in &block.instrs {
                if let Instruction::Fallback { reason, .. } = instr {
                    let row = ledger
                        .entry((module.key.clone(), name.clone()))
                        .or_default()
                        .entry(reason.family())
                        .or_insert_with(|| (0, reason.display(&compiled.symbols)));
                    row.0 += 1;
                }
            }
        }
    }
    ledger
}

#[test]
fn fixture_fallback_ledger_attributes_each_emission_to_its_function() {
    // The per-function ledger is measurement, never a gate: it must
    // repeat for the same inputs and account for exactly the module-level
    // emissions the census reports, so a later lowering can name the
    // function whose count moved.
    for stem in ["_FastMAs", "_IntegrationLite"] {
        let modules = fixture::linked_fixture(stem);
        let first = function_fallback_ledger(&modules);
        let second = function_fallback_ledger(&modules);
        assert_eq!(first, second, "{stem}: ledger repeats for the same inputs");
        let per_function: usize = first
            .values()
            .flat_map(|row| row.values().map(|(count, _)| count))
            .sum();
        let (clean, total) = count_fully_lowered(&modules);
        eprintln!("{stem}: {per_function} emissions, {clean}/{total} modules fully lowered");
        for ((module, block), row) in &first {
            let count: usize = row.values().map(|(count, _)| count).sum();
            eprintln!("{stem} {module} {block}: {count}");
            for (family, (hits, example)) in row {
                eprintln!("    {family} x{hits}: {example}");
            }
        }
    }
}

#[test]
fn fixture_fully_lowered_counts_pin_baseline() {
    // Coverage ledger: the recorded per-fixture fully-linked counts are
    // measurement, never a gate — except against silent lowering. The
    // same inputs must produce the same counts on rerun, module totals
    // must match the recorded fixture shape, and fully-linked counts must
    // never drop below the recorded baseline (improvement stays allowed).
    // Recorded baseline: `_FastMAs` 3/3, `_IntegrationLite` 5/8.
    for (stem, total_recorded, clean_recorded) in [("_FastMAs", 3, 3), ("_IntegrationLite", 8, 5)] {
        let modules = fixture::linked_fixture(stem);
        let first = count_fully_lowered(&modules);
        let second = count_fully_lowered(&modules);
        assert_eq!(
            first, second,
            "{stem}: same inputs must count identically on rerun"
        );
        let (clean, total) = first;
        assert_eq!(
            total, total_recorded,
            "{stem}: fixture shape drifted: {total} modules, recorded {total_recorded}"
        );
        assert!(
            clean >= clean_recorded,
            "{stem}: fully-linked count dropped below baseline: {clean}, recorded {clean_recorded}"
        );
        eprintln!(
            "{stem}: {clean}/{total} modules fully lowered (baseline {clean_recorded}/{total_recorded})"
        );
    }
}

#[test]
fn visual_captures_lower_without_fallback() {
    // Each visual capture family lowers to its own instruction when the
    // shape is static; the tree-walk only runs the dynamic-title and
    // wrong-shape cases.
    let cases = [
        (
            "plotcandle(open, high, low, close, title=\"C\")",
            "PlotCandle",
        ),
        ("barcolor(close > open ? color.green : na)", "BarColor"),
        ("plotshape(close > open, title=\"S\")", "PlotShape"),
        ("hline(50.0, \"L\")", "Hline"),
        (
            "a = close\nb = open\nplot(a, \"A\")\nplot(b, \"B\")\nfill(a, b)",
            "Fill",
        ),
    ];
    for (script, want) in cases {
        let module = compile_script(script);
        let kinds: Vec<String> = module
            .blocks
            .iter()
            .flat_map(|block| block.instrs.iter())
            .filter_map(|instr| match instr {
                Instruction::PlotCandle { .. } => Some("PlotCandle".to_owned()),
                Instruction::BarColor { .. } => Some("BarColor".to_owned()),
                Instruction::PlotShape { .. } => Some("PlotShape".to_owned()),
                Instruction::Hline { .. } => Some("Hline".to_owned()),
                Instruction::Fill { .. } => Some("Fill".to_owned()),
                _ => None,
            })
            .collect();
        assert!(
            kinds.iter().any(|kind| kind == want),
            "{want}: the capture lowers: {kinds:?} for {script:?}"
        );
        assert!(
            !module.blocks.iter().any(has_fallback),
            "{want}: no fallback remains for {script:?}"
        );
    }
}

#[test]
fn unlowerable_constructs_fall_back_by_name() {
    let cases = [
        ("for [a, b, c] in [1, 2, 3]\n    x = a", "loop"),
        ("plotshape(close, title=close)", "plotshape"),
        ("fill(close, open)", "fill"),
        (
            "x = request.security(syminfo.tickerid, \"D\", [foo(close)])",
            "request",
        ),
        ("x = input.enum(myEnum.A, \"K\")", "enum"),
    ];
    for (script, want) in cases {
        let program = parse_program(script).expect("case parses");
        let module = compile_script_of(&program);
        let reasons: Vec<String> = module
            .blocks
            .iter()
            .flat_map(|block| fallback_reasons(&module, block))
            .collect();
        assert!(
            !reasons.is_empty(),
            "{want}: unlowerable code must fall back, got none"
        );
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains(want) || want == "unknown"),
            "{want}: the reason names the construct: {reasons:?}"
        );
    }
}

#[test]
fn fixture_closures_compile_with_measured_coverage() {
    // Coverage is measured, never gated: every fixture module compiles,
    // fallback counts and reasons are reported for the closeout work list.
    // FastMAs keeps lowered color inputs (total color constructors infer
    // statically) while enum-driven inputs and library switches stay on
    // the tree-walk by design.
    for stem in ["_FastMAs", "_IntegrationLite"] {
        let modules = fixture::linked_fixture(stem);
        // One registry per fixture closure, mirroring the engine: enum
        // members resolve across the linked set exactly as a run would
        // see them, so measured coverage matches served coverage.
        let sources = modules
            .iter()
            .map(|module| (module.key.as_str(), &module.program, &module.aliases))
            .collect::<Vec<_>>();
        let enums = EnumRegistry::build(&sources);
        let decls = DeclRegistry::build(&sources);
        let exports = ExportRegistry::build(&sources);
        let mut fallback_total = 0;
        let mut instr_total = 0;
        let mut reasons = std::collections::BTreeMap::new();
        for module in &modules {
            let compiled = compile_module(
                &module.key,
                &module.program,
                &module.aliases,
                &enums,
                &decls,
                &exports,
            );
            assert!(
                !compiled.blocks.is_empty(),
                "{stem}: every module compiles to at least an entry block"
            );
            for block in &compiled.blocks {
                for instr in &block.instrs {
                    instr_total += 1;
                    if let Instruction::Fallback { reason, .. } = instr {
                        fallback_total += 1;
                        *reasons
                            .entry(reason.display(&compiled.symbols))
                            .or_insert(0) += 1;
                    }
                }
            }
        }
        eprintln!("{stem}: {instr_total} instrs, {fallback_total} fallbacks");
        eprintln!("{stem} fallback reasons: {reasons:?}");
        assert!(instr_total > 0, "{stem}: the compiler emits code");
    }
    // Spot check: the FastMAs entry keeps lowered color inputs.
    let modules = fixture::linked_fixture("_FastMAs");
    let entry = modules
        .iter()
        .find(|module| module.key == "local/_FastMAs")
        .expect("fixture root linked");
    let sources = modules
        .iter()
        .map(|module| (module.key.as_str(), &module.program, &module.aliases))
        .collect::<Vec<_>>();
    let enums = EnumRegistry::build(&sources);
    let decls = DeclRegistry::build(&sources);
    let exports = ExportRegistry::build(&sources);
    let compiled = compile_module(
        &entry.key,
        &entry.program,
        &entry.aliases,
        &enums,
        &decls,
        &exports,
    );
    let entry_block = &compiled.blocks[compiled.entry];
    let has_color_input = entry_block.instrs.iter().any(|instr| match instr {
        Instruction::Call {
            target:
                CallTarget::Builtin {
                    namespace: BuiltinNamespace::Input,
                    method,
                },
            ..
        } => compiled.symbols.resolve(*method).unwrap_or("?") == "color",
        _ => false,
    });
    assert!(
        has_color_input,
        "total color constructors infer statically: {:?}",
        entry_block.instrs
    );
}

#[test]
fn enum_members_lower_to_identity_consts() {
    // Declared-before-use members fold to `ConstValue::Enum` (identity
    // handles, exactly what the tree-walk's table returns); builtin
    // handles (`size.*`) fold the same way — they resolve
    // unconditionally, with no declaration to order against.
    let module = compile_script_with_enums(
        "export enum E\n    A = \"a\"\n    B = \"b\"\nv = E.A\nw = size.auto\nplot(close, title=\"C\")",
    );
    assert!(
        !module.blocks.iter().any(has_fallback),
        "declared members lower: {:?}",
        module
            .blocks
            .iter()
            .flat_map(|block| block.instrs.iter())
            .collect::<Vec<_>>()
    );
    let mut handles = Vec::new();
    for block in &module.blocks {
        for instr in &block.instrs {
            if let Instruction::Const { value, .. } = instr {
                if let Some(ConstValue::Enum { namespace, member }) = module.consts.get(*value) {
                    handles.push((
                        module.symbols.resolve(*namespace).unwrap_or("?").to_owned(),
                        module.symbols.resolve(*member).unwrap_or("?").to_owned(),
                    ));
                }
            }
        }
    }
    for (namespace, member) in [("E", "A"), ("size", "auto")] {
        assert!(
            handles
                .iter()
                .any(|handle| handle == &(namespace.to_owned(), member.to_owned())),
            "{namespace}.{member} folds to an identity const: {handles:?}"
        );
    }
}

#[test]
fn enum_uses_respect_declaration_order() {
    // A use resolves statically only when its declaration already
    // executed: later statements in the same module, never earlier ones.
    // Anything else resolves through the registered table at run time
    // (marks observe every declaration execution exactly like the
    // tree-walk): use-before-declaration and same-module function-body
    // uses lower to the checked read instead of falling back, and refuse
    // identically when reached before registration.
    let before =
        compile_script_with_enums("x = E.A\nexport enum E\n    A = \"a\"\nplot(close, \"C\")");
    assert!(
        !before.blocks.iter().any(has_fallback),
        "use-before-declaration takes the checked read"
    );
    assert!(
        before.blocks.iter().any(has_enum_read),
        "use-before-declaration reads through the table"
    );
    let body = compile_script_with_enums(
        "export enum E\n    A = \"a\"\nf() => E.A\nplot(f(), title=\"F\")",
    );
    assert!(
        !body.blocks.iter().any(has_fallback),
        "same-module function-body use takes the checked read"
    );
    assert!(
        body.blocks.iter().any(has_enum_read),
        "same-module function-body use reads through the table"
    );
    let after =
        compile_script_with_enums("export enum E\n    A = \"a\"\nv = E.A\nplot(v, title=\"V\")");
    assert!(
        !after.blocks.iter().any(has_fallback),
        "declared-before-use lowers"
    );
}

#[test]
fn enum_registry_orders_declarations() {
    // Unit contract for the registry: first declaration wins across
    // modules, trailing `Enum.Member` pairs resolve on their last two
    // segments, and unknown members, unknown enums, and bare names miss.
    // Bodies resolve only when no importer could arrive first (neither
    // fixture module imports the other here, so earlier-module bodies
    // resolve while backward-called ones do not).
    let a = parse_program("export enum E\n    A = 1\nx = 1").expect("parses");
    let b = parse_program("export enum F\n    B = 1\ny = F.B").expect("parses");
    let empty: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let registry = EnumRegistry::build(&[("lib/A", &a, &empty), ("lib/B", &b, &empty)]);
    let hit = registry
        .lookup("E.A", "lib/B", 1, Some(1), false)
        .expect("earlier module resolves");
    assert_eq!((hit.enum_name.as_str(), hit.member.as_str()), ("E", "A"));
    assert!(
        registry.lookup("F.B", "lib/B", 1, Some(1), false).is_some(),
        "same-module later use resolves"
    );
    assert!(
        registry.lookup("F.B", "lib/B", 1, Some(0), false).is_none(),
        "use-before-declaration misses even for declared members"
    );
    assert!(
        registry.lookup("E.A", "lib/B", 1, Some(0), true).is_some(),
        "bodies resolve earlier modules no importer precedes"
    );
    assert!(
        registry.lookup("F.B", "lib/A", 0, Some(5), true).is_none(),
        "function bodies never see their own module"
    );
    // A backward-called body keeps its miss: the caller imports the body
    // module from a position at or before the declaration.
    let caller =
        parse_program("import test/use_/1 as L\nv = L.f()\nplot(v, title=\"V\")").expect("parses");
    let decl = parse_program("export enum E\n    A = \"a\"").expect("parses");
    let use_ = parse_program("export f() => E.A").expect("parses");
    let backward = EnumRegistry::build(&[
        (
            "caller",
            &caller,
            &std::collections::HashMap::from([("L".to_owned(), "use_".to_owned())]),
        ),
        ("decl", &decl, &empty),
        ("use_", &use_, &empty),
    ]);
    assert!(
        backward.lookup("E.A", "use_", 2, None, true).is_none(),
        "backward-called bodies never resolve"
    );
    assert!(
        registry.lookup("E.B", "lib/B", 1, Some(1), false).is_none(),
        "first declaration wins: later modules never extend the table"
    );
    assert!(
        registry.lookup("E.Z", "lib/B", 1, Some(1), false).is_none(),
        "unknown members miss"
    );
    assert!(
        registry.lookup("Q.A", "lib/B", 1, Some(1), false).is_none(),
        "unknown enums miss"
    );
    assert!(
        registry.lookup("E", "lib/B", 1, Some(1), false).is_none(),
        "bare names miss"
    );
    // Alias prefixes resolve on the trailing pair, like the tree-walk.
    assert!(
        registry
            .lookup("UTIL.Smoothings.WMA", "lib/B", 1, Some(1), false)
            .is_none(),
        "unregistered aliases miss"
    );
}

#[test]
fn multi_statement_items_report_their_own_reason() {
    // Multi-declarations and multi-assignments lower item by item, so an
    // unlowerable item reports the construct that failed instead of a
    // tuple-shaped wrapper. A dotted destructure target stays a tuple
    // fallback until the shared home model can carry it.
    let module = compile_script("float a = 1.0, b = UTIL.Nope.value");
    let families: Vec<&'static str> = module
        .blocks
        .iter()
        .flat_map(|block| &block.instrs)
        .filter_map(|instr| match instr {
            Instruction::Fallback { reason, .. } => Some(reason.family()),
            _ => None,
        })
        .collect();
    assert!(
        families.contains(&"DottedName"),
        "the failing item names its own family: {families:?}"
    );
    assert!(
        !families.contains(&"TupleUnpack"),
        "the tuple shape never masks the item reason: {families:?}"
    );
    let linked = compile_script("float a = 1.0, b = 2.0\nplot(a + b, title=\"V\")");
    assert!(
        !linked.blocks.iter().any(has_fallback),
        "a clean multi-declaration links: {:?}",
        linked
            .blocks
            .iter()
            .flat_map(|block| &block.instrs)
            .collect::<Vec<_>>()
    );
    let destructured =
        compile_script("arr = [1.0, 2.0]\n[a, box.field] = arr\nplot(a, title=\"V\")");
    assert!(
        destructured
            .blocks
            .iter()
            .flat_map(|block| &block.instrs)
            .any(|instr| matches!(
                instr,
                Instruction::Fallback {
                    reason: FallbackReason::TupleUnpack,
                    ..
                }
            )),
        "a dotted destructure target keeps the tuple fallback"
    );
}

#[test]
fn literal_arithmetic_folds_to_const_at_compile_time() {
    // `1.0 + 2.0` evaluates once at compile time through the shared leaf:
    // the block holds `Const` instructions and no `BinOp`/`UnOp` for the
    // folded shapes, while a refusing pair (`1.0 + true`) keeps its `BinOp`
    // so the refusal fires at run time, never at link.
    let module = compile_script("plot(1.0 + 2.0, \"F\")\nplot(-3.0, \"G\")");
    let block = entry(&module);
    assert!(
        !block.instrs.iter().any(|instr| matches!(
            instr,
            Instruction::BinOp { .. } | Instruction::UnOp { .. }
        )),
        "folded block has no arithmetic: {:?}",
        block.instrs
    );
    let module = compile_script("x = 1.0 + true\nplot(x, \"F\")");
    let block = entry(&module);
    assert!(
        block.instrs.iter().any(|instr| matches!(instr, Instruction::BinOp { .. })),
        "a refusing pair keeps its BinOp: {:?}",
        block.instrs
    );
}
