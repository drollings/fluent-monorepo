//! Credit-gated bounded worker pool: a [`WorkerPool`] whose submissions are
//! additionally bounded by a [`CreditFlow`] pair (chain backpressure).
//!
//! The shape was extracted from the router's two structurally-identical
//! workers — the parse-review worker and the entity-link overlay worker
//! (`server/review.rs`, `server/entity_link.rs`). Each worker is a
//! `WorkerPool` whose `submit` path acquires a credit token before enqueuing
//! and whose handler releases the token via [`crate::flow::CreditReceiver::recv`] after
//! each processed job. Centralizing it removes the drift risk that a third
//! copy-pasted worker gets the `CreditSpec` formulas or the worker cap wrong.
//!
//! **Load-bearing tuning.** The defaults in [`CreditGatedConfig`] (worker
//! cap `2`, `initial = credit_limit.max(1)`, `more_after =
//! (credit_limit / 2).max(1)`) match both originating workers byte-for-byte —
//! do not "improve" them without changing every consumer's behavior.
//!
//! [`CreditFlow`]: crate::flow

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use fluent_wvr::Runtime;
use tokio::sync::{Mutex, Notify};

use crate::flow::{self, CreditSender, CreditSpec};
use crate::pool::{PoolError, SinkPool, WorkerPool};

/// Tuning for [`CreditGatedPool`]: the worker cap and the credit-formula
/// parameters, previously hard-coded magic (`WORKER_CAP = 2`,
/// `initial = limit.max(1)`, `more_after = (limit / 2).max(1)`).
/// Defaults reproduce those values byte-for-byte.
#[derive(Debug, Clone, Copy)]
pub struct CreditGatedConfig {
    /// Worker task count.
    pub worker_cap: usize,
    /// Floor for the initial credit (`credit_limit.max(..)`).
    pub min_initial_credit: usize,
    /// Divisor for the refill batch (`credit_limit / ..`).
    pub refill_divisor: usize,
    /// Floor for the refill batch (`(..).max(..)`).
    pub min_refill_batch: usize,
}

impl Default for CreditGatedConfig {
    fn default() -> Self {
        Self {
            worker_cap: 2,
            min_initial_credit: 1,
            refill_divisor: 2,
            min_refill_batch: 1,
        }
    }
}

/// A bounded worker pool whose submissions are gated by a credit flow.
///
/// [`submit`](Self::submit) acquires a credit token before enqueuing (blocking
/// only when credit is exhausted — the hot path returns immediately
/// otherwise) and the handler releases the token once per processed job. A
/// drained pool returns [`PoolError::Closed`]; the inner queue's full state is
/// handled by backpressure (`submit` waits for space — `PoolError::Full` is
/// the synchronous fast-path error and is never returned on the submit path).
pub struct CreditGatedPool<J: Send + 'static> {
    pool: Mutex<Option<Arc<WorkerPool<J>>>>,
    /// Set under the pool mutex in `drain`, checked under the same mutex
    /// in `submit`: submit-before-drain runs, drain-before-submit is
    /// `Closed`, and a concurrent pair resolves to either (both sound —
    /// the in-flight counter below keeps the pool alive for the winner).
    closed: AtomicBool,
    /// Snapshots currently outside the mutex (submitted but not yet
    /// returned). `drain` waits for zero before shutting down, so the
    /// shutdown always observes the last `Arc`.
    inflight: AtomicUsize,
    /// Woken when `inflight` reaches zero.
    drain_wake: Notify,
    credit: CreditSender,
    worker_count: usize,
}

/// Balances the [`CreditGatedPool::inflight`] count on every exit path,
/// including task cancellation (which drops the submit future).
struct InFlight<'a, J: Send + 'static> {
    pool: &'a CreditGatedPool<J>,
}

impl<J: Send + 'static> Drop for InFlight<'_, J> {
    fn drop(&mut self) {
        if self.pool.inflight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.pool.drain_wake.notify_waiters();
        }
    }
}

