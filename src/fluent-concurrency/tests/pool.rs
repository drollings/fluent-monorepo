use super::*;
use crate::tokio_runtime;
use fluent_wvr::Runtime;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// `run_sync` from a thread with no active runtime must bridge on a runtime
/// that outlives the call.
///
/// The bridged future spawns work and returns immediately. That work must
/// still be able to make progress *after* `run_sync` has returned — if the
/// bridge built a runtime for the call alone, dropping it would also drop
/// the spawned task, and anything that task was holding a connection for
/// would be stranded with a waker pointing at a dead runtime.
#[test]
fn run_sync_no_runtime_keeps_spawned_work_alive() {
    let limiter = Limiter::new(1);
    let completed = Arc::new(AtomicUsize::new(0));
    let flag = Arc::clone(&completed);

    limiter.run_sync(|| async move {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            flag.store(1, Ordering::SeqCst);
        });
    });

    // `run_sync` has returned. The spawned task must still run.
    for _ in 0..300 {
        if completed.load(Ordering::SeqCst) == 1 {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("spawned work was dropped when run_sync returned: the bridge runtime did not outlive the call");
}

/// `run_sync` from a thread with no active runtime must bridge on a
/// multi-thread runtime, so that several sync callers make progress together
/// instead of serializing on one runtime's single driver.
///
/// Asserted on the mechanism, not on wall-clock timing, which would be flaky.
#[test]
fn run_sync_no_runtime_bridges_on_multi_thread_runtime() {
    let limiter = Limiter::new(1);
    let flavor = limiter.run_sync(|| async { tokio::runtime::Handle::current().runtime_flavor() });
    assert_eq!(
        flavor,
        tokio::runtime::RuntimeFlavor::MultiThread,
        "the no-runtime bridge must be multi-thread so concurrent callers do not serialize"
    );
}

/// Concurrent sync callers on a runtime-less thread must all complete. A
/// limiter exists to bound concurrency across such callers, so this must not
/// deadlock regardless of how the bridge is wired.
#[test]
fn run_sync_supports_concurrent_runtime_less_callers() {
    let limiter = Arc::new(Limiter::new(4));
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let limiter = Arc::clone(&limiter);
            std::thread::spawn(move || limiter.run_sync(|| async move { i }))
        })
        .collect();

    let mut got: Vec<usize> = handles
        .into_iter()
        .map(|h| h.join().expect("run_sync caller panicked"))
        .collect();
    got.sort_unstable();
    assert_eq!(got, (0..8).collect::<Vec<_>>());
}

#[tokio::test]
async fn priority_pool_dispatches_high_first() {
    let rt = tokio_runtime();
    let pool = PriorityResultPool::<i32, String, String>::new(rt, 1, |job: i32| async move {
        // Simulate work
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        Ok(format!("job_{job}"))
    });

    // Submit low priority first, then high priority.
    // With a single worker, they process in queue order, but the
    // priority queue ensures high-priority items are popped first.
    let high = pool.submit(100, 10);
    let low = pool.submit(1, 0);

    let high_result = high.await.unwrap();
    let low_result = low.await.unwrap();

    // Both should complete successfully.
    assert_eq!(high_result, "job_100");
    assert_eq!(low_result, "job_1");

    pool.shutdown().await;
}

#[tokio::test]
async fn test_priority_pool_burst_drains_all() {
    let rt = tokio_runtime();
    let pool = PriorityResultPool::<i32, i32, String>::new(
        Arc::clone(&rt) as Arc<dyn Runtime>,
        4,
        |job: i32| async move { Ok(job * 2) },
    );

    // Submit 100 high-priority items back-to-back with 4 workers.
    let mut handles = Vec::with_capacity(100);
    for i in 0..100 {
        handles.push(pool.submit(i, 10));
    }

    // All 100 results must resolve before timeout.
    for (i, h) in handles.into_iter().enumerate() {
        let result = tokio::time::timeout(Duration::from_secs(5), h)
            .await
            .expect("burst result must not hang")
            .unwrap();
        assert_eq!(result, i as i32 * 2);
    }

    pool.shutdown().await;
}

