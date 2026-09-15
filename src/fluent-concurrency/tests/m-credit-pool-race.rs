//! Submit/drain race coverage: concurrent submitters racing a `drain`
//! must every one resolve `Ok` or `Closed` (never panic, never deadlock),
//! and a drained pool stays `Closed` (idempotent drain).

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn runtime() -> Arc<dyn Runtime> {
    crate::tokio_runtime()
}

#[tokio::test]
async fn submit_drain_race_resolves_cleanly() {
    let pool = Arc::new(CreditGatedPool::new(
        runtime(),
        8,
        16,
        |_: usize| async move {},
    ));
    for i in 0..4 {
        pool.submit(i).await.expect("pre-drain submit");
    }
    let mut handles = Vec::new();
    for t in 0..8usize {
        let p = Arc::clone(&pool);
        handles.push(tokio::spawn(async move {
            let mut oks = 0;
            let mut closeds = 0;
            for i in 0..50usize {
                match p.submit(t * 100 + i).await {
                    Ok(()) => oks += 1,
                    Err(PoolError::Closed) => closeds += 1,
                    Err(other) => panic!("unexpected submit error: {other:?}"),
                }
            }
            (oks, closeds)
        }));
    }
    // Let the submitters get going, then drain mid-flight. Drain waits
    // for in-flight snapshots (no deadlock: workers never need the pool
    // mutex), and every racing submit lands Ok or Closed.
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    pool.drain().await;
    let mut total_ok = 0;
    for h in handles {
        let (oks, closeds) = h.await.expect("submitter joins");
        assert_eq!(oks + closeds, 50);
        total_ok += oks;
    }
    assert!(total_ok >= 4, "pre-drain submits all ran");
    assert!(matches!(pool.submit(999).await, Err(PoolError::Closed)));
    // Drain is idempotent.
    pool.drain().await;
    assert!(matches!(pool.submit(1000).await, Err(PoolError::Closed)));
}

#[tokio::test]
async fn concurrent_submits_all_succeed_without_drain() {
    let counter = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&counter);
    let pool = Arc::new(CreditGatedPool::new(
        runtime(),
        16,
        32,
        move |_: usize| {
            let c = Arc::clone(&c);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        },
    ));
    let mut handles = Vec::new();
    for t in 0..8usize {
        let p = Arc::clone(&pool);
        handles.push(tokio::spawn(async move {
            for i in 0..25usize {
                p.submit(t * 100 + i).await.expect("submit");
            }
        }));
    }
    for h in handles {
        h.await.expect("submitter joins");
    }
    // All 200 jobs eventually process (credit released per job).
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while counter.load(Ordering::SeqCst) < 200 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "only {}/200 jobs processed",
            counter.load(Ordering::SeqCst)
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    pool.drain().await;
}
