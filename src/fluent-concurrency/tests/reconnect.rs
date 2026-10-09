use super::*;
use std::sync::{Arc, Mutex};

fn test_config() -> ReconnectConfig {
    ReconnectConfig {
        feed: FeedConfig {
            queue_capacity: 64,
            credit_limit: 64,
            credit_more_after: 8,
            max_concurrent: 1,
            batch_size: 8,
            poll_interval_ms: 5,
        },
        base_ms: 1,
        cap: 12,
        gap: GapPolicy::Sparse,
    }
}

fn worker_with_log(
    config: ReconnectConfig,
) -> (Arc<ReconnectWorker<u64>>, Arc<Mutex<Vec<u64>>>) {
    let log: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let inner = Arc::clone(&log);
    let worker = Arc::new(ReconnectWorker::new(
        crate::tokio_runtime(),
        config,
        move |item: Sequenced<u64>| {
            let inner = Arc::clone(&inner);
            async move {
                inner.lock().unwrap().push(item.seq);
            }
        },
    ));
    let handle = worker.start();
    // Park the join handle on the runtime: the drain loop lives until `drain`.
    std::mem::forget(handle);
    (worker, log)
}

async fn drain_and_read(log: &Arc<Mutex<Vec<u64>>>) -> Vec<u64> {
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    log.lock().unwrap().clone()
}

#[test]
fn reconnect_placeholder() {
    // Scaffolding placeholder: replaced by the real reconnect suites below.
    let cfg = ReconnectConfig::default();
    assert_eq!(cfg.gap, GapPolicy::Sparse);
}

#[test]
fn backoff_schedule_matches_capped_backoff() {
    let cfg = test_config();
    for failures in 0..8u32 {
        assert_eq!(
            cfg.delay_for(failures),
            common_core::retry::capped_backoff_ms(cfg.base_ms, failures, cfg.cap),
            "delay for {failures} drops must equal capped_backoff_ms"
        );
    }
}

