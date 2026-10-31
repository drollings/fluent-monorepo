//! Shared worker-loop internals for every public pool shape.
//!
//! Private to the `pool` module tree (`pub(crate)` seams): `spawn_workers`
//! is the one worker loop every pool parameterizes, `run_wrapped` is the
//! abort-racing reply path shared by both result pools, and
//! `spawn_queue_workers` / `spawn_priority_workers` are the two queue
//! disciplines. No pool shape keeps a private copy.

use std::future::Future;
use std::sync::Arc;

use fluent_wvr::Runtime;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::queue::Queue;

/// Internal wrapper pairing a job with a oneshot sender for its result and an
/// optional abort signal. When the signal fires while the handler is running,
/// the handler future is dropped and the sender is dropped without a result —
/// the submitter observes [`ResultPoolError::Canceled`]. Shared by
/// `ResultPool` and `PriorityResultPool`.
pub(crate) struct WrappedJob<T, R, E> {
    pub(crate) job: T,
    pub(crate) result_tx: tokio::sync::oneshot::Sender<Result<R, E>>,
    pub(crate) abort: Option<crate::stream::StreamAbort>,
}

/// Spawn `cap` worker tasks running one shared worker core.
///
/// Each worker loops: `next_job().await` resolves to `Some(job)` when work is
/// available or `None` when the pool is shutting down, and `handle(job).await`
/// processes it. `next_job` encapsulates the pool's queue discipline — the
/// `Queue`-backed pools `select!` between a shutdown notification and `pop`
/// (so shutdown can preempt a pending pop); the priority pool drains its queue
/// and then waits on shutdown/notify (drain-then-wait). `handle` runs the job;
/// result pools also reply through the job's oneshot channel. This is the one
/// worker loop every public pool type parameterizes.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn spawn_workers<J, F, Fut, H, HFut>(
    runtime: Arc<dyn Runtime>,
    cap: usize,
    next_job: F,
    handle: H,
) -> Vec<JoinHandle<()>>
where
    J: Send + 'static,
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Option<J>> + Send + 'static,
    H: Fn(J) -> HFut + Send + Sync + 'static,
    HFut: Future<Output = ()> + Send + 'static,
{
    let next_job = Arc::new(next_job);
    let handle = Arc::new(handle);
    let mut workers = Vec::with_capacity(cap);
    for _ in 0..cap {
        let n = Arc::clone(&next_job);
        let h = Arc::clone(&handle);
        let r = Arc::clone(&runtime);
        workers.push(r.spawn(Box::pin(async move {
            while let Some(job) = n().await {
                h(job).await;
            }
        })));
    }
    workers
}

/// Run a wrapped job's handler, racing it against the optional abort signal.
///
/// When the abort fires mid-flight, the handler future is dropped (cancelling
/// its transport — an in-flight HTTP call is aborted, a worker slot is freed)
/// and the oneshot sender is dropped without a result, so the submitter
/// observes [`ResultPoolError::Canceled`]. Without an abort signal this is
/// exactly `result_tx.send(handler(job).await)`. Shared by `ResultPool` and
/// `PriorityResultPool`.
pub(crate) async fn run_wrapped<T, R, E, F, Fut>(wrapped: WrappedJob<T, R, E>, handler: Arc<F>)
where
    T: Send + 'static,
    R: Send + 'static,
    E: Send + 'static,
    F: Fn(T) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<R, E>> + Send,
{
    let WrappedJob {
        job,
        result_tx,
        abort,
    } = wrapped;
    let fut = handler(job);
    match abort {
        Some(abort) => {
            let cancelled = abort.cancelled();
            tokio::pin!(fut);
            tokio::pin!(cancelled);
            tokio::select! {
                r = &mut fut => { let _ = result_tx.send(r); }
                () = &mut cancelled => {}
            }
        }
        None => {
            let _ = result_tx.send(fut.await);
        }
    }
}

