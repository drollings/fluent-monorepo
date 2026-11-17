use std::time::Duration;

use fluent_concurrency::pool::{ResultPool, ResultPoolError};

fn pool(cap: usize, queue: usize) -> ResultPool<u64, u64, String> {
    ResultPool::new(
        fluent_concurrency::tokio_runtime(),
        cap,
        queue,
        |job: u64| async move {
            let delay = (10 - job.min(10)) * 5;
            tokio::time::sleep(Duration::from_millis(delay)).await;
            Ok::<u64, String>(job * 2)
        },
    )
}

#[tokio::test]
async fn order_preserved_across_job_counts() {
    for n in [1u64, 3, 7, 16] {
        let jobs: Vec<u64> = (0..n).collect();
        let expected: Vec<u64> = jobs.iter().map(|job| job * 2).collect();
        let p = pool(4, n.max(1) as usize);
        let outcomes = p.submit_all(jobs, None).await;
        p.shutdown().await;
        let values: Vec<u64> = outcomes
            .into_iter()
            .map(|result| result.expect("job succeeds"))
            .collect();
        assert_eq!(values, expected, "submission order survives n={n}");
    }
}

#[tokio::test]
async fn inner_errors_surface_per_job_without_stopping_siblings() {
    let p = ResultPool::new(
        fluent_concurrency::tokio_runtime(),
        4,
        8,
        |job: u64| async move {
            if job % 2 == 0 {
                Err::<u64, String>(format!("bad {job}"))
            } else {
                Ok(job * 10)
            }
        },
    );
    let outcomes = p.submit_all(vec![0, 1, 2, 3], None).await;
    p.shutdown().await;
    assert_eq!(outcomes.len(), 4);
    assert!(matches!(&outcomes[0], Err(ResultPoolError::Inner(_))));
    assert_eq!(outcomes[1].as_ref().expect("odd succeeds"), &10);
    assert!(matches!(&outcomes[2], Err(ResultPoolError::Inner(_))));
    assert_eq!(outcomes[3].as_ref().expect("odd succeeds"), &30);
}

#[tokio::test]
async fn fired_abort_yields_canceled() {
    let p = ResultPool::new(
        fluent_concurrency::tokio_runtime(),
        2,
        4,
        |job: u64| async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok::<u64, String>(job)
        },
    );
    let abort = fluent_concurrency::stream::StreamAbort::new();
    abort.cancel();
    let outcomes = p.submit_all(vec![0, 1], Some(abort)).await;
    p.shutdown().await;
    assert_eq!(outcomes.len(), 2);
    for outcome in outcomes {
        assert!(
            matches!(outcome, Err(ResultPoolError::Canceled)),
            "{outcome:?}"
        );
    }
}

#[tokio::test]
async fn empty_input_yields_empty_output() {
    let p = pool(2, 1);
    let outcomes = p.submit_all(Vec::new(), None).await;
    p.shutdown().await;
    assert!(outcomes.is_empty());
}

#[tokio::test]
async fn determinism_cap1_vs_cap_n_resorted_identical() {
    let jobs: Vec<u64> = vec![9, 3, 14, 1, 11, 6, 0, 7, 4, 12, 2, 10, 5, 13, 8];
    let run = |cap: usize, queue: usize| {
        let jobs = jobs.clone();
        async move {
            let p = ResultPool::new(
                fluent_concurrency::tokio_runtime(),
                cap,
                queue,
                |job: u64| async move {
                    let delay = (job % 5) * 4;
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    if job % 4 == 0 {
                        Err::<u64, String>(format!("bad {job}"))
                    } else {
                        Ok(job * 3)
                    }
                },
            );
            let outcomes = p.submit_all(jobs, None).await;
            p.shutdown().await;
            outcomes
                .into_iter()
                .map(|result| result.map_err(|e| format!("{e:?}")))
                .collect::<Vec<_>>()
        }
    };
    let solo = run(1, 15).await;
    let wide = run(8, 15).await;
    let wide_again = run(8, 15).await;
    assert_eq!(solo, wide, "cap-1 and cap-N agree");
    assert_eq!(wide, wide_again, "repeated runs agree");
}

