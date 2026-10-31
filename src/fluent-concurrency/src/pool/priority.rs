//! Priority-ordered result pool (`PriorityResultPool`), re-exported through
//! the `pool` facade.

use std::future::Future;
use std::sync::Arc;

use fluent_wvr::Runtime;
use tokio::task::JoinHandle;

use super::common::{run_wrapped, spawn_priority_workers, WrappedJob};
use super::result::ResultPoolError;
use super::traits::PriorityRequestPool;

/// A worker pool where jobs are dispatched in priority order.
///
/// Higher priority values are dispatched first. Jobs with the same priority
/// are dispatched in FIFO order. Backed by a
/// [`crate::queue::BoundedPriorityQueue`] — the same close-wakes-waiters
/// discipline as `Queue`, with `PriorityQueue` ordering. `submit`/`submit_with_abort`
/// apply backpressure: they block while the queue is at capacity instead of
/// growing without bound.
pub struct PriorityResultPool<T, R, E>
where
    T: Send + 'static,
    R: Send + 'static,
    E: Send + 'static,
{
    queue: Arc<crate::queue::BoundedPriorityQueue<WrappedJob<T, R, E>>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    workers: Vec<JoinHandle<()>>,
}

/// Default queue capacity for `PriorityResultPool::new`: `cap * 4` (saturating).
const DEFAULT_PRIORITY_QUEUE_MULTIPLIER: usize = 4;

impl<T, R, E> PriorityResultPool<T, R, E>
where
    T: Send + Sync + 'static,
    R: Send + 'static,
    E: Send + 'static,
{
    /// Creates a new priority result pool with `cap` workers and a queue of
    /// `cap * 4` slots (the documented default; bounded growth).
    /// The handler receives a job and returns `Result<R, E>`.
    #[allow(clippy::needless_pass_by_value)]
    pub fn new<F, Fut>(runtime: Arc<dyn Runtime>, cap: usize, handler: F) -> Self
    where
        F: Fn(T) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R, E>> + Send,
    {
        Self::with_queue_capacity(
            runtime,
            cap,
            cap.saturating_mul(DEFAULT_PRIORITY_QUEUE_MULTIPLIER),
            handler,
        )
    }

    /// Creates a new priority result pool with `cap` workers and an explicit
    /// queue capacity. `submit`/`submit_with_abort` block while the queue is
    /// at capacity (`PoolError::Full` is never returned on the submit path —
    /// it is the synchronous [`crate::queue::BoundedPriorityQueue::push`]
    /// fast path's error). Returns `Err(Pool(Closed))` from a submit once the
    /// pool has shut down.
    #[allow(clippy::needless_pass_by_value)]
    pub fn with_queue_capacity<F, Fut>(
        runtime: Arc<dyn Runtime>,
        cap: usize,
        queue_capacity: usize,
        handler: F,
    ) -> Self
    where
        F: Fn(T) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R, E>> + Send,
    {
        let queue: Arc<crate::queue::BoundedPriorityQueue<WrappedJob<T, R, E>>> =
            Arc::new(crate::queue::BoundedPriorityQueue::new(queue_capacity));
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let handler = Arc::new(handler);
        let workers = spawn_priority_workers(runtime, cap, &queue, &shutdown_rx, {
            let handler = Arc::clone(&handler);
            move |wrapped: WrappedJob<T, R, E>| {
                let handler = Arc::clone(&handler);
                async move {
                    run_wrapped(wrapped, handler).await;
                }
            }
        });

        Self {
            queue,
            shutdown_tx,
            workers,
        }
    }

    /// Submits a job with the given priority and awaits the handler's result.
    /// Higher priority values are dispatched first. Applies backpressure:
    /// blocks while the queue is at capacity.
    pub async fn submit(&self, job: T, priority: i32) -> Result<R, ResultPoolError<E>> {
        self.submit_with_abort(job, priority, None).await
    }

    /// Submits a job with the given priority and an optional abort signal.
    /// Higher priority values are dispatched first; when the abort fires
    /// mid-flight the handler future is dropped and the submitter observes
    /// [`ResultPoolError::Canceled`]. Applies backpressure: blocks while the
    /// queue is at capacity (`Pool(Closed)` on a submit after shutdown).
    pub async fn submit_with_abort(
        &self,
        job: T,
        priority: i32,
        abort: Option<crate::stream::StreamAbort>,
    ) -> Result<R, ResultPoolError<E>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let wrapped = WrappedJob {
            job,
            result_tx: tx,
            abort,
        };
        self.queue.push_wait(wrapped, priority).await?;
        rx.await
            .map_err(|_| ResultPoolError::Canceled)?
            .map_err(ResultPoolError::Inner)
    }

    /// Shuts down the pool: closes the queue (waking any blocked
    /// `submit`/`submit_with_abort` with `Pool(Closed)` and letting parked
    /// workers drain-and-exit), signals every worker through the sticky watch,
    /// then awaits their completion. The watch sender keeps the receivers
    /// alive; workers that outlive an explicit `shutdown` also exit when the
    /// pool (and thus the sender) is dropped.
    pub async fn shutdown(self) {
        self.queue.close();
        let _ = self.shutdown_tx.send(true);
        for w in self.workers {
            let _ = w.await;
        }
    }
}

impl<T, R, E> PriorityRequestPool for PriorityResultPool<T, R, E>
where
    T: Send + Sync + 'static,
    R: Send + 'static,
    E: Send + 'static,
{
    type Job = T;
    type Output = R;
    type Error = E;
    async fn submit(
        &self,
        job: Self::Job,
        priority: i32,
    ) -> Result<Self::Output, ResultPoolError<Self::Error>> {
        PriorityResultPool::submit(self, job, priority).await
    }
    async fn submit_with_abort(
        &self,
        job: Self::Job,
        priority: i32,
        abort: Option<crate::stream::StreamAbort>,
    ) -> Result<Self::Output, ResultPoolError<Self::Error>> {
        PriorityResultPool::submit_with_abort(self, job, priority, abort).await
    }
    fn worker_count(&self) -> usize {
        self.workers.len()
    }
    async fn shutdown(self) {
        PriorityResultPool::shutdown(self).await;
    }
}
