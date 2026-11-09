//! Headless fail-closed proof for version-skewed cross-module references,
//! plus the directory-driven closure verdict over the live data root.
//!
//! The external indicator files this suite once proved (`ApexPPAS`,
//! `ApexRSIMD`) were removed by an upstream sync of the git-ignored data
//! directory, and an interpreted run cannot be fixture-pinned without the
//! script bytes — so the executable half of this suite is synthetic:
//! a driver calling a function its library does not export refuses with
//! the exact pinned reason at full-history scale, on every timeframe.
//! The fail-closed shape (unknown cross-module reference refuses instead
//! of resolving across the alias) is what the engine guarantees; the
//! script bytes were only ever its vehicle.
//!
//! DVP (`DeltaVolumeProfileProtopyne`) runs no interpreted suite here by
//! human verdict (2026-10-12: prune every PineScript DVP run, keep native
//! Rust only): its numeric legs stay covered by the registered-kernel
//! suites, the native-provider agreement tests, and the kernel-against-
//! reference expression checks, without end-to-end interpretation or
//! checked-in goldens.
//!
//! No test here names a file under `data/protopyne-rs/indicators/`:
//! adding, removing, or renaming a script changes no outcome in this file.

use std::path::PathBuf;

use protopyne_kernels::{InputSet, LibraryId, LinkedModule, ModuleInput};

mod csv {
    pub use protopyne_server::csv::load_bars_csv;
}
#[path = "common/mtf.rs"]
mod common_mtf;
use common_mtf::{engine_for, linked_from_graph, load_history, resolve_inputs};

/// A library that does not export `isPatternType` (the shape of the
/// on-disk `libCandles` the quarantined reference once resolved against).
const SKEWED_LIB: &str = "//@version=6\nlibrary(\"libCandles\")\nexport helper() => 1\n";

/// A driver calling the missing export, then plotting the result so the
/// reference sits on the evaluation path on every bar.
const SKEWED_DRIVER: &str = "import showmethegrail/libCandles/11 as libCandles\n\
     x = libCandles.isPatternType(close)\n\
     plot(x)\n";

/// Synthetic skewed closure: the importer pins an older version than the
/// library declares, exactly like the quarantined reference did. Versions
/// are metadata only — the refusal below is about the missing export,
/// never the skew.
fn skew_modules() -> Vec<LinkedModule> {
    let lib_source = protopyne_kernels::parse_pine_source("showmethegrail/libCandles", SKEWED_LIB);
    let lib_program = protopyne_kernels::parse_program(SKEWED_LIB).expect("library parses");
    let driver_source = protopyne_kernels::parse_pine_source("local/Driver", SKEWED_DRIVER);
    let driver_program = protopyne_kernels::parse_program(SKEWED_DRIVER).expect("driver parses");
    linked_from_graph(&resolve_inputs(vec![
        ModuleInput::new(
            LibraryId {
                publisher: "showmethegrail".to_owned(),
                name: "libCandles".to_owned(),
                version: 21,
            },
            lib_source,
            lib_program,
        ),
        ModuleInput::new(
            LibraryId {
                publisher: "local".to_owned(),
                name: "Driver".to_owned(),
                version: 1,
            },
            driver_source,
            driver_program,
        ),
    ]))
}

/// Fail-closed pin: the skewed pattern reference refuses with the exact
/// pinned reason instead of resolving across the alias. Runs at
/// full-history scale on every timeframe; no golden is blessed over a
/// refusing path.
fn prove_skewed_reference_refuses(chart: &str, bars: &[protopyne_types::Bar], label: &str) {
    let modules = skew_modules();
    let mut engine = engine_for(chart);
    let reason = engine
        .run_linked(&modules, bars, InputSet::new())
        .expect_err("skewed reference refuses at full history")
        .to_string();
    assert_eq!(
        reason, "pine evaluation refused: unknown function 'libCandles.isPatternType'",
        "{label}: exact refusal pins the missing export"
    );
}

/// Skewed reference over the full 1-minute history.
#[test]
fn skewed_reference_refuses_on_1m() {
    let bars = load_history("SPY_1M.csv");
    assert_eq!(bars.len(), 436, "1M fixture row count pinned");
    prove_skewed_reference_refuses("1", &bars, "skew 1m");
}

/// Skewed reference over the full 5-minute history.
#[test]
fn skewed_reference_refuses_on_5m() {
    let bars = load_history("SPY_5M.csv");
    assert_eq!(bars.len(), 346, "5M fixture row count pinned");
    prove_skewed_reference_refuses("5", &bars, "skew 5m");
}

/// Skewed reference over the full 15-minute history.
#[test]
fn skewed_reference_refuses_on_15m() {
    let bars = load_history("SPY_15M.csv");
    assert_eq!(bars.len(), 116, "15M fixture row count pinned");
    prove_skewed_reference_refuses("15", &bars, "skew 15m");
}

/// Directory-driven verdict over the live data root: every indicator file
/// on disk either discovers to a linkable closure or refuses with a
/// reason. The fixture suite pins exact behavior; this probe only guards
/// the shape, so directory churn changes no outcome here.
#[test]
fn live_indicators_link_or_refuse_with_a_reason() {
    use protopyne_kernels::manifest_files::load_pine_sources;
    use protopyne_kernels::{closure_inputs, discover_closures, ClosureOutcome};
    let live = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs");
    let (indicators, libraries) = load_pine_sources(&live).expect("live root loads");
    let outcomes = discover_closures(&indicators, &libraries);
    assert_eq!(
        outcomes.len(),
        indicators.len(),
        "one verdict per indicator file on disk"
    );
    for outcome in &outcomes {
        match outcome {
            ClosureOutcome::Linked(closure) => {
                let inputs = closure_inputs(closure).expect("linked closure parses");
                resolve_inputs(inputs);
            }
            ClosureOutcome::Refused { stem, reason, .. } => {
                assert!(!reason.is_empty(), "{stem} refuses with a reason");
            }
        }
    }
}
