//! Overlap freeze: every parity-matrix row executes in the engine with its
//! pinned outcome, and the accept/refuse tallies hold at 1.0 before any new
//! output is cached or persisted.
//!
//! What each test owns:
//! - `freeze_matrix_covers_registry`: the kernel matrix still covers the
//!   builtin registry bijectively (frozen-eps widening itself is pinned by
//!   the calibration suite's `frozen_eps_table_never_widens`, which runs in
//!   the same gate; this file does not duplicate that table).
//! - `freeze_match_rows_carry_probes` / `freeze_correct_rows_carry_probes`:
//!   every `Match`/`Correct` row carries an executable probe, except the
//!   three linked-library rows below. A row without a probe is an
//!   unchecked claim, so the suite fails until the matrix gains one.
//! - `freeze_match_ta_bridge_agrees_with_kernels`: each bridged `ta.*`
//!   shape serves its kernel (streaming update agrees with the kernel
//!   batch at `1e-9` on a 60-bar ramp), with hand-derived pins where the
//!   bridge serves shared window helpers instead of a kernel.
//! - `freeze_match_scalars_pin_values`: literal-shaped rows pin their
//!   last-bar value from hand-computed expectations, never measured ones.
//! - `freeze_match_captures_recorded`: drawing rows record their captures.
//! - `freeze_correct_divergences_pinned` / `freeze_correct_refusals_*`:
//!   deliberate divergences behave exactly as their row reasons state,
//!   including the `Correct` rows whose pinned behavior is a refusal.
//! - `freeze_refuse_probes_pin_strings`: every `Refuse` row fails with its
//!   recorded string.
//! - `freeze_tally_full_accept_full_refuse`: the task-value gate — 1.0
//!   accept over every must-run program, 1.0 refuse over every must-refuse
//!   program (mirrors the spike `CalibrationTally` shape one crate over).
//! - `freeze_imported_rows_run_linked`: the two import rows run through a
//!   synthetic two-module closure (no shipped files).
//! - `freeze_lower_tf_serves_injected_columns`: the injected lower-tf row
//!   serves registered columns.
//!
//! Exempt from row probes (asserted exact by the presence test): the three
//! linked-library rows (`factorySmoothing dispatch`, `normalizeOsc table`,
//! `getOscillatorColor table`) execute only through the shipped
//! `libUtil`+`libMAs` closure, which the `pine_libmas` golden suite pins
//! comprehensively in this same gate; duplicating that fixture here would
//! add setup without new signal.
//!
//! Test path note: this file rides the `pine::egui_parity` unit module, so
//! the runnable filter is `pine_parity_freeze` (matched by module path),
//! not the file stem.

use super::*;
use crate::compat::assert_matrix_covers;
use crate::filters::{KalmanIndicator, VossIndicator};
use crate::indicator::{OwnedOhlcv, ParamSet, builtin_registry};
use crate::{
    InputSet, LibraryId, LinkedModule, LtfDataset, ModuleInput, PineEngine, PineRuntimeConfig,
    parse_pine_source, parse_program, resolve_imports,
};
use chrono::TimeZone as _;
use protopyne_types::Bar;
use std::collections::HashMap;

/// Frozen agreement tolerance shared with the kernel-vs-Pine matrix.
const EPS: f64 = 1e-9;

