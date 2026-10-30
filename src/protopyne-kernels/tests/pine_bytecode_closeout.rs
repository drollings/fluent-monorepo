//! Closeout proof: the fallback audit, the coverage ledger, the no-weakening
//! floors, and the documentation scan.
//!
//! These tests are the roadmap's last gate. They do not test behaviour the
//! differential suite does not already cover; they pin *dispositions* — the
//! claims a later change could quietly weaken (a retired family coming back,
//! a fixture count drifting, an assertion being deleted, a doc claiming an
//! engine shape that no longer exists).

#![cfg(feature = "bytecode")]

use crate::pine::bytecode::{
    fallback_disposition, FallbackDisposition, FallbackReason, Instruction, FALLBACK_DISPOSITIONS,
};
use crate::pine::test_fixture as fixture;
use crate::pine::trigger::{trigger_kind, TriggerKind, TRIGGERS};
use crate::pine::BytecodeMode;
use crate::{InputSet, PineEngine, PineRuntimeConfig};

fn corpus_fixtures() -> Vec<&'static str> {
    vec!["_FastMAs", "_IntegrationLite"]
}

/// Every emitted fallback in one compiled module set, with its family name.
fn emitted_families(modules: &[crate::LinkedModule]) -> Vec<&'static str> {
    let program = crate::pine::bytecode_diff_tests::compile_case(modules);
    let mut families = Vec::new();
    for module in modules {
        let Some(compiled) = program.get(&module.key) else {
            continue;
        };
        for block in &compiled.blocks {
            for instr in &block.instrs {
                if let Instruction::Fallback { reason, .. } = instr {
                    families.push(reason.wire_name());
                }
            }
        }
    }
    families
}

#[test]
fn closeout_fallback_disposition_is_complete() {
    // Every family has exactly one row, every row names a real family, and
    // `wire_name` cannot be added to without a disposition (the registry and
    // the exhaustive match are cross-checked here).
    let mut names: Vec<&str> = FALLBACK_DISPOSITIONS
        .iter()
        .map(|(name, _)| *name)
        .collect();
    names.sort_unstable();
    let mut deduped = names.clone();
    deduped.dedup();
    assert_eq!(names, deduped, "one row per family, never two");
    // The variant count is pinned: a new variant must add a row and a name.
    assert_eq!(
        FALLBACK_DISPOSITIONS.len(),
        38,
        "the audit covers every FallbackReason variant"
    );
    for (name, _) in FALLBACK_DISPOSITIONS {
        assert!(
            fallback_disposition(name).is_some(),
            "{name} resolves through the registry"
        );
    }
    assert_eq!(fallback_disposition("not-a-family"), None);
}

/// Every `pub const` gate declaration in the files that carry gates.
fn gate_consts(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("pub const "))
        .map(|rest| rest.split(':').next().unwrap_or("").trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

#[test]
fn unregistered_gate_fails_the_build() {
    // Self-enforcing trigger registry: every numeric gate constant declared
    // in the gate-carrying files has a registry row with its kind, and every
    // constant-backed row names a declared constant. A new gate constant
    // without a row fails the scan below instead of drifting into the wrong
    // category; a removed constant fails the pinned declaration list.
    let mut names: Vec<&str> = TRIGGERS.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "headless_effect",
            "max_call_depth",
            "max_loop_iterations",
            "split_brain_coverage",
            "tier_stable_bars",
        ],
        "one registry row per known gate, never two"
    );
    let exec = include_str!("../src/pine/bytecode_exec.rs");
    let eval = include_str!("../src/pine/eval.rs");
    let engine = include_str!("../src/pine/engine.rs");
    // Constant-backed rows: the backing constant is declared with its kind.
    for (constant, trigger, kind) in [
        (
            "TIER_STABLE_BARS",
            "tier_stable_bars",
            TriggerKind::Confidence,
        ),
        (
            "SPLIT_BRAIN_COVERAGE",
            "split_brain_coverage",
            TriggerKind::Correctness,
        ),
        ("MAX_CALL_DEPTH", "max_call_depth", TriggerKind::Correctness),
        (
            "MAX_LOOP_ITERATIONS",
            "max_loop_iterations",
            TriggerKind::Correctness,
        ),
    ] {
        assert!(
            exec.contains(&format!("pub const {constant}"))
                || eval.contains(&format!("pub const {constant}"))
                || engine.contains(&format!("pub const {constant}")),
            "{constant} is declared as the backing constant"
        );
        assert_eq!(
            trigger_kind(trigger),
            Some(kind),
            "{trigger} is registered with its kind"
        );
    }
    // Classifier-backed row: no numeric constant, but a known entry point
    // carrying the decision.
    {
        let (trigger, kind, file, entry) = (
            "headless_effect",
            TriggerKind::Correctness,
            include_str!("../src/pine/builtins_core.rs"),
            "pub fn call(",
        );
        assert_eq!(
            trigger_kind(trigger),
            Some(kind),
            "{trigger} is registered with its kind"
        );
        assert!(
            file.contains(entry),
            "{trigger} names its carrying entry point"
        );
    }
    // Full scan: the gate-carrying files declare exactly the four known
    // gate constants, and each one is registered. A fifth `pub const` here
    // fails the pinned list before its kind is even asked.
    let mut consts = gate_consts(exec);
    consts.extend(gate_consts(eval));
    consts.extend(gate_consts(engine));
    consts.sort_unstable();
    assert_eq!(
        consts,
        vec![
            "MAX_CALL_DEPTH".to_owned(),
            "MAX_LOOP_ITERATIONS".to_owned(),
            "SPLIT_BRAIN_COVERAGE".to_owned(),
            "TIER_STABLE_BARS".to_owned(),
        ],
        "the gate files declare exactly the registered gate constants"
    );
    for constant in &consts {
        assert!(
            trigger_kind(&constant.to_lowercase()).is_some(),
            "{constant} has a registry row"
        );
    }
    // What a violation looks like: the scan sees a new gate constant while
    // the registry has no row for it.
    let flagged = gate_consts("pub const FAKE_GATE: u32 = 1;\n");
    assert_eq!(
        flagged,
        vec!["FAKE_GATE".to_owned()],
        "the scan sees a new gate constant"
    );
    assert_eq!(
        trigger_kind("fake_gate"),
        None,
        "an unregistered gate resolves to nothing"
    );
}

