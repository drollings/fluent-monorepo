//! Repair classification: the status calibration, plus the worker contract
//! controls.
//!
//! Positive controls (must read `Deferred`): a locked store, a timing-out
//! fetch, a transport I/O failure. Negative controls (must read
//! `Unavailable` and never enter the retry loop): "no data before the
//! listing date", "no such symbol", unusable provider bytes, and every
//! non-contention store failure. The worker controls prove the supervised
//! shape both ways: an incoherent config refuses, a missing capability is
//! denied, a foreign permit is refused on its face — and a well-formed unit
//! repairs, directly and under batch supervision.

use super::test_history as th;

use crate::coverage::FreshnessPolicy;
use crate::error::{PlanError, StoreError};
use crate::history::HistoryError;
use crate::range::TimeRange;
use crate::repair::{
    classify_history_error, classify_store_error, RecoveryCoordinator, RepairConfig, RepairReason,
    RepairStatus, RepairWorker,
};
use fluent_wvr::validate::Validate as _;
use fluent_wvr::WorkUnit as _;

#[test]
fn locked_store_classifies_deferred() {
    assert_eq!(
        classify_store_error(&StoreError::Busy("database is locked".to_owned())),
        RepairStatus::Deferred
    );
}

#[test]
fn timing_out_fetch_classifies_deferred() {
    assert_eq!(
        classify_history_error(&HistoryError::Timeout("read timed out".to_owned())),
        RepairStatus::Deferred
    );
}

#[test]
fn transport_io_classifies_deferred() {
    let io = HistoryError::from(std::io::Error::new(
        std::io::ErrorKind::ConnectionReset,
        "reset by peer",
    ));
    assert_eq!(classify_history_error(&io), RepairStatus::Deferred);
}

#[test]
fn no_data_before_listing_classifies_unavailable() {
    assert_eq!(
        classify_history_error(&HistoryError::Unavailable(
            "no data before the listing date".to_owned()
        )),
        RepairStatus::Unavailable
    );
}

#[test]
fn no_such_symbol_classifies_unavailable() {
    assert_eq!(
        classify_history_error(&HistoryError::Unavailable(
            "no such symbol: provider serves other only".to_owned()
        )),
        RepairStatus::Unavailable
    );
}

#[test]
fn unusable_provider_bytes_classify_unavailable() {
    assert_eq!(
        classify_history_error(&HistoryError::BadRow {
            line: 4,
            reason: "invalid epoch timestamp".to_owned(),
        }),
        RepairStatus::Unavailable
    );
    assert_eq!(
        classify_history_error(&HistoryError::BadHeader(
            "missing columns: close".to_owned()
        )),
        RepairStatus::Unavailable
    );
}

#[test]
fn non_contention_store_failures_classify_unavailable() {
    for err in [
        StoreError::Denied("no store capability".to_owned()),
        StoreError::InvalidBar("negative volume".to_owned()),
        StoreError::Plan(PlanError::BlankSymbol),
    ] {
        assert_eq!(
            classify_store_error(&err),
            RepairStatus::Unavailable,
            "{err} must not retry"
        );
    }
}

/// An `Unavailable` repair terminates: one attempt, no backoff, reported.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unavailable_repair_terminates_without_backing_off() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (handle, permit, caps) = th::open_file_handle(&dir, "store");
    let key = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            th::register_m1(&handle, &permit, "SPY")
        })
        .await;
    let provider: std::sync::Arc<dyn crate::history::HistoryProvider> =
        std::sync::Arc::new(th::ScriptedProvider::new(
            None,
            th::ScriptedFetch::Unavailable("no data before the listing date".to_owned()),
        ));
    let coordinator = RecoveryCoordinator::new(
        std::sync::Arc::new(handle),
        vec![(key.symbol_id, provider)],
        th::test_config(),
    )
    .expect("valid config");
    let want = TimeRange::bounded(1_000, 2_000).expect("bounded");
    let plan = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            coordinator
                .plan(
                    &permit,
                    &[(key, want)],
                    3_000,
                    FreshnessPolicy::new().build(),
                )
                .expect("plan builds")
        })
        .await;
    assert_eq!(plan.len(), 1);
    let reports = fluent_wvr::CURRENT_CAPS
        .scope(caps, async { coordinator.run_once(&permit, &plan).await })
        .await;
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].status, RepairStatus::Unavailable);
    assert_eq!(reports[0].attempts, 1, "terminal on the first attempt");
    assert!(
        reports[0].detail.contains("listing date"),
        "reason named: {}",
        reports[0].detail
    );
}

/// An incoherent repair bound refuses before any provider is touched.
#[test]
fn incoherent_repair_config_refuses() {
    let bad = RepairConfig {
        max_repair_span_ms: 0,
        repair_concurrency: 2,
        backoff_base_ms: 1,
        backoff_max_shift: 0,
        backoff_ceiling_ms: 5,
    };
    // Validation fires on the config, which is the assertion; no handle is
    // needed because construction never gets past the bound check.
    assert!(bad.validate().is_err());
}

