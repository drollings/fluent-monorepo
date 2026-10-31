//! Semaphore-based concurrency limiter (`Limiter`), re-exported through the
//! `pool` facade.

use std::future::Future;
use std::sync::Arc;

use tokio::sync::Semaphore;

/// A semaphore-based concurrency limiter. Runs at most `cap` futures concurrently.
///
/// # Examples
///
/// ```no_run
/// use fluent_concurrency::pool::Limiter;
///
/// # async fn example() {
/// let limiter = Limiter::new(3);
/// limiter.run(|| async {
///     // At most 3 of these closures run at a time.
///     println!("concurrent work");
/// }).await;
/// # }
/// ```
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

    /// Synchronous version of `run` for callers without an async context.
    /// Uses `Handle::block_on` if a tokio runtime is active, otherwise bridges
    /// on `common_core::runtime::block_on_shared`.
    ///
    /// When invoked from *inside* a running multi-threaded tokio runtime (the
    /// router's HTTP handler does this), the future is driven via
    /// `tokio::task::block_in_place` so the worker thread is not starved —
    /// the same canonical pattern as `fluent_llm::client::chat_complete`
    /// (see `src/llm/src/client.rs`). A bare `Handle::block_on` would panic
    /// with "Cannot start a runtime from within a runtime".
    ///
    /// With **no active runtime** the bridge goes to the workspace's
    /// process-wide shared runtime (`common_core::runtime::block_on_shared`),
    /// deliberately **not** a runtime built for this one call. Two properties
    /// follow, both required for correctness:
    ///
    /// - The runtime **outlives the call**. A per-call runtime is dropped the
    ///   instant `run_sync` returns, taking with it anything the bridged
    ///   future spawned onto it. When the work touches a pooled HTTP
    ///   connection, that spawn is the continuation which returns the
    ///   connection to its client's pool; losing it strands the connection
    ///   with a waker whose runtime no longer exists, so the next caller that
    ///   checks that connection out waits forever — a silent lost wakeup,
    ///   bounded only by the client's timeout. Worker-pool threads reach this
    ///   branch (they carry no tokio runtime), so any `run_sync` whose work
    ///   performs pooled HTTP must not bridge on a throwaway runtime.
    /// - **Concurrent callers do not serialize.** This is a limiter: several
    ///   threads bridging at once is the normal case. The shared runtime is
    ///   multi-threaded, so each caller runs its own future on its own thread
    ///   while one worker pool services everything they spawn. A
    ///   single-threaded fallback would serialize them and make the limiter's
    ///   cap the only thing bounding a queue.
    ///
    /// **Caveat:** on the multi-thread scheduler `block_in_place` *parks the
    /// calling worker thread* for the duration of the call — the scheduler
    /// spins up a replacement thread and the parked thread is unavailable for
    /// work stealing until the call returns. This is the correct sync↔async
    /// bridge, but it must NOT be used on throughput-critical paths (that is
    /// what the async [`Limiter::run`] is for); bound its use to control-plane
    /// callers that genuinely cannot await.
    pub fn run_sync<F, Fut, T>(&self, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let block = async move {
            let _permit = self.sem.acquire().await.expect("semaphore closed");
            f().await
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| handle.block_on(block))
            }
            Ok(handle) => handle.block_on(block),
            Err(_) => common_core::runtime::block_on_shared(block),
        }
    }
}