#[test]
fn closeout_no_deterministic_headless_failures_hide_behind_retention() {
    // A family marked `Retired` must have no emission site left in the
    // compiler, and a fixture emission must always be a retained family:
    // nothing deterministic-headless is written off as "live/external".
    let compiler = include_str!("../src/pine/bytecode_compile.rs");
    for (name, disposition) in FALLBACK_DISPOSITIONS {
        if *disposition != FallbackDisposition::Retired {
            continue;
        }
        // Variant spelling is the family name in CamelCase; the compiler
        // constructs variants as `FallbackReason::<Variant>`.
        let variant: String = name
            .split('_')
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                    None => String::new(),
                }
            })
            .collect();
        assert!(
            !compiler.contains(&format!("FallbackReason::{variant}")),
            "{name} is retired but the compiler still emits it"
        );
    }
    // Every fixture emission is retained (external/runtime/object-model/
    // wrapper). A `Lowered` family appearing in a fixture is fine — it means
    // the shape is still unlowered there — but a `Retired` one would be a
    // regression, which the loop above already forbids at the source.
    for stem in corpus_fixtures() {
        let modules = fixture::linked_fixture(stem);
        for family in emitted_families(&modules) {
            let disposition =
                fallback_disposition(family).unwrap_or_else(|| panic!("{stem}: {family} audited"));
            assert_ne!(
                disposition,
                FallbackDisposition::Retired,
                "{stem}: retired family {family} reappeared"
            );
        }
    }
}

#[test]
fn corpus_has_no_fallback_emissions() {
    // The cutover freeze: `_FastMAs` and every differential script emit
    // nothing; `_IntegrationLite` emits only the three signed residuals
    // (two persistent-shadowing bindings and one dead live-fetch, each
    // with a table row and a divergence/deadness proof); each data-root
    // closure stays within its frozen family set. Any new family or any
    // rise fails here; further lowerings only ever relax these ceilings.
    let fastmas = emitted_families(&fixture::linked_fixture("_FastMAs"));
    assert!(
        fastmas.is_empty(),
        "_FastMAs emits no fallback: {fastmas:?}"
    );
    let lite = fixture::linked_fixture("_IntegrationLite");
    let lite_rows = emitted_rows(&lite);
    let mut lite_counts: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    for (_, _, family) in &lite_rows {
        *lite_counts.entry(family).or_default() += 1;
    }
    assert_eq!(
        lite_counts,
        std::collections::BTreeMap::from([("function_body_binding", 2), ("outside_builtin", 1),]),
        "_IntegrationLite emits only the frozen triple"
    );
    let mut sites: std::collections::BTreeSet<(&str, &str, &str)> =
        std::collections::BTreeSet::new();
    for (module, func, family) in &lite_rows {
        sites.insert((module.as_str(), func.as_str(), *family));
    }
    assert!(
        sites.contains(&(
            "showmethegrail/libBars",
            "Bars.getAlpha",
            "function_body_binding"
        )),
        "bar_ratio residual stays attributed: {sites:?}"
    );
    assert!(
        sites.contains(&(
            "showmethegrail/libMAs",
            "getVossPredictiveFilter",
            "function_body_binding"
        )),
        "alpha residual stays attributed: {sites:?}"
    );
    assert!(
        sites.contains(&(
            "showmethegrail/libCandles",
            "MarketContext.fetchWithCandlePatterns",
            "outside_builtin"
        )),
        "dead live-fetch residual stays attributed: {sites:?}"
    );
    // Every differential script links fallback-free; a new script that
    // needs a fallback updates this gate, never sneaks past it.
    for case in crate::pine::bytecode_diff_tests::diff_corpus() {
        let families = emitted_families(&case.modules);
        assert!(
            families.is_empty(),
            "differential script {} emits no fallback: {families:?}",
            case.name
        );
    }
    // Data-root closures stay within their frozen family sets: no new
    // family may appear and no count may rise. Lowerings relax these
    // ceilings by editing the frozen table below.
    let frozen: &[(&str, &[(&str, usize)])] = &[
        ("alexgrover_QuadraticRegression", &[]),
        (
            "BigBeluga_GradientRange",
            &[("call_argument", 1), ("dotted_name", 1), ("outside_builtin", 1)],
        ),
        (
            "ChartPrime_ZoneShift",
            &[("dotted_name", 2), ("ternary_arm", 2)],
        ),
        ("Eterna_Pullback_Indicator_Trial", &[]),
        ("LazyBear_SqueezeMomentum", &[("call_argument", 1)]),
        (
            "LuxAlgo_VolumeDeltaCandles_5",
            &[
                ("call_argument", 5),
                ("dotted_name", 1),
                ("outside_builtin", 1),
                ("ternary_condition", 1),
            ],
        ),
        (
            "LuxAlgo_VolumeDeltaMethods_5",
            &[("call_argument", 3), ("outside_builtin", 2)],
        ),
        (
            "TradingIQ_LiquidityWalls_ASAP",
            &[("call_argument", 3), ("dotted_name", 2), ("outside_builtin", 1)],
        ),
        (
            "TradingIQ_VolumeSupplyAndDemand",
            &[
                ("call_argument", 5),
                ("dotted_name", 1),
                ("if_condition", 7),
                ("outside_builtin", 4),
            ],
        ),
        ("Zeiierman_MLMI", &[]),
        (
            "_DeltaVolumeProfileProtopyne",
            &[
                ("call_argument", 1),
                ("outside_builtin", 6),
                ("ternary_arm", 1),
            ],
        ),
        ("_FastMAs", &[]),
        (
            "_IntegrationLite",
            &[("function_body_binding", 2), ("outside_builtin", 1)],
        ),
    ];
    let root = fixture::data_root();
    assert!(root.exists(), "the data root exists for the census");
    let (indicators, libraries) =
        crate::manifest_files::load_pine_sources(&root).expect("data root loads");
    let mut seen = std::collections::BTreeSet::new();
    for outcome in crate::discover_closures(&indicators, &libraries) {
        let closure = match outcome {
            crate::ClosureOutcome::Linked(closure) => closure,
            crate::ClosureOutcome::Refused { .. } => continue,
        };
        let Ok(inputs) = crate::closure_inputs(&closure) else {
            continue;
        };
        let modules = fixture::linked_modules(inputs);
        let families = emitted_families(&modules);
        let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for family in &families {
            *counts.entry(family).or_default() += 1;
        }
        let (_, allowed) = frozen
            .iter()
            .find(|(stem, _)| *stem == closure.stem.as_str())
            .unwrap_or_else(|| panic!("data-root closure {} has no frozen row", closure.stem));
        for (family, hits) in &counts {
            let ceiling = allowed
                .iter()
                .find(|(name, _)| name == family)
                .unwrap_or_else(|| {
                    panic!(
                        "data-root closure {} emits a new family {family}",
                        closure.stem
                    )
                })
                .1;
            assert!(
                *hits <= ceiling,
                "data-root closure {}: {family} rose to {hits} (ceiling {ceiling})",
                closure.stem
            );
        }
        seen.insert(closure.stem.clone());
    }
    for (stem, _) in frozen {
        assert!(
            seen.contains(*stem),
            "frozen data-root closure {stem} was censused"
        );
    }
}

