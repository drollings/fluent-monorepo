//! Pine evaluation baselines: the fixture `_FastMAs` closure and two
//! synthetic scripts over checked-in histories. Throughput numbers only,
//! never optimization targets; a slow bench re-examines the eval path,
//! never an assertion threshold (there are none — benches assert nothing).
//!
//! Workload honesty: every bench composes the public loader/linker/engine
//! API the `pine_*` suites already prove correct (`load_pine_sources`,
//! `discover_closures`, `closure_inputs`, `resolve_imports`,
//! `PineEngine::run_linked`), so no separate smoke test guards harness
//! rot — what fails there never reaches `cargo bench`. Linking happens
//! once per bench (outside `iter`); only the per-run evaluation is timed.
//! Histories are the checked-in `tests/data/SPY_*.csv` trims, never the
//! volatile live root.

use criterion::{criterion_group, criterion_main, Criterion};
use std::fmt::Write as _;
#[cfg(feature = "bytecode")]
use protopyne_kernels::BytecodeMode;
use protopyne_kernels::{
    closure_inputs, discover_closures, manifest_files::load_pine_sources, parse_program,
    resolve_imports, InputSet, LinkedModule, ModuleGraph, PineEngine, PineRuntimeConfig,
};
use protopyne_types::Bar;
use std::path::PathBuf;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../protopyne-server/tests/data/pine_closure")
}

fn history_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/SPY_1M.csv")
}