#[tokio::test]
async fn priority_pool_dispatches_in_fifo_within_priority() {
    let rt = tokio_runtime();
    let pool =
        PriorityResultPool::<u64, u64, String>::new(rt, 1, |job: u64| async move { Ok(job * 2) });

    // Same priority — should be FIFO.
    let r1 = pool.submit(1, 5);
    let r2 = pool.submit(2, 5);
    let r3 = pool.submit(3, 5);

    assert_eq!(r1.await.unwrap(), 2);
    assert_eq!(r2.await.unwrap(), 4);
    assert_eq!(r3.await.unwrap(), 6);

    pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_priority_pool_single_submit_wakes_sleeper() {
    // A job submitted while every worker is parked (queue
    // observed empty) must always wake one worker. The pre-fix worker
    // registered its `notified()` future only *after* releasing the queue
    // mutex, so a submit landing in that window notified nobody and the
    // job sat until the next submit. Multi-threaded + repeated so the race
    // window has a real chance to be exercised; on the fixed code every
    // iteration completes within the timeout.
    let rt = tokio_runtime();
    for i in 0..64 {
        let pool = PriorityResultPool::<i32, i32, String>::new(
            Arc::clone(&rt) as Arc<dyn Runtime>,
            2,
            |job: i32| async move { Ok(job) },
        );
        // Give the workers a chance to pop-empty and park in the select.
        tokio::task::yield_now().await;
        let result = tokio::time::timeout(Duration::from_secs(5), pool.submit(i, 0))
            .await
            .expect("single submit to an idle pool must wake a sleeping worker")
            .expect("handler must not error");
        assert_eq!(result, i);
        pool.shutdown().await;
    }
}

#[tokio::test(start_paused = true)]
async fn test_priority_pool_single_submit_paused_time() {
    // Deterministic variant under virtual time: the worker parks before the
    // submit, so the pre-registered `notified()` future (created before the
    // queue mutex is taken) is what the submit's `notify_one` fires.
    tokio::time::resume();
    let rt = tokio_runtime();
    let pool = PriorityResultPool::<i32, i32, String>::new(
        Arc::clone(&rt) as Arc<dyn Runtime>,
        1,
        |job: i32| async move { Ok(job) },
    );
    tokio::task::yield_now().await;
    let result = tokio::time::timeout(Duration::from_secs(5), pool.submit(7, 0))
        .await
        .expect("single submit must resolve")
        .unwrap();
    assert_eq!(result, 7);
    pool.shutdown().await;
}

/// The abort signal drops the in-flight handler future: the submitter
/// observes `Canceled` well before the (slow) handler would have finished.
#[tokio::test]
async fn result_pool_submit_with_abort_cancels_running_handler() {
    let rt = tokio_runtime();
    let pool = Arc::new(ResultPool::<i32, i32, String>::new(
        rt,
        1,
        10,
        |_: i32| async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(42)
        },
    ));
    let abort = crate::stream::StreamAbort::new();
    let p = Arc::clone(&pool);
    let a = abort.clone();
    let submitter = tokio::spawn(async move { p.submit_with_abort(1, Some(a)).await });
    // Let the handler start its 500ms sleep, then abort it.
    tokio::time::sleep(Duration::from_millis(50)).await;
    abort.cancel();
    let result = submitter
        .await
        .expect("submitter resolves")
        .expect_err("aborted handler yields Canceled");
    assert!(
        matches!(result, ResultPoolError::Canceled),
        "expected Canceled, got {result:?}"
    );
    if let Ok(pool) = Arc::try_unwrap(pool) {
        pool.shutdown().await;
    }
}

/// Without an abort signal the submit behaves exactly as before.
#[tokio::test]
async fn result_pool_submit_with_abort_none_is_noop() {
    let rt = tokio_runtime();
    let pool =
        ResultPool::<i32, i32, String>::new(rt, 1, 10, |job: i32| async move { Ok(job * 2) });
    let result = pool.submit_with_abort(21, None).await.expect("no abort");
    assert_eq!(result, 42);
    pool.shutdown().await;
}