/// One fallback emission attributed to its owning function: module key,
/// function or method spelling (`<entry>` for the module top level), and
/// reason family. Block ownership resolves through the module's own
/// function and method tables, so a row never depends on block ordering.
fn emitted_rows(modules: &[crate::LinkedModule]) -> Vec<(String, String, &'static str)> {
    let program = crate::pine::bytecode_diff_tests::compile_case(modules);
    let mut rows = Vec::new();
    for module in modules {
        let Some(compiled) = program.get(&module.key) else {
            continue;
        };
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
                    rows.push((module.key.clone(), name.clone(), reason.wire_name()));
                }
            }
        }
    }
    rows
}

#[test]
fn must_lower_corpus_is_exhaustive() {
    // The must-lower control group: one program per lowered shape. Every
    // program links with coverage exactly 1.0 and both selections agree
    // byte-for-byte, so the splitter trusts the link and the gate trusts
    // the loop. Adding a lowering adds its program here.
    let bars = crate::pine::bytecode_diff_tests::ramp_bars(10);
    let lib = crate::parse_program(
        "export enum E\n    A = \"a\"\n    B = \"b\"\nexport foo(float a, float b, float c = 10.0) =>\n    a + b + c\nexport with_enum(float a, E e = E.A) =>\n    a + (e == E.A ? 100.0 : 200.0)",
    )
    .expect("control library parses");
    let cases: Vec<(&str, Vec<crate::LinkedModule>)> = vec![
        (
            "baseline values",
            crate::pine::bytecode_diff_tests::link_script("a = close + 1.0\nplot(a, title=\"V\")"),
        ),
        (
            "cross-module omitted defaults",
            vec![
                crate::LinkedModule::new("test/lib", lib.clone(), Default::default()),
                crate::LinkedModule::new(
                    "local/Driver",
                    crate::parse_program(
                        "import test/lib/1 as L\nv = L.foo(close, 1.0)\nplot(v, title=\"V\")",
                    )
                    .expect("parses"),
                    [("L".to_owned(), "test/lib".to_owned())].into_iter().collect(),
                ),
            ],
        ),
        (
            "cross-module enum default",
            vec![
                crate::LinkedModule::new("test/lib", lib.clone(), Default::default()),
                crate::LinkedModule::new(
                    "local/Driver",
                    crate::parse_program(
                        "import test/lib/1 as L\nv = L.with_enum(close)\nplot(v, title=\"V\")",
                    )
                    .expect("parses"),
                    [("L".to_owned(), "test/lib".to_owned())].into_iter().collect(),
                ),
            ],
        ),
        (
            "checked rebind live and dead",
            crate::pine::bytecode_diff_tests::link_script(
                "x = 1.0\nx := 2.0\nif close < 0.0\n    q := 1.0\nplot(x, title=\"X\")",
            ),
        ),
        (
            "declared persistent reads na",
            crate::pine::bytecode_diff_tests::link_script(
                "var x = na\nv = x\nplot(close, title=\"C\")",
            ),
        ),
        (
            "in-function cross-module enum",
            vec![
                crate::LinkedModule::new(
                    "test/enum",
                    crate::parse_program("export enum E\n    A = \"a\"\n    B = \"b\"")
                        .expect("parses"),
                    Default::default(),
                ),
                crate::LinkedModule::new(
                    "test/user",
                    crate::parse_program("export pick() =>\n    E.A == E.A ? 10.0 : 20.0")
                        .expect("parses"),
                    Default::default(),
                ),
                crate::LinkedModule::new(
                    "local/Driver",
                    crate::parse_program(
                        "import test/user/1 as U\nv = U.pick()\nplot(v, title=\"V\")",
                    )
                    .expect("parses"),
                    [("U".to_owned(), "test/user".to_owned())].into_iter().collect(),
                ),
            ],
        ),
        (
            "unexported alias read",
            vec![
                crate::LinkedModule::new(
                    "test/lib",
                    crate::parse_program("export const float SHOWN = 1.0\nhidden = 2.0\n")
                        .expect("parses"),
                    Default::default(),
                ),
                crate::LinkedModule::new(
                    "local/Driver",
                    crate::parse_program("v = L.hidden + close\nplot(v, title=\"V\")")
                        .expect("parses"),
                    [("L".to_owned(), "test/lib".to_owned())].into_iter().collect(),
                ),
            ],
        ),
        (
            "barstate flags",
            crate::pine::bytecode_diff_tests::link_script(
                "v = (barstate.isfirst ? 1.0 : 0.0) + (barstate.islast ? 10.0 : 0.0)\nplot(v, title=\"V\")",
            ),
        ),
        (
            "trailing statement-switch in body",
            crate::pine::bytecode_diff_tests::link_script(
                "pick(int e) =>\n    switch e\n        1 => 10.0\n        2 => 20.0\n        => 0.0\nv = pick(1)\nplot(v, title=\"V\")",
            ),
        ),
        (
            "computed receiver method call",
            crate::pine::bytecode_diff_tests::link_script(
                "type Box2\n    int n = 1\nmethod bump(Box2 this) =>\n    this.n := this.n + 1\n    this.n\nmake() => Box2.new()\ny = make().bump()\nplot(y, title=\"V\")",
            ),
        ),
        (
            "alias construction",
            vec![
                crate::LinkedModule::new(
                    "test/lib",
                    crate::parse_program("export type Widget\n    int n = 1\n")
                        .expect("parses"),
                    Default::default(),
                ),
                crate::LinkedModule::new(
                    "test/user",
                    crate::parse_program(
                        "import test/lib/1 as A\nexport run() =>\n    A.Widget.new().n",
                    )
                    .expect("parses"),
                    [("A".to_owned(), "test/lib".to_owned())].into_iter().collect(),
                ),
                crate::LinkedModule::new(
                    "local/Driver",
                    crate::parse_program("import test/user/1 as U\nv = U.run()\nplot(v, title=\"V\")")
                        .expect("parses"),
                    [("U".to_owned(), "test/user".to_owned())].into_iter().collect(),
                ),
            ],
        ),
        (
            "this statement call",
            crate::pine::bytecode_diff_tests::link_script(
                "type B\n    int n = 1\nmethod helper(B this, int d) =>\n    this.n := d\n    this.n\nmethod go(B this) =>\n    this.helper(5)\n    this.n\nw = B.new()\ny = w.go()\nplot(y, title=\"V\")",
            ),
        ),
        (
            "shadowing parameter history",
            crate::pine::bytecode_diff_tests::link_script(
                "g = 0.0\nf(g) =>\n g[1]\nplot(f(1.0), title=\"V\")",
            ),
        ),
        (
            "dynamic offset history",
            crate::pine::bytecode_diff_tests::link_script(
                "f(p, i) =>\n    p[i]\nv = f(close, 1)\nplot(v, title=\"V\")",
            ),
        ),
        (
            "nested dotted home writeback",
            crate::pine::bytecode_diff_tests::link_script(
                "type Box\n    float v = 0.0\nid(x) => x\nb = Box.new()\nb.v := 3.0\ny = id(b.v)\nplot(y, title=\"V\")",
            ),
        ),
    ];
    for (name, modules) in &cases {
        let program = crate::pine::bytecode_diff_tests::compile_case(modules);
        let vm = crate::pine::bytecode_diff_tests::link_case(&program, modules)
            .unwrap_or_else(|err| panic!("{name}: must-lower links clean: {err:?}"));
        // Coverage exactly 1.0 reads as zero fallback instructions: the
        // link above already refused anything under the split-brain
        // guard, so an emission scan pins the 1.0 without comparing
        // floats.
        let mut emissions = 0;
        for module in modules {
            if let Some(compiled) = program.get(&module.key) {
                for block in &compiled.blocks {
                    emissions += block
                        .instrs
                        .iter()
                        .filter(|instr| {
                            matches!(instr, crate::pine::bytecode::Instruction::Fallback { .. })
                        })
                        .count();
                }
            }
        }
        assert_eq!(emissions, 0, "{name}: must-lower links with coverage 1.0");
        let config = crate::pine::bytecode_exec::VmConfig::headless();
        let got = crate::pine::bytecode_diff_tests::run_vm(&vm, &bars, &config);
        let mut engine = PineEngine::new(engine_config());
        engine.set_bytecode_mode(BytecodeMode::TreeWalk);
        let tree = engine.run_linked(modules, &bars, InputSet::new());
        assert_eq!(
            crate::pine::bytecode_diff_tests::fingerprint_result(&got),
            crate::pine::bytecode_diff_tests::fingerprint_result(&tree),
            "{name}: must-lower agrees"
        );
    }
}

