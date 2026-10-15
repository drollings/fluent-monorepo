//! On-demand fill: the seed rule, both directions.
//!
//! A cached window shorter than `warmup` is a miss, never merged; exactly
//! `warmup` is a hit. The hit case doubles as the `trim(adjust(x)) == x` pin
//! for the fill path: the fetch widens by the headroom, the answer narrows
//! back to precisely the asked window.

use super::test_history as th;

use crate::coverage::FreshnessPolicy;
use crate::error::{PlanError, StoreError};

/// Fill `limit` bars at `start_step` with `warmup`, against a file holding
/// `file_first..file_first + file_len` and a store holding
/// `store_from..store_from + store_len` (all step counts; an empty store is
/// a registered-but-unfilled tape).
async fn fill(
    file_first: i64,
    file_len: usize,
    store_from: i64,
    store_len: usize,
    start_step: i64,
    limit: usize,
    warmup: usize,
) -> Result<Vec<protopyne_types::Bar>, StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_bars = th::ascending_bars(file_first, file_len);
    let csv = th::write_tape_csv(&dir, "spy.csv", &file_bars);
    let (handle, permit, caps) = th::open_file_handle(&dir, "store.db").await;
    let offset = (store_from - file_first).max(0) as usize;
    let store_bars: Vec<protopyne_types::Bar> =
        file_bars[offset..(offset + store_len).min(file_bars.len())].to_vec();
    let key = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            let key = th::register_m1(&handle, &permit, "SPY").await;
            th::seed_append(&handle, &permit, key, &store_bars).await;
            key
        })
        .await;
    let provider: std::sync::Arc<dyn crate::history::HistoryProvider> = std::sync::Arc::new(
        crate::history::FileHistory::new(csv, key, th::server_parse).expect("provider reads"),
    );
    let start = th::ascending_bars(start_step, 1)
        .pop()
        .expect("one bar")
        .ts
        .timestamp_millis();
    fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            handle
                .bars_filled(
                    &permit,
                    "SPY",
                    th::VENUE,
                    protopyne_types::Timeframe::M1,
                    Some(start),
                    Some(limit),
                    warmup,
                    th::now_after(&file_bars),
                    FreshnessPolicy::new().build(),
                    &th::test_config(),
                    provider.as_ref(),
                )
                .await
        })
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fill_returns_precisely_the_asked_window() {
    // File 4..16, store 10..16: the headroom fills whole, seven bars serve
    // a warmup of six, and the answer narrows to the two asked bars.
    let bars = fill(4, 13, 10, 7, 10, 2, 6)
        .await
        .expect("seven cached bars meet warmup six");
    let asked: Vec<protopyne_types::Bar> = th::ascending_bars(10, 2);
    assert_eq!(bars, asked, "trim(adjust(x)) == x for the fill path");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exactly_warmup_bars_is_a_hit() {
    // File 4..10, store only step 10: seven bars serve a warmup of seven.
    // The tail past the tape end is staleness answered short, not a silent
    // truncation — alignment holds because the headroom is whole.
    let bars = fill(4, 7, 10, 1, 10, 2, 7)
        .await
        .expect("seven cached bars meet warmup seven");
    let asked: Vec<protopyne_types::Bar> = th::ascending_bars(10, 1);
    assert_eq!(bars, asked);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn warmup_minus_one_bars_is_a_miss() {
    // File 3..9, empty store: the leading edge fills whole, but seven
    // served bars miss a warmup of eight — refused loudly with the
    // shortfall named rather than merged into a half-warmed answer.
    let err = fill(3, 7, 3, 0, 10, 2, 8)
        .await
        .expect_err("seven cached bars miss warmup eight");
    assert!(
        matches!(err, StoreError::Plan(PlanError::UncoveredRange { .. })),
        "the miss names its shortfall: {err}"
    );
}