/// Backpressure: with `cap=1` worker and `queue_capacity=1`, a second
/// submit blocks while the queue is full (the worker is busy, the single
/// slot is held) and resolves only once a worker pops a queued job and
/// frees space.
#[tokio::test]
async fn priority_pool_submit_blocks_when_queue_full() {
    let rt = tokio_runtime();
    let pool = Arc::new(PriorityResultPool::<i32, i32, String>::with_queue_capacity(
        Arc::clone(&rt) as Arc<dyn Runtime>,
        1,
        1,
        |job: i32| async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(job)
        },
    ));
    // Job 1 is picked up by the single worker; job 2 fills the one slot.
    let p1 = Arc::clone(&pool);
    let f1 = tokio::spawn(async move { p1.submit(1, 0).await });
    let p2 = Arc::clone(&pool);
    let f2 = tokio::spawn(async move { p2.submit(2, 0).await });
    // Give the worker time to grab job 1 and job 2 to land in the queue.
    tokio::time::sleep(Duration::from_millis(20)).await;
    // Job 3 must block: queue full, worker busy on job 1.
    let p3 = Arc::clone(&pool);
    let f3 = tokio::spawn(async move { p3.submit(3, 0).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !f3.is_finished(),
        "submit must block while the queue is full (worker busy, slot held)"
    );
    // Once the worker finishes job 1 it pops job 2, freeing space for job 3.
    assert_eq!(f1.await.unwrap().unwrap(), 1);
    assert_eq!(f2.await.unwrap().unwrap(), 2);
    assert_eq!(f3.await.unwrap().unwrap(), 3);
    if let Ok(pool) = Arc::try_unwrap(pool) {
        pool.shutdown().await;
    }
}

/// Close-during-pop on `Queue::pop` must return `None` within a bound.
/// Exercises `Queue::pop` with `CloseWaker::close` mid-wait, plus a direct
/// `CloseWaker::items_available` probe showing a `notify_waiters` fired
/// between future creation and first poll is lost without an armed wait.
#[tokio::test]
async fn close_wakes_pop() {
    let q = std::sync::Arc::new(Queue::<i32>::new(4).expect("test capacity is nonzero"));
    let waiter = std::sync::Arc::clone(&q);
    let popped = tokio::spawn(async move { waiter.pop().await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    q.close();
    let item = tokio::time::timeout(Duration::from_secs(5), popped)
        .await
        .expect("pop must not hang after close")
        .expect("join");
    assert!(item.is_none(), "closed empty queue pops None");

    let waker = crate::close::CloseWaker::new();
    let pending = waker.items_available();
    waker.close();
    // A broadcast fired after creation is observed on first poll; this is
    // the guarantee `Queue::pop` relies on via `CloseWaker::items_available`.
    let observed = tokio::time::timeout(Duration::from_secs(5), pending).await;
    assert!(observed.is_ok(), "close after creation must wake the waiter");
}

/// Zero capacity is refused at construction: leaf queues return `Err`
/// and limiters fail fast, instead of hanging on a zero permit.
#[test]
fn zero_capacity_refused() {
    assert!(Queue::<i32>::new(0).is_err());
    assert!(crate::queue::BoundedPriorityQueue::<i32>::new(0).is_err());
    assert!(std::panic::catch_unwind(|| Limiter::new(0)).is_err());
}

/// Capacity one builds and works on every constructor: the refusal
/// boundary is exactly zero, nothing above it.
#[test]
fn nonzero_boundary_accepts_one() {
    assert!(Queue::<i32>::new(1).is_ok());
    assert!(crate::queue::BoundedPriorityQueue::<i32>::new(1).is_ok());
    let _limiter = Limiter::new(1);
}

/// Pools refuse a zero worker cap or zero queue capacity immediately,
/// before spawning any worker, instead of hanging on undrained submits.
#[test]
fn result_pool_refuses_zero_capacity() {
    let refused = |cap: usize, queue_capacity: usize| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ResultPool::<i32, i32, String>::new(
                crate::tokio_runtime(),
                cap,
                queue_capacity,
                |job: i32| async move { Ok(job) },
            )
        }))
        .is_err()
    };
    assert!(refused(0, 8));
    assert!(refused(1, 0));
}