#[test]
fn must_refuse_corpus_is_exact() {
    // The must-refuse control group: a reached runtime error, a served
    // live fetch with no tapes injected, an unserved live-fetch shape,
    // and a reached external dialogue call. Every program refuses with
    // its typed wording on both selections; precision (refusals that
    // were expected) and recall (expected refusals that fired) both pin
    // at 1.0, so the gate trusts refusals exactly as far as this set.
    let bars = crate::pine::bytecode_diff_tests::ramp_bars(10);
    let cases: Vec<(&str, Vec<crate::LinkedModule>, &str)> = vec![
        (
            "reached runtime error",
            crate::pine::bytecode_diff_tests::link_script(
                "runtime.error(\"boom\")\nplot(close, title=\"C\")",
            ),
            "boom",
        ),
        (
            "served live fetch with no injection",
            crate::pine::bytecode_diff_tests::link_script(
                "v = request.security(syminfo.tickerid, \"D\", close)\nplot(v, title=\"V\")",
            ),
            "live request.security",
        ),
        (
            "unserved live-fetch shape",
            crate::pine::bytecode_diff_tests::link_script(
                "v = request.security(syminfo.tickerid, \"D\", open[1])\nplot(v, title=\"V\")",
            ),
            "live request.security",
        ),
        (
            "reached external dialogue",
            crate::pine::bytecode_diff_tests::link_script(
                "x = request.marker(close)\nplot(x, title=\"V\")",
            ),
            "request.marker",
        ),
    ];
    let mut expected = 0;
    let mut fired = 0;
    let mut fired_expected = 0;
    for (name, modules, want) in &cases {
        expected += 1;
        let mut engine = PineEngine::new(engine_config());
        engine.set_bytecode_mode(BytecodeMode::Compiled);
        let compiled = engine.run_linked(modules, &bars, InputSet::new());
        let mut tree_engine = PineEngine::new(engine_config());
        tree_engine.set_bytecode_mode(BytecodeMode::TreeWalk);
        let tree = tree_engine.run_linked(modules, &bars, InputSet::new());
        let compiled_text = compiled.expect_err(&format!("{name}: compiled refuses"));
        let tree_text = tree
            .expect_err(&format!("{name}: tree refuses"))
            .to_string();
        let compiled_text = compiled_text.to_string();
        assert_eq!(
            compiled_text, tree_text,
            "{name}: refusal wording is engine-independent"
        );
        assert!(
            compiled_text.contains(want),
            "{name}: refusal names {want}: {compiled_text}"
        );
        fired += 1;
        fired_expected += 1;
    }
    // Precision and recall over the control group: every refusal was
    // expected (no must-lower program refuses — proven by the exhaustive
    // test above, which would fail first) and every expected refusal
    // fired here. Pinned as exact integer equality, which is precision
    // 1.0 and recall 1.0 with no float comparison involved.
    assert_eq!(fired_expected, fired, "precision pins at 1.0");
    assert_eq!(fired_expected, expected, "recall pins at 1.0");
}

#[test]
fn closeout_coverage_ledger() {
    // The recorded census: fallback emissions per checked-in closure.
    // Emissions may only fall; a move in the wrong direction fails here
    // instead of drifting silently.
    let bars = crate::pine::bytecode_diff_tests::ramp_bars(20);
    for (stem, max_emissions) in [("_FastMAs", 65), ("_IntegrationLite", 136)] {
        let modules = fixture::linked_fixture(stem);
        let emissions = emitted_families(&modules).len();
        assert!(
            emissions <= max_emissions,
            "{stem}: emissions rose to {emissions} (ceiling {max_emissions})"
        );
        // The compiled selection must still agree with the oracle.
        let mut tree_engine = PineEngine::new(engine_config());
        tree_engine.set_bytecode_mode(BytecodeMode::TreeWalk);
        let tree = tree_engine.run_linked(&modules, &bars, InputSet::new());
        let mut engine = PineEngine::new(engine_config());
        engine.set_bytecode_mode(BytecodeMode::Compiled);
        let compiled = engine.run_linked(&modules, &bars, InputSet::new());
        assert_eq!(
            crate::pine::bytecode_diff_tests::fingerprint_result(&compiled),
            crate::pine::bytecode_diff_tests::fingerprint_result(&tree),
            "{stem}: compiled selection is byte-identical"
        );
    }
}

