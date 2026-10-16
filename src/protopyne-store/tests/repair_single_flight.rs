//! Concurrent repair idempotence: the property that replaces the per-symbol
//! lock.
//!
//! Two concurrent `run_once` calls over one plan produce exactly one
//! `Repaired` and one `AlreadyComplete` — through single-flight when the
//! passes overlap, through `InsertIfAbsent` idempotence when they do not —
//! and the committed series is byte-identical to a single-pass run.

use super::test_history as th;

use crate::coverage::FreshnessPolicy;
use crate::range::TimeRange;
use crate::repair::{RecoveryCoordinator, RepairStatus};

fn full_series(
    handle: &crate::tape::MarketTape,
    permit: &crate::capability::StoreStorage,
    first_ms: i64,
    end_ms: i64,
) -> Vec<protopyne_types::Bar> {
    let request = crate::store::ReadRequest {
        symbol: "SPY".to_owned(),
        venue: th::VENUE.to_owned(),
        timeframe: protopyne_types::Timeframe::M1,
        range: TimeRange::bounded(first_ms, end_ms).expect("bounded"),
        limit: None,
        policy: crate::fold::FoldPolicy::ExtendToFullBuckets,
    };
    handle
        .read_scoped(permit, &request)
        .expect("full read resolves")
        .bars
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_passes_yield_one_repair_and_one_complete() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_bars = th::ascending_bars(0, 12);
    let csv = th::write_tape_csv(&dir, "spy.csv", &file_bars);
    let (handle, permit, caps) = th::open_file_handle(&dir, "store");
    // A torn tape: steps 5, 6, 7 missing, ends current.
    let mut held: Vec<protopyne_types::Bar> = file_bars[..5].to_vec();
    held.extend_from_slice(&file_bars[8..]);
    let (key, first_ms, end_ms) = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            let key = th::register_m1(&handle, &permit, "SPY");
            th::seed_append(&handle, &permit, key, &held);
            let first = file_bars.first().expect("nonempty").ts.timestamp_millis();
            let end = file_bars
                .last()
                .expect("nonempty")
                .ts
                .timestamp_millis()
                .saturating_add(60_000);
            (key, first, end)
        })
        .await;
    let provider: std::sync::Arc<dyn crate::history::HistoryProvider> = std::sync::Arc::new(
        crate::history::FileHistory::new(csv.clone(), key, th::server_parse)
            .expect("provider reads"),
    );
    let handle = std::sync::Arc::new(handle);
    let coordinator = std::sync::Arc::new(
        RecoveryCoordinator::new(
            std::sync::Arc::clone(&handle),
            vec![(key.symbol_id, provider)],
            th::test_config(),
        )
        .expect("valid config"),
    );
    let want = TimeRange::bounded(first_ms, end_ms).expect("bounded");
    let now = th::now_after(&file_bars);
    let plan = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            coordinator
                .plan(&permit, &[(key, want)], now, FreshnessPolicy::new().build())
                .expect("plan builds")
        })
        .await;
    assert_eq!(plan.len(), 1, "the torn tape plans one repair");

    let (first_pass, second_pass) = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            let left = coordinator.run_once(&permit, &plan);
            let right = coordinator.run_once(&permit, &plan);
            tokio::join!(left, right)
        })
        .await;
    let mut statuses: Vec<RepairStatus> = [first_pass, second_pass]
        .into_iter()
        .map(|reports| {
            assert_eq!(reports.len(), 1);
            reports[0].status
        })
        .collect();
    statuses.sort_by_key(|status| *status as u8);
    assert_eq!(
        statuses,
        vec![RepairStatus::Repaired, RepairStatus::AlreadyComplete],
        "exactly one repair and one no-op"
    );

    let series = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            full_series(&handle, &permit, first_ms, end_ms)
        })
        .await;
    assert_eq!(series, file_bars, "the tape holds the whole file");

    // The single-pass reference: a second store, one pass, same plan shape.
    // The concurrent run must commit byte-identically to it.
    let (solo_handle, solo_permit, solo_caps) = th::open_file_handle(&dir, "solo");
    let solo_series = fluent_wvr::CURRENT_CAPS
        .scope(solo_caps.clone(), async {
            let solo_key = th::register_m1(&solo_handle, &solo_permit, "SPY");
            th::seed_append(&solo_handle, &solo_permit, solo_key, &held);
            let solo_provider: std::sync::Arc<dyn crate::history::HistoryProvider> =
                std::sync::Arc::new(
                    crate::history::FileHistory::new(csv, solo_key, th::server_parse)
                        .expect("provider reads"),
                );
            let solo_handle = std::sync::Arc::new(solo_handle);
            let solo = RecoveryCoordinator::new(
                std::sync::Arc::clone(&solo_handle),
                vec![(solo_key.symbol_id, solo_provider)],
                th::test_config(),
            )
            .expect("valid config");
            let solo_plan = solo
                .plan(
                    &solo_permit,
                    &[(solo_key, want)],
                    now,
                    FreshnessPolicy::new().build(),
                )
                .expect("plan builds");
            let reports = solo.run_once(&solo_permit, &solo_plan).await;
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].status, RepairStatus::Repaired);
            full_series(&solo_handle, &solo_permit, first_ms, end_ms)
        })
        .await;
    assert_eq!(
        series, solo_series,
        "concurrent and solo passes commit identically"
    );
}
