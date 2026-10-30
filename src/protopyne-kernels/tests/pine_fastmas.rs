//! Headless `_FastMAs` proof: the real `libUtil < libMAs < FastMAs`
//! triple resolves through the import graph, links into one engine run,
//! and evaluates over the vendored `SPY_5M` history with the indicator's
//! default inputs (WMA3, ALMA8, EMA8/21/34/55, SMA50/100/200).
//!
//! The reference runtime is never a dependency; shapes below are literal
//! excerpts from `data/protopyne-rs/`. Headless only: `OwnedOhlcv` plus the
//! bar-by-bar engine, no charting, no network, hermetic.
//!
//! Proof command: `UPDATE_FASTMAS_SPY5M=1 cargo test -p protopyne-kernels
//! fastmas_spy5m` writes the golden; the frozen run compares. No
//! TradingView export ships with this repo, so the recorded proof is
//! self-consistency against direct kernel `batch` output at `eps = 1e-9`
//! plus the frozen golden — never an assumed external match.

use super::*;
use crate::{load_pine_source, parse_program, InputSet, LibraryId, ModuleInput, PineRuntimeConfig};
use chrono::TimeZone as _;
use protopyne_types::Bar;
use std::collections::HashMap;
use std::path::PathBuf;

/// Fixture root (server `tests/data/pine_closure`), never the volatile live root.
fn repo_data(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../protopyne-server/tests/data/pine_closure")
        .join(path)
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("golden")
        .join("fastmas_spy5m.csv")
}

fn library_id(publisher: &str, name: &str, version: u64) -> LibraryId {
    LibraryId {
        publisher: publisher.to_owned(),
        name: name.to_owned(),
        version,
    }
}

fn real_module(id: LibraryId, file: &str) -> ModuleInput {
    let source = load_pine_source(&repo_data(file)).expect("target loads");
    let program = parse_program(&source.text).expect("target parses");
    ModuleInput::new(id, source, program)
}

/// Link the real triple in graph order: one entry per module with its
/// importer-local alias table, dependencies before the indicator root.
fn real_triple_linked() -> Vec<LinkedModule> {
    let graph = crate::resolve_imports(vec![
        real_module(
            library_id("showmethegrail", "libUtil", 12),
            "libraries/_libUtil.pine",
        ),
        real_module(
            library_id("showmethegrail", "libMAs", 95),
            "libraries/_libMAs.pine",
        ),
        real_module(
            library_id("local", "FastMAs", 6),
            "indicators/_FastMAs.pine",
        ),
    ])
    .expect("triple resolves");
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
                .collect::<HashMap<String, String>>();
            LinkedModule {
                key: id.to_string(),
                program: entry.program.clone(),
                aliases,
            }
        })
        .collect()
}

/// The 5-minute history as engine bars (all five legs; the engine reads
/// closes and volumes for this indicator).
fn spy5m_bars() -> Vec<Bar> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("data")
                .join("SPY_5M.csv"),
        )
        .expect("vendored SPY_5M present");
    let mut bars = Vec::new();
    for record in reader.records() {
        let row = record.expect("readable row");
        let num = |i: usize| {
            row.get(i)
                .expect("column present")
                .parse::<f64>()
                .expect("parseable number")
        };
        let ts = chrono::Utc
            .timestamp_opt(
                row.get(0)
                    .expect("time present")
                    .parse::<i64>()
                    .expect("parseable time"),
                0,
            )
            .single()
            .expect("valid timestamp");
        let dec = |v: f64| {
            v.to_string()
                .parse::<rust_decimal::Decimal>()
                .expect("decimal leg")
        };
        bars.push(
            Bar::new()
                .ts(ts)
                .o(dec(num(1)))
                .h(dec(num(2)))
                .l(dec(num(3)))
                .c(dec(num(4)))
                .volume(num(5) as u64)
                .maybe_adj(None)
                .build(),
        );
    }
    bars
}

fn run_linked(modules: &[LinkedModule], bars: &[Bar]) -> RunOutcome {
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine
        .run_linked(modules, bars, InputSet::new())
        .expect("linked run evaluates")
}

