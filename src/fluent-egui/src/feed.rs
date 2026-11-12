//! Concurrency perimeter: feeds, waves, caps, aborts, backpressure.
//!
//! History and live bars flow through a [`ChartFeed`]: a `Scope`-owned set
//! of feed tasks pushing [`ChartBars`] over a bounded channel, so a burst
//! tape can never grow the queue. Pane recompute waves run as
//! [`PaneWaveUnit`]s in a [`SupervisedBatch`] ordered by the pane plan — a
//! failed unit cancels its transitive dependents only. Heavy series rebuilds
//! fan out through caller-composed [`ResultPool`]s; preview/refresh passes
//! through a [`RefreshGate`]; closing a [`PaneHandle`] fires its
//! [`StreamAbort`], the single source of truth for "consumer gone". Intake
//! is credit-bounded ([`CreditIntake`]), history sources fall back through
//! the canonical first-Ok ladder ([`load_history`]), and transport retries
//! compose the shared jittered-backoff helper ([`TransportRetry]).
//!
//! Capacities measure boundedness correctness (queues must not grow without
//! bound), sized from history-length fixtures and recorded as observed
//! wall-time — never asserted, never tuned to pass a benchmark. No raw
//! spawns, no sleeps, no blocking I/O in `execute` bodies or in paint code.
//!
//! [`ResultPool`]: fluent_concurrency::pool::ResultPool

use std::sync::{Arc, Mutex};

use common_core::retry::retry_async;
use common_core::sync::lock;
use fluent_concurrency::batch::{SupervisedBatch, SupervisedBatchError, SupervisedBatchSummary};
use fluent_concurrency::capability::default_capability_set;
use fluent_concurrency::flow::{self, CreditReceiver, CreditSender, CreditSpec};
use fluent_concurrency::ladder::first_accept_in_order;
use fluent_concurrency::pool::{Limiter, ResultPool, ResultPoolError};
use fluent_concurrency::scope::Scope;
use fluent_concurrency::stream::StreamAbort;
use fluent_concurrency::tokio_runtime;
use fluent_wvr::prelude::*;
use fluent_wvr::{Describable, FieldAccess};
use protopyne_chart_model::ChartBars;
use tokio::sync::mpsc;

use crate::error::EguiError;
use crate::plan::PanePlan;

/// `Scope`-owned history and live feed pushing [`ChartBars`] over a bounded
/// channel.
///
/// Dropping without [`close`](Self::close) aborts feed tasks synchronously;
/// closing the feed drains them first. Either way tasks never outlive the feed.
pub struct ChartFeed {
    scope: Scope,
    tx: mpsc::Sender<ChartBars>,
}

impl ChartFeed {
    /// Open a feed whose channel holds at most `queue_capacity` batches;
    /// refuses a zero capacity with a reason.
    pub fn new(queue_capacity: usize) -> Result<(Self, mpsc::Receiver<ChartBars>), EguiError> {
        if queue_capacity == 0 {
            return Err(EguiError::InvalidParam {
                what: "queue_capacity".to_owned(),
                reason: "feed channel must hold at least one batch".to_owned(),
            });
        }
        let (tx, rx) = mpsc::channel(queue_capacity);
        Ok((
            Self {
                scope: Scope::new(),
                tx,
            },
            rx,
        ))
    }

    /// Clone the batch sender for producers outside the scope.
    #[must_use]
    pub fn sender(&self) -> mpsc::Sender<ChartBars> {
        self.tx.clone()
    }

    /// Spawn a scope-owned task pushing `batches` in order; stops early when
    /// every receiver is gone.
    pub fn spawn_history(&mut self, batches: Vec<ChartBars>) {
        let tx = self.tx.clone();
        let _ = self.scope.spawn(async move {
            for batch in batches {
                if tx.send(batch).await.is_err() {
                    break;
                }
            }
        });
    }

    /// Drain feed tasks, awaiting their completion.
    pub async fn close(&mut self) {
        self.scope.close().await;
    }
}

impl Drop for ChartFeed {
    fn drop(&mut self) {
        self.scope.close_sync();
    }
}