/// A worker without a granted capability is denied, not defaulted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_without_capability_is_denied() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (handle, permit, _caps) = th::open_file_handle(&dir, "store");
    let key = crate::key::TapeKey {
        symbol_id: crate::key::SymbolId(1),
        timeframe: protopyne_types::Timeframe::M1,
    };
    let worker = RepairWorker::new(
        std::sync::Arc::new(handle),
        std::sync::Arc::new(crate::history::NetworkStub::new()),
        permit,
        key,
        TimeRange::bounded(1_000, 2_000).expect("bounded"),
        vec![TimeRange::bounded(1_000, 2_000).expect("bounded")],
        RepairReason::Empty,
        th::test_config(),
    );
    // No capability scope installed: presence check fails deterministically.
    let ctx = fluent_wvr::WorkContext::for_unit(&worker, fluent_wvr::CapabilitySet::new());
    let err = worker.execute(&ctx).expect_err("must be denied");
    assert!(
        matches!(err, fluent_wvr::WorkError::Execution(_)),
        "denial is an execution refusal: {err:?}"
    );
}

/// A permit bound to another file is refused on its face, without
/// consulting the task-local set.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_with_foreign_permit_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (handle, _permit, _caps) = th::open_file_handle(&dir, "store");
    let foreign = crate::capability::StoreStorage::new("elsewhere.db".into());
    let worker = RepairWorker::new(
        std::sync::Arc::new(handle),
        std::sync::Arc::new(crate::history::NetworkStub::new()),
        foreign,
        crate::key::TapeKey {
            symbol_id: crate::key::SymbolId(1),
            timeframe: protopyne_types::Timeframe::M1,
        },
        TimeRange::bounded(1_000, 2_000).expect("bounded"),
        vec![TimeRange::bounded(1_000, 2_000).expect("bounded")],
        RepairReason::Empty,
        th::test_config(),
    );
    let ctx = fluent_wvr::WorkContext::for_unit(&worker, fluent_wvr::CapabilitySet::new());
    let err = worker.execute(&ctx).expect_err("must be refused");
    assert!(
        format!("{err:?}").contains("elsewhere"),
        "refusal names the binding: {err:?}"
    );
}

/// A well-formed unit repairs through `execute`: one attempt, typed report.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_execute_repairs_one_attempt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bars = th::ascending_bars(0, 8);
    let csv = th::write_tape_csv(&dir, "spy.csv", &bars);
    let (handle, permit, caps) = th::open_file_handle(&dir, "store");
    let key = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            th::register_m1(&handle, &permit, "SPY")
        })
        .await;
    let provider: std::sync::Arc<dyn crate::history::HistoryProvider> = std::sync::Arc::new(
        crate::history::FileHistory::new(csv, key, th::server_parse).expect("provider reads"),
    );
    let first = bars.first().expect("nonempty").ts.timestamp_millis();
    let last = bars.last().expect("nonempty").ts.timestamp_millis() + 60_000;
    let want = TimeRange::bounded(first, last).expect("bounded");
    let worker = RepairWorker::new(
        std::sync::Arc::new(handle),
        provider,
        permit.clone(),
        key,
        want,
        vec![want],
        RepairReason::Empty,
        th::test_config(),
    );
    let output = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            let ctx = fluent_wvr::WorkContext::for_unit(&worker, caps);
            worker.execute(&ctx).expect("repair succeeds")
        })
        .await;
    assert!(output.success);
    assert_eq!(output.data["status"], serde_json::json!("Repaired"));
    assert_eq!(output.data["inserted"], serde_json::json!(8));
    assert_eq!(output.data["attempts"], serde_json::json!(1));
}

/// The same unit runs under batch supervision: scheduled, gated, completed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_runs_under_batch_supervision() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bars = th::ascending_bars(0, 4);
    let csv = th::write_tape_csv(&dir, "spy.csv", &bars);
    let (handle, permit, caps) = th::open_file_handle(&dir, "store");
    let key = fluent_wvr::CURRENT_CAPS
        .scope(caps.clone(), async {
            th::register_m1(&handle, &permit, "SPY")
        })
        .await;
    let provider: std::sync::Arc<dyn crate::history::HistoryProvider> = std::sync::Arc::new(
        crate::history::FileHistory::new(csv, key, th::server_parse).expect("provider reads"),
    );
    let first = bars.first().expect("nonempty").ts.timestamp_millis();
    let last = bars.last().expect("nonempty").ts.timestamp_millis() + 60_000;
    let want = TimeRange::bounded(first, last).expect("bounded");
    let worker = RepairWorker::new(
        std::sync::Arc::new(handle),
        provider,
        permit,
        key,
        want,
        vec![want],
        RepairReason::Empty,
        th::test_config(),
    );
    let name = worker.name().to_owned();
    let rt = fluent_concurrency::tokio_runtime();
    let mut batch = fluent_concurrency::batch::SupervisedBatch::new(rt.clone(), caps.clone());
    let ctx = fluent_wvr::WorkContext::for_unit_in_batch(&rt, &caps, |_| {});
    batch
        .register_with_context(
            std::sync::Arc::new(worker) as std::sync::Arc<dyn fluent_wvr::Component>,
            ctx,
        )
        .expect("registration succeeds");
    let summary = batch.await;
    assert!(summary.panicked.is_empty());
    assert!(summary.failed.is_empty());
    let mut completed = false;
    for event in &summary.completed {
        if let fluent_concurrency::batch::SupervisedBatchEvent::Completed {
            name: event_name, ..
        } = event
        {
            if &**event_name == name.as_str() {
                completed = true;
            }
        }
    }
    assert!(completed, "unit completed by name: {name}");
}
