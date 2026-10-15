//! Repair backoff: the shared schedule, saturated, with connectivity first.
//!
//! The backoff sequence matches `capped_exp_backoff_ms` and saturates at the
//! configured ceiling — asserted literally, without timing. The retry loop
//! re-checks connectivity at the top of every iteration, so a long sleep
//! never commits to a doomed fetch — asserted through the ordered call log
//! over a scripted fail-fail-heal run. Cancellation is asserted separately
//! through a pre-fired abort: no attempt starts, no fetch runs.

use super::test_history as th;

use crate::range::TimeRange;
use crate::repair::{
    RecoveryCoordinator, RepairConfig, RepairReason, RepairStatus, retry_delay_ms,
};

fn backoff_config() -> RepairConfig {
    RepairConfig::new()
        .max_repair_span_ms(u64::MAX)
        .repair_concurrency(1)
        .backoff_base_ms(5_000)
        .backoff_max_shift(3)
        .backoff_ceiling_ms(100_000)
        .build()
}

#[test]
fn backoff_sequence_matches_the_shared_schedule() {
    let config = backoff_config();
    let expected: Vec<u64> = (0..7)
        .map(|failures| common_core::retry::capped_exp_backoff_ms(5_000, failures, 3).min(100_000))
        .collect();
    assert_eq!(
        expected,
        vec![5_000, 10_000, 20_000, 40_000, 40_000, 40_000, 40_000]
    );
    for (failures, want) in expected.into_iter().enumerate() {
        assert_eq!(
            retry_delay_ms(&config, failures as u32),
            want,
            "failures = {failures}"
        );
    }
}

#[test]
fn backoff_saturates_at_the_configured_ceiling() {
    let config = RepairConfig::new()
        .max_repair_span_ms(u64::MAX)
        .repair_concurrency(1)
        .backoff_base_ms(5_000)
        .backoff_max_shift(10)
        .backoff_ceiling_ms(12_000)
        .build();
    let delays: Vec<u64> = (0..5).map(|n| retry_delay_ms(&config, n)).collect();
    assert_eq!(delays, vec![5_000, 10_000, 12_000, 12_000, 12_000]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connectivity_is_rechecked_before_every_fetch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_bars = th::ascending_bars(0, 4);
    let (handle, permit, caps) = th::open_file_handle(&dir, "store.db").await;
    let key = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            th::register_m1(&handle, &permit, "SPY").await
        })
        .await;
    // Fail twice, then serve: three attempts, three checks, one repair.
    let scripted = std::sync::Arc::new(th::ScriptedProvider::new(
        None,
        th::ScriptedFetch::Sequence(vec![
            th::ScriptedFetch::Timeout("read timed out".to_owned()),
            th::ScriptedFetch::Timeout("read timed out".to_owned()),
            th::ScriptedFetch::Bars(file_bars.clone()),
        ]),
    ));
    let provider: std::sync::Arc<dyn crate::history::HistoryProvider> = scripted.clone();
    let coordinator = RecoveryCoordinator::new(
        std::sync::Arc::new(handle),
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
    let span = TimeRange::bounded(first, end).expect("bounded");
    let plan = crate::repair::RecoveryPlan {
        repairs: vec![crate::repair::SymbolRepair {
            key,
            wanted: span,
            spans: vec![span],
            reason: RepairReason::Empty,
            status: RepairStatus::Deferred,
            attempts: 0,
            inserted: 0,
            conflicts: 0,
            detail: "scripted pass".to_owned(),
        }],
    };
    let reports = fluent_wvr::CURRENT_CAPS
        .scope(caps, async { coordinator.run_once(&permit, &plan).await })
        .await;
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].status, RepairStatus::Repaired);
    assert_eq!(reports[0].attempts, 3, "two timeouts then a fill");
    assert_eq!(reports[0].inserted, 4);
    assert_eq!(scripted.fetch_count(), 3);
    // Every fetch was preceded by its connectivity check, in order: the log
    // reads check, horizon, fetch per attempt — the horizon probe runs even
    // when the provider claims none — and no fetch ever precedes its check.
    let mut checks = 0usize;
    let mut fetches = 0usize;
    for call in scripted.calls() {
        if call == "check" || call == "horizon" {
            checks += 1;
        } else {
            assert!(call.starts_with("fetch "), "unexpected call: {call}");
            fetches += 1;
            assert!(
                checks > fetches,
                "fetch {fetches} ran before its connectivity check"
            );
        }
    }
    assert_eq!((checks, fetches), (6, 3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pre_fired_abort_starts_no_attempt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (handle, permit, caps) = th::open_file_handle(&dir, "store.db").await;
    let key = crate::key::TapeKey {
        symbol_id: crate::key::SymbolId(7),
        timeframe: protopyne_types::Timeframe::M1,
    };
    let scripted = std::sync::Arc::new(th::ScriptedProvider::new(
        None,
        th::ScriptedFetch::Timeout("read timed out".to_owned()),
    ));
    let provider: std::sync::Arc<dyn crate::history::HistoryProvider> = scripted.clone();
    let abort = fluent_concurrency::stream::StreamAbort::new();
    abort.cancel();
    let coordinator = RecoveryCoordinator::new(
        std::sync::Arc::new(handle),
        vec![(key.symbol_id, provider)],
        th::test_config(),
    )
    .expect("valid config")
    .with_abort(abort);
    let span = TimeRange::bounded(0, 600_000).expect("bounded");
    let plan = crate::repair::RecoveryPlan {
        repairs: vec![crate::repair::SymbolRepair {
            key,
            wanted: span,
            spans: vec![span],
            reason: RepairReason::Empty,
            status: RepairStatus::Deferred,
            attempts: 0,
            inserted: 0,
            conflicts: 0,
            detail: "scripted pass".to_owned(),
        }],
    };
    let reports = fluent_wvr::CURRENT_CAPS
        .scope(caps, async { coordinator.run_once(&permit, &plan).await })
        .await;
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].status, RepairStatus::Deferred);
    assert_eq!(reports[0].attempts, 0, "no attempt started");
    assert_eq!(scripted.fetch_count(), 0, "no fetch ran");
}
