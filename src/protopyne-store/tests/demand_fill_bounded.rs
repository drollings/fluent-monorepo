//! Bounded demand fill: spans wider than the bound, or outside the
//! provider's horizon, are refused with a named reason — never silently
//! truncated into a servable-looking prefix.
//!
//! Three refusals, three names: a cold symbol's span past
//! `max_repair_span_ms` names the bound (and never touches the provider); a
//! span no horizon covers names the horizon; a span partially covered serves
//! its subset and the seed rule names the shortfall loudly.

use super::test_history as th;

use crate::coverage::FreshnessPolicy;
use crate::error::StoreError;
use crate::store::CandleStore;

fn tight_config() -> crate::repair::RepairConfig {
    crate::repair::RepairConfig::new()
        .max_repair_span_ms(60_000)
        .repair_concurrency(1)
        .backoff_base_ms(1)
        .backoff_max_shift(0)
        .backoff_ceiling_ms(1)
        .build()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn span_past_max_repair_width_is_refused_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (handle, permit, caps) = th::open_file_handle(&dir, "store");
    fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            th::register_m1(&handle, &permit, "SPY");
        })
        .await;
    // A provider that counts its fetches: the refusal must land before any.
    let scripted = std::sync::Arc::new(th::ScriptedProvider::new(
        None,
        th::ScriptedFetch::Bars(Vec::new()),
    ));
    let provider: std::sync::Arc<dyn crate::history::HistoryProvider> = scripted.clone();
    let err = fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            handle
                .bars_filled(
                    &permit,
                    "SPY",
                    th::VENUE,
                    protopyne_types::Timeframe::M1,
                    Some(0),
                    Some(1_000),
                    2,
                    60_000_000,
                    FreshnessPolicy::new().build(),
                    &tight_config(),
                    provider.as_ref(),
                )
                .await
                .expect_err("a thousand-bar span past a one-bar bound must refuse")
        })
        .await;
    assert!(
        matches!(err, StoreError::History(ref detail) if detail.contains("max_repair_span_ms")),
        "the refusal names the bound: {err}"
    );
    assert_eq!(scripted.fetch_count(), 0, "refused before fetching");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn span_outside_every_horizon_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_bars = th::ascending_bars(0, 6);
    let csv = th::write_tape_csv(&dir, "spy.csv", &file_bars);
    let (handle, permit, caps) = th::open_file_handle(&dir, "store");
    let key = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            th::register_m1(&handle, &permit, "SPY")
        })
        .await;
    let provider: std::sync::Arc<dyn crate::history::HistoryProvider> = std::sync::Arc::new(
        crate::history::FileHistory::new(csv, key, th::server_parse).expect("provider reads"),
    );
    // The whole want sits past the file's last bar: neither the bound nor
    // the horizon can serve any of it.
    let start = file_bars.last().expect("nonempty").ts.timestamp_millis() + 600_000;
    let err = fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            handle
                .bars_filled(
                    &permit,
                    "SPY",
                    th::VENUE,
                    protopyne_types::Timeframe::M1,
                    Some(start),
                    Some(3),
                    2,
                    start + 600_000,
                    FreshnessPolicy::new().build(),
                    &th::test_config(),
                    provider.as_ref(),
                )
                .await
                .expect_err("a span past every horizon must refuse")
        })
        .await;
    assert!(
        matches!(err, StoreError::History(ref detail) if detail.contains("horizon")),
        "the refusal names the horizon: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partial_horizon_serves_its_subset_and_names_the_shortfall() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_bars = th::ascending_bars(0, 6);
    let csv = th::write_tape_csv(&dir, "spy.csv", &file_bars);
    let (handle, permit, caps) = th::open_file_handle(&dir, "store");
    let key = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            th::register_m1(&handle, &permit, "SPY")
        })
        .await;
    let provider: std::sync::Arc<dyn crate::history::HistoryProvider> = std::sync::Arc::new(
        crate::history::FileHistory::new(csv, key, th::server_parse).expect("provider reads"),
    );
    // The want reaches past the file's horizon: the servable subset lands,
    // and the seed rule — not silence — reports the six-bar shortfall
    // against a warmup of eight.
    let step3 = file_bars[3].ts.timestamp_millis();
    let err = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            handle
                .bars_filled(
                    &permit,
                    "SPY",
                    th::VENUE,
                    protopyne_types::Timeframe::M1,
                    Some(step3),
                    Some(5),
                    8,
                    th::now_after(&file_bars),
                    FreshnessPolicy::new().build(),
                    &th::test_config(),
                    provider.as_ref(),
                )
                .await
                .expect_err("six servable bars miss warmup eight")
        })
        .await;
    assert!(
        matches!(
            err,
            StoreError::Plan(crate::error::PlanError::UncoveredRange { .. })
        ),
        "the shortfall is a named miss, not a truncation: {err}"
    );
    let coverage = fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            handle
                .coverage(key)
                .expect("coverage reads")
                .expect("the subset landed")
        })
        .await;
    assert_eq!(coverage.bars, 6, "the servable subset was stored");
}
