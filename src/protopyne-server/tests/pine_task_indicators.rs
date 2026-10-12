//! Headless end-to-end proof for the task indicators over the real
//! histories: PPAS (`ApexPPAS`) and RSIMD (`ApexRSIMD`), each linked through
//! its task closure and run bar-by-bar over `SPY_1M.csv` (436 bars),
//! `SPY_5M.csv` (346 bars), and `SPY_15M.csv` (116 bars) with default
//! inputs.
//!
//! Regimes (cost-driven, each the honest-minimal injection set for the
//! path the indicator evaluates):
//!
//! * PPAS runs full histories with no injections: its only `fetch` is
//!   chart-scoped (chart legs, no pair needed). Full-history PPAS is
//!   quarantined by human verdict (2026-10-12, see below): every full
//!   history reaches the version-skewed `libCandles.isPatternType`
//!   reference once ATR warms and a reversal prints, so no PPAS golden is
//!   checked in. The suite pins the exact refusal per timeframe instead,
//!   extending the kernels refusal pin to full-history scale; the
//!   executable PPAS proof stays the five-bar clear-regime contract plus
//!   this pin.
//! * RSIMD runs full histories with no injections: divergence detection
//!   runs on chart legs and it calls no `fetch` at all.
//!
//! DVP (`DeltaVolumeProfileProtopyne`) runs no interpreted suite here by
//! human verdict (2026-10-12: prune every PineScript DVP run, keep native
//! Rust only): its numeric legs stay covered by the registered-kernel
//! suites, the native-provider agreement tests, and the kernel-against-
//! reference expression checks, without end-to-end interpretation or
//! checked-in goldens.
//!
//! Goldens: `tests/data/pine_task_rsimd_<1m,5m,15m>.json` hold the slim
//! canonical dump (pinned series plus every capture) in the exact shared
//! dialect (`common_golden`), beside the untouched
//! `pine_integration_lite_*.json`. Set `PROTOPYNE_RECORD=1` to regenerate
//! after an intentional change; a missing file fails proving nothing.
//! Calibration from the look-ahead and drift suites must already be green —
//! no golden is checked in over a red gate.

use std::path::PathBuf;

use protopyne_kernels::{InputSet, RunOutcome};
use protopyne_types::Bar;

use super::common_golden::{check_golden, dump_canonical};
use super::common_mtf::{WARMUP, data_root, engine_for, link_task_closure, load_history};

/// Golden file for one indicator and chart timeframe.
fn golden_path(indicator: &str, timeframe: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(format!("pine_task_{indicator}_{timeframe}.json"))
}

/// Link PPAS once and run it over one chart history with no injections:
/// its only `fetch` is chart-scoped. Full histories reach the skewed
/// pattern reference (quarantined — no PPAS golden is blessed).
fn run_ppas(chart: &str, bars: &[Bar]) -> Result<RunOutcome, protopyne_kernels::KernelError> {
    let modules = link_task_closure("ApexPPAS");
    let mut engine = engine_for(chart);
    engine.run_linked(&modules, bars, InputSet::new())
}

/// Fail-closed pin for full-history PPAS (quarantine verdict,
/// human-approved 2026-10-12): once ATR warms and a reversal prints, the
/// pattern branch names `libCandles.isPatternType`, which the on-disk v21
/// `libCandles` does not export. The run refuses with the exact pinned
/// reason instead of resolving across the alias; no golden is blessed
/// over this path. The executable PPAS proof stays the five-bar
/// clear-regime contract (kernels suite) plus this pin.
fn prove_ppas_refuses(chart: &str, bars: &[Bar], label: &str) {
    let reason = run_ppas(chart, bars)
        .expect_err("skewed pattern reference refuses at full history")
        .to_string();
    assert_eq!(
        reason,
        "pine evaluation refused: unknown function 'libCandles.isPatternType'",
        "{label}: exact refusal pins the missing export"
    );
}

/// Link RSIMD once and run it over one chart history with no injections:
/// divergence detection runs on chart legs; it calls no `fetch`.
fn run_rsimd(chart: &str, bars: &[Bar]) -> RunOutcome {
    let modules = link_task_closure("ApexRSIMD");
    let mut engine = engine_for(chart);
    engine
        .run_linked(&modules, bars, InputSet::new())
        .expect("RSIMD evaluates with zero refusals")
}

/// Plot titles in capture order.
fn plot_titles(outcome: &RunOutcome) -> Vec<&str> {
    outcome
        .plots
        .iter()
        .map(|plot| plot.title.as_str())
        .collect()
}

/// Structural proof shared by every task run: every non-warmup bar
/// evaluates with one snapshot, and an identical second run is
/// byte-identical (rerun determinism, in memory — the golden pins the
/// bytes on disk).
fn prove_task(
    outcome: &RunOutcome,
    rerun: impl FnOnce() -> RunOutcome,
    bars: &[Bar],
    label: &str,
) {
    let expect_evaluated = bars.len() - WARMUP as usize;
    assert_eq!(
        outcome.evaluated, expect_evaluated,
        "{label}: every non-warmup bar evaluates"
    );
    assert_eq!(
        outcome.snapshots.len(),
        expect_evaluated,
        "{label}: one snapshot per evaluated bar"
    );
    assert_eq!(
        dump_canonical(&rerun()),
        dump_canonical(outcome),
        "{label}: rerun over identical inputs is byte-identical"
    );
}