/// One pane's recompute wave as a supervised unit.
///
/// `execute` recounts kept display points (finite `Some` legs) — the
/// skip-policy recompute in miniature — and reports the kept/total tally.
/// `fault` injects a deterministic failure (fault-injection surface, never
/// set in production wiring); a failed unit cancels its transitive
/// dependents in the batch and nothing else.
#[derive(Debug, Clone, FieldAccess, Describable)]
/// One pane's recompute wave as a supervised unit.
/// 
/// `execute` recounts kept display points (finite `Some` legs) — the
/// skip-policy recompute in miniature — and reports the kept/total tally.
/// `fault` injects a deterministic failure (fault-injection surface, never
/// set in production wiring); a failed unit cancels its transitive
/// dependents in the batch and nothing else.
pub struct PaneWaveUnit {
    /// Pane name (plan node).
    #[field(desc = "Pane name (plan node)")]
    pub name: String,
    /// Upstream pane names, interned once at construction.
    #[field(skip)]
    pub depends: Vec<ArcIntern<str>>,
    /// Provided asset (the pane itself).
    #[field(skip)]
    pub provides: Vec<ArcIntern<str>>,
    /// Series under recompute.
    #[field(skip)]
    pub values: Vec<Option<f64>>,
    /// Injected failure reason; empty means healthy.
    #[field(desc = "Injected failure reason; empty means healthy")]
    pub fault: Option<String>,
}

impl PaneWaveUnit {
    /// Bind one pane's wave: dependencies come from the plan so wave edges
    /// can never drift from plan edges.
    #[must_use]
    pub fn for_plan(plan: &PanePlan, name: &str, values: Vec<Option<f64>>) -> Self {
        let depends = plan
            .dependencies(name)
            .iter()
            .map(|d| ArcIntern::from(d.as_str()))
            .collect();
        Self {
            name: name.to_owned(),
            depends,
            provides: vec![ArcIntern::from(name)],
            values,
            fault: None,
        }
    }

    /// Inject a deterministic failure for cancellation tests.
    #[must_use]
    pub fn with_fault(mut self, reason: impl Into<String>) -> Self {
        self.fault = Some(reason.into());
        self
    }
}

impl WorkUnit for PaneWaveUnit {
    fn name(&self) -> &str {
        &self.name
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &self.depends
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        if let Some(reason) = &self.fault {
            return Err(WorkError::execution_display(reason.clone()));
        }
        let kept = self
            .values
            .iter()
            .filter(|v| matches!(v, Some(x) if x.is_finite()))
            .count();
        Ok(WorkOutput::ok(format!(
            "{}: {}/{} kept",
            self.name,
            kept,
            self.values.len()
        )))
    }
}

impl_component!(PaneWaveUnit);

/// Run one recompute wave: register every unit, then drain the batch to its
/// summary. Duplicate pane names refuse at registration.
pub async fn run_wave(
    units: Vec<PaneWaveUnit>,
) -> Result<SupervisedBatchSummary, SupervisedBatchError> {
    let mut batch = SupervisedBatch::new(tokio_runtime(), default_capability_set());
    for unit in units {
        batch.register(Arc::new(unit))?;
    }
    Ok(batch.await)
}

/// Preview/refresh concurrency gate: at most `permits` refreshes in flight.
pub struct RefreshGate {
    limiter: Limiter,
}

impl RefreshGate {
    /// Open the gate; refuses a zero permit count with a reason.
    pub fn new(permits: usize) -> Result<Self, EguiError> {
        if permits == 0 {
            return Err(EguiError::InvalidParam {
                what: "permits".to_owned(),
                reason: "refresh gate needs at least one permit".to_owned(),
            });
        }
        Ok(Self {
            limiter: Limiter::new(permits),
        })
    }

    /// Run `work` under the gate.
    pub async fn run<F, Fut, T>(&self, work: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        self.limiter.run(work).await
    }
}

/// Pane lifetime handle: closing fires the abort signal, the single source
/// of truth for "consumer gone". Feed and recompute tasks select their
/// upstream reads against it so a closed pane never wedges a task.
#[derive(Clone)]
/// Pane lifetime handle: closing fires the abort signal, the single source
/// of truth for "consumer gone". Feed and recompute tasks select their
/// upstream reads against it so a closed pane never wedges a task.
pub struct PaneHandle {
    abort: StreamAbort,
}

impl PaneHandle {
    /// Open a handle with a live (unfired) signal.
    #[must_use]
    pub fn new() -> Self {
        Self {
            abort: StreamAbort::new(),
        }
    }

    /// Borrow the abort signal for task `select!` wiring.
    #[must_use]
    pub fn abort(&self) -> &StreamAbort {
        &self.abort
    }

    /// Close the pane: fire the signal (idempotent), waking every waiter.
    pub fn close(&self) {
        self.abort.cancel();
    }

    /// Whether the pane is closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.abort.is_cancelled()
    }
}

impl Default for PaneHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Credit-bounded intake: producers spend credit per batch, the consumer
/// releases it per processed batch, so a burst tape cannot grow the queue.
pub struct CreditIntake {
    sender: CreditSender,
    receiver: CreditReceiver,
}

