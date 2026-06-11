//! Bounded async queue, worker pool, and concurrency limiter.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use fluent_wvr::Runtime;
use thiserror::Error;
use tokio::sync::{Mutex, Notify, Semaphore};
use tokio::task::JoinHandle;

/// Errors returned by `Queue` and `WorkerPool` operations.
#[derive(Error, Debug, Clone, PartialEq)]
pub enum PoolError {
    #[error("queue is full")]
    Full,
    #[error("queue is closed")]
    Closed,
}

struct QueueInner<T> {
    items: Mutex<VecDeque<T>>,
    capacity: usize,
    closed: AtomicBool,
    notify: Notify,
}

/// A bounded, concurrent, single-consumer queue with close-wakes-waiters semantics.
pub struct Queue<T> {
    inner: Arc<QueueInner<T>>,
}

impl<T: Send + 'static> Queue<T> {
    /// Creates a new bounded queue with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(QueueInner {
                items: Mutex::new(VecDeque::with_capacity(capacity)),
                capacity,
                closed: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        }
    }

    /// Pushes an item into the queue. Returns `Err(Full)` if at capacity.
    pub async fn push(&self, item: T) -> Result<(), PoolError> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(PoolError::Closed);
        }
        let mut items = self.inner.items.lock().await;
        if items.len() >= self.inner.capacity {
            return Err(PoolError::Full);
        }
        items.push_back(item);
        self.inner.notify.notify_one();
        Ok(())
    }

    /// Pops an item from the queue, awaiting if empty. Returns `None` when closed.
    pub async fn pop(&self) -> Option<T> {
        loop {
            let notified = self.inner.notify.notified();
            {
                let mut items = self.inner.items.lock().await;
                if let Some(item) = items.pop_front() {
                    return Some(item);
                }
                if self.inner.closed.load(Ordering::SeqCst) {
                    return None;
                }
            }
            notified.await;
        }
    }

    /// Closes the queue, waking all waiters. Subsequent `pop`s return `None`.
    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }
}

/// A bounded worker pool that spawns `cap` tokio tasks to process jobs from a shared queue.
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
        let mut workers = Vec::with_capacity(cap);

        for _ in 0..cap {
            let q = Arc::clone(&queue);
            let sd = Arc::clone(&shutdown);
            let h = Arc::clone(&handler);
            let r = Arc::clone(&runtime);
            workers.push(r.spawn(Box::pin(async move {
                loop {
                    tokio::select! {
                        () = sd.notified() => break,
                        item = q.pop() => {
                            if let Some(item) = item {
                                h(item).await;
                            } else {
                                break;
                            }
                        }
                    }
                }
            })));
        }

        Self {
            queue,
            workers,
            shutdown,
        }
    }

    /// Submits a job to the pool. Returns `Err(Full)` if the queue is at capacity.
    pub async fn submit(&self, job: T) -> Result<(), PoolError> {
        self.queue.push(job).await
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

/// A semaphore-based concurrency limiter. Runs at most `cap` futures concurrently.
pub struct Limiter {
    sem: Arc<Semaphore>,
}

impl Limiter {
    /// Creates a new limiter that allows at most `cap` concurrent executions.
    pub fn new(cap: usize) -> Self {
        Self {
            sem: Arc::new(Semaphore::new(cap)),
        }
    }

    /// Acquires a semaphore permit, runs `f().await`, then releases the permit.
    pub async fn run<F, Fut, T>(&self, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let _permit = self.sem.acquire().await.expect("semaphore closed");
        f().await
    }
}