/// Numeric reading of one plot cell (`int` auto-casts, like the engine).
fn plot_num(outcome: &RunOutcome, title: &str, at_bar: usize) -> Option<f64> {
    let plot = outcome
        .plots
        .iter()
        .find(|plot| plot.title == title)
        .expect("plot captured");
    match plot.values.get(at_bar).cloned().flatten() {
        Some(PineValue::Float(v)) => Some(v),
        Some(PineValue::Int(v)) => Some(v as f64),
        _ => None,
    }
}

#[test]
fn linked_module_headers_are_inert() {
    // Headers declare metadata for the loader/graph; executing a program
    // that carries them must not refuse.
    let bars = spy5m_bars()[..8].to_vec();
    let program = parse_program(
        "import showmethegrail/libUtil/12 as UTIL\nlibrary('L')\nindicator(title=\"T\")\nx = 1 + 1",
    )
    .expect("headers parse");
    let modules = [LinkedModule {
        key: "local/T".to_owned(),
        program,
        aliases: HashMap::new(),
    }];
    let outcome = run_linked(&modules, &bars);
    assert_eq!(outcome.evaluated, 8);
    match outcome.value_at(0, "x") {
        Some(Some(PineValue::Float(v))) => assert!((v - 2.0).abs() <= 1e-12),
        other => panic!("inert headers keep the body live, got {other:?}"),
    }
}

#[test]
fn linked_alias_call_binds_to_exporting_module() {
    let lib = parse_program("export double(float x) =>\n    x * 2.0").expect("lib parses");
    let root = parse_program("y = L.double(21.0)").expect("root parses");
    let modules = [
        LinkedModule {
            key: "pub/Lib".to_owned(),
            program: lib,
            aliases: HashMap::new(),
        },
        LinkedModule {
            key: "local/Root".to_owned(),
            program: root,
            aliases: HashMap::from([("L".to_owned(), "pub/Lib".to_owned())]),
        },
    ];
    let bars = spy5m_bars()[..4].to_vec();
    let outcome = run_linked(&modules, &bars);
    match outcome.value_at(0, "y") {
        Some(Some(PineValue::Float(v))) => assert!((v - 42.0).abs() <= 1e-12),
        other => panic!("aliased call binds, got {other:?}"),
    }
}

#[test]
fn linked_bare_calls_stay_module_local() {
    // Same-named helpers in two modules must not leak: each bare call
    // resolves in its own module, and the root reaches the library's copy
    // only through its alias. The library's unexported `helper` stays
    // hidden; its exported `wrapped` proves the library's own bare call
    // binds the library's `pick` (1.0), not the root's (2.0).
    let lib = parse_program(
        "export pick() =>\n    1.0\nexport wrapped() =>\n    helper()\nhelper() =>\n    pick()",
    )
    .expect("lib parses");
    let root = parse_program("pick() =>\n    2.0\nb = L.pick()\nc = pick()\nd = L.wrapped()")
        .expect("root parses");
    let modules = [
        LinkedModule {
            key: "pub/Lib".to_owned(),
            program: lib,
            aliases: HashMap::new(),
        },
        LinkedModule {
            key: "local/Root".to_owned(),
            program: root,
            aliases: HashMap::from([("L".to_owned(), "pub/Lib".to_owned())]),
        },
    ];
    let bars = spy5m_bars()[..4].to_vec();
    let outcome = run_linked(&modules, &bars);
    for (name, want) in [("b", 1.0), ("c", 2.0), ("d", 1.0)] {
        match outcome.value_at(0, name) {
            Some(Some(PineValue::Float(v))) => {
                assert!((v - want).abs() <= 1e-12, "{name} binds its own module");
            }
            other => panic!("{name} binds its own module, got {other:?}"),
        }
    }
}

#[test]
fn linked_unknown_alias_refuses_with_reason() {
    let root = parse_program("y = Nope.fn(1.0)").expect("root parses");
    let modules = [LinkedModule {
        key: "local/Root".to_owned(),
        program: root,
        aliases: HashMap::new(),
    }];
    let bars = spy5m_bars()[..4].to_vec();
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    let err = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect_err("unknown alias refuses")
        .to_string();
    assert!(
        err.contains("Nope"),
        "refusal names the unknown alias, got {err}"
    );
}

#[test]
fn bare_source_input_rides_the_series_through() {
    // `input(close)` is the source overload: the default is the live
    // per-bar series expression, not a frozen scalar.
    let bars = spy5m_bars()[..8].to_vec();
    let program = parse_program("s = input(close, title=\"Source\")").expect("parses");
    let modules = [LinkedModule {
        key: "local/T".to_owned(),
        program,
        aliases: HashMap::new(),
    }];
    let outcome = run_linked(&modules, &bars);
    let closes: Vec<f64> = bars
        .iter()
        .map(|bar| {
            use rust_decimal::prelude::ToPrimitive as _;
            bar.c.to_f64().expect("display leg")
        })
        .collect();
    for (at_bar, want) in closes.iter().enumerate() {
        match outcome.value_at(at_bar, "s") {
            Some(Some(PineValue::Float(v))) => {
                assert!((v - want).abs() <= 1e-12, "source rides bar {at_bar}");
            }
            other => panic!("source rides bar {at_bar}, got {other:?}"),
        }
    }
}

#[test]
fn bare_color_input_declares_its_handle() {
    // `input(color.rgb(...))` infers the color overload from its default.
    let bars = spy5m_bars()[..4].to_vec();
    let program = parse_program("c = input(color.rgb(192, 192, 192, 20), \"\")").expect("parses");
    let modules = [LinkedModule {
        key: "local/T".to_owned(),
        program,
        aliases: HashMap::new(),
    }];
    let outcome = run_linked(&modules, &bars);
    assert!(
        matches!(
            outcome.value_at(0, "c"),
            Some(Some(PineValue::ColorHandle(_)))
        ),
        "color default declares a color input"
    );
}

// ---- The headless proof ----

/// The nine indicator plots in capture order, each with the direct kernel
/// (id, length params) it must match at `eps = 1e-9`.
fn fastmas_expectations() -> Vec<(&'static str, &'static str, crate::ParamSet)> {
    let len = |n: i64| crate::ParamSet::new().with_int("length", n);
    vec![
        ("First MA", "wma", len(3)),
        (
            "Second MA",
            "alma",
            crate::ParamSet::new()
                .with_int("length", 8)
                .with_float("offset", 0.85)
                .with_float("sigma", 6.0),
        ),
        ("Signal MA", "ema", len(8)),
        ("Mean MA", "ema", len(21)),
        ("Trend MA", "ema", len(34)),
        ("Base MA", "ema", len(55)),
        ("SMA50", "sma", len(50)),
        ("SMA100", "sma", len(100)),
        ("SMA200", "sma", len(200)),
    ]
}

fn fastmas_outcome() -> (RunOutcome, Vec<Bar>) {
    let bars = spy5m_bars();
    assert_eq!(bars.len(), 346, "SPY_5M row-count golden");
    let outcome = run_linked(&real_triple_linked(), &bars);
    (outcome, bars)
}

#[test]
fn fastmas_headless_proof_captures_nine_plots() {
    let (outcome, _) = fastmas_outcome();
    assert_eq!(outcome.evaluated, 346, "every bar evaluates");
    let titles: Vec<&str> = outcome
        .plots
        .iter()
        .map(|plot| plot.title.as_str())
        .collect();
    assert_eq!(
        titles,
        vec![
            "First MA",
            "Second MA",
            "Signal MA",
            "Mean MA",
            "Trend MA",
            "Base MA",
            "SMA50",
            "SMA100",
            "SMA200"
        ],
        "all nine plots capture in source order"
    );
    for plot in &outcome.plots {
        assert_eq!(
            plot.values.len(),
            346,
            "plot '{}' covers every bar",
            plot.title
        );
    }
}

#[test]
fn fastmas_warmup_alignment_holds() {
    // SMA200 needs 200 bars: the first 199 cells stay `na`, bar 199 is the
    // first defined value, and no `NaN` survives inside a defined cell.
    let (outcome, _) = fastmas_outcome();
    for at_bar in 0..199 {
        assert!(
            plot_num(&outcome, "SMA200", at_bar).is_none(),
            "SMA200 stays na on bar {at_bar}"
        );
    }
    assert!(
        plot_num(&outcome, "SMA200", 199).is_some(),
        "SMA200 defines bar 199"
    );
    for plot in &outcome.plots {
        for (at_bar, cell) in plot.values.iter().enumerate() {
            if let Some(PineValue::Float(v)) = cell {
                assert!(
                    v.is_finite(),
                    "plot '{}' bar {at_bar} stays finite",
                    plot.title
                );
            }
        }
    }
}