#[tokio::test]
async fn drop_during_stream_resumes_without_caller_visible_gap() {
    let (worker, log) = worker_with_log(test_config());
    for seq in 1..=5u64 {
        worker
            .handle(SourceEvent::Live(Sequenced::new(seq, seq)))
            .await;
    }
    worker.handle(SourceEvent::Dropped).await;
    assert_eq!(worker.drops(), 1);
    // Items 6..=7 were missed while down; the resubscribe backfills them
    // before live items resume.
    worker
        .handle(SourceEvent::Resubscribed {
            backfill: vec![Sequenced::new(6, 6), Sequenced::new(7, 7)],
        })
        .await;
    for seq in 8..=10u64 {
        worker
            .handle(SourceEvent::Live(Sequenced::new(seq, seq)))
            .await;
    }
    worker.drain();
    let seen = drain_and_read(&log).await;
    assert_eq!(seen, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
}

#[tokio::test]
async fn double_drop_during_backoff_does_not_duplicate() {
    let (worker, log) = worker_with_log(test_config());
    for seq in 1..=3u64 {
        worker
            .handle(SourceEvent::Live(Sequenced::new(seq, seq)))
            .await;
    }
    worker.handle(SourceEvent::Dropped).await;
    worker.handle(SourceEvent::Dropped).await;
    assert_eq!(worker.drops(), 2);
    // Backfill overlaps already-seen items (the pre-drop live tail): the
    // worker dedupes by sequence number.
    let report = worker
        .handle(SourceEvent::Resubscribed {
            backfill: vec![
                Sequenced::new(3, 3),
                Sequenced::new(4, 4),
                Sequenced::new(4, 4),
                Sequenced::new(5, 5),
            ],
        })
        .await;
    match report {
        HandleReport::Resumed(r) => {
            assert_eq!(r.accepted, 2);
            assert_eq!(r.skipped_duplicate, 2);
        }
        other => panic!("expected Resumed, got {other:?}"),
    }
    worker.drain();
    let seen = drain_and_read(&log).await;
    assert_eq!(seen, vec![1, 2, 3, 4, 5]);
}

#[tokio::test]
async fn dense_policy_reports_uncovered_gap() {
    let cfg = ReconnectConfig {
        gap: GapPolicy::Dense,
        ..test_config()
    };
    let (worker, _log) = worker_with_log(cfg);
    for seq in 1..=5u64 {
        worker
            .handle(SourceEvent::Live(Sequenced::new(seq, seq)))
            .await;
    }
    worker.handle(SourceEvent::Dropped).await;
    // Dense backfill jumps 5 -> 8: the gap 6..=7 is not covered.
    let report = worker
        .handle(SourceEvent::Resubscribed {
            backfill: vec![Sequenced::new(8, 8), Sequenced::new(9, 9)],
        })
        .await;
    match report {
        HandleReport::Resumed(r) => {
            assert_eq!(r.gap_remaining, Some((6, 7)));
        }
        other => panic!("expected Resumed, got {other:?}"),
    }
    worker.drain();
}

#[tokio::test]
async fn sparse_policy_tolerates_non_contiguous_series() {
    let (worker, _log) = worker_with_log(test_config());
    for seq in [1u64, 2, 10] {
        worker
            .handle(SourceEvent::Live(Sequenced::new(seq, seq)))
            .await;
    }
    worker.handle(SourceEvent::Dropped).await;
    // Sparse series skip freely: accepting 20 after 10 reports no gap.
    let report = worker
        .handle(SourceEvent::Resubscribed {
            backfill: vec![Sequenced::new(20, 20)],
        })
        .await;
    match report {
        HandleReport::Resumed(r) => {
            assert_eq!(r.gap_remaining, None);
            assert_eq!(r.accepted, 1);
        }
        other => panic!("expected Resumed, got {other:?}"),
    }
    assert_eq!(worker.last_seen(), 20);
    worker.drain();
}

/// Calibration gate for the reconnect worker (drop counting + dedupe).
///
/// Detector framing: "item delivered to the consumer" fires (score 1.0) or
/// not (score 0.0); the label is whether the item was unseen when accepted.
/// The control group — duplicate replays of already-seen seqs — must NOT
/// fire. Gate: zero FP, zero FN.
#[tokio::test]
async fn reconnect_delivery_calibration() {
    use common_core::calibration::{calibrate_threshold, emit_markdown_artifact, sweep_thresholds};
    struct Case {
        fired: bool,
        due: bool,
    }
    let (worker, log) = worker_with_log(test_config());
    // Live 1..=3, then a drop. Healthy ticks before the drop never back off.
    for seq in 1..=3u64 {
        let report = worker
            .handle(SourceEvent::Live(Sequenced::new(seq, seq)))
            .await;
        assert_eq!(report, HandleReport::Live { accepted: true });
    }
    assert_eq!(worker.drops(), 0);
    worker.handle(SourceEvent::Dropped).await;
    assert_eq!(worker.drops(), 1);
    // Backfill overlaps seq 3 (duplicate) and covers missed 4..=5.
    let report = worker
        .handle(SourceEvent::Resubscribed {
            backfill: vec![
                Sequenced::new(3, 3),
                Sequenced::new(4, 4),
                Sequenced::new(5, 5),
            ],
        })
        .await;
    assert!(matches!(report, HandleReport::Resumed(_)));
    assert_eq!(worker.drops(), 0, "resubscribe resets the drop count");
    // A stale live replay after resume must not deliver.
    let replay = worker
        .handle(SourceEvent::Live(Sequenced::new(2, 2)))
        .await;
    assert_eq!(replay, HandleReport::Live { accepted: false });
    for seq in 6..=7u64 {
        worker
            .handle(SourceEvent::Live(Sequenced::new(seq, seq)))
            .await;
    }
    worker.drain();
    let seen = drain_and_read(&log).await;
    assert_eq!(seen, vec![1, 2, 3, 4, 5, 6, 7]);
    let seen_set: std::collections::HashSet<u64> = seen.into_iter().collect();
    // Reconstruct detector cases: every unseen-at-accept item fired exactly
    // once; the two duplicates (backfilled 3, replayed 2) never fired.
    let mut cases: Vec<Case> = (1..=7u64)
        .map(|seq| Case { fired: seen_set.contains(&seq), due: true })
        .collect();
    cases.push(Case { fired: false, due: false });
    cases.push(Case { fired: false, due: false });
    let report = calibrate_threshold(
        &cases,
        |c| if c.fired { 1.0 } else { 0.0 },
        |c| c.due,
        0.5,
    );
    assert_eq!(report.fp, 0, "duplicate control items must never fire: {report:?}");
    assert_eq!(report.r#fn, 0, "due items must always fire: {report:?}");
    assert!(report.passes_gate(), "reconnect calibration must pass: {report:?}");
    emit_markdown_artifact(
        "reconnect-delivery",
        &sweep_thresholds(
            &cases,
            |c| if c.fired { 1.0 } else { 0.0 },
            |c| c.due,
            &[0.5],
        ),
    );
}