impl CreditIntake {
    /// Open the pair; refuses zero limits with reasons.
    pub fn new(credit_limit: usize, more_after: usize) -> Result<Self, EguiError> {
        if credit_limit == 0 {
            return Err(EguiError::InvalidParam {
                what: "credit_limit".to_owned(),
                reason: "intake needs at least one credit".to_owned(),
            });
        }
        if more_after == 0 {
            return Err(EguiError::InvalidParam {
                what: "more_after".to_owned(),
                reason: "intake must release credit after processing".to_owned(),
            });
        }
        let (sender, receiver) = flow::new(CreditSpec {
            initial: credit_limit,
            more_after,
        });
        Ok(Self { sender, receiver })
    }

    /// Run `op` once credit allows; waits on exhaustion instead of queueing
    /// without bound.
    pub async fn send<F, Fut, T>(&self, op: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        self.sender.send(op).await
    }

    /// Release credit after one processed batch.
    pub fn release(&self) {
        self.receiver.recv();
    }

    /// Whether a producer is currently waiting on credit.
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        self.sender.is_blocked()
    }
}

/// One history source on the fallback ladder.
#[derive(Debug, Clone)]
/// One history source on the fallback ladder.
pub struct HistoryRung {
    /// Source name for diagnostics.
    pub name: String,
    /// Bars on hit; `None` means this rung misses.
    pub rows: Option<ChartBars>,
    /// Whether a miss here is terminal (abort the ladder) rather than a skip.
    pub terminal: bool,
}

/// History ladder failure: a terminal rung aborted the walk.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
/// History ladder failure: a terminal rung aborted the walk.
pub enum HistoryError {
    /// A terminal rung refused; the walk aborted with this rung's name.
    #[error("terminal history failure at {0}")]
    Terminal(String),
}

/// Load history through the canonical first-Ok ladder: the first rung with
/// bars wins, misses skip to the next rung, terminal misses abort the walk,
/// and total exhaustion is a clean `Ok(None)`.
pub async fn load_history(rungs: Vec<HistoryRung>) -> Result<Option<ChartBars>, HistoryError> {
    first_accept_in_order(rungs, run_rung, |error| {
        matches!(error, HistoryError::Terminal(_))
    })
    .await
}

async fn run_rung(rung: HistoryRung) -> Result<Option<ChartBars>, HistoryError> {
    match rung {
        HistoryRung {
            rows: Some(rows), ..
        } => Ok(Some(rows)),
        HistoryRung {
            name,
            terminal: true,
            ..
        } => Err(HistoryError::Terminal(name)),
        HistoryRung {
            terminal: false, ..
        } => Ok(None),
    }
}

/// Transport retry policy composing the shared jittered-backoff helper.
#[derive(Debug, Clone, Copy)]
/// Transport retry policy composing the shared jittered-backoff helper.
pub struct TransportRetry {
    /// Attempts per call (at least 1).
    pub attempts: u32,
    /// Backoff base in milliseconds.
    pub base_ms: u64,
    /// Jitter percent.
    pub jitter_pct: u32,
}

impl Default for TransportRetry {
    fn default() -> Self {
        Self {
            attempts: 3,
            base_ms: 100,
            jitter_pct: 50,
        }
    }
}

impl TransportRetry {
    /// Build the policy; refuses a zero attempt count with a reason.
    pub fn new(attempts: u32, base_ms: u64, jitter_pct: u32) -> Result<Self, EguiError> {
        if attempts == 0 {
            return Err(EguiError::InvalidParam {
                what: "attempts".to_owned(),
                reason: "transport retry needs at least one attempt".to_owned(),
            });
        }
        Ok(Self {
            attempts,
            base_ms,
            jitter_pct,
        })
    }

    /// Run `op` to success or exhaustion: non-retryable errors short-circuit
    /// immediately, retryable ones back off between attempts.
    pub async fn run<T, E, F, Fut>(
        &self,
        is_retryable: impl Fn(&E) -> bool,
        mut op: F,
    ) -> Result<T, E>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        retry_async(
            self.attempts,
            self.base_ms,
            self.jitter_pct,
            is_retryable,
            &mut op,
        )
        .await
    }
}

/// One owned async binding: a single in-flight pool submission whose latest
/// outcome the frame polls.
///
/// `Idle` is the rest state, `Pending` the in-flight state, `Finished` the
/// delivered result, `Failed` the delivered taxonomy. Failures carry the
/// `WorkError` taxonomy, never strings: a timed-out or refused job routes to
/// its own arm, and a pool-level fault arrives as an execution failure naming
/// its cause.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One owned async binding: a single in-flight pool submission whose latest
/// outcome the frame polls.
/// 
/// `Idle` is the rest state, `Pending` the in-flight state, `Finished` the
/// delivered result, `Failed` the delivered taxonomy. Failures carry the
/// `WorkError` taxonomy, never strings: a timed-out or refused job routes to
/// its own arm, and a pool-level fault arrives as an execution failure naming
/// its cause.
pub enum BindState<T, E> {
    /// No job has run yet, or the last attempt never left the queue.
    Idle,
    /// A job is running; resubmits coalesce until it settles.
    Pending,
    /// The job delivered its value.
    Finished(T),
    /// The job delivered its failure.
    Failed(E),
}