fn bar_at(index: i64) -> Bar {
    Bar::new()
        .ts(chrono::Utc
            .timestamp_opt(1_700_000_000 + index * 60, 0)
            .single()
            .expect("valid test timestamp"))
        .o("100.00".parse().expect("decimal"))
        .h("101.00".parse().expect("decimal"))
        .l("99.00".parse().expect("decimal"))
        .c(format!("{}.00", 100 + index).parse().expect("decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

/// 24-bar scalar fixture: constant open/high/low, close ramps `100+i`.
fn bars24() -> Vec<Bar> {
    (0..24).map(bar_at).collect()
}

/// 60-bar kernel fixture: same shape, long enough that every bridged
/// method (including `macd(12, 26, 9)` and `voss(20)`) prints defined
/// values on the last bar, so the oracle pins never compare `na`.
fn bars60() -> Vec<Bar> {
    (0..60).map(bar_at).collect()
}

/// `f64` legs mirroring a bar fixture for the kernel-batch oracle.
fn legs_of(count: usize) -> OwnedOhlcv {
    OwnedOhlcv {
        open: vec![100.0; count],
        high: vec![101.0; count],
        low: vec![99.0; count],
        close: (0..count).map(|i| 100.0 + i as f64).collect(),
        volume: vec![1_000.0; count],
    }
}

fn run_probe(script: &str) -> Result<crate::RunOutcome, crate::KernelError> {
    let program = parse_program(script).expect("probe parses");
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine.run(&program, &bars24(), InputSet::new())
}

fn run_probe60(script: &str) -> Result<crate::RunOutcome, crate::KernelError> {
    let program = parse_program(script).expect("probe parses");
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine.run(&program, &bars60(), InputSet::new())
}

fn run_probe_warmup(script: &str, warmup: u64) -> Result<crate::RunOutcome, crate::KernelError> {
    let program = parse_program(script).expect("probe parses");
    let mut engine = PineEngine::new(
        PineRuntimeConfig::new()
            .warmup_bars(warmup)
            .build(),
    );
    engine.run(&program, &bars24(), InputSet::new())
}

/// Last-bar cell of `name`: `None` when the binding is missing or `na`.
fn cell(outcome: &crate::RunOutcome, name: &str) -> Option<crate::PineValue> {
    let at = outcome.snapshots.len().checked_sub(1).expect("bar ran");
    outcome.value_at(at, name).flatten()
}

/// Kernel-batch oracle: the confirmed last value of `kernel` over the
/// 60-bar legs. Panics when the tail is `na` — a vacuous agreement would
/// prove nothing, so oracle silence is a test bug, not a pass.
fn kernel_last(kernel: &str, params: &ParamSet) -> f64 {
    let registry = builtin_registry().expect("builtins register");
    let indicator = registry.get(kernel).expect("kernel bound");
    let data = legs_of(60);
    let result = indicator
        .batch(&data.views(), params)
        .expect("batch runs");
    result
        .values
        .last()
        .copied()
        .flatten()
        .expect("oracle prints on the last bar")
}

fn len14() -> ParamSet {
    ParamSet::new().with_int("length", 14)
}

/// Hand-derived pin for one last-bar scalar.
#[derive(Debug, Clone, Copy)]
enum Pin {
    Num(f64),
    Int(i64),
    Bool(bool),
    Text(&'static str),
    /// Array of optional floats, element-wise within `EPS`.
    Floats(&'static [Option<f64>]),
    /// Any present (non-`na`) value; the shape is asserted structurally.
    Present,
}

fn assert_pin(outcome: &crate::RunOutcome, var: &str, pin: Pin, ctx: &str) {
    let got = cell(outcome, var);
    match pin {
        Pin::Num(want) => match got {
            Some(crate::PineValue::Float(got)) => assert!(
                (got - want).abs() <= EPS * want.abs().max(1.0),
                "{ctx}: {var} got {got}, want {want}"
            ),
            Some(crate::PineValue::Int(got)) => assert!(
                (got as f64 - want).abs() <= EPS * want.abs().max(1.0),
                "{ctx}: {var} got int {got}, want {want}"
            ),
            other => panic!("{ctx}: {var} reads numeric, got {other:?}"),
        },
        Pin::Int(want) => assert_eq!(
            got,
            Some(crate::PineValue::Int(want)),
            "{ctx}: {var} pins int {want}"
        ),
        Pin::Bool(want) => assert_eq!(
            got,
            Some(crate::PineValue::Bool(want)),
            "{ctx}: {var} pins bool {want}"
        ),
        Pin::Text(want) => match got {
            Some(crate::PineValue::Str(got)) => {
                assert_eq!(got, want, "{ctx}: {var} pins text");
            }
            other => panic!("{ctx}: {var} reads text, got {other:?}"),
        },
        Pin::Floats(want) => match got {
            Some(crate::PineValue::Array(items)) => {
                assert_eq!(items.len(), want.len(), "{ctx}: {var} pins array length");
                for (index, (slot, want)) in items.iter().zip(want.iter()).enumerate() {
                    match (slot, want) {
                        (Some(crate::PineValue::Float(got)), Some(want)) => assert!(
                            (got - want).abs() <= EPS * want.abs().max(1.0),
                            "{ctx}: {var}[{index}] got {got}, want {want}"
                        ),
                        (Some(crate::PineValue::Int(got)), Some(want)) => assert!(
                            (*got as f64 - want).abs() <= EPS * want.abs().max(1.0),
                            "{ctx}: {var}[{index}] got int {got}, want {want}"
                        ),
                        (None, None) => {}
                        other => panic!("{ctx}: {var}[{index}] mismatch, got {other:?}"),
                    }
                }
            }
            other => panic!("{ctx}: {var} reads an array, got {other:?}"),
        },
        Pin::Present => assert!(
            got.is_some(),
            "{ctx}: {var} reads a present value"
        ),
    }
}

fn assert_absent(outcome: &crate::RunOutcome, var: &str, ctx: &str) {
    let at = outcome.snapshots.len().checked_sub(1).expect("bar ran");
    assert_eq!(
        outcome.value_at(at, var),
        Some(None),
        "{ctx}: {var} reads na"
    );
}

/// `(construct, probe args)` for every bridged `ta.*` row, in matrix
/// order. Probes bind `x`; the oracle table below pins each one.
const TA_PROBES: &[(&str, &str)] = &[
    ("ta.sma", "x = ta.sma(close, 14)"),
    ("ta.ema", "x = ta.ema(close, 14)"),
    ("ta.rsi", "x = ta.rsi(close, 14)"),
    ("ta.macd", "x = ta.macd(close, 12, 26, 9)"),
    ("ta.bb", "x = ta.bb(close, 14, 2.0)"),
    ("ta.atr", "x = ta.atr(14)"),
    ("ta.stoch", "x = ta.stoch(close, high, low, 14)"),
    ("ta.wma", "x = ta.wma(close, 14)"),
    ("ta.hma", "x = ta.hma(close, 14)"),
    ("ta.vwma", "x = ta.vwma(close, 14)"),
    ("ta.highest", "x = ta.highest(close, 14)"),
    ("ta.lowest", "x = ta.lowest(close, 14)"),
    ("ta.tr", "x = ta.tr()"),
    ("ta.change", "x = ta.change(close)"),
    ("ta.mom", "x = ta.mom(close, 14)"),
    ("ta.roc", "x = ta.roc(close, 14)"),
    ("ta.crossover", "x = ta.crossover(close, open)"),
    ("ta.crossunder", "x = ta.crossunder(close, open)"),
    ("ta.cum", "x = ta.cum(close)"),
    ("ta.stdev", "x = ta.stdev(close, 14)"),
    ("ta.vwap", "x = ta.vwap()"),
    ("ta.variance", "x = ta.variance(close, 14)"),
    ("ta.median", "x = ta.median(close, 14)"),
    ("ta.correlation", "x = ta.correlation(close, close, 14)"),
    ("ta.wpr", "x = ta.wpr(14)"),
    ("ta.cmo", "x = ta.cmo(close, 14)"),
    ("ta.mfi", "x = ta.mfi(close, 14)"),
    ("ta.obv", "x = ta.obv()"),
    ("ta.rma", "x = ta.rma(close, 14)"),
    ("ta.cci", "x = ta.cci(close, 14)"),
    ("ta.alma", "x = ta.alma(close, 14, 0.85, 6.0)"),
    ("ta.highestbars", "x = ta.highestbars(close, 14)"),
    ("ta.lowestbars", "x = ta.lowestbars(close, 14)"),
    ("ta.eit", "x = ta.eit(close, 0.5)"),
    ("ta.kalman", "x = ta.kalman(close, 14)"),
    ("ta.voss", "x = ta.voss(close, 20)"),
];

/// Oracle for one bridged call: kernel batch agreement, or a hand-derived
/// pin where the bridge serves shared window helpers instead of a kernel.
#[derive(Debug, Clone, Copy)]
enum Oracle {
    Kernel(&'static str, fn() -> ParamSet),
    Num(f64),
    Int(i64),
    Bool(bool),
}

fn alma14() -> ParamSet {
    ParamSet::new()
        .with_int("length", 14)
        .with_float("offset", 0.85)
        .with_float("sigma", 6.0)
}

fn macd12269() -> ParamSet {
    ParamSet::new()
        .with_int("fast_length", 12)
        .with_int("slow_length", 26)
        .with_int("signal_length", 9)
}

fn bb14mult2() -> ParamSet {
    ParamSet::new()
        .with_int("length", 14)
        .with_float("mult", 2.0)
}

fn spread14() -> ParamSet {
    ParamSet::new()
        .with_int("length", 14)
        .with_float("nbdev", 1.0)
}

fn eit_half() -> ParamSet {
    ParamSet::new().with_float("alpha", 0.5)
}

fn kalman14() -> ParamSet {
    ParamSet::new()
        .with_int("length", 14)
        .with_float("alpha", KalmanIndicator::DEFAULT_ALPHA)
        .with_float("beta", KalmanIndicator::DEFAULT_BETA)
}

fn voss20() -> ParamSet {
    ParamSet::new()
        .with_int("length", 20)
        .with_int("predict", VossIndicator::DEFAULT_PREDICT)
        .with_float("band", VossIndicator::DEFAULT_BAND)
}

fn empty_params() -> ParamSet {
    ParamSet::new()
}

fn ta_oracle(construct: &str) -> Oracle {
    match construct {
        "ta.sma" => Oracle::Kernel("sma", len14),
        "ta.ema" => Oracle::Kernel("ema", len14),
        "ta.rsi" => Oracle::Kernel("rsi", len14),
        "ta.macd" => Oracle::Kernel("macd", macd12269),
        "ta.bb" => Oracle::Kernel("bb", bb14mult2),
        "ta.atr" => Oracle::Kernel("atr", len14),
        "ta.stoch" => Oracle::Kernel("stoch", len14),
        "ta.wma" => Oracle::Kernel("wma", len14),
        "ta.hma" => Oracle::Kernel("hma", len14),
        "ta.vwma" => Oracle::Kernel("vwma", len14),
        // Linear ramp `100+i` on closes: extremes are the window ends.
        "ta.highest" => Oracle::Num(159.0),
        "ta.lowest" => Oracle::Num(146.0),
        "ta.tr" => Oracle::Kernel("tr", empty_params),
        // One-bar lookback on a slope-1 ramp, and self-correlation.
        "ta.change" | "ta.correlation" => Oracle::Num(1.0),
        "ta.mom" => Oracle::Kernel("mom", len14),
        "ta.roc" => Oracle::Kernel("roc", len14),
        // `close` meets `open` only on bar 0, then stays above: the cross
        // fires once (bar 1) and never uncrosses, so the last bar is quiet.
        "ta.crossover" | "ta.crossunder" => Oracle::Bool(false),
        // `100 + (0 + ... + 59)` over the 60-bar ramp.
        "ta.cum" => Oracle::Num(7770.0),
        "ta.stdev" => Oracle::Kernel("stddev", spread14),
        "ta.vwap" => Oracle::Kernel("vwap", empty_params),
        "ta.variance" => Oracle::Kernel("var", spread14),
        // Even window `146..=159`: mean of the two middle prints.
        "ta.median" => Oracle::Num(152.5),
        "ta.wpr" => Oracle::Kernel("willr", len14),
        "ta.cmo" => Oracle::Kernel("cmo", len14),
        "ta.mfi" => Oracle::Kernel("mfi", len14),
        "ta.obv" => Oracle::Kernel("obv", empty_params),
        "ta.rma" => Oracle::Kernel("rma", len14),
        "ta.cci" => Oracle::Kernel("cci", len14),
        "ta.alma" => Oracle::Kernel("alma", alma14),
        // Ramp extremes sit at the window ends: newest high, oldest low.
        "ta.highestbars" => Oracle::Int(0),
        "ta.lowestbars" => Oracle::Int(-13),
        "ta.eit" => Oracle::Kernel("eit", eit_half),
        "ta.kalman" => Oracle::Kernel("kalman", kalman14),
        "ta.voss" => Oracle::Kernel("voss", voss20),
        other => panic!("no oracle for bridged {other}"),
    }
}

#[test]
fn freeze_matrix_covers_registry() {
    let registry = builtin_registry().expect("builtins register");
    assert_matrix_covers(&registry).expect("matrix covers registry");
}

/// Rows that run only through linked closures, so they carry no
/// single-module probe: the three library-table rows ride the
/// `pine_libmas` golden suite, and imported-type construction rides the
/// linked test below — all in this same gate.
const LINKED_ROWS: &[&str] = &[
    "request.security_lower_tf (injected)",
    "imported type construction",
    "factorySmoothing dispatch",
    "normalizeOsc table",
    "getOscillatorColor table",
];

#[test]
fn freeze_match_rows_carry_probes() {
    let mut probeless = Vec::new();
    for row in EGUI_PARITY {
        if row.verdict != ParityVerdict::Match {
            continue;
        }
        if row.probe.is_none() {
            probeless.push(row.construct);
        }
    }
    assert_eq!(
        probeless,
        LINKED_ROWS,
        "every Match row carries a probe except the linked rows"
    );
}

#[test]
fn freeze_correct_rows_carry_probes() {
    for row in EGUI_PARITY {
        if row.verdict != ParityVerdict::Correct {
            continue;
        }
        assert!(
            row.probe.is_some(),
            "Correct row {} carries a probe for its divergence",
            row.construct
        );
    }
}

#[test]
fn freeze_match_ta_bridge_agrees_with_kernels() {
    assert_eq!(TA_PROBES.len(), 36, "one probe per bridged method");
    for (construct, probe) in TA_PROBES {
        let row = parity_for(construct).expect("bridged row known");
        assert_eq!(row.verdict, ParityVerdict::Match, "{construct} stands");
        assert_eq!(
            row.probe,
            Some(*probe),
            "{construct} probe frozen in the matrix"
        );
        let outcome = run_probe60(probe).expect("bridged probe runs");
        match ta_oracle(construct) {
            Oracle::Kernel(kernel, params) => {
                let want = kernel_last(kernel, &params());
                let got = cell(&outcome, "x");
                match got {
                    Some(crate::PineValue::Float(got)) => assert!(
                        (got - want).abs() <= EPS * want.abs().max(1.0),
                        "{construct} serves its kernel: got {got}, want {want}"
                    ),
                    other => panic!("{construct} reads numeric, got {other:?}"),
                }
            }
            Oracle::Num(want) => assert_pin(&outcome, "x", Pin::Num(want), construct),
            Oracle::Int(want) => assert_pin(&outcome, "x", Pin::Int(want), construct),
            Oracle::Bool(want) => assert_pin(&outcome, "x", Pin::Bool(want), construct),
        }
    }
    // Unbiased spread takes the sample path, not the kernel: hand-derived
    // `sqrt(227.5 / 13)` over window `146..=159`.
    let outcome =
        run_probe60("x = ta.stdev(close, 14, false)").expect("unbiased stdev runs");
    assert_pin(
        &outcome,
        "x",
        Pin::Num((227.5_f64 / 13.0).sqrt()),
        "ta.stdev unbiased",
    );
    let outcome =
        run_probe60("x = ta.variance(close, 14, false)").expect("unbiased variance runs");
    assert_pin(&outcome, "x", Pin::Num(227.5 / 13.0), "ta.variance unbiased");
}

/// `(construct, var, pin)`: hand-computed last-bar pins for literal-shaped
/// `Match` rows on the 24-bar fixture (`close` ends at 123, `open` holds
/// 100, `time` ends at `1_700_001_380` seconds).
const SCALAR_PINS: &[(&str, &str, Pin)] = &[
    ("assignment (=)", "x", Pin::Num(41.0)),
    ("if/else", "x", Pin::Num(1.0)),
    ("while loop", "x", Pin::Num(3.0)),
    ("break", "x", Pin::Num(3.0)),
    ("number literal", "x", Pin::Num(42.0)),
    ("string literal", "x", Pin::Text("hi")),
    ("boolean literal", "x", Pin::Bool(true)),
    ("variable", "x", Pin::Num(7.0)),
    ("built-in series (open/high/low/close/volume/time)", "x", Pin::Num(123.0)),
    ("arithmetic (+ - * %)", "x", Pin::Num(10.0)),
    ("comparison (== != < <= > >=)", "x", Pin::Bool(true)),
    ("unary (- not)", "x", Pin::Bool(true)),
    ("namespaced call (bridged)", "x", Pin::Num(4.0)),
    ("array access (arr[i])", "x", Pin::Num(2.0)),
    ("series offset (close[1])", "x", Pin::Num(122.0)),
    ("array literal", "x", Pin::Int(2)),
    ("na", "x", Pin::Bool(true)),
    ("nz", "x", Pin::Num(0.0)),
    ("math.abs", "x", Pin::Num(3.5)),
    ("math.max", "x", Pin::Num(5.0)),
    ("math.min", "x", Pin::Num(1.0)),
    ("math.round", "x", Pin::Num(2.0)),
    ("math.floor", "x", Pin::Num(2.0)),
    ("math.ceil", "x", Pin::Num(3.0)),
    ("math.sqrt", "x", Pin::Num(4.0)),
    ("math.pow", "x", Pin::Num(1024.0)),
    ("math.log", "x", Pin::Num(0.0)),
    ("math.log10", "x", Pin::Num(2.0)),
    ("math.exp", "x", Pin::Num(1.0)),
    ("math.sign", "x", Pin::Num(-1.0)),
    ("math.avg", "x", Pin::Num(2.0)),
    ("math.sum", "x", Pin::Num(6.0)),
    ("str.contains", "x", Pin::Bool(true)),
    ("str.length", "x", Pin::Int(3)),
    ("str.replace", "x", Pin::Text("bbb")),
    ("str.split", "s", Pin::Num(2.0)),
    ("str.tostring", "x", Pin::Text("42")),
    ("str.tonumber", "x", Pin::Num(3.5)),
    ("str.upper", "x", Pin::Text("AB")),
    ("str.lower", "x", Pin::Text("ab")),
    ("str.startswith", "x", Pin::Bool(true)),
    ("str.endswith", "x", Pin::Bool(true)),
    ("str.substring", "x", Pin::Text("el")),
    ("str.trim", "x", Pin::Text("hi")),
    ("input.int", "x", Pin::Int(10)),
    ("input.float", "x", Pin::Num(1.5)),
    ("input.bool", "x", Pin::Bool(true)),
    ("input.string", "x", Pin::Text("hi")),
    ("input.source", "x", Pin::Num(123.0)),
    ("input.session", "x", Pin::Text("0930-1600")),
    ("bar_index", "x", Pin::Int(23)),
    ("barstate.*", "x", Pin::Bool(false)),
    ("time (series)", "x", Pin::Num(1_700_001_380.0)),
    ("timenow", "x", Pin::Num(1_700_001_380.0)),
    ("syminfo.tickerid", "x", Pin::Text("LOCAL")),
    ("timeframe.period", "x", Pin::Text("1")),
    ("time(timeframe, session)", "x", Pin::Num(1_700_001_380.0)),
    ("matrix.get", "x", Pin::Text("x")),
    ("matrix.rows", "x", Pin::Num(2.0)),
    ("matrix.columns", "x", Pin::Num(2.0)),
    ("for...in", "x", Pin::Num(4.0)),
    ("descending for...to", "x", Pin::Num(3.0)),
    ("array.shift", "s", Pin::Num(1.0)),
    ("array.variance", "x", Pin::Num(2.0 / 3.0)),
    ("array.median", "x", Pin::Num(2.0)),
    ("array.mode", "x", Pin::Num(2.0)),
    // Z-scores under the population spread: `(x - 2) / sqrt(2 / 3)`.
    (
        "array.standardize",
        "x",
        Pin::Floats(&[Some(-1.224_744_871_391_589), Some(0.0), Some(1.224_744_871_391_589)]),
    ),
    (
        "array.percentile_linear_interpolation",
        "x",
        Pin::Num(1.5),
    ),
    ("array.covariance", "x", Pin::Num(0.5)),
    ("int() cast", "x", Pin::Int(5)),
    ("line.style_* constants", "x", Pin::Present),
    ("trailing if yields branch value", "x", Pin::Num(1.0)),
    ("trailing collection call writes back", "hit", Pin::Num(8.0)),
    ("switch expression value", "x", Pin::Num(10.0)),
    ("switch expression value", "y", Pin::Num(20.0)),
    ("nested scope history isolation", "first", Pin::Num(1220.0)),
    ("nested scope history isolation", "second", Pin::Num(122.0)),
    (
        "array.concat",
        "x",
        Pin::Floats(&[Some(1.0), Some(2.0)]),
    ),
];

#[test]
fn freeze_match_scalars_pin_values() {
    for (construct, var, pin) in SCALAR_PINS {
        let row = parity_for(construct).expect("scalar row known");
        assert_eq!(row.verdict, ParityVerdict::Match, "{construct} stands");
        let probe = row.probe.expect("scalar row carries a probe");
        let outcome = run_probe(probe).expect("scalar probe runs");
        assert_pin(&outcome, var, *pin, construct);
    }
}

#[test]
fn freeze_match_captures_recorded() {
    // `plotcandle` legs, `barcolor` series, `fill` region, and the
    // label/table/box/line lifecycles all record one entry per bar.
    for construct in [
        "plotcandle",
        "barcolor",
        "fill",
        "label.new",
        "table.new",
        "box.new",
        "line.new",
    ] {
        let row = parity_for(construct).expect("capture row known");
        assert_eq!(row.verdict, ParityVerdict::Match, "{construct} stands");
        let probe = row.probe.expect("capture row carries a probe");
        let outcome = run_probe(probe).expect("capture probe runs");
        let bars = outcome.snapshots.len();
        assert!(bars > 0, "{construct} evaluates bars");
        match construct {
            "plotcandle" => assert_eq!(
                outcome.plotcandles.len(),
                1,
                "one plotcandle site captured"
            ),
            "barcolor" => {
                assert_eq!(outcome.barcolors.len(), 1, "one barcolor site captured");
                assert_eq!(
                    outcome.barcolors[0].values.len(),
                    bars,
                    "barcolor covers every bar"
                );
            }
            "fill" => assert_eq!(outcome.fills.len(), 1, "one fill captured"),
            _ => assert!(
                !outcome.draw_events.is_empty(),
                "{construct} records lifecycle events"
            ),
        }
    }
    // Two plot sites capture two titled series (the reference merges them
    // into one; the divergence is pinned by the `plot` Correct row).
    let row = parity_for("plot").expect("plot row known");
    let outcome = run_probe(row.probe.expect("plot carries a probe")).expect("plot runs");
    assert_eq!(outcome.plots.len(), 2, "one series per plot site");
    let titles: Vec<&str> = outcome.plots.iter().map(|plot| plot.title.as_str()).collect();
    assert_eq!(titles, ["A", "B"], "plot titles recorded per site");
}

#[test]
fn freeze_match_input_handles() {
    // `input.enum` answers the declared member as an opaque handle.
    let row = parity_for("input.enum").expect("enum row known");
    assert_eq!(row.verdict, ParityVerdict::Match, "enum stands");
    let outcome = run_probe(row.probe.expect("enum carries a probe")).expect("enum runs");
    let at = outcome.snapshots.len() - 1;
    assert_eq!(
        outcome.value_at(at, "x"),
        Some(Some(crate::PineValue::EnumHandle(
            crate::value::EnumHandle::new("Smoothings", "WMA")
        ))),
        "enum default reads back"
    );
    // `input.color` answers an opaque color handle.
    let row = parity_for("input.color").expect("color row known");
    assert_eq!(row.verdict, ParityVerdict::Match, "color stands");
    let outcome = run_probe(row.probe.expect("color carries a probe")).expect("color runs");
    let at = outcome.snapshots.len() - 1;
    assert!(
        matches!(
            outcome.value_at(at, "x"),
            Some(Some(crate::PineValue::ColorHandle(_)))
        ),
        "color stays opaque"
    );
}

/// `(construct, var, pin)`: hand-computed last-bar pins for the `array.*`
/// basics on small literal arrays (no fixture dependence).
const ARRAY_PINS: &[(&str, &str, Pin)] = &[
    ("array.new", "x", Pin::Int(3)),
    ("array.from", "x", Pin::Int(2)),
    ("array.size", "x", Pin::Int(1)),
    ("array.get", "x", Pin::Num(6.0)),
    ("array.push", "x", Pin::Int(2)),
    ("array.pop", "x", Pin::Num(2.0)),
    ("array.first", "x", Pin::Num(1.0)),
    ("array.last", "x", Pin::Num(2.0)),
    ("array.copy", "x", Pin::Int(1)),
    ("array.clear", "x", Pin::Int(0)),
    ("array.includes", "x", Pin::Bool(true)),
    ("array.indexof", "x", Pin::Int(1)),
    ("array.sum", "x", Pin::Num(6.0)),
    ("array.avg", "x", Pin::Num(2.0)),
    ("array.max", "x", Pin::Num(5.0)),
    ("array.min", "x", Pin::Num(1.0)),
    // Biased population spread of `[1, 2, 3]`: `sqrt(2 / 3)`.
    ("array.stdev", "x", Pin::Num(0.816_496_580_927_726)),
    ("array.fill", "x", Pin::Num(9.0)),
    (
        "array.slice",
        "x",
        Pin::Floats(&[Some(2.0), Some(3.0)]),
    ),
    ("array.reverse", "x", Pin::Num(2.0)),
    ("array.sort", "x", Pin::Num(1.0)),
];

#[test]
fn freeze_match_array_basics() {
    assert_eq!(ARRAY_PINS.len(), 21, "one pin per array-basic row");
    for (construct, var, pin) in ARRAY_PINS {
        let row = parity_for(construct).expect("array row known");
        assert!(
            row.verdict == ParityVerdict::Match || row.verdict == ParityVerdict::Correct,
            "{construct} stands as Match or Correct"
        );
        let probe = row.probe.expect("array row carries a probe");
        let outcome = run_probe(probe).expect("array probe runs");
        assert_pin(&outcome, var, *pin, construct);
    }
}

/// `Correct` rows whose pinned behavior is a value: `(construct, var, pin)`
/// plus absence pins asserted separately below.
const CORRECT_PINS: &[(&str, &str, Pin)] = &[
    ("reassignment (:=)", "x", Pin::Num(2.0)),
    ("for loop", "x", Pin::Num(3.0)),
    ("boolean and/or with na", "a", Pin::Bool(false)),
    ("boolean and/or with na", "b", Pin::Bool(true)),
    ("function call", "x", Pin::Num(3.0)),
    ("method body scope (calls owner, reads caller)", "x", Pin::Num(1001.0)),
    ("ta.* source argument", "a", Pin::Num(100.0)),
    ("ta.* source argument", "x", Pin::Num(152.5)),
];

#[test]
fn freeze_correct_divergences_pinned() {
    for (construct, var, pin) in CORRECT_PINS {
        let row = parity_for(construct).expect("correct row known");
        assert_eq!(row.verdict, ParityVerdict::Correct, "{construct} stands");
        let probe = row.probe.expect("correct row carries a probe");
        // The source-honoring probe needs the 60-bar ramp (windowed mean
        // over `146..=159`); every other divergence pins on 24 bars.
        let outcome = if *construct == "ta.* source argument" {
            run_probe60(probe).expect("source probe runs")
        } else {
            run_probe(probe).expect("divergence probe runs")
        };
        assert_pin(&outcome, var, *pin, construct);
    }
    // Multi-output kernels yield their primary series: `ta.macd` on the
    // 60-bar ramp equals the kernel batch primary at `1e-9`.
    let row = parity_for("ta.macd/ta.bb/ta.stoch tuple shape").expect("tuple row known");
    let outcome = run_probe60(row.probe.expect("tuple carries a probe")).expect("runs");
    let want = kernel_last("macd", &macd12269());
    assert_pin(&outcome, "x", Pin::Num(want), "ta.macd/ta.bb/ta.stoch tuple shape");
    // `var` initializes on the first *evaluated* bar: under a two-bar
    // warmup the binding opens at `close[2]`, not the reference's bar 0.
    let row = parity_for("var declaration").expect("var row known");
    let outcome = run_probe_warmup(row.probe.expect("var carries a probe"), 2)
        .expect("var probe runs");
    assert_eq!(outcome.snapshots.len(), 22, "warmup bars stay unseen");
    let first = outcome.snapshots.first().expect("first evaluated bar");
    assert_eq!(first.bar, 2, "first snapshot is the first evaluated bar");
    assert_eq!(
        first.values.get("v").cloned().flatten(),
        Some(crate::PineValue::Float(102.0)),
        "var opens at close[2], not the reference bar-0 close"
    );
    // A zero denominator reads absence on both float and int division.
    let row = parity_for("division (/)").expect("division row known");
    let outcome = run_probe(row.probe.expect("division carries a probe")).expect("runs");
    assert_absent(&outcome, "x", "division (/)");
    assert_absent(&outcome, "y", "division (/)");
    // `na` conditions skip: the guarded assignment never fires.
    let row = parity_for("if/while na condition").expect("na-condition row known");
    let outcome = run_probe(row.probe.expect("na-condition carries a probe")).expect("runs");
    assert_pin(&outcome, "x", Pin::Num(0.0), "if/while na condition");
    // An `na` ternary condition yields `na`.
    let row = parity_for("ternary").expect("ternary row known");
    let outcome = run_probe(row.probe.expect("ternary carries a probe")).expect("runs");
    assert_absent(&outcome, "x", "ternary");
    // `fixnan` forward-fills per call site: `na` until the first print,
    // then the series itself (`close[5]` lands on bar 5 at 105).
    let row = parity_for("fixnan").expect("fixnan row known");
    let outcome = run_probe(row.probe.expect("fixnan carries a probe")).expect("runs");
    assert_eq!(
        outcome.value_at(0, "x"),
        Some(None),
        "fixnan opens na with no prior print"
    );
    assert_pin(&outcome, "x", Pin::Num(118.0), "fixnan");
    // Failing `and` / passing `or` annihilate; the other two pairings
    // stay `na` (re-run here: the live `outcome` above is the fixnan run).
    let row = parity_for("boolean and/or with na").expect("bool row known");
    let outcome = run_probe(row.probe.expect("bool carries a probe")).expect("runs");
    assert_absent(&outcome, "c", "boolean and/or with na");
    assert_absent(&outcome, "d", "boolean and/or with na");
}

/// `Correct` rows whose pinned behavior is a refusal: `(construct, probe,
/// refusal)` with the exact engine string (prefix `pine evaluation
/// refused: ` is added by the error rendering).
const CORRECT_REFUSALS: &[(&str, &str, &str)] = &[
    (
        "for loop",
        "for i = 1.5 to 3\n    x = i",
        "'for start' needs an int, got 1.5",
    ),
    (
        "boolean and/or with na",
        "x = 1 and true",
        "operator 'and' needs bools on both sides, got 1 and true",
    ),
];

#[test]
fn freeze_correct_refusals_pin_strings() {
    // Fractional `for` bounds refuse instead of truncating.
    let (construct, probe, refusal) = CORRECT_REFUSALS[0];
    let row = parity_for(construct).expect("for row known");
    assert_eq!(row.verdict, ParityVerdict::Correct, "{construct} stands");
    let err = run_probe(probe).expect_err("fractional bounds refuse");
    assert_eq!(
        err.to_string(),
        format!("pine evaluation refused: {refusal}"),
        "exact refusal for {construct}"
    );
    // Numbers in `and`/`or` refuse instead of coercing.
    let (construct, probe, refusal) = CORRECT_REFUSALS[1];
    let err = run_probe(probe).expect_err("numeric connective refuses");
    assert_eq!(
        err.to_string(),
        format!("pine evaluation refused: {refusal}"),
        "exact refusal for {construct}"
    );
    // Numbers in `if` conditions refuse instead of coercing.
    let err = run_probe("x = 0\nif 1\n    x := 9").expect_err("numeric condition refuses");
    assert_eq!(
        err.to_string(),
        "pine evaluation refused: 'if condition' needs a bool, got 1",
        "exact refusal for boolean positions"
    );
    // Destructuring a non-array call result refuses instead of binding a
    // partial tuple (the tuple-shape row's pinned arm).
    let err =
        run_probe("[a, b] = 42").expect_err("non-array destructure refuses");
    assert_eq!(
        err.to_string(),
        "pine evaluation refused: destructuring binds array values, got 42",
        "exact refusal for tuple shape"
    );
    // `ta.tr(true)` requests alternate `na` handling the bridge does not
    // serve.
    let err = run_probe("x = ta.tr(true)").expect_err("tr(true) refuses");
    assert_eq!(
        err.to_string(),
        "pine evaluation refused: ta.tr(true) requests alternate na handling, which is not bridged",
        "exact refusal for tr(true)"
    );
}

#[test]
fn freeze_refuse_probes_pin_strings() {
    let mut count = 0;
    for row in EGUI_PARITY {
        if row.verdict != ParityVerdict::Refuse {
            continue;
        }
        let (probe, refusal) = row
            .probe
            .zip(row.refusal)
            .expect("refuse row pins probe plus string");
        let err = run_probe(probe).expect_err("refuse probe fails");
        assert_eq!(
            err.to_string(),
            format!("pine evaluation refused: {refusal}"),
            "exact refusal for {}",
            row.construct
        );
        count += 1;
    }
    assert!(count >= 11, "refuse suite covers every Refuse row, got {count}");
}

/// Task-value gate: 1.0 accept over every must-run program, 1.0 refuse
/// over every must-refuse program. Counts print with the test so the
/// gate record names its coverage; both rates assert exactly.
#[test]
fn freeze_tally_full_accept_full_refuse() {
    let mut accept = 0_usize;
    let mut accept_total = 0_usize;
    let mut refuse = 0_usize;
    let mut refuse_total = 0_usize;
    let mut accept_names = Vec::new();
    let mut refuse_names = Vec::new();
    for row in EGUI_PARITY {
        match row.verdict {
            ParityVerdict::Match => {
                let Some(probe) = row.probe else {
                    assert!(
                        LINKED_ROWS.contains(&row.construct),
                        "unprobed Match row is declared linked"
                    );
                    continue;
                };
                accept_total += 1;
                if run_probe(probe).is_ok() {
                    accept += 1;
                } else {
                    accept_names.push(row.construct);
                }
            }
            ParityVerdict::Correct => {
                let probe = row.probe.expect("correct row carries a probe");
                // The var divergence needs a warmed run; every other
                // divergence probe runs clean on the 24-bar fixture
                // (value pins live in the dedicated tests above).
                accept_total += 1;
                let outcome = if row.construct == "var declaration" {
                    run_probe_warmup(probe, 2)
                } else {
                    run_probe(probe)
                };
                if outcome.is_ok() {
                    accept += 1;
                } else {
                    accept_names.push(row.construct);
                }
            }
            ParityVerdict::Refuse => {
                let (probe, refusal) = row
                    .probe
                    .zip(row.refusal)
                    .expect("refuse row pins probe plus string");
                refuse_total += 1;
                match run_probe(probe) {
                    Err(err)
                        if err.to_string()
                            == format!("pine evaluation refused: {refusal}") =>
                    {
                        refuse += 1;
                    }
                    _ => refuse_names.push(row.construct),
                }
            }
        }
    }
    // The Correct rows whose pinned behavior is a refusal ride the refuse
    // tally with their exact strings.
    for (probe, refusal) in [
        (
            "for i = 1.5 to 3\n    x = i",
            "'for start' needs an int, got 1.5",
        ),
        (
            "x = 1 and true",
            "operator 'and' needs bools on both sides, got 1 and true",
        ),
        (
            "x = 0\nif 1\n    x := 9",
            "'if condition' needs a bool, got 1",
        ),
        (
            "[a, b] = 42",
            "destructuring binds array values, got 42",
        ),
        (
            "x = ta.tr(true)",
            "ta.tr(true) requests alternate na handling, which is not bridged",
        ),
    ] {
        refuse_total += 1;
        match run_probe(probe) {
            Err(err)
                if err.to_string() == format!("pine evaluation refused: {refusal}") =>
            {
                refuse += 1;
            }
            _ => refuse_names.push(probe),
        }
    }
    assert!(
        accept_total > 0 && refuse_total > 0,
        "both suites non-empty: {accept_total} accept, {refuse_total} refuse"
    );
    assert!(
        accept_names.is_empty(),
        "must-accept programs failing: {accept_names:?}"
    );
    assert!(
        refuse_names.is_empty(),
        "must-refuse programs passing: {refuse_names:?}"
    );
    assert_eq!(accept, accept_total, "accept rate 1.0");
    assert_eq!(refuse, refuse_total, "refuse rate 1.0");
    println!("freeze tally: {accept}/{accept_total} accept, {refuse}/{refuse_total} refuse");
}

fn synthetic_module(publisher: &str, name: &str, text: &str) -> ModuleInput {
    let source = parse_pine_source(&format!("{publisher}/{name}"), text);
    let program = parse_program(text).expect("synthetic parses");
    ModuleInput::new(
        LibraryId {
            publisher: publisher.to_owned(),
            name: name.to_owned(),
            version: 1,
        },
        source,
        program,
    )
}

fn linked(modules: Vec<ModuleInput>) -> Vec<LinkedModule> {
    let graph = resolve_imports(modules).expect("synthetic closure resolves");
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

#[test]
fn freeze_imported_rows_run_linked() {
    // The library owns the constant; the driver shadows it. A method body
    // that resolved under the caller would add the driver's `K`.
    let lib = synthetic_module(
        "local",
        "L",
        "export const int K = 41\nexport type Box\n    float k = 0\nexport method read(Box this) =>\n    this.k + K",
    );
    let driver = synthetic_module(
        "local",
        "Driver",
        "import local/L/1 as L\nK = 1000\nbag = L.Box.new()\nbag.k := 1\nx = bag.read()\ny = bag.k",
    );
    let modules = linked(vec![lib, driver]);
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    let outcome = engine
        .run_linked(&modules, &bars24(), InputSet::new())
        .expect("imported construction runs");
    // Imported-type construction binds a live instance with writable fields.
    assert_pin(&outcome, "y", Pin::Num(1.0), "imported type construction");
    // The method body reads the bare `K` through the live caller frame, so
    // the driver's shadowing global wins over the owner's constant: this
    // pins the caller-reads half of the scope row (the corrected standing).
    assert_pin(&outcome, "x", Pin::Num(1001.0), "method body scope reads caller");
}

#[test]
fn freeze_lower_tf_serves_injected_columns() {
    let row = parity_for("request.security_lower_tf (injected)").expect("lower-tf row known");
    assert_eq!(row.verdict, ParityVerdict::Match, "lower-tf stands");
    let program = parse_program(
        "x = request.security_lower_tf(\"SYM\", \"5\", [close], calc_bars_count = 3)",
    )
    .expect("lower-tf probe parses");
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine
        .inject_lower_tf(
            "SYM",
            "5",
            LtfDataset {
                times: vec![1, 2, 3, 4, 5],
                opens: vec![1.0; 5],
                highs: vec![1.0; 5],
                lows: vec![1.0; 5],
                closes: vec![10.0, 20.0, 30.0, 40.0, 50.0],
                volumes: vec![1.0; 5],
            },
        )
        .expect("dataset injects");
    let outcome = engine
        .run(&program, &bars24(), InputSet::new())
        .expect("injected lower-tf serves");
    // One requested leg yields one column: the last three closes.
    match cell(&outcome, "x") {
        Some(crate::PineValue::Array(columns)) => {
            assert_eq!(columns.len(), 1, "one column served");
            match &columns[0] {
                Some(crate::PineValue::Array(rows)) => {
                    let got: Vec<Option<f64>> = rows
                        .iter()
                        .map(|slot| match slot {
                            Some(crate::PineValue::Float(v)) => Some(*v),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(
                        got,
                        vec![Some(30.0), Some(40.0), Some(50.0)],
                        "tail-taken closes served"
                    );
                }
                other => panic!("column reads an array, got {other:?}"),
            }
        }
        other => panic!("lower-tf serves an array of columns, got {other:?}"),
    }
}
