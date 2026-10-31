//! Pool error and control-plane pool traits (AFIT, generic, zero-cost).
//!
//! Re-exported through the `pool` facade so the stable
//! `fluent_concurrency::pool::{PoolError, SinkPool, RequestPool,
//! PriorityRequestPool}` paths keep resolving.

use thiserror::Error;

use super::result::ResultPoolError;

/// Errors returned by `Queue` and `WorkerPool` operations.
#[derive(Error, Debug, Clone, PartialEq)]
pub enum PoolError {
    #[error("queue is full")]
    Full,
    #[error("queue is closed")]
    Closed,
}

// ─── Pool trait family (AFIT) ────────────────────────────────────────────
// Native `async fn` in trait (AFIT, stable since 1.75) — no `async_trait`
// crate, no hidden `Box`. These are control-plane traits (not hot-loop per-item
// dispatch) and are deliberately **not** object-safe; pools are used
// monomorphized via generics so the submit path stays zero-cost/inlinable
// per `fluent-wvr/SKILL.md:70` "dyn at request boundary, concrete in tight loop".
// See `doc/skills/fluent-concurrency/SKILL.md:7.1`.
// `async_fn_in_trait` is allowed here — Send bounds are explicit via the
// `+ Send + Sync + 'static` bounds on associated types and the `Send` on the
// future is implied by the `Send + Sync` receiver; desugaring to
// `-> impl Future + Send` would be noisy for no gain in this internal crate.

/// Fire-and-forget pool — `WorkerPool` and `CreditGatedPool`.
///
/// `submit` enqueues without a per-job response channel (no `oneshot` alloc).
#[allow(async_fn_in_trait)]
pub trait SinkPool {
    type Job: Send + 'static;
    async fn submit(&self, job: Self::Job) -> Result<(), PoolError>;
    fn worker_count(&self) -> usize;
    async fn shutdown(self);
}

/// Request/response pool — `ResultPool` (one `oneshot` per submit).
#[allow(async_fn_in_trait)]
pub trait RequestPool {
    type Job: Send + Sync + 'static;
    type Output: Send + 'static;
    type Error: Send + 'static;
    async fn submit(&self, job: Self::Job) -> Result<Self::Output, ResultPoolError<Self::Error>>;
    async fn submit_with_abort(
        &self,
        job: Self::Job,
        abort: Option<crate::stream::StreamAbort>,
    ) -> Result<Self::Output, ResultPoolError<Self::Error>>;
    fn worker_count(&self) -> usize;
    async fn shutdown(self);
}

/// Priority-ordered request/response pool — `PriorityResultPool`.
#[allow(async_fn_in_trait)]
pub trait PriorityRequestPool {
    type Job: Send + Sync + 'static;
    type Output: Send + 'static;
    type Error: Send + 'static;
    async fn submit(
        &self,
        job: Self::Job,
        priority: i32,
    ) -> Result<Self::Output, ResultPoolError<Self::Error>>;
    async fn submit_with_abort(
        &self,
        job: Self::Job,
        priority: i32,
        abort: Option<crate::stream::StreamAbort>,
    ) -> Result<Self::Output, ResultPoolError<Self::Error>>;
    fn worker_count(&self) -> usize;
    async fn shutdown(self);
}