#[tokio::test]
async fn pool_bound_inflight_never_exceeds_cap() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let cap = 4usize;
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let p = ResultPool::new(fluent_concurrency::tokio_runtime(), cap, 32, {
        let live = Arc::clone(&live);
        let peak = Arc::clone(&peak);
        move |job: u64| {
            let live = Arc::clone(&live);
            let peak = Arc::clone(&peak);
            async move {
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(15)).await;
                live.fetch_sub(1, Ordering::SeqCst);
                Ok::<u64, String>(job)
            }
        }
    });
    let jobs: Vec<u64> = (0..20).collect();
    let outcomes = p.submit_all(jobs, None).await;
    p.shutdown().await;
    assert_eq!(outcomes.len(), 20);
    assert!(
        peak.load(Ordering::SeqCst) <= cap,
        "in-flight peaks within the cap"
    );
    assert_eq!(live.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn abort_frees_slot_for_reuse() {
    let p = ResultPool::new(
        fluent_concurrency::tokio_runtime(),
        2,
        4,
        |job: u64| async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            Ok::<u64, String>(job)
        },
    );
    let abort = fluent_concurrency::stream::StreamAbort::new();
    abort.cancel();
    let cancelled = p.submit_all(vec![0, 1], Some(abort)).await;
    for outcome in cancelled {
        assert!(matches!(outcome, Err(ResultPoolError::Canceled)));
    }
    let reused = p.submit_all(vec![2, 3], None).await;
    p.shutdown().await;
    let values: Vec<u64> = reused
        .into_iter()
        .map(|result| result.expect("slot reusable"))
        .collect();
    assert_eq!(values, vec![2, 3]);
}

#[tokio::test]
async fn control_group_cap1_matches_sequential_loop() {
    let jobs: Vec<u64> = (0..8).collect();
    let handler_ok = |job: u64| async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        if job % 3 == 0 {
            Err::<u64, String>(format!("bad {job}"))
        } else {
            Ok(job + 100)
        }
    };
    let p = ResultPool::new(fluent_concurrency::tokio_runtime(), 1, 8, handler_ok);
    let batched = p.submit_all(jobs.clone(), None).await;
    p.shutdown().await;
    let q = ResultPool::new(
        fluent_concurrency::tokio_runtime(),
        1,
        8,
        |job: u64| async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            if job % 3 == 0 {
                Err::<u64, String>(format!("bad {job}"))
            } else {
                Ok(job + 100)
            }
        },
    );
    let mut sequential = Vec::new();
    for job in jobs {
        sequential.push(q.submit(job).await.map_err(|e| format!("{e:?}")));
    }
    q.shutdown().await;
    let batched = batched
        .into_iter()
        .map(|result| result.map_err(|e| format!("{e:?}")))
        .collect::<Vec<_>>();
    assert_eq!(batched, sequential);
}

#[tokio::test]
async fn submit_fails_fast_when_full_while_backpressure_waits() {
    use fluent_concurrency::pool::PoolError;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let gate = Arc::new(tokio::sync::Notify::new());
    let started = Arc::new(AtomicBool::new(false));
    let p = ResultPool::new(fluent_concurrency::tokio_runtime(), 1, 1, {
        let gate = Arc::clone(&gate);
        let started = Arc::clone(&started);
        move |job: u64| {
            let gate = Arc::clone(&gate);
            let started = Arc::clone(&started);
            async move {
                started.store(true, Ordering::SeqCst);
                gate.notified().await;
                Ok::<u64, String>(job)
            }
        }
    });
    // Drive the first submit until the worker is parked in the handler: the
    // initial poll performs the queue push, later polls just observe.
    let mut first = Box::pin(p.submit(1));
    while !started.load(Ordering::SeqCst) {
        assert!(futures_util::poll!(&mut first).is_pending());
        tokio::task::yield_now().await;
    }
    // The worker is busy, so this push lands in the single queue slot.
    let mut second = Box::pin(p.submit(2));
    assert!(futures_util::poll!(&mut second).is_pending());
    // Fail-fast: the queue is full, so `submit` errors immediately.
    assert!(
        matches!(
            p.submit(3).await,
            Err(ResultPoolError::Pool(PoolError::Full))
        ),
        "submit must fail fast when full"
    );
    // Backpressure: waits for space instead of failing.
    let mut pressured = Box::pin(p.submit_with_backpressure(4));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut pressured)
            .await
            .is_err(),
        "backpressure submit must still be pending while full"
    );
    // Drain: each notify releases one handler past the gate.
    gate.notify_one();
    assert_eq!(first.await.expect("first completes"), 1);
    gate.notify_one();
    assert_eq!(second.await.expect("second completes"), 2);
    gate.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), pressured)
            .await
            .expect("pressured resolves")
            .expect("pressured succeeds"),
        4
    );
    p.shutdown().await;
}
