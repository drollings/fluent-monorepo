//! Test-only harness proving the `ResultPool + try_join_all` composition
//! that `batch_embed` migrates to: indexed jobs resolve in input order
//! without serializing submissions, and one failure short-circuits the
//! join instead of waiting for every outstanding result.

use std::sync::Arc;

use fluent_concurrency::pool::{ResultPool, ResultPoolError};

/// Indexed jobs through `ResultPool + try_join_all` return input order
/// while overlapping: the two embeds rendezvous, so serialization would
/// trip the latch guard instead of passing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn result_pool_join_keeps_index_order_without_serializing() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};
    struct Latch {
        state: Mutex<(usize, u64)>,
        cvar: Condvar,
    }
    let latch = Arc::new(Latch {
        state: Mutex::new((0, 0)),
        cvar: Condvar::new(),
    });
    let entered = Arc::new(AtomicUsize::new(0));
    let handler_latch = Arc::clone(&latch);
    let handler_entered = Arc::clone(&entered);
    let pool = ResultPool::new(
        fluent_concurrency::tokio_runtime(),
        2,
        4,
        move |(index, _unit): (usize, ())| {
            let latch = Arc::clone(&handler_latch);
            let entered = Arc::clone(&handler_entered);
            async move {
                entered.fetch_add(1, Ordering::SeqCst);
                // Two-party rendezvous with a guard: serial execution
                // would expire the guard instead of passing.
                let mut guard = latch.state.lock().expect("latch");
                let generation = guard.1;
                guard.0 += 1;
                if guard.0 == 2 {
                    guard.0 = 0;
                    guard.1 += 1;
                    latch.cvar.notify_all();
                } else {
                    loop {
                        let (next, result) = latch
                            .cvar
                            .wait_timeout(guard, std::time::Duration::from_secs(30))
                            .expect("latch");
                        guard = next;
                        if guard.1 != generation || result.timed_out() {
                            break;
                        }
                    }
                    assert_ne!(guard.1, generation, "rendezvous met: jobs overlap");
                }
                Ok::<(usize, u32), String>((index, index as u32 * 10))
            }
        },
    );
    let mut indexed = futures_util::future::try_join_all((0..4usize).map(|index| {
        let pool = &pool;
        async move { pool.submit((index, ())).await }
    }))
    .await
    .expect("join succeeds");
    indexed.sort_by_key(|(index, _)| *index);
    assert_eq!(
        indexed,
        vec![(0, 0), (1, 10), (2, 20), (3, 30)],
        "input order survives the join"
    );
    assert_eq!(entered.load(Ordering::SeqCst), 4);
    pool.shutdown().await;
}

/// One failing job short-circuits the join while a peer is still blocked:
/// the error returns first, the peer completes only after the test
/// releases it. No timing assumption — the latch orders the events.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn result_pool_join_short_circuits_on_the_first_error() {
    use std::sync::{Condvar, Mutex};
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let order = Arc::new(Mutex::new(Vec::new()));
    let handler_pair = Arc::clone(&pair);
    let pool = ResultPool::new(
        fluent_concurrency::tokio_runtime(),
        2,
        2,
        move |(index, _unit): (usize, ())| {
            let pair = Arc::clone(&handler_pair);
            async move {
                if index == 0 {
                    return Err::<(usize, u32), String>("boom".to_string());
                }
                let (flag, cvar) = &*pair;
                let mut open = flag.lock().expect("latch");
                while !*open {
                    open = cvar.wait(open).expect("latch");
                }
                Ok::<(usize, u32), String>((index, 99))
            }
        },
    );
    let outcome: Result<Vec<(usize, u32)>, ResultPoolError<String>> =
        futures_util::future::try_join_all((0..2usize).map(|index| {
            let pool = &pool;
            async move { pool.submit((index, ())).await }
        }))
        .await;
    let error = outcome.expect_err("job 0 fails the join");
    assert!(
        matches!(error, ResultPoolError::Inner(ref message) if message == "boom"),
        "inner error surfaces verbatim: {error:?}"
    );
    order.lock().expect("order").push("error");
    // The join already failed while job 1 is still parked: release it now.
    {
        let (flag, cvar) = &*pair;
        *flag.lock().expect("latch") = true;
        cvar.notify_all();
    }
    // Drain the parked job so the pool shuts down cleanly.
    let parked = pool.submit((1, ())).await.expect("parked job drains");
    assert_eq!(parked, (1, 99));
    order.lock().expect("order").push("drained");
    assert_eq!(*order.lock().expect("order"), vec!["error", "drained"]);
    pool.shutdown().await;
}

/// Error-text pins for the `batch_embed` mapping: pool failures carry the
/// underlying queue text, cancellation names the worker-loss outcome.
#[test]
fn result_pool_error_text_matches_the_embed_mapping() {
    let pool_error: ResultPoolError<String> =
        ResultPoolError::Pool(fluent_concurrency::pool::PoolError::Closed);
    assert_eq!(format!("{pool_error}"), "pool error: queue is closed");
    let canceled: ResultPoolError<String> = ResultPoolError::Canceled;
    assert_eq!(format!("{canceled}"), "response canceled");
}
