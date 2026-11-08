//! Result-returning worker pool (`ResultPool`), re-exported through the
//! `pool` facade.

use std::future::Future;
use std::sync::Arc;

use fluent_wvr::Runtime;
use thiserror::Error;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::common::{run_wrapped, spawn_queue_workers, WrappedJob};
use super::queue::Queue;
use super::traits::{PoolError, RequestPool};

/// Error type for `ResultPool::submit`.
#[derive(Error, Debug)]
pub enum ResultPoolError<E> {
    /// The pool's queue is full or closed.
    #[error("pool error: {0}")]
    Pool(#[from] PoolError),
    /// The handler returned an error.
    #[error("handler error: {0}")]
    Inner(E),
    /// The response channel was canceled (worker panicked or pool shut down).
    #[error("response canceled")]
    Canceled,
}

/// Worker pool where the handler returns a `Result<R, E>` and `submit`
/// returns a future resolving to that result.
pub struct ResultPool<T, R, E>
where
    T: Send + 'static,
    R: Send + 'static,
    E: Send + 'static,
{
    queue: Arc<Queue<WrappedJob<T, R, E>>>,
    workers: Vec<JoinHandle<()>>,
    worker_count: usize,
    shutdown: Arc<Notify>,
}

impl<T, R, E> ResultPool<T, R, E>
where
    T: Send + Sync + 'static,
    R: Send + 'static,
    E: Send + 'static,
{
    /// Creates a new result pool with `cap` workers and a queue of `queue_capacity`.
    /// The handler receives a job and returns `Result<R, E>`.
    #[allow(clippy::needless_pass_by_value)]
    pub fn new<F, Fut>(
        runtime: Arc<dyn Runtime>,
        cap: usize,
        queue_capacity: usize,
        handler: F,
    ) -> Self
    where
        F: Fn(T) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R, E>> + Send,
    {
        let queue: Arc<Queue<WrappedJob<T, R, E>>> = Arc::new(Queue::new(queue_capacity));
        let shutdown = Arc::new(Notify::new());
        let handler = Arc::new(handler);
        let workers = spawn_queue_workers(runtime, cap, &queue, &shutdown, {
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
            workers,
            worker_count: cap,
            shutdown,
        }
    }

    /// Returns the configured number of workers.
    pub fn worker_count(&self) -> usize {
        self.worker_count
    }

    /// Submits a job and awaits the handler's result.
    /// Returns `ResultPoolError::Canceled` if the worker panics, the pool shuts
    /// down, or the submission's abort signal fires.
    pub async fn submit(&self, job: T) -> Result<R, ResultPoolError<E>> {
        self.submit_with_abort(job, None).await
    }

    /// Submits a job with an optional abort signal and awaits the handler's
    /// result. When the signal fires mid-flight the handler future is dropped
    /// and the submitter observes [`ResultPoolError::Canceled`].
    pub async fn submit_with_abort(
        &self,
        job: T,
        abort: Option<crate::stream::StreamAbort>,
    ) -> Result<R, ResultPoolError<E>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let wrapped = WrappedJob {
            job,
            result_tx: tx,
            abort,
        };
        self.queue.push(wrapped)?;
        rx.await
            .map_err(|_| ResultPoolError::Canceled)?
            .map_err(ResultPoolError::Inner)
    }

    /// Submits every job concurrently and returns per-job outcomes in
    /// submission order. Joins the existing `submit_with_abort` futures with
    /// no spawning and no new pool. `None` behaves exactly like `submit`.
    pub async fn submit_all(
        &self,
        jobs: Vec<T>,
        abort: Option<crate::stream::StreamAbort>,
    ) -> Vec<Result<R, ResultPoolError<E>>> {
        let futures: Vec<_> = jobs
            .into_iter()
            .map(|job| self.submit_with_abort(job, abort.clone()))
            .collect();
        futures_util::future::join_all(futures).await
    }

    /// Shuts down the pool: closes the queue, notifies workers, and awaits their completion.
    pub async fn shutdown(self) {
        self.queue.close();
        self.shutdown.notify_waiters();
        for w in self.workers {
            let _ = w.await;
        }
    }
}

impl<T, R, E> RequestPool for ResultPool<T, R, E>
where
    T: Send + Sync + 'static,
    R: Send + 'static,
    E: Send + 'static,
{
    type Job = T;
    type Output = R;
    type Error = E;
    async fn submit(&self, job: Self::Job) -> Result<Self::Output, ResultPoolError<Self::Error>> {
        ResultPool::submit(self, job).await
    }
    async fn submit_with_abort(
        &self,
        job: Self::Job,
        abort: Option<crate::stream::StreamAbort>,
    ) -> Result<Self::Output, ResultPoolError<Self::Error>> {
        ResultPool::submit_with_abort(self, job, abort).await
    }
    fn worker_count(&self) -> usize {
        ResultPool::worker_count(self)
    }
    async fn shutdown(self) {
        ResultPool::shutdown(self).await;
    }
}