fn engine_config() -> PineRuntimeConfig {
    PineRuntimeConfig::new()
        .chart_symbol("SPY".to_owned())
        .chart_period("1".to_owned())
        .warmup_bars(0)
        .build()
}

/// Every fallback family the deterministic corpus emits: all hermetic
/// fixture closures that link, the data-root closures that link, and the
/// fixed differential scripts.
fn corpus_emitted_families() -> std::collections::BTreeSet<&'static str> {
    let mut emitted = std::collections::BTreeSet::new();
    for outcome in fixture::fixture_outcomes() {
        let closure = match outcome {
            crate::ClosureOutcome::Linked(closure) => closure,
            crate::ClosureOutcome::Refused { .. } => continue,
        };
        let Ok(inputs) = crate::closure_inputs(&closure) else {
            continue;
        };
        emitted.extend(emitted_families(&fixture::linked_modules(inputs)));
    }
    let root = fixture::data_root();
    if root.exists() {
        let (indicators, libraries) =
            crate::manifest_files::load_pine_sources(&root).expect("data root loads");
        for outcome in crate::discover_closures(&indicators, &libraries) {
            let closure = match outcome {
                crate::ClosureOutcome::Linked(closure) => closure,
                crate::ClosureOutcome::Refused { .. } => continue,
            };
            let Ok(inputs) = crate::closure_inputs(&closure) else {
                continue;
            };
            emitted.extend(emitted_families(&fixture::linked_modules(inputs)));
        }
    }
    for case in crate::pine::bytecode_diff_tests::diff_corpus() {
        emitted.extend(emitted_families(&case.modules));
    }
    emitted
}

/// One parsed decision-table row: family plus verdict.
struct CoverageRow {
    family: String,
    verdict: String,
}

/// The per-family decision table: family and verdict columns of the
/// table in `doc/protopyne-rs/COVERAGE.md`.
fn coverage_table() -> Vec<CoverageRow> {
    let doc = include_str!("../../../doc/protopyne-rs/COVERAGE.md");
    let mut rows = Vec::new();
    for line in doc.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        if cells.len() < 7 {
            continue;
        }
        let family = cells[1];
        let verdict = cells[5];
        if family == "family" || family.chars().all(|c| c == '-' || c == ':') {
            continue;
        }
        rows.push(CoverageRow {
            family: family.to_owned(),
            verdict: verdict.to_owned(),
        });
    }
    rows
}

#[test]
fn coverage_table_matches_corpus_emissions() {
    // The decision table is total: every registered family has exactly one
    // row, and every family the corpus emits carries a `lower` verdict, so
    // the lowerings that follow know their full work list.
    let rows = coverage_table();
    let mut table = std::collections::BTreeMap::new();
    for row in &rows {
        assert!(
            table
                .insert(row.family.as_str(), row.verdict.as_str())
                .is_none(),
            "{} has two decision-table rows",
            row.family
        );
    }
    for (name, _) in FALLBACK_DISPOSITIONS {
        assert!(table.contains_key(name), "{name} has a decision-table row");
    }
    assert_eq!(
        table.len(),
        FALLBACK_DISPOSITIONS.len(),
        "one row per family, never two"
    );
    let emitted = corpus_emitted_families();
    assert!(!emitted.is_empty(), "the census found corpus emissions");
    for family in &emitted {
        assert_eq!(
            table.get(family),
            Some(&"lower"),
            "{family} is emitted on the corpus: verdict is lower"
        );
    }
}

#[test]
fn refuse_rows_are_absent_from_the_corpus() {
    // Every refusal row proves corpus absence: a family signed external
    // never appears over fixtures, data root, or differential scripts.
    // Every row also names a real family with a known verdict, so a typo
    // cannot smuggle an uncovered family past the table.
    let rows = coverage_table();
    let emitted = corpus_emitted_families();
    for row in &rows {
        assert!(
            fallback_disposition(&row.family).is_some(),
            "{} names a registered family",
            row.family
        );
        assert!(
            row.verdict == "lower" || row.verdict == "refuse-external",
            "{} carries a known verdict: {}",
            row.family,
            row.verdict
        );
        if row.verdict == "refuse-external" {
            assert!(
                !emitted.contains(row.family.as_str()),
                "{} is signed external but emitted on the corpus",
                row.family
            );
        }
    }
}

/// Tests plus assertion macros in one suite file: line-initial test
/// attributes plus the `assert!` family. Prose and helper names count for
/// nothing.
fn count_tests_and_assertions(source: &str) -> usize {
    source.matches("\n#[test]").count()
        + source.matches("assert!(").count()
        + source.matches("assert_eq!(").count()
        + source.matches("assert_ne!(").count()
}

#[test]
fn no_weakening_counts_tests_and_assertions() {
    // No-weakening floor: each suite's test-plus-assertion count may not
    // fall. The floors are the counts recorded when the substring method was
    // retired; adding tests or assertions is free, deleting any fails here.
    // The counters see real macros, not prose: helper names and comments
    // mentioning assertions count for nothing, unlike the retired
    // substring count.
    let probe = "#[test]\nfn probe() {\nassert!(true);\nassert_eq!(1, 1);\nassert_ne!(1, 2);\n}\n";
    assert_eq!(
        count_tests_and_assertions(probe),
        3,
        "three macros; a test attribute inside a string literal is not a test"
    );
    let prose = "// assertion of parity via assert_expr_parity\nfn helper() {}\n";
    assert_eq!(
        count_tests_and_assertions(prose),
        0,
        "prose and helper names count for nothing"
    );
    let floors: &[(&str, usize)] = &[
        ("pine_bytecode_diff", 48),
        ("pine_bytecode_adversarial", 48),
        ("pine_bytecode_calibration", 250),
        ("pine_bytecode_integration", 438),
        ("pine_bytecode_repr", 44),
        ("pine_bytecode_series", 68),
        ("pine_bytecode_exec", 113),
        ("pine_bytecode_closeout", 98),
    ];
    for (stem, floor) in floors {
        let source = match *stem {
            "pine_bytecode_diff" => include_str!("../tests/pine_bytecode_diff.rs"),
            "pine_bytecode_adversarial" => include_str!("../tests/pine_bytecode_adversarial.rs"),
            "pine_bytecode_calibration" => include_str!("../tests/pine_bytecode_calibration.rs"),
            "pine_bytecode_integration" => include_str!("../tests/pine_bytecode_integration.rs"),
            "pine_bytecode_repr" => include_str!("../tests/pine_bytecode_repr.rs"),
            "pine_bytecode_series" => include_str!("../tests/pine_bytecode_series.rs"),
            "pine_bytecode_exec" => include_str!("../tests/pine_bytecode_exec.rs"),
            "pine_bytecode_closeout" => include_str!("../tests/pine_bytecode_closeout.rs"),
            other => panic!("unknown suite {other}"),
        };
        let count = count_tests_and_assertions(source);
        assert!(
            count >= *floor,
            "{stem}: test-plus-assertion count fell to {count} (floor {floor})"
        );
    }
}

