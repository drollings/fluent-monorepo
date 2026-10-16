//! A tape-backed leg rides the multi-symbol harness without perturbing its
//! per-symbol reproducibility pins.
//!
//! One symbol reads a durable tape, another replays a dynamic series. Each
//! leg pins its own manifest, and two runs over the same inputs are
//! byte-identical — the tape leg carries a digest and the dynamic leg does
//! not, and that difference never reaches a verdict.

use std::path::PathBuf;
use std::sync::Arc;

use crate::backtest::HistorySource;
use crate::common_tape as tape;
use crate::runner::{MultiSymbolInput, run_multi_backtest};
use protopyne_store::CandleStore;
use protopyne_types::{Bar, Timeframe};

fn csv_bars(name: &str) -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(name);
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
}

async fn run_once() -> crate::runner::MultiBacktestRun {
    let bars = csv_bars("SPY_15M.csv");
    let caps = tape::caps();
    let (store, key, request) = tape::seed(&bars, "SPY", Timeframe::M15, &caps);
    let store: Arc<dyn CandleStore> = Arc::new(store);

    let strategy = tape::strategy();
    let risk = tape::risk();
    let grant = tape::grant("SPY");

    let spy = MultiSymbolInput::new()
        .venue_config(tape::venue_config("SPY", "spy-15m-tape"))
        .runner_config(tape::runner_config("SPY"))
        .history(HistorySource::Tape { key, request })
        .store(Arc::clone(&store))
        .build();
    let qqq_bars = csv_bars("SPY_5M.csv");
    let qqq = MultiSymbolInput::new()
        .venue_config(tape::venue_config("QQQ", "qqq-synth-v1"))
        .runner_config(tape::runner_config("QQQ"))
        .history(HistorySource::Dynamic(qqq_bars))
        .build();

    fluent_wvr::CURRENT_CAPS
        .scope(caps, async move {
            run_multi_backtest(&strategy, &risk, &grant, tape::paper(), vec![spy, qqq], 11)
                .await
                .expect("multi-symbol run")
        })
        .await
}

#[tokio::test]
async fn a_tape_leg_keeps_the_multi_symbol_pins() {
    let first = run_once().await;
    let second = run_once().await;
    assert_eq!(
        first.manifest.to_canonical_json(),
        second.manifest.to_canonical_json(),
        "the same inputs must pin byte-identical manifests"
    );
    assert_eq!(
        serde_json::to_string(&first).expect("run serializes"),
        serde_json::to_string(&second).expect("run serializes"),
        "per-symbol runs are deterministic end to end"
    );
    // The tape leg pins provenance; the dynamic leg pins none.
    assert!(
        first.manifest.symbols[0].data_digest.is_some(),
        "the store-backed leg pins a digest"
    );
    assert!(
        first.manifest.symbols[1].data_digest.is_none(),
        "the dynamic leg pins no tape digest"
    );
    assert_eq!(first.runs.len(), 2, "one run per symbol");
}