/// Spawn `cap` workers for a `Queue`-backed pool. Each worker
/// `select!`s between a shutdown notification and `pop`, so shutdown can
/// preempt a pending pop; `handle` runs each job. Shared by `WorkerPool`
/// and `ResultPool`.
pub(crate) fn spawn_queue_workers<J, H, HFut>(
    runtime: Arc<dyn Runtime>,
    cap: usize,
    queue: &Arc<Queue<J>>,
    shutdown: &Arc<Notify>,
    handle: H,
) -> Vec<JoinHandle<()>>
where
    J: Send + 'static,
    H: Fn(J) -> HFut + Send + Sync + 'static,
    HFut: Future<Output = ()> + Send + 'static,
{
    spawn_workers(
        runtime,
        cap,
        {
            let queue = Arc::clone(queue);
            let shutdown = Arc::clone(shutdown);
            move || {
                let queue = Arc::clone(&queue);
                let shutdown = Arc::clone(&shutdown);
                async move {
                    tokio::select! {
                        () = shutdown.notified() => None,
                        item = queue.pop() => item,
                    }
                }
            }
        },
        handle,
    )
}

/// Spawn `cap` workers for `PriorityResultPool`. Each worker pops one
/// job from the shared `BoundedPriorityQueue` (priority desc, FIFO within
/// priority) and, when empty, waits for a submit notification or shutdown.
///
/// Shutdown is signaled through a `watch` channel rather than a `Notify`
/// because `Notify::notify_waiters` does **not** store a permit: a worker that
/// re-registers its shutdown future *after* `notify_waiters` would miss the
/// wake and park forever. `watch::Receiver::changed()` is sticky — a receiver
/// that registers after the send sees it immediately. (The same reasoning
/// means `BoundedPriorityQueue::close` cannot be the only shutdown path for a
/// worker mid-`pop` — the outer sticky watch closes that race.)
pub(crate) fn spawn_priority_workers<T, R, E, H, HFut>(
    runtime: Arc<dyn Runtime>,
    cap: usize,
    queue: &Arc<crate::queue::BoundedPriorityQueue<WrappedJob<T, R, E>>>,
    shutdown: &tokio::sync::watch::Receiver<bool>,
    handle: H,
) -> Vec<JoinHandle<()>>
where
    T: Send + 'static,
    R: Send + 'static,
    E: Send + 'static,
    H: Fn(WrappedJob<T, R, E>) -> HFut + Send + Sync + 'static,
    HFut: Future<Output = ()> + Send + 'static,
{
    spawn_workers(
        runtime,
        cap,
        {
            let queue = Arc::clone(queue);
            let shutdown = shutdown.clone();
            move || {
                let queue = Arc::clone(&queue);
                let mut shutdown = shutdown.clone();
                async move {
                    let shutdown = shutdown.changed();
                    tokio::pin!(shutdown);
                    // `biased` so a shutdown that is already pending (a leftover
                    // job-wake permit, or a close that landed while this worker was
                    // mid-loop) is never shadowed by the job notify. `queue.pop()`
                    // internally registers its notify before checking the queue
                    // (the anti-missed-wakeup discipline from `Queue::pop`) and
                    // parks until an item arrives or the queue is closed.
                    tokio::select! {
                        biased;
                        _ = &mut shutdown => None,
                        item = queue.pop() => item,
                    }
                }
            }
        },
        handle,
    )
}

/// Factory helper for auto-sized global `ResultPool` singletons.
///
/// Workers = max(available_parallelism, min_workers).
/// Queue capacity = workers * queue_multiplier.
pub fn global_pool_config(min_workers: usize, queue_multiplier: usize) -> (usize, usize) {
    let workers = std::thread::available_parallelism()
        .map_or(min_workers, std::num::NonZero::get)
        .max(min_workers);
    let queue_cap = workers * queue_multiplier;
    (workers, queue_cap)
}