/// An async value bound to a pane's lifetime: jobs run as pool submissions
/// under the pane's abort handle, the frame polls the latest outcome through
/// a lock-free read, and the settler wakes the frame exactly once per
/// delivery.
///
/// The bind never spawns: the caller drives `submit` from its own task (a
/// feed task, a test task), and the pool owns the worker. Closing the pane
/// fires the abort, so a late completion observes the dead handle and is
/// never delivered — and never wakes the frame.
pub struct AsyncBind<J, T>
where
    J: Send + 'static,
    T: Send + 'static,
{
    pool: Arc<ResultPool<J, T, WorkError>>,
    gate: Mutex<()>,
    abort: Mutex<StreamAbort>,
    state_tx: tokio::sync::watch::Sender<BindState<T, WorkError>>,
    state_rx: tokio::sync::watch::Receiver<BindState<T, WorkError>>,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl<J, T> AsyncBind<J, T>
where
    J: Send + 'static,
    T: Send + 'static,
{
    /// Bind a pool to a pane's abort line. `notify` wakes the frame once per
    /// delivery — the shell passes its repaint, headless callers a counter.
    pub fn new(
        pool: Arc<ResultPool<J, T, WorkError>>,
        abort: StreamAbort,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (state_tx, state_rx) = tokio::sync::watch::channel(BindState::Idle);
        Self {
            pool,
            gate: Mutex::new(()),
            abort: Mutex::new(abort),
            state_tx,
            state_rx,
            notify: Arc::new(notify),
        }
    }

    /// The latest outcome. A lock-free read for the frame path: polling
    /// never blocks, never wakes, and never disturbs the in-flight job.
    #[must_use]
    pub fn poll(&self) -> tokio::sync::watch::Ref<'_, BindState<T, WorkError>> {
        self.state_rx.borrow()
    }

    /// Run `job` on the pool unless one is already in flight (coalesced).
    /// Delivers exactly once: a completion past a fired abort is swallowed,
    /// a pool-level fault arrives as an execution failure, and only a
    /// delivery wakes the frame.
    pub async fn submit(&self, job: J)
    where
        J: Send + Sync + 'static,
        T: Send + 'static,
    {
        {
            let _gate = lock(&self.gate);
            if matches!(*self.state_rx.borrow(), BindState::Pending) {
                return;
            }
            let _ = self.state_tx.send(BindState::Pending);
        }
        let abort = lock(&self.abort).clone();
        let outcome = self.pool.submit_with_abort(job, Some(abort.clone())).await;
        if abort.is_cancelled() {
            return;
        }
        let state = match outcome {
            Ok(value) => BindState::Finished(value),
            Err(ResultPoolError::Inner(error)) => BindState::Failed(error),
            Err(other) => BindState::Failed(WorkError::execution_display(other)),
        };
        let _ = self.state_tx.send(state);
        (self.notify)();
    }

    /// Restart after the previous attempt settled or its abort fired: install
    /// the pane's current abort line, drop the stale outcome, and submit.
    /// Only restart on a dead or settled bind — a live attempt the old abort
    /// still covers would deliver underneath the new one.
    pub async fn restart(&self, abort: StreamAbort, job: J)
    where
        J: Send + Sync + 'static,
        T: Send + 'static,
    {
        *lock(&self.abort) = abort;
        let _ = self.state_tx.send(BindState::Idle);
        self.submit(job).await;
    }
}

/// Submit one heavy rebuild to `pool` under the pane's abort line: a close
/// mid-flight cancels the rebuild and frees the worker slot immediately,
/// instead of completing into a closed handle. Nothing else changes — the
/// pool, the job, and the result path are the caller's.
pub async fn submit_rebuild<J, T, E>(
    pool: &ResultPool<J, T, E>,
    handle: &PaneHandle,
    job: J,
) -> Result<T, ResultPoolError<E>>
where
    J: Send + Sync + 'static,
    T: Send + 'static,
    E: Send + 'static,
{
    pool.submit_with_abort(job, Some(handle.abort().clone()))
        .await
}

#[cfg(test)]
#[path = "../tests/feed.rs"]
mod tests;