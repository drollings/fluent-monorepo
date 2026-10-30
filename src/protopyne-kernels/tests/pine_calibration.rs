//! Pine calibration gate: frozen bridge coverage, accept/reject tallies
//! mirroring the spike shape, script-level controls, and the no-cache
//! review assertion.
//!
//! The reference runtime is never a dependency; script shapes below are
//! transcribed from the spike corpus and the real indicator files. The
//! nine canonical accept scripts mirror the spike's nine-count shape with
//! pine-native content (`trima`/`midpoint` stay spike-only — the bridge
//! refuses them by name, pinned below — so `wma`/`alma` stand in). The
//! twelve canonical reject scripts mirror the spike's twelve-count shape
//! with pine-surface failures, plus the two import-graph refusals.
//!
//! Thresholds come from the compatibility matrix, never from literals here.

use super::*;
use crate::{
    load_pine_source, parse_pine_source, parse_program, InputSet, LibraryId, ModuleInput,
    PineRuntimeConfig, TaBridge,
};
use chrono::TimeZone as _;
use protopyne_types::Bar;
use std::collections::HashMap;
use std::path::PathBuf;

// ---- Bar builders (deterministic control frames) ----

fn bar_at(slot: i64, price: f64, volume: u64) -> Bar {
    let ts = chrono::Utc
        .timestamp_opt(1_767_344_400 + slot * 300, 0)
        .single()
        .expect("valid timestamp");
    let dec = |v: f64| {
        v.to_string()
            .parse::<rust_decimal::Decimal>()
            .expect("decimal leg")
    };
    Bar::new()
        .ts(ts)
        .o(dec(price))
        .h(dec(price + 0.5))
        .l(dec(price - 0.5))
        .c(dec(price))
        .volume(volume)
        .maybe_adj(None)
        .build()
}

/// Trend frame, spike-corpus style (`600 + i * 0.5`).
fn seeded_bars(count: usize) -> Vec<Bar> {
    (0..count)
        .map(|i| bar_at(i as i64, 600.0 + i as f64 * 0.5, 1_000 + i as u64))
        .collect()
}

/// Flat market: no kernel may false-fire a refusal here.
fn flat_bars(count: usize) -> Vec<Bar> {
    (0..count).map(|i| bar_at(i as i64, 100.0, 1_000)).collect()
}

fn run_script(script: &str, bars: &[Bar]) -> Result<RunOutcome, crate::KernelError> {
    let program = parse_program(script).map_err(|err| {
        crate::KernelError::Eval(format!("calibration accept script must parse: {err}"))
    })?;
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine.run(&program, bars, InputSet::new())
}

/// Fixture root (server `tests/data/pine_closure`), never the volatile live root.
fn repo_data(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../protopyne-server/tests/data/pine_closure")
        .join(path)
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

/// The real triple linked in graph order (same shape as the headless proof).
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

// ---- Frozen bridge coverage (stale-row analog at the bridge level) ----

/// One supported `ta.*` method: its canonical script plus the backing
/// kernel id (`None` = shared-primitive backed, covered by the bridge
/// parity suite rather than the matrix).
fn ta_canonical() -> Vec<(&'static str, Option<&'static str>, &'static str)> {
    vec![
        ("sma", Some("sma"), "v = ta.sma(close, 5)"),
        ("ema", Some("ema"), "v = ta.ema(close, 5)"),
        ("wma", Some("wma"), "v = ta.wma(close, 5)"),
        ("hma", Some("hma"), "v = ta.hma(close, 5)"),
        ("rma", Some("rma"), "v = ta.rma(close, 5)"),
        ("vwma", Some("vwma"), "v = ta.vwma(close, 5)"),
        ("rsi", Some("rsi"), "v = ta.rsi(close, 5)"),
        ("mom", Some("mom"), "v = ta.mom(close, 5)"),
        ("roc", Some("roc"), "v = ta.roc(close, 5)"),
        ("cci", Some("cci"), "v = ta.cci(close, 5)"),
        ("cmo", Some("cmo"), "v = ta.cmo(close, 5)"),
        ("mfi", Some("mfi"), "v = ta.mfi(close, 5)"),
        ("alma", Some("alma"), "v = ta.alma(close, 8, 0.85, 6.0)"),
        ("macd", Some("macd"), "v = ta.macd(close, 12, 26, 9)"),
        ("bb", Some("bb"), "v = ta.bb(close, 14, 2.0)"),
        ("atr", Some("atr"), "v = ta.atr(14)"),
        ("tr", Some("tr"), "v = ta.tr()"),
        ("obv", Some("obv"), "v = ta.obv()"),
        ("stoch", Some("stoch"), "v = ta.stoch(close, high, low, 14)"),
        ("vwap", Some("vwap"), "v = ta.vwap()"),
        ("wpr", Some("willr"), "v = ta.wpr(14)"),
        ("stdev", Some("stddev"), "v = ta.stdev(close, 14)"),
        ("variance", Some("var"), "v = ta.variance(close, 14)"),
        ("highest", None, "v = ta.highest(high, 5)"),
        ("lowest", None, "v = ta.lowest(low, 5)"),
        ("highestbars", None, "v = ta.highestbars(high, 5)"),
        ("lowestbars", None, "v = ta.lowestbars(low, 5)"),
        ("change", None, "v = ta.change(close, 3)"),
        (
            "crossover",
            None,
            "v = ta.crossover(ta.sma(close, 5), ta.sma(close, 10))",
        ),
        (
            "crossunder",
            None,
            "v = ta.crossunder(ta.sma(close, 5), ta.sma(close, 10))",
        ),
        ("cum", None, "v = ta.cum(close)"),
        ("correlation", None, "v = ta.correlation(close, open, 10)"),
        ("median", None, "v = ta.median(close, 5)"),
        ("linreg", None, "v = ta.linreg(close, 5, 0)"),
        ("dmi", None, "v = ta.dmi(14, 17)"),
        ("eit", Some("eit"), "v = ta.eit(close, 0.5)"),
        ("kalman", Some("kalman"), "v = ta.kalman(close, 14)"),
        ("voss", Some("voss"), "v = ta.voss(close, 20)"),
        ("barssince", None, "v = ta.barssince(close > open)"),
        (
            "valuewhen",
            None,
            "v = ta.valuewhen(close > open, close, 0)",
        ),
        ("pivothigh", None, "v = ta.pivothigh(high, 2, 2)"),
        ("pivotlow", None, "v = ta.pivotlow(low, 2, 2)"),
    ]
}

#[test]
fn bridge_method_list_is_frozen() {
    // Stale-row analog: the table above and the bridge agree exactly, so a
    // silent addition or removal fails here first. Deliberate changes update
    // both together, with justification.
    let table: Vec<&str> = ta_canonical()
        .iter()
        .map(|(method, _, _)| *method)
        .collect();
    assert_eq!(
        table,
        TaBridge::supported_methods(),
        "bridge method drift: table and bridge disagree"
    );
}

#[test]
fn bridge_kernel_backings_all_have_matrix_rows() {
    // Every kernel-backed bridge method resolves to a frozen matrix row —
    // the bridge can never route to an uncalibrated kernel.
    for (method, kernel, _) in ta_canonical() {
        if let Some(id) = kernel {
            assert!(
                crate::compat_for(id).is_some(),
                "bridge method ta.{method} backs onto {id} with no matrix row"
            );
        }
    }
}

#[test]
fn bridge_gaps_refuse_naming_the_method() {
    // The documented gap set (uncertain formulas or no kernel) plus the two
    // spike-only overlaps refuse with the method named, never miscompute.
    let bars = seeded_bars(40);
    for method in [
        "mode",
        "percentrank",
        "linreg",
        "dmi",
        "adx",
        "sar",
        "dc",
        "trima",
        "midpoint",
    ] {
        let reason = run_script(&format!("v = ta.{method}(close, 14)"), &bars)
            .expect_err("gap method refuses")
            .to_string();
        assert!(
            reason.contains(method),
            "ta.{method} refusal names it: {reason}"
        );
    }
}

#[test]
fn zlema_composition_matches_kernel_batch() {
    // The library's zero-lag shape (`2 * data - data[floor((len-1)/2)]`
    // through `ta.ema`) agrees with an independent EMA over the same
    // adjustment built in plain Rust — pinning the arm wiring, not just the
    // bridge it rides on.
    use crate::{EmaIndicator, Indicator, OwnedOhlcv, ParamSet};
    let bars = seeded_bars(40);
    let closes: Vec<f64> = bars
        .iter()
        .map(|bar| {
            use rust_decimal::prelude::ToPrimitive as _;
            bar.c.to_f64().expect("display leg")
        })
        .collect();
    let outcome = run_script(
        "z = ta.ema(2 * close - close[math.floor((10 - 1) / 2)], 10)",
        &bars,
    )
    .expect("zlema shape evaluates");
    let lag = 4;
    let adjusted: Vec<f64> = closes
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            if i >= lag {
                2.0 * c - closes[i - lag]
            } else {
                f64::NAN
            }
        })
        .collect();
    let legs = OwnedOhlcv {
        open: closes.clone(),
        high: closes.clone(),
        low: closes.clone(),
        close: adjusted,
        volume: vec![1_000.0; closes.len()],
    };
    let direct = EmaIndicator::new()
        .batch(&legs.views(), &ParamSet::new().with_int("length", 10))
        .expect("ema batch runs");
    for (at_bar, want) in direct.values.iter().enumerate() {
        let got = match outcome.value_at(at_bar, "z") {
            Some(Some(PineValue::Float(v))) => Some(v),
            Some(Some(PineValue::Int(v))) => Some(v as f64),
            _ => None,
        };
        match (got, want) {
            (None, None) => {}
            (Some(g), Some(w)) => assert!(
                (g - w).abs() <= 1e-9,
                "zlema drift at bar {at_bar}: {g} vs {w}"
            ),
            _ => panic!("zlema na drift at bar {at_bar}: {got:?} vs {want:?}"),
        }
    }
}

// ---- Tally sets mirroring the spike shape ----

/// Nine canonical scripts the pine surface must accept: sma, input-bound
/// ema, open-source sma, a two-plot pair, basis arithmetic, mixed-constant
/// arithmetic, left-associative arithmetic, wma, and alma.
fn pine_must_accept() -> Vec<&'static str> {
    vec![
        "s = ta.sma(close, 14)\nplot(s, title=\"sma\")",
        "len = input.int(14, \"len\")\ne = ta.ema(close, len)\nplot(e, title=\"ema\")",
        "s = ta.sma(open, 10)\nplot(s, title=\"sma-open\")",
        "len = input.int(14, \"len\")\ns = ta.sma(close, len)\ne = ta.ema(close, len)\nplot(s, title=\"s\")\nplot(e, title=\"e\")",
        "mult = input.float(2.0, \"mult\")\nbasis = ta.sma(close, 14)\nupper = basis + mult\nplot(upper, title=\"upper\")",
        "x = 2 * 3 + 4\ns = ta.sma(close, 14)\nplot(s, title=\"s\")",
        "y = 100 / 10 / 2\nz = 10 - 2 * 3\ne = ta.ema(close, 14)\nplot(e, title=\"e\")",
        "w = ta.wma(close, 9)\nplot(w, title=\"wma\")",
        "a = ta.alma(close, 8, 0.85, 6.0)\nplot(a, title=\"alma\")",
    ]
}

/// Eleven scripts the pine surface must refuse, never miscompile: two
/// syntax failures, unknown function/method/alias, arity, type, length,
/// truthy-number branch, out-of-bounds index, and an explicit
/// `runtime.error`. (Division by zero reads `na`, it does not refuse.)
fn pine_must_reject() -> Vec<&'static str> {
    vec![
        "x = (2 + 3",
        "x = 2 +",
        "x = nosuchfn(close)",
        "s = ta.sma(close)",
        "s = ta.sma(close, \"x\")",
        "s = ta.sma(close, 0)",
        "if 1\n    x = 1",
        "x = close.foo()",
        "x = XX.fn(1.0)",
        "a = [1.0, 2.0]\nx = a[5]",
        "runtime.error(\"boom\")\nx = 1",
    ]
}

/// Calibration tally mirroring the spike `CalibrationTally` shape: accept
/// rate on the must-accept set, reject rate on the must-reject set. Both
/// must be `1.0` before lowered pine output is cached or persisted
/// anywhere; nothing here caches or persists.
#[derive(Debug, Clone, PartialEq)]
struct PineTally {
    /// Must-accept scripts presented.
    must_accept: usize,
    /// Must-accept scripts accepted (parsed and evaluated clean).
    accepted: usize,
    /// Must-reject scripts presented.
    must_reject: usize,
    /// Must-reject scripts refused (parse error or evaluation refusal).
    refused: usize,
}

impl PineTally {
    /// Fraction of the must-accept set accepted.
    fn accept_rate(&self) -> f64 {
        if self.must_accept == 0 {
            return 0.0;
        }
        self.accepted as f64 / self.must_accept as f64
    }

    /// Fraction of the must-reject set refused.
    fn reject_rate(&self) -> f64 {
        if self.must_reject == 0 {
            return 0.0;
        }
        self.refused as f64 / self.must_reject as f64
    }

    /// Whether lowered pine output may be trusted: every must-accept
    /// accepted and every must-reject refused.
    fn is_green(&self) -> bool {
        (self.accept_rate() - 1.0).abs() < f64::EPSILON
            && (self.reject_rate() - 1.0).abs() < f64::EPSILON
    }
}

fn synthetic_module(publisher: &str, name: &str, text: &str) -> ModuleInput {
    let source = parse_pine_source(&format!("{publisher}/{name}"), text);
    let program = parse_program(text).expect("synthetic parses");
    ModuleInput::new(library_id(publisher, name, 1), source, program)
}

#[test]
fn pine_calibration_is_green() {
    let bars = seeded_bars(40);
    let accept_scripts = pine_must_accept();
    let builtin_scripts: Vec<&str> = ta_canonical()
        .iter()
        .map(|(_, _, script)| *script)
        .collect();
    let mut tally = PineTally {
        must_accept: accept_scripts.len() + 3 + builtin_scripts.len(),
        accepted: 0,
        must_reject: pine_must_reject().len() + 2,
        refused: 0,
    };
    for script in accept_scripts {
        assert!(
            run_script(script, &bars).is_ok(),
            "must-accept fired a refusal: {script}"
        );
        tally.accepted += 1;
    }
    // The three real files accept as one linked run each (libraries accept
    // alone too — declarations with no root statements evaluate clean).
    let triple = real_triple_linked();
    for end in 1..=3 {
        let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
        assert!(
            engine
                .run_linked(&triple[..end], &bars, InputSet::new())
                .is_ok(),
            "real prefix ..{end} accepts"
        );
        tally.accepted += 1;
    }
    for script in &builtin_scripts {
        assert!(
            run_script(script, &bars).is_ok(),
            "per-builtin canonical fired a refusal: {script}"
        );
        tally.accepted += 1;
    }
    for script in pine_must_reject() {
        let refused = match parse_program(script) {
            Err(_) => true,
            Ok(program) => {
                let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
                engine.run(&program, &bars, InputSet::new()).is_err()
            }
        };
        assert!(refused, "must-reject silently accepted: {script}");
        tally.refused += 1;
    }
    // Import-graph refusals: a cycle and an unknown import never link.
    let cycle = vec![
        synthetic_module(
            "pub",
            "A",
            "import pub/B/1 as B\nlibrary('A')\nexport f() =>\n    1.0",
        ),
        synthetic_module(
            "pub",
            "B",
            "import pub/A/1 as A\nlibrary('B')\nexport g() =>\n    2.0",
        ),
    ];
    assert!(crate::resolve_imports(cycle).is_err(), "import cycle links");
    tally.refused += 1;
    let unknown = vec![synthetic_module(
        "local",
        "Root",
        "import pub/Nowhere/1 as N\nindicator(title=\"T\")\nx = 1.0",
    )];
    assert!(
        crate::resolve_imports(unknown).is_err(),
        "unknown import links"
    );
    tally.refused += 1;
    assert_eq!(tally.must_accept, 9 + 3 + 42, "accept set shape drift");
    assert_eq!(tally.must_reject, 11 + 2, "reject set shape drift");
    assert!(
        (tally.accept_rate() - 1.0).abs() < f64::EPSILON,
        "accept < 1.0: {tally:?}"
    );
    assert!(
        (tally.reject_rate() - 1.0).abs() < f64::EPSILON,
        "reject < 1.0: {tally:?}"
    );
    assert!(tally.is_green(), "pine tally is not green: {tally:?}");
}

// ---- Script-level controls (must-NOT-fire) ----

#[test]
fn pine_controls_quiet() {
    // Every must-accept script evaluates clean on flat, short, and
    // single-tick frames: 0 false-fire refusals. Null injection cannot be
    // represented in decimal bars, so it stays at the kernel level
    // (`null_alignment_diffs` below); the candle-control suite lives in the
    // candle predicates' own calibration tests.
    let frames: Vec<(&str, Vec<Bar>)> = vec![
        ("flat", flat_bars(40)),
        ("short", seeded_bars(3)),
        ("single-tick", seeded_bars(1)),
    ];
    let mut scripts = pine_must_accept();
    scripts.extend(ta_canonical().iter().map(|(_, _, script)| *script));
    for (frame_name, bars) in &frames {
        for script in &scripts {
            assert!(
                run_script(script, bars).is_ok(),
                "{frame_name} control false-fired: {script}"
            );
        }
    }
    assert!(
        crate::calibration::null_alignment_diffs().is_empty(),
        "kernel null alignment drifted"
    );
}

// ---- No-cache-until-green review assertion ----

/// Persistence primitives that must never appear in the pine surface: no
/// lowered output may be cached or persisted before the gates above are
/// green, so the production modules own no path to disk, no global cache,
/// and no lazy static. Golden writes live in test code behind reviewer
/// environment flags, never here.
fn banned_persistence_patterns() -> Vec<&'static str> {
    vec![
        "std::fs::",
        "std::io::Write",
        "File::create",
        "OpenOptions",
        "OnceLock",
        "LazyLock",
        "static mut",
        "write_all",
        "create_dir",
        ".persist(",
        "persist!",
    ]
}

/// Approved in-memory memo entries: each string here names a cache the
/// reviewers explicitly approved. Approval requires all of: the cached
/// value is a deterministic pure function of the key (never a
/// confidence-based promotion), no calibration trigger feeds it, it is
/// memory-only and bounded, and the differential suite exercises the
/// cached path. Anything matching the banned list below without matching
/// an entry here fails the review.
fn allowlisted_cache_patterns() -> Vec<&'static str> {
    vec![
        // Compile/link memo: keyed by source content hash, stores the
        // deterministic link outcome (linked program or recorded
        // refusal). Tier-up and deopt state stay per-run; the
        // differential suite runs through this cache on every
        // compiled-mode test, proving the memo changes no outcome.
        "static LINK_CACHE: std::sync::OnceLock<",
    ]
}

#[test]
fn pine_surface_caches_nothing() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("pine");
    let mut sources = Vec::new();
    let entries = std::fs::read_dir(&dir).expect("pine source directory readable");
    for entry in entries {
        let path = entry.expect("readable entry").path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            sources.push(path);
        }
    }
    assert!(
        !sources.is_empty(),
        "pine surface sources found for the cache review"
    );
    let allowlisted = allowlisted_cache_patterns();
    for path in &sources {
        let text = std::fs::read_to_string(path).expect("source readable");
        for pattern in banned_persistence_patterns() {
            for (number, line) in text.lines().enumerate() {
                if line.contains(pattern) && !allowlisted.iter().any(|ok| line.contains(ok)) {
                    panic!(
                        "{}:{} caches or persists ({pattern}): {line}",
                        path.display(),
                        number + 1,
                    );
                }
            }
        }
    }
}