const RSIMD_SERIES: &[&str] = &["rsi_val", "trend_health"];
/// (no golden) — pins the exact full-history refusal instead.
#[test]
fn ppas_full_history_refuses_on_1m() {
    let bars = load_history("SPY_1M.csv");
    assert_eq!(bars.len(), 436, "1M fixture row count pinned");
    prove_ppas_refuses("1", &bars, "PPAS 1m");
}

/// PPAS over the full 5-minute history with no injections: quarantined
/// (no golden) — pins the exact full-history refusal instead.
#[test]
fn ppas_full_history_refuses_on_5m() {
    let bars = load_history("SPY_5M.csv");
    assert_eq!(bars.len(), 346, "5M fixture row count pinned");
    prove_ppas_refuses("5", &bars, "PPAS 5m");
}

/// PPAS over the full 15-minute history with no injections: quarantined
/// (no golden) — pins the exact full-history refusal instead.
#[test]
fn ppas_full_history_refuses_on_15m() {
    let bars = load_history("SPY_15M.csv");
    assert_eq!(bars.len(), 116, "15M fixture row count pinned");
    prove_ppas_refuses("15", &bars, "PPAS 15m");
}

/// Tick-accumulation surface a closed-bar indicator must not touch: the
/// `Bars` accumulator objects, the lower-timeframe pull legs, and the
/// realtime gate. A future reference fails by token name.
const TICK_SURFACE_TOKENS: &[&str] = &["Bars.", "security_lower_tf", "isrealtime"];

/// Strip `//` line comments (string literals stay: a token inside a
/// string still counts — loud by design, diagnosed by token name).
fn code_without_line_comments(text: &str) -> String {
    text.lines()
        .map(|line| line.split("//").next().unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// PPAS is closed-bar-only: its file references no tick-accumulation
/// surface, so no tick path exists to prove — recorded here instead of
/// forced. (Its only `fetch` is chart-scoped; the linked libraries'
/// lower-timeframe legs stay unreached on this path.)
#[test]
fn ppas_closed_bars_only_no_tick_path() {
    let path = data_root().join("indicators/_ApexPPAS.pine");
    let text = std::fs::read_to_string(&path).expect("PPAS loads");
    let code = code_without_line_comments(&text);
    for token in TICK_SURFACE_TOKENS {
        assert!(
            !code.contains(token),
            "PPAS references no tick surface {token}"
        );
    }
}

/// RSIMD is closed-bar-only: its file references no tick-accumulation
/// surface, so no tick path exists to prove — recorded here instead of
/// forced. (It calls no `fetch` at all; divergence runs on chart legs.)
#[test]
fn rsimd_closed_bars_only_no_tick_path() {
    let path = data_root().join("indicators/_ApexRSIMD.pine");
    let text = std::fs::read_to_string(&path).expect("RSIMD loads");
    let code = code_without_line_comments(&text);
    for token in TICK_SURFACE_TOKENS {
        assert!(
            !code.contains(token),
            "RSIMD references no tick surface {token}"
        );
    }
}

/// RSIMD over the full 1-minute history with no injections.
#[test]
fn rsimd_runs_clean_on_1m() {
    let bars = load_history("SPY_1M.csv");
    assert_eq!(bars.len(), 436, "1M fixture row count pinned");
    let outcome = run_rsimd("1", &bars);
    prove_task(&outcome, || run_rsimd("1", &bars), &bars, "RSIMD 1m");
    assert_eq!(
        plot_titles(&outcome),
        vec![
            "RSI",
            "plot:1",
            "Bullish RSI HL",
            "Bearish RSI LH",
            "Bullish Price LL",
            "Bullish Price LL BG",
            "Bearish Price HH",
            "Bearish Price HH BG",
            "Trend Health",
        ],
        "RSIMD captures all nine plot sites"
    );
    check_golden(&golden_path("rsimd", "1m"), &outcome, RSIMD_SERIES);
}

/// RSIMD over the full 5-minute history with no injections.
#[test]
fn rsimd_runs_clean_on_5m() {
    let bars = load_history("SPY_5M.csv");
    assert_eq!(bars.len(), 346, "5M fixture row count pinned");
    let outcome = run_rsimd("5", &bars);
    prove_task(&outcome, || run_rsimd("5", &bars), &bars, "RSIMD 5m");
    assert_eq!(outcome.plots.len(), 9, "RSIMD captures all nine plot sites");
    check_golden(&golden_path("rsimd", "5m"), &outcome, RSIMD_SERIES);
}

/// RSIMD over the full 15-minute history with no injections.
#[test]
fn rsimd_runs_clean_on_15m() {
    let bars = load_history("SPY_15M.csv");
    assert_eq!(bars.len(), 116, "15M fixture row count pinned");
    let outcome = run_rsimd("15", &bars);
    prove_task(&outcome, || run_rsimd("15", &bars), &bars, "RSIMD 15m");
    assert_eq!(outcome.plots.len(), 9, "RSIMD captures all nine plot sites");
    check_golden(&golden_path("rsimd", "15m"), &outcome, RSIMD_SERIES);
}