#[test]
fn closeout_docs_describe_behavior() {
    // Stale engine claims are gone and the closeout surfaces are documented.
    let exec_doc = include_str!("../src/pine/bytecode_exec.rs");
    for stale in [
        "no journal or\n//!   teardown",
        "no shared mutable state to\n//!   synchronize",
    ] {
        assert!(
            !exec_doc.contains(stale),
            "bytecode_exec still claims: {stale}"
        );
    }
    assert!(
        exec_doc.contains("One execution path."),
        "the single execution path is documented"
    );
    let architecture = include_str!("../../../doc/protopyne-rs/ARCHITECTURE.md");
    for surface in ["One pathway, one oracle", "Trigger", "b0"] {
        assert!(
            architecture.contains(surface),
            "ARCHITECTURE names {surface}"
        );
    }
    // No dead fallback family is announced as current.
    for (name, disposition) in FALLBACK_DISPOSITIONS {
        if *disposition == FallbackDisposition::Retired {
            assert!(
                !exec_doc.contains(&format!("`{name}` emits")),
                "a retired family is described as live: {name}"
            );
        }
    }
    let _ = FallbackReason::Loops;
}

#[test]
fn closeout_singularities_hold() {
    // One implementation per shared primitive: a second take/put, journal,
    // method table, input store, home model, site spelling, refusal wording,
    // or argument binder would be a regression the differential suite might
    // not catch on every path.
    let sources: &[(&str, &str)] = &[
        ("eval", include_str!("../src/pine/eval.rs")),
        ("engine", include_str!("../src/pine/engine.rs")),
        ("oracle", include_str!("../src/pine/oracle/mod.rs")),
        ("semantics", include_str!("../src/pine/semantics.rs")),
        (
            "bytecode_exec",
            include_str!("../src/pine/bytecode_exec.rs"),
        ),
        (
            "bytecode_compile",
            include_str!("../src/pine/bytecode_compile.rs"),
        ),
        ("bytecode", include_str!("../src/pine/bytecode.rs")),
        ("home", include_str!("../src/pine/home.rs")),
        (
            "builtins_state",
            include_str!("../src/pine/builtins_state.rs"),
        ),
        (
            "builtins_core",
            include_str!("../src/pine/builtins_core.rs"),
        ),
        ("bind", include_str!("../src/pine/bind.rs")),
        ("syntax", include_str!("../src/pine/syntax.rs")),
        ("inputs", include_str!("../src/inputs.rs")),
    ];
    let count = |needle: &str| -> usize {
        sources
            .iter()
            .map(|(_, source)| source.matches(needle).count())
            .sum()
    };
    for (needle, expected, why) in [
        ("fn bind_call_args(", 1, "one tree-side argument binder"),
        ("struct ArgBinder", 1, "one VM-side positional binder"),
        ("fn take_object(", 1, "one object take"),
        ("fn put_object(", 1, "one object put"),
        ("trait HomeStore", 1, "one home model"),
        ("struct RunBuiltins", 1, "one run-scoped builtin owner"),
        ("struct InputSet", 1, "one first-call-wins input store"),
        ("fn site_string(", 1, "one call-site spelling"),
        ("fn journal_write_through(", 1, "one caller-home journal"),
        (
            "fn frame_var_snapshot(",
            1,
            "one per-instance snapshot view",
        ),
        ("fn written_namespace(", 1, "one written-namespace view"),
        (
            "fn unknown_function_message(",
            1,
            "one unknown-callee wording",
        ),
        ("fn call_depth_over_limit(", 1, "one depth refusal"),
        ("fn written_columns(", 1, "one written-column projection"),
    ] {
        assert_eq!(count(needle), expected, "{why}: {needle}");
    }
    // The method dispatch index lives on the linked program only.
    let exec = sources
        .iter()
        .find(|(name, _)| *name == "bytecode_exec")
        .expect("bytecode_exec stays in the source list")
        .1;
    assert_eq!(
        exec.matches("method_index:").count(),
        1,
        "one method dispatch index"
    );
}

#[cfg(feature = "oracle")]
#[test]
fn oracle_build_keeps_both_engines() {
    // Oracle builds keep today's selections: a linkable program succeeds
    // identically under either selection, and an unlinkable one reports
    // the same script-level refusal under either — never a typed
    // load-time diagnostic, which only production builds raise.
    let bars = crate::pine::bytecode_diff_tests::ramp_bars(5);
    let config = || {
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("1".to_owned())
            .warmup_bars(0)
            .build()
    };
    let link = |script: &str| {
        let program = crate::parse_program(script).expect("control script parses");
        vec![crate::LinkedModule::new(
            "local",
            program,
            std::collections::HashMap::new(),
        )]
    };
    let run = |modules: &[crate::LinkedModule], compiled: bool| {
        let mut engine = PineEngine::new(config());
        engine.set_bytecode_mode(if compiled {
            BytecodeMode::Compiled
        } else {
            BytecodeMode::TreeWalk
        });
        engine.run_linked(modules, &bars, InputSet::new())
    };
    let linked = link("v = 1.0 + 2.0\nplot(v, title=\"V\")");
    assert_eq!(
        crate::pine::bytecode_diff_tests::fingerprint_result(&run(&linked, true)),
        crate::pine::bytecode_diff_tests::fingerprint_result(&run(&linked, false)),
        "linkable programs agree across selections"
    );
    let unlinked = link("x = request.marker(close)\nplot(x, title=\"V\")");
    let (compiled, tree) = (run(&unlinked, true), run(&unlinked, false));
    let (compiled_text, tree_text) = match (&compiled, &tree) {
        (Err(first), Err(second)) => (first.to_string(), second.to_string()),
        _ => panic!("oracle build answers the dialogue call on the tree: {compiled:?} vs {tree:?}"),
    };
    assert_eq!(
        compiled_text, tree_text,
        "unlinkable programs keep one wording across selections"
    );
    assert!(
        !matches!(
            compiled,
            Err(crate::KernelError::Uncompilable { .. } | crate::KernelError::EngineRetired { .. })
        ),
        "oracle builds never raise the typed diagnostics"
    );
}