#[test]
fn fastmas_series_match_direct_kernels() {
    // Self-consistency: every scripted series equals the direct kernel
    // `batch` over the same legs at `eps = 1e-9`.
    use crate::{builtin_registry, OwnedOhlcv};
    let (outcome, bars) = fastmas_outcome();
    let data = OwnedOhlcv::from_bars(&bars);
    let views = data.views();
    let registry = builtin_registry().expect("builtins register");
    for (title, kernel_id, params) in fastmas_expectations() {
        let kernel = registry.get(kernel_id).expect("kernel registered");
        let direct = kernel.batch(&views, &params).expect("valid params");
        assert_eq!(direct.values.len(), 346, "length drift in {title}");
        for (at_bar, want) in direct.values.iter().enumerate() {
            let got = plot_num(&outcome, title, at_bar);
            match (got, want) {
                (None, None) => {}
                (Some(g), Some(w)) => assert!(
                    (g - w).abs() <= 1e-9,
                    "kernel drift in {title}[{at_bar}]: {g} vs {w}"
                ),
                _ => panic!("na drift in {title}[{at_bar}]: {got:?} vs {want:?}"),
            }
        }
    }
}

fn write_spy5m_csv(outcome: &RunOutcome) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("first,second,signal,mean,trend,base,sma50,sma100,sma200\n");
    let titles = [
        "First MA",
        "Second MA",
        "Signal MA",
        "Mean MA",
        "Trend MA",
        "Base MA",
        "SMA50",
        "SMA100",
        "SMA200",
    ];
    for at_bar in 0..outcome.evaluated {
        let cells: Vec<String> = titles
            .iter()
            .map(|title| match plot_num(outcome, title, at_bar) {
                Some(v) => format!("{v:?}"),
                None => String::new(),
            })
            .collect();
        let _ = writeln!(out, "{}", cells.join(","));
    }
    out
}

fn read_spy5m_csv(content: &str) -> Vec<Vec<Option<f64>>> {
    let mut lines = content.lines();
    assert_eq!(
        lines.next(),
        Some("first,second,signal,mean,trend,base,sma50,sma100,sma200"),
        "golden header drift"
    );
    lines
        .map(|line| {
            line.split(',')
                .map(|cell| {
                    let cell = cell.trim();
                    if cell.is_empty() {
                        None
                    } else {
                        Some(cell.parse::<f64>().expect("parseable golden value"))
                    }
                })
                .collect()
        })
        .collect()
}

#[test]
fn fastmas_spy5m_golden_frozen() {
    let (outcome, _) = fastmas_outcome();
    let path = golden_path();
    if std::env::var("UPDATE_FASTMAS_SPY5M").is_ok() {
        std::fs::write(&path, write_spy5m_csv(&outcome)).expect("golden writable");
    }
    let frozen = read_spy5m_csv(
        &std::fs::read_to_string(&path)
            .expect("golden frozen; rerun with UPDATE_FASTMAS_SPY5M=1 to write it"),
    );
    assert_eq!(frozen.len(), 346, "golden row-count drift");
    let titles = [
        "First MA",
        "Second MA",
        "Signal MA",
        "Mean MA",
        "Trend MA",
        "Base MA",
        "SMA50",
        "SMA100",
        "SMA200",
    ];
    for (at_bar, row) in frozen.iter().enumerate() {
        assert_eq!(row.len(), 9, "golden width drift on row {at_bar}");
        for (title, want) in titles.iter().zip(row.iter()) {
            let got = plot_num(&outcome, title, at_bar);
            match (got, *want) {
                (None, None) => {}
                (Some(g), Some(w)) => assert!(
                    (g - w).abs() <= 1e-12,
                    "golden drift in {title}[{at_bar}]: {g} vs {w}"
                ),
                _ => panic!("golden na drift in {title}[{at_bar}]: {got:?} vs {want:?}"),
            }
        }
    }
}