/// Same fail-fast contract for the fire-and-forget pool: zero workers
/// or zero queue slots panic before any task spawns.
#[tokio::test]
async fn worker_pool_refuses_zero_capacity() {
    let refused = |cap: usize, queue_capacity: usize| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            WorkerPool::<i32>::new(crate::tokio_runtime(), cap, queue_capacity, |_| async {})
        }))
        .is_err()
    };
    assert!(refused(0, 8));
    assert!(refused(1, 0));
    WorkerPool::<i32>::new(crate::tokio_runtime(), 1, 1, |_| async {})
        .shutdown()
        .await;
}

/// Same fail-fast contract for the priority-ordered pool, on both the
/// default-sized and explicit queue-capacity paths.
#[tokio::test]
async fn priority_pool_refuses_zero_capacity() {
    let rt = || crate::tokio_runtime();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            PriorityResultPool::<i32, i32, String>::new(rt(), 0, |job: i32| async move { Ok(job) })
        }))
        .is_err()
    );
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            PriorityResultPool::<i32, i32, String>::with_queue_capacity(
                rt(),
                1,
                0,
                |job: i32| async move { Ok(job) },
            )
        }))
        .is_err()
    );
    PriorityResultPool::<i32, i32, String>::with_queue_capacity(
        rt(),
        1,
        1,
        |job: i32| async move { Ok(job) },
    )
    .shutdown()
    .await;
}

/// Closing mid-flight terminates every blocked task: full-queue
/// `push_wait` submitters observe `Closed`, empty-queue `pop` waiters
/// observe `None`, and none hangs past the bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn close_storm_wakes_pushers_and_poppers() {
    let q = Arc::new(Queue::<i32>::new(4).expect("test capacity is nonzero"));
    for i in 0..4 {
        q.push(i).expect("fill");
    }
    let mut pushers = Vec::new();
    let mut poppers = Vec::new();
    for i in 0..8 {
        let q = Arc::clone(&q);
        pushers.push(tokio::spawn(async move {
            let _ = q.push_wait(100 + i).await;
        }));
    }
    for _ in 0..8 {
        let q = Arc::clone(&q);
        poppers.push(tokio::spawn(async move {
            let _ = q.pop().await;
        }));
    }
    tokio::task::yield_now().await;
    q.close();
    for task in pushers {
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("blocked pusher must terminate after close")
            .expect("join");
    }
    for task in poppers {
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("blocked popper must terminate after close")
            .expect("join");
    }
}

/// Same storm contract for the priority-ordered queue: blocked
/// `push_wait` and `pop` tasks all terminate once closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn bounded_priority_close_storm_wakes_pushers_and_poppers() {
    let q = Arc::new(crate::queue::BoundedPriorityQueue::<i32>::new(4).expect("test capacity is nonzero"));
    for i in 0..4 {
        q.push(i, 0).expect("fill");
    }
    let mut pushers = Vec::new();
    let mut poppers = Vec::new();
    for i in 0..8 {
        let q = Arc::clone(&q);
        pushers.push(tokio::spawn(async move {
            let _ = q.push_wait(100 + i, 0).await;
        }));
    }
    for _ in 0..8 {
        let q = Arc::clone(&q);
        poppers.push(tokio::spawn(async move {
            let _ = q.pop().await;
        }));
    }
    tokio::task::yield_now().await;
    q.close();
    for task in pushers {
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("blocked pusher must terminate after close")
            .expect("join");
    }
    for task in poppers {
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("blocked popper must terminate after close")
            .expect("join");
    }
}