#[test]
fn no_mixed_symbols_remain() {
    // The split machinery is deleted, not deprecated: no mixed planner,
    // bridge, host program, partition classifier, or re-entrant function
    // path may be named anywhere in the Pine surface. A reintroduced
    // name fails here before it can split a run again.
    let sources: &[(&str, &str)] = &[
        ("engine", include_str!("../src/pine/engine.rs")),
        ("oracle", include_str!("../src/pine/oracle/mod.rs")),
        ("semantics", include_str!("../src/pine/semantics.rs")),
        ("eval", include_str!("../src/pine/eval.rs")),
        (
            "bytecode_exec",
            include_str!("../src/pine/bytecode_exec.rs"),
        ),
        (
            "bytecode_compile",
            include_str!("../src/pine/bytecode_compile.rs"),
        ),
        ("bytecode", include_str!("../src/pine/bytecode.rs")),
        ("surface", include_str!("../src/pine/mod.rs")),
    ];
    let retired = [
        "MixedPlan",
        "cached_mixed",
        "MIXED_CACHE",
        "MixedCache",
        "MixedRuntime",
        "plan_mixed",
        "try_mixed",
        "run_mixed",
        "CrossCall",
        "CrossResult",
        "CrossCallBridge",
        "MixedBridge",
        "mixed_bridge_call",
        "function_host",
        "legs_for_vm",
        "exec_function",
        "vm_functions",
        "mixed_function_calls",
        "mixed_partition",
        "mixed_display",
        "mixed_called_targets",
        "mixed_declared_top",
        "mixed_touched_names",
        "mixed_alias_calls",
        "mixed_side_channel",
        "mixed_has_fill",
        "mixed_has_side_channel",
        "PartitionDecision",
        "PartitionVeto",
        "RunStateClass",
        "StateSharing",
        "FunctionSlot",
        "FunctionPlan",
        "Impurity",
        "walk_any",
        "AnySideChannel",
        "AnyFill",
        "try_vm_function",
        "call_vm_function",
        "install_mixed_functions",
        "compile_program",
        "classify_modules",
        "plan_modules",
        "plan_functions",
        "link_plan",
        "ModuleSplit",
        "FunctionSplit",
    ];
    for (stem, source) in sources {
        for symbol in retired {
            assert!(
                !source.contains(symbol),
                "{stem} still names the retired split machinery: {symbol}"
            );
        }
    }
    // The classifier module itself is gone from the surface.
    let surface = sources
        .iter()
        .find(|(stem, _)| *stem == "surface")
        .expect("surface scanned")
        .1;
    assert!(
        !surface.contains("partition"),
        "the partition module is gone from the surface"
    );
}

#[test]
fn single_run_path_is_exhaustive() {
    // The engine has one execution body per build shape: the oracle body
    // compiles, links, runs, and falls back to the tree on refusal, while
    // the two production bodies (with and without the compiled loop)
    // compile, link, run, or refuse typed. No second planner, bridge, or
    // host program remains beside them.
    let engine = include_str!("../src/pine/engine.rs");
    assert_eq!(
        engine.matches("fn run_linked_inner(").count(),
        3,
        "one run body per build shape"
    );
    assert!(
        engine.contains("try_bytecode"),
        "both bodies route through the compiled loop"
    );
    assert!(
        engine.contains("Uncompilable"),
        "the production body names the typed refusal"
    );
}

#[test]
fn confidence_gates_never_decide_correctness() {
    // Structural proof of the axis split: the one confidence constant
    // (`TIER_STABLE_BARS`, representation only) appears nowhere that
    // decides correctness — no refusal constructor, no error type, no
    // registry row outside its kind, no link/classify/compile decision,
    // no selection surface, no oracle read. A confidence signal wired
    // into any of those fails here instead of silently grading a run.
    let tier = "TIER_STABLE_BARS";
    for (stem, source) in [
        ("semantics", include_str!("../src/pine/semantics.rs")),
        ("error", include_str!("../src/error.rs")),
        ("trigger", include_str!("../src/pine/trigger.rs")),
        ("engine", include_str!("../src/pine/engine.rs")),
        ("oracle", include_str!("../src/pine/oracle/mod.rs")),
        ("bytecode", include_str!("../src/pine/bytecode.rs")),
        (
            "bytecode_compile",
            include_str!("../src/pine/bytecode_compile.rs"),
        ),
    ] {
        assert!(
            !source.contains(tier),
            "{stem}: the confidence gate decides outside representation"
        );
    }
    // The tiering loop is the only production home: declaration, doc
    // lines, and float-mirror tier checks. Anything else there fails
    // below instead of hiding a second decision procedure.
    let exec = include_str!("../src/pine/bytecode_exec.rs");
    assert!(
        exec.contains(tier),
        "the tiering loop names its own gate (non-vacuous scan)"
    );
    for (index, line) in exec.lines().enumerate() {
        if !line.contains(tier) {
            continue;
        }
        let representation = line.contains("u32 = 4")
            || line.trim_start().starts_with("///")
            || line.trim_start().starts_with("//")
            || line.contains("stable")
            || line.contains("float");
        assert!(
            representation,
            "bytecode_exec line {}: confidence outside tiering: {line}",
            index + 1
        );
    }
    // Re-exports carry the name across the surface without deciding:
    // every containing statement is a `use`.
    let surface = include_str!("../src/pine/mod.rs");
    assert!(
        surface.contains(tier),
        "the surface re-exports the gate (non-vacuous scan)"
    );
    for statement in surface.split(';') {
        if !statement.contains(tier) {
            continue;
        }
        assert!(
            statement.contains("use "),
            "surface carries confidence outside a re-export: {statement}"
        );
    }
}

