//! Fire-and-forget worker pool (`WorkerPool`), re-exported through the
//! `pool` facade.

use std::future::Future;
use std::sync::Arc;

use fluent_wvr::Runtime;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::common::spawn_queue_workers;
use super::queue::Queue;
use super::traits::{PoolError, SinkPool};

/// A bounded worker pool that spawns `cap` tokio tasks to process jobs from a shared queue.
///
/// # Examples
///
/// ```no_run
/// use std::sync::Arc;
/// use fluent_concurrency::pool::WorkerPool;
/// use fluent_wvr::Runtime;
///
/// # async fn example() {
/// let rt: Arc<dyn Runtime> = Arc::new(fluent_concurrency::runtime::tokio::TokioRuntime);
/// let pool = WorkerPool::new(rt, 4, 100, |n: i32| async move {
///     println!("processing {n}");
/// });
///
/// pool.try_submit(1).await.unwrap();
/// pool.try_submit(2).await.unwrap();
/// pool.shutdown().await;
/// # }
/// ```
pub struct WorkerPool<T: Send + 'static> {
    queue: Arc<Queue<T>>,
    workers: Vec<JoinHandle<()>>,
    shutdown: Arc<Notify>,
}

impl<T: Send + Sync + 'static> WorkerPool<T> {
    /// Creates a new worker pool with `cap` workers and a queue of `queue_capacity`.
    /// All worker tasks are spawned through the injected `runtime` to avoid ambient
    /// `tokio::spawn` calls in the Data Plane.
    #[allow(clippy::needless_pass_by_value)]
    pub fn new<F, Fut>(
        runtime: Arc<dyn Runtime>,
        cap: usize,
        queue_capacity: usize,
        handler: F,
    ) -> Self
    where
        F: Fn(T) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let queue = Arc::new(Queue::new(queue_capacity));
        let shutdown = Arc::new(Notify::new());
        let handler = Arc::new(handler);
        let workers = spawn_queue_workers(runtime, cap, &queue, &shutdown, move |job: T| {
            let handler = Arc::clone(&handler);
            async move { handler(job).await }
        });

        Self {
            queue,
            workers,
            shutdown,
        }
    }

    /// Tries to submit a job without waiting. Returns `Err(Full)` if the queue is at capacity.
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    pub async fn try_submit(&self, job: T) -> Result<(), PoolError> {
        self.queue.push(job)
    }

    /// Submits a job, waiting if the queue is full.
    /// Returns `Err(Closed)` if the queue is closed.
    pub async fn submit(&self, job: T) -> Result<(), PoolError> {
        self.queue.push_wait(job).await
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

impl<T: Send + Sync + 'static> SinkPool for WorkerPool<T> {
    type Job = T;
    async fn submit(&self, job: Self::Job) -> Result<(), PoolError> {
        WorkerPool::submit(self, job).await
    }
    fn worker_count(&self) -> usize {
        self.workers.len()
    }
    async fn shutdown(self) {
        WorkerPool::shutdown(self).await;
    }
}