fn read_bars(path: &PathBuf, limit: usize) -> Vec<Bar> {
    let content = std::fs::read_to_string(path).expect("history loads");
    let mut lines = content.lines();
    assert_eq!(
        lines.next().expect("header present"),
        "time,open,high,low,close,Volume",
        "history header pinned"
    );
    lines
        .take(limit)
        .map(|line| {
            let mut cells = line.split(',');
            let secs: i64 = cells.next().expect("time").parse().expect("int time");
            let num =
                |cell: Option<&str>| cell.expect("leg").parse::<f64>().expect("parseable number");
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

/// One linked closure as the engine consumes it: fixture discovery over
/// the checked-in tree, parsed inputs, dependency-first order, alias
/// tables attached.
fn link_fixture(stem: &str) -> Vec<LinkedModule> {
    let (indicators, libraries) = load_pine_sources(&fixture_root()).expect("fixtures load");
    let closure = discover_closures(&indicators, &libraries)
        .into_iter()
        .find_map(|outcome| match outcome {
            protopyne_kernels::ClosureOutcome::Linked(closure) if closure.stem == stem => {
                Some(closure)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("fixture closure {stem} discovered"));
    let inputs = closure_inputs(&closure).expect("closure parses");
    let graph: ModuleGraph = resolve_imports(inputs).expect("closure resolves");
    graph
        .order()
        .iter()
        .map(|id| {
            let entry = graph.get(id).expect("ordered module resolves");
            let aliases = graph
                .alias_table(id)
                .unwrap_or_default()
                .into_iter()
                .map(|(alias, target)| (alias, target.to_string()))
                .collect::<std::collections::HashMap<String, String>>();
            LinkedModule {
                key: id.to_string(),
                program: entry.program.clone(),
                aliases,
            }
        })
        .collect()
}

/// One synthetic single-module script as a linked closure: parsed under a
/// `local/Driver` key, no imports.
fn link_script(script: &str) -> Vec<LinkedModule> {
    let program = parse_program(script).expect("script parses");
    vec![LinkedModule::new(
        "local/Driver",
        program,
        std::collections::HashMap::new(),
    )]
}

fn engine(period: &str) -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period(period.to_owned())
            .warmup_bars(0)
            .build(),
    )
}

fn bench_fastmas_full_history(c: &mut Criterion) {
    let modules = link_fixture("_FastMAs");
    let bars = read_bars(&history_path(), usize::MAX);
    assert!(!bars.is_empty(), "history loads");
    c.bench_function("pine_eval/fastmas_full_history", |b| {
        b.iter(|| {
            let outcome = engine("1")
                .run_linked(&modules, &bars, InputSet::new())
                .expect("evaluates");
            std::hint::black_box(outcome.evaluated);
        });
    });
}

fn bench_plot_heavy_synthetic(c: &mut Criterion) {
    // Nine plots, ternary gating, history offsets, and one `ta.sma` call:
    // the per-bar statement mix of a typical indicator, without imports.
    let modules = link_script(
        "a = plot(close, title=\"A\")\n\
         b = plot(close * 2.0, title=\"B\")\n\
         c = plot(ta.sma(close, 14), title=\"C\")\n\
         d = plot(close[1], title=\"D\")\n\
         e = plot(close > open ? high : na, title=\"E\")\n\
         f = plot(low, title=\"F\")\n\
         g = plot(high, title=\"G\")\n\
         h = plot(open, title=\"H\")\n\
         i = plot(50.0, title=\"I\")\n\
         x = a + b\n\
         y = c * 2.0\n",
    );
    let bars = read_bars(&history_path(), 1_000);
    c.bench_function("pine_eval/plot_heavy_synthetic", |b| {
        b.iter(|| {
            let outcome = engine("1")
                .run_linked(&modules, &bars, InputSet::new())
                .expect("evaluates");
            std::hint::black_box(outcome.evaluated);
        });
    });
}

fn bench_offset_scaling(c: &mut Criterion) {
    // History-offset depth scaling: the same script over 1k bars with
    // lookbacks of 1, 10, and 50 — offset cost must stay flat in depth.
    let full = read_bars(&history_path(), 1_000);
    assert!(full.len() >= 200, "history covers the window");
    let bars = full[..200].to_vec();
    for depth in [1, 10, 50] {
        let modules = link_script(&format!("v = close[{depth}]\nplot(v)\n"));
        c.bench_function(&format!("pine_eval/offset_depth_{depth}"), |b| {
            b.iter(|| {
                let outcome = engine("1")
                    .run_linked(&modules, &bars, InputSet::new())
                    .expect("evaluates");
                std::hint::black_box(outcome.evaluated);
            });
        });
    }
}

criterion_group!(
    pine_eval,
    bench_fastmas_full_history,
    bench_plot_heavy_synthetic,
    bench_offset_scaling
);

/// Compiled workload with one oracle reference: titled plots,
/// user-series history, `var` persistence, and one `ta.sma` call, so the
/// loop serves every bar and the pair times the representation change.
/// Linking happens once per bench; only evaluation is timed. The tree
/// row exists only in oracle builds; every other row is the shipping
/// path.
#[cfg(feature = "bytecode")]
fn bench_compiled_numeric(c: &mut Criterion) {
    let modules = link_script(
        "var x = 0.0\nx := x + close\ny = x[1] + close[2]\nplot(y, title=\"Y\")\nplot(ta.sma(close, 14), title=\"S\")",
    );
    let bars = read_bars(&history_path(), 1_000);
    assert!(!bars.is_empty(), "history loads");
    c.bench_function("pine_eval/numeric_compiled", |b| {
        b.iter(|| {
            let mut engine = engine("1");
            engine.set_bytecode_mode(BytecodeMode::Compiled);
            let outcome = engine
                .run_linked(&modules, &bars, InputSet::new())
                .expect("evaluates");
            std::hint::black_box(outcome.evaluated);
        });
    });
    // The single oracle reference row: the same workload on the tree, so
    // the representation ratio stays measurable. Oracle builds only.
    #[cfg(feature = "oracle")]
    c.bench_function("pine_eval/numeric_tree", |b| {
        b.iter(|| {
            let mut engine = engine("1");
            engine.set_bytecode_mode(BytecodeMode::TreeWalk);
            let outcome = engine
                .run_linked(&modules, &bars, InputSet::new())
                .expect("evaluates");
            std::hint::black_box(outcome.evaluated);
        });
    });
}

/// Cross-module shapes on the loop: the `_FastMAs` fixture closure plus
/// a synthetic library-driver closure. Linking happens once per bench;
/// only evaluation is timed. Histories are bounded slices of the
/// checked-in trim — history scaling is covered by `fastmas_full_history`.
/// Both closures link fully, so every row here times the one shipping
/// path.
#[cfg(feature = "bytecode")]
fn bench_mixed_shapes(c: &mut Criterion) {
    // Fixture shape: the whole closure links and runs on the loop.
    let mixed_fixture = link_fixture("_FastMAs");
    let fixture_bars = read_bars(&history_path(), 500);
    assert!(!fixture_bars.is_empty(), "history loads");
    {
        c.bench_function("pine_eval/mixed_fastmas_compiled", |b| {
            b.iter(|| {
                let mut engine = engine("1");
                engine.set_bytecode_mode(BytecodeMode::Compiled);
                let outcome = engine
                    .run_linked(&mixed_fixture, &fixture_bars, InputSet::new())
                    .expect("evaluates");
                std::hint::black_box(outcome.evaluated);
            });
        });
    }
    // Synthetic shape: a library-driver closure that links fully.
    let lib = parse_program(
        "export bump(x) =>\n    b = [1.0, 2.0]\n    array.shift(b)\n    x + array.get(b, 0)",
    )
    .expect("lib parses");
    let driver = parse_program("v = L.bump(close)\nplot(v, title=\"V\")").expect("entry parses");
    let mixed_synthetic = vec![
        LinkedModule::new("test/lib", lib, Default::default()),
        LinkedModule::new(
            "local/Driver",
            driver,
            [("L".to_owned(), "test/lib".to_owned())]
                .into_iter()
                .collect(),
        ),
    ];
    let synthetic_bars = read_bars(&history_path(), 200);
    assert!(!synthetic_bars.is_empty(), "history loads");
    {
        c.bench_function("pine_eval/mixed_synthetic_compiled", |b| {
            b.iter(|| {
                let mut engine = engine("1");
                engine.set_bytecode_mode(BytecodeMode::Compiled);
                let outcome = engine
                    .run_linked(&mixed_synthetic, &synthetic_bars, InputSet::new())
                    .expect("evaluates");
                std::hint::black_box(outcome.evaluated);
            });
        });
    }
}

/// Cold compile/link: the whole fixture closure compiled and linked with
/// every cache bypassed, once per iteration. This is the front-door cost
/// a process pays once per distinct program; the run-time benches above
/// deliberately time evaluation only, so this group is where compile/link
/// regressions surface.
#[cfg(feature = "bytecode")]
fn bench_cold_compile_link(c: &mut Criterion) {
    use protopyne_kernels::pine::bytecode_exec::link;

    fn compile_and_link(modules: &[LinkedModule]) -> (bool, usize) {
        let (program, counts) = compile_workload(modules);
        let fallbacks = counts.iter().map(|(_, count)| *count).sum();
        let order: Vec<&str> = modules.iter().map(|module| module.key.as_str()).collect();
        (link(&program, &order).is_ok(), fallbacks)
    }

    for stem in ["_FastMAs", "_IntegrationLite"] {
        let modules = link_fixture(stem);
        let (links, fallbacks) = compile_and_link(&modules);
        eprintln!(
            "cold_compile_link {stem}: {} modules, {fallbacks} fallbacks, links={links}",
            modules.len()
        );
        c.bench_function(
            &format!(
                "pine_eval/cold_compile_link_{}",
                stem.trim_start_matches('_')
            ),
            |b| {
                b.iter(|| {
                    let (links, fallbacks) = compile_and_link(&modules);
                    std::hint::black_box((links, fallbacks));
                });
            },
        );
    }
}

/// Snapshot-heavy: one script declaring many globals, so the per-bar
/// snapshot map carries a wide value set. Times the commit path's
/// namespace merge and per-column bookkeeping, never the run itself.
#[cfg(feature = "bytecode")]
fn bench_snapshot_heavy(c: &mut Criterion) {
    let mut script = String::from("v0 = close + 0.0\n");
    for index in 1..64 {
        writeln!(script, "v{index} = v{} * 1.0001 + 0.5", index - 1).expect("script builds");
    }
    script.push_str("plot(v63, title=\"Tail\")\n");
    let modules = link_script(&script);
    let bars = read_bars(&history_path(), 500);
    assert!(!bars.is_empty(), "history loads");
    {
        c.bench_function("pine_eval/snapshot_heavy_compiled", |b| {
            b.iter(|| {
                let mut engine = engine("1");
                engine.set_bytecode_mode(BytecodeMode::Compiled);
                let outcome = engine
                    .run_linked(&modules, &bars, InputSet::new())
                    .expect("evaluates");
                std::hint::black_box((outcome.evaluated, outcome.snapshots.len()));
            });
        });
    }
}

/// Method-heavy: one user type mutated through many method calls per bar.
/// Times receiver binding, take/put homes, and the per-call frame churn on
/// both representations.
#[cfg(feature = "bytecode")]
fn bench_method_heavy(c: &mut Criterion) {
    let mut script = String::from(
        "type Box2\n int n = 0\nmethod bump(Box2 this, int d) =>\n this.n := d\n this.n * 2.0 + 1.0\nw = Box2.new()\n",
    );
    script.push_str("acc = 0.0\n");
    for index in 1..=24 {
        writeln!(script, "acc := acc + w.bump({index})").expect("script builds");
    }
    script.push_str("plot(acc, title=\"Acc\")\n");
    let modules = link_script(&script);
    let bars = read_bars(&history_path(), 500);
    assert!(!bars.is_empty(), "history loads");
    {
        c.bench_function("pine_eval/method_heavy_compiled", |b| {
            b.iter(|| {
                let mut engine = engine("1");
                engine.set_bytecode_mode(BytecodeMode::Compiled);
                let outcome = engine
                    .run_linked(&modules, &bars, InputSet::new())
                    .expect("evaluates");
                std::hint::black_box(outcome.evaluated);
            });
        });
    }
}

/// Stateful cross-module shape: a library with stateful `ta.*` sites
/// under an entry that declares an `input.*` and a `fixnan` read. Times
/// the loop on shared run-scoped state, which the differential suites
/// keep byte-identical with the oracle.
#[cfg(feature = "bytecode")]
fn bench_mixed_stateful(c: &mut Criterion) {
    let lib = parse_program(
        "export smooth(src, len) =>\n    ta.sma(src, len)\n\
         export hold(val) =>\n    fixnan(val)\n\
         export step(val) =>\n    buf = [1.0, 2.0]\n    array.shift(buf)\n    val + array.get(buf, 0)",
    )
    .expect("lib parses");
    let driver = parse_program(
        "n = input.int(3, \"N\")\n\
         a = L.smooth(close, n)\n\
         b = L.hold(close - open)\n\
         plot(a + b, title=\"V\")\n\
         plot(ta.ema(close, 5), title=\"E\")",
    )
    .expect("entry parses");
    let modules = vec![
        LinkedModule::new("test/stateful", lib, Default::default()),
        LinkedModule::new(
            "local/Driver",
            driver,
            [("L".to_owned(), "test/stateful".to_owned())]
                .into_iter()
                .collect(),
        ),
    ];
    let bars = read_bars(&history_path(), 500);
    assert!(!bars.is_empty(), "history loads");
    eprintln!("mixed_stateful linkage: {:?}", compile_workload(&modules).1);
    {
        c.bench_function("pine_eval/mixed_stateful_compiled", |b| {
            b.iter(|| {
                let mut engine = engine("1");
                engine.set_bytecode_mode(BytecodeMode::Compiled);
                let outcome = engine
                    .run_linked(&modules, &bars, InputSet::new())
                    .expect("evaluates");
                std::hint::black_box(outcome.evaluated);
            });
        });
    }
}

/// Cross-module call shape: an entry that calls into a library several
/// times per bar, each call carrying per-argument values and a collection
/// result. Times call binding, site identity, and write-back harvest on
/// the hot per-bar path.
#[cfg(feature = "bytecode")]
fn bench_mixed_boundary(c: &mut Criterion) {
    let lib = parse_program(
        "export bump(x) =>\n    b = [1.0, 2.0]\n    array.shift(b)\n    x + array.get(b, 0)",
    )
    .expect("lib parses");
    let mut driver = String::new();
    driver.push_str("v = 0.0\n");
    for offset in ["close", "open", "high", "low"] {
        for _ in 0..2 {
            writeln!(driver, "v := v + L.bump({offset})").expect("driver builds");
        }
    }
    driver.push_str("plot(v, title=\"V\")\n");
    let driver = parse_program(&driver).expect("entry parses");
    let modules = vec![
        LinkedModule::new("test/boundary", lib, Default::default()),
        LinkedModule::new(
            "local/Driver",
            driver,
            [("L".to_owned(), "test/boundary".to_owned())]
                .into_iter()
                .collect(),
        ),
    ];
    let bars = read_bars(&history_path(), 500);
    assert!(!bars.is_empty(), "history loads");
    eprintln!("mixed_boundary linkage: {:?}", compile_workload(&modules).1);
    {
        c.bench_function("pine_eval/mixed_boundary_compiled", |b| {
            b.iter(|| {
                let mut engine = engine("1");
                engine.set_bytecode_mode(BytecodeMode::Compiled);
                let outcome = engine
                    .run_linked(&modules, &bars, InputSet::new())
                    .expect("evaluates");
                std::hint::black_box(outcome.evaluated);
            });
        });
    }
}

/// Compile every bench workload module with the engine-mirroring
/// registries, returning the program plus each module's fallback count.
/// The counts are printed once at setup: every workload here links
/// fully, so a nonzero count is visible in the bench log instead of
/// timing a fallback silently.
#[cfg(feature = "bytecode")]
fn compile_workload(
    modules: &[LinkedModule],
) -> (
    protopyne_kernels::pine::bytecode::CompiledProgram,
    Vec<(String, usize)>,
) {
    use protopyne_kernels::pine::bytecode::{CompiledProgram, Instruction};
    use protopyne_kernels::pine::bytecode_compile::{
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
    let mut counts = Vec::with_capacity(modules.len());
    for module in modules {
        let compiled = compile_module(
            &module.key,
            &module.program,
            &module.aliases,
            &enums,
            &decls,
            &exports,
        );
        let count = compiled
            .blocks
            .iter()
            .flat_map(|block| block.instrs.iter())
            .filter(|instr| matches!(instr, Instruction::Fallback { .. }))
            .count();
        counts.push((module.key.clone(), count));
        program.insert(compiled);
    }
    (program, counts)
}

#[cfg(feature = "bytecode")]
criterion_group!(
    pine_eval_bytecode,
    bench_compiled_numeric,
    bench_mixed_shapes,
    bench_cold_compile_link,
    bench_snapshot_heavy,
    bench_method_heavy,
    bench_mixed_stateful,
    bench_mixed_boundary
);
#[cfg(feature = "bytecode")]
criterion_main!(pine_eval, pine_eval_bytecode);
#[cfg(not(feature = "bytecode"))]
criterion_main!(pine_eval);