#[test]
fn link_refusal_controls_are_exact() {
    // Control corpora for the two load-time diagnostics: programs no
    // engine may run (live dialogue, live fetch, unserved lower-tf
    // shape) refuse, and linkable programs (including every M20-fixed
    // shape: clock, cursor history, enum-defaulted calls, chart scope)
    // evaluate. In oracle builds both selections refuse the must-refuse
    // set with byte-identical script wordings and never raise the typed
    // diagnostics — the probe asserts the production mapping per shape.
    // Precision (refusals that were expected) and recall (expected
    // refusals that fired) both pin at 1.0.
    let bars = crate::pine::bytecode_diff_tests::ramp_bars(10);
    let link = |script: &str| {
        let program = crate::parse_program(script).expect("control script parses");
        vec![crate::LinkedModule::new(
            "local",
            program,
            std::collections::HashMap::new(),
        )]
    };
    let must_refuse = [
        (
            "live dialogue",
            link("x = request.marker(close)\nplot(x, title=\"V\")"),
        ),
        (
            "live fetch",
            link("v = request.security(syminfo.tickerid, \"D\", close)\nplot(v, title=\"V\")"),
        ),
        (
            "unserved lower-tf",
            link("v = request.security_lower_tf(syminfo.tickerid, \"D\", [close])\nplot(v, title=\"V\")"),
        ),
    ];
    let must_run = [
        ("arithmetic", link("v = 1.0 + 2.0\nplot(v, title=\"V\")")),
        ("clock", link("plot(timenow, title=\"V\")")),
        (
            "cursor history",
            link("x = bar_index[1]\nplot(bar_index, title=\"V\")"),
        ),
        (
            "chart scope",
            link("plot(timeframe.period == \"1\" ? close : open, title=\"V\")"),
        ),
    ];
    let run = |modules: &[crate::LinkedModule], compiled: bool| {
        let mut engine = PineEngine::new(engine_config());
        engine.set_bytecode_mode(if compiled {
            BytecodeMode::Compiled
        } else {
            BytecodeMode::TreeWalk
        });
        engine.run_linked(modules, &bars, InputSet::new())
    };
    let mut fired_expected = 0;
    let expected = must_refuse.len() * 2;
    let mut fired = 0;
    for (name, modules) in &must_refuse {
        let (compiled, tree) = (run(modules, true), run(modules, false));
        let (compiled_text, tree_text) = match (&compiled, &tree) {
            (Err(first), Err(second)) => (first.to_string(), second.to_string()),
            _ => panic!("{name}: control refusal must fire on both selections"),
        };
        assert_eq!(
            compiled_text, tree_text,
            "{name}: one wording across selections"
        );
        assert!(
            !matches!(
                compiled,
                Err(crate::KernelError::Uncompilable { .. }
                    | crate::KernelError::EngineRetired { .. })
            ),
            "{name}: oracle builds never raise the typed diagnostics"
        );
        fired_expected += 1;
        fired += 2;
    }
    assert_eq!(fired_expected, must_refuse.len(), "every control fired");
    assert_eq!(fired, expected, "recall pins at 1.0");
    for (name, modules) in &must_run {
        let (compiled, tree) = (run(modules, true), run(modules, false));
        assert_eq!(
            crate::pine::bytecode_diff_tests::fingerprint_result(&compiled),
            crate::pine::bytecode_diff_tests::fingerprint_result(&tree),
            "{name}: linkable programs agree"
        );
        compiled.expect("linkable control evaluates");
    }
}

#[test]
fn single_pathway_is_total() {
    // Totality proof: the retired selection is unnameable in production
    // and the production body holds no engine branch. A reintroduced
    // production tree path fails here before it can split a run again.
    let engine = include_str!("../src/pine/engine.rs");
    // The tree-walk variant exists only where the interpreter does.
    assert!(
        engine.contains("#[cfg(any(test, feature = \"oracle\"))]\n    TreeWalk,"),
        "the retired selection is oracle-gated"
    );
    // The production body with the loop: compile, link, run, or refuse
    // typed — with no mention of the selection or the retired engine.
    let marker = "#[cfg(all(not(any(test, feature = \"oracle\")), feature = \"bytecode\"))]";
    let start = engine
        .find(marker)
        .expect("the production body is marked");
    let body_start = engine[start..]
        .find("fn run_linked_inner(")
        .expect("the production body follows its gate");
    let mut depth = 0u32;
    let mut seen = false;
    let mut end = None;
    for (offset, key) in engine[start + body_start..].match_indices(['{', '}']) {
        if key == "{" {
            depth += 1;
            seen = true;
        } else {
            depth -= 1;
        }
        if seen && depth == 0 {
            end = Some(start + body_start + offset);
            break;
        }
    }
    let body = &engine[start + body_start..end.expect("the production body closes")];
    for banned in ["bytecode_mode", "TreeWalk", "EngineRetired"] {
        assert!(
            !body.contains(banned),
            "the production body names no retired selection: {banned}"
        );
    }
    for required in ["try_bytecode", "Uncompilable"] {
        assert!(
            body.contains(required),
            "the production body keeps the loop and the typed refusal"
        );
    }
    // The strategy opt-out is gated the same way: production signals
    // cannot name another engine.
    let facade = include_str!("../../../src/protopyne-strategy/src/signals_pine.rs");
    assert!(
        facade.contains("#[cfg(any(test, feature = \"oracle\"))]\n    TreeWalk,"),
        "the signal opt-out is oracle-gated"
    );
}

#[test]
fn capability_limits_are_documented() {
    // Every capability limit in the decision table names its probes, and
    // every named probe exists: a limit without a proof fails here
    // instead of drifting into documentation.
    let coverage = include_str!("../../../doc/protopyne-rs/COVERAGE.md");
    let section = coverage
        .split("## Capability limits")
        .nth(1)
        .expect("the limits section exists");
    let table = section
        .split("| limit |")
        .nth(1)
        .expect("the limits table exists");
    let pool = [
        include_str!("pine_bytecode_closeout.rs"),
        include_str!("pine_bytecode_contract.rs"),
        include_str!("pine_bytecode_calibration.rs"),
        include_str!("pine_bytecode_diff.rs"),
        include_str!("pine_bytecode_integration.rs"),
        include_str!("native_bridge.rs"),
        include_str!("native_apex.rs"),
        include_str!("../examples/production_refusal_probe.rs"),
    ]
    .join("\n");
    let mut probes = 0;
    for row in table.lines().skip(2) {
        if !row.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = row.split('|').collect();
        let cell = cells.get(3).expect("limit, behavior, probe cells");
        let mut in_tick = false;
        let mut name = String::new();
        for key in cell.chars() {
            if key == '`' {
                if in_tick {
                    assert!(
                        pool.contains(&name),
                        "limit probe {name} names no test or example"
                    );
                    probes += 1;
                    name.clear();
                }
                in_tick = !in_tick;
            } else if in_tick {
                name.push(key);
            }
        }
    }
    assert!(probes >= 7, "every limit row names its probes");
}