impl<J: Send + Sync + 'static> CreditGatedPool<J> {
    /// Construct the pool. `credit_limit` bounds in-flight jobs (chain
    /// backpressure); `queue_capacity` bounds the pool queue. Tuning comes
    /// from [`CreditGatedConfig::default`] — identical to the historical
    /// values (worker cap `2`, floors `1`, refill divisor `2`).
    #[allow(clippy::needless_pass_by_value)]
    pub fn new<F, Fut>(
        runtime: Arc<dyn Runtime>,
        credit_limit: usize,
        queue_capacity: usize,
        handler: F,
    ) -> Self
    where
        F: Fn(J) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send,
    {
        Self::new_with_config(
            runtime,
            credit_limit,
            queue_capacity,
            handler,
            CreditGatedConfig::default(),
        )
    }

    /// Construct the pool with explicit tuning (same semantics as [`new`](Self::new)).
    #[allow(clippy::needless_pass_by_value)]
    pub fn new_with_config<F, Fut>(
        runtime: Arc<dyn Runtime>,
        credit_limit: usize,
        queue_capacity: usize,
        handler: F,
        config: CreditGatedConfig,
    ) -> Self
    where
        F: Fn(J) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let (credit, credit_receiver) = flow::new(CreditSpec {
            initial: credit_limit.max(config.min_initial_credit),
            more_after: (credit_limit / config.refill_divisor.max(1)).max(config.min_refill_batch),
        });
        let receiver = Arc::new(credit_receiver);
        let worker_cap = config.worker_cap.max(1);
        let handler = Arc::new(handler);
        let pool = Arc::new(WorkerPool::new(
            runtime,
            worker_cap,
            queue_capacity,
            move |job: J| {
                let receiver = Arc::clone(&receiver);
                let handler = Arc::clone(&handler);
                async move {
                    handler(job).await;
                    // Release the credit token so the next submit can proceed.
                    receiver.recv();
                }
            },
        ));

        Self {
            pool: Mutex::new(Some(pool)),
            closed: AtomicBool::new(false),
            inflight: AtomicUsize::new(0),
            drain_wake: Notify::new(),
            credit,
            worker_count: worker_cap,
        }
    }

    /// Enqueue a job, bounded by the credit gate (blocks only when credit is
    /// exhausted — the hot path returns immediately otherwise).
    ///
    /// Only the closed-check + handle snapshot run under the pool mutex;
    /// the guard is dropped before the credit gate, so producers no longer
    /// serialize on the mutex while waiting for credit.
    pub async fn submit(&self, job: J) -> Result<(), PoolError> {
        // Count first so `drain` can wait out snapshots; the guard balances
        // the count on every exit (cancel-safe).
        self.inflight.fetch_add(1, Ordering::AcqRel);
        let _in_flight = InFlight { pool: self };
        let pool = {
            let guard = self.pool.lock().await;
            if self.closed.load(Ordering::Acquire) {
                return Err(PoolError::Closed);
            }
            let Some(pool) = guard.as_ref() else {
                return Err(PoolError::Closed);
            };
            Arc::clone(pool)
        };
        self.credit.send(|| pool.submit(job)).await
    }

    /// Whether the credit gate is currently blocking producers.
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        self.credit.is_blocked()
    }

    /// Drain in-flight jobs and shut the pool down (graceful shutdown). The
    /// pool is unusable afterward. Idempotent: concurrent or repeated drains
    /// take the pool at most once and all return after shutdown.
    pub async fn drain(&self) {
        let pool = {
            let mut guard = self.pool.lock().await;
            self.closed.store(true, Ordering::Release);
            guard.take()
        };
        // Wait out snapshotted submitters (they hold the only other `Arc`s
        // and balance `inflight` on exit, even on cancellation). No new
        // snapshot can be taken past the closed flag + take above. The
        // register-then-check order closes the missed-notification race.
        loop {
            let notified = self.drain_wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.inflight.load(Ordering::Acquire) == 0 {
                break;
            }
            notified.await;
        }
        if let Some(pool) = pool {
            match Arc::try_unwrap(pool) {
                Ok(pool) => pool.shutdown().await,
                Err(_) => debug_assert!(
                    false,
                    "in-flight count reached zero but the pool Arc is still shared"
                ),
            }
        }
    }

    /// The number of worker tasks processing jobs.
    #[must_use]
    pub fn worker_count(&self) -> usize {
        self.worker_count
    }
}

impl<J: Send + Sync + 'static> SinkPool for CreditGatedPool<J> {
    type Job = J;
    async fn submit(&self, job: Self::Job) -> Result<(), PoolError> {
        CreditGatedPool::submit(self, job).await
    }
    fn worker_count(&self) -> usize {
        CreditGatedPool::worker_count(self)
    }
    async fn shutdown(self) {
        CreditGatedPool::drain(&self).await;
    }
}

#[cfg(test)]
#[path = "../tests/credit_pool.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/m-credit-pool-race.rs"]
mod m_credit_pool_race_tests;
