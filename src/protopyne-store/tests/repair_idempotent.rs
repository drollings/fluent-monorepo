//! Repair idempotence: filling the same span twice stores once.
//!
//! Repair a hole, then repair again over the same span: the second pass
//! reports `AlreadyComplete` with `inserted == 0`, and the series is
//! unchanged. Re-delivery is an observable conflict count, never a silent
//! overwrite — which is what makes the concurrent case safe too.

use super::test_history as th;

use crate::coverage::FreshnessPolicy;
use crate::range::TimeRange;
use crate::repair::{RecoveryCoordinator, RepairStatus};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_repair_over_the_same_span_stores_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_bars = th::ascending_bars(0, 10);
    let csv = th::write_tape_csv(&dir, "spy.csv", &file_bars);
    let (handle, permit, caps) = th::open_file_handle(&dir, "store");
    // Steps 4 and 5 missing from an otherwise current tape.
    let mut held: Vec<protopyne_types::Bar> = file_bars[..4].to_vec();
    held.extend_from_slice(&file_bars[6..]);
    let handle = std::sync::Arc::new(handle);
    let key = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            let key = th::register_m1(&handle, &permit, "SPY");
            th::seed_append(&handle, &permit, key, &held);
            key
        })
        .await;
    let provider: std::sync::Arc<dyn crate::history::HistoryProvider> = std::sync::Arc::new(
        crate::history::FileHistory::new(csv, key, th::server_parse).expect("provider reads"),
    );
    let coordinator = RecoveryCoordinator::new(
        std::sync::Arc::clone(&handle),
        vec![(key.symbol_id, provider)],
        th::test_config(),
    )
    .expect("valid config");
    let first = file_bars.first().expect("nonempty").ts.timestamp_millis();
    let end = file_bars
        .last()
        .expect("nonempty")
        .ts
        .timestamp_millis()
        .saturating_add(60_000);
    let want = TimeRange::bounded(first, end).expect("bounded");
    let now = th::now_after(&file_bars);

    let read_all = || {
        let request = crate::store::ReadRequest {
            symbol: "SPY".to_owned(),
            venue: th::VENUE.to_owned(),
            timeframe: protopyne_types::Timeframe::M1,
            range: TimeRange::bounded(first, end).expect("bounded"),
            limit: None,
            policy: crate::fold::FoldPolicy::ExtendToFullBuckets,
        };
        let handle = std::sync::Arc::clone(&handle);
        let permit = permit.clone();
        async move {
            handle
                .read_scoped(&permit, &request)
                .expect("read resolves")
                .bars
        }
    };

    let plan = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            coordinator
                .plan(&permit, &[(key, want)], now, FreshnessPolicy::new().build())
                .expect("plan builds")
        })
        .await;
    assert_eq!(plan.len(), 1);

    // First pass fills the hole.
    let first_reports = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            coordinator.run_once(&permit, &plan).await
        })
        .await;
    assert_eq!(first_reports.len(), 1);
    assert_eq!(first_reports[0].status, RepairStatus::Repaired);
    assert_eq!(first_reports[0].inserted, 2);
    let series_after_first = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), read_all())
        .await;
    assert_eq!(series_after_first, file_bars);

    // Same plan object, second pass: the rows collide instead of
    // overwriting, and the series does not move.
    let second_reports = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            coordinator.run_once(&permit, &plan).await
        })
        .await;
    assert_eq!(second_reports.len(), 1);
    assert_eq!(second_reports[0].status, RepairStatus::AlreadyComplete);
    assert_eq!(second_reports[0].inserted, 0, "nothing stored twice");
    let series_after_second = fluent_wvr::CURRENT_CAPS.scope(caps, read_all()).await;
    assert_eq!(series_after_second, series_after_first, "series unchanged");
}
