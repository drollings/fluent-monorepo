//! Supervised trading session: feeds, per-bar supervision, bounded fan-out.
//!
//! All concurrency composes monorepo primitives — never raw `tokio::spawn`
//! outside scope/batch/pool, never ambient I/O outside capability-gated
//! entry points:
//!
//! - [`Scope`] owns feed tasks; dropping without close panics by design.
//! - [`SupervisedBatch`] runs each bar's units with containment-only
//!   supervision (a failing freshness check cancels its dependents; nothing
//!   restarts).
//! - [`ResultPool`] fans out venue submissions and returns each verdict.
//! - Credit flow bounds the producer: at most `credit_limit` submits are
//!   in flight before backpressure bites (**(correctness-as-boundedness)**).
//! - [`first_accept_in_order`] resolves quotes through the source ladder:
//!   primary first, stale-quote fallback last, trading blind never.

use chrono::{DateTime, Utc};
use fluent_concurrency::flow::{self, CreditReceiver, CreditSender, CreditSpec};
use fluent_concurrency::ladder::first_accept_in_order;
use fluent_concurrency::pool::{ResultPool, ResultPoolError};
use fluent_concurrency::scope::Scope;
use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::prelude::*;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

use protopyne_types::Bar;

use crate::command::{StateHandle, TradingState, TradingStateUnit};
use crate::error::VenueError;
use crate::journal::{Journal, VenueStorage};
use crate::market::MarketEvent;
use crate::order::{OrderIntent, OrderRecord};
use crate::quote::Quote;
use crate::registry::VenueRegistry;
use crate::stream::{StreamBreaker, StreamHealthUnit, SupervisedStream};
use crate::submit::safe_submit;
use crate::tastytrade::TastytradeTransport;
use crate::venue::{AccountEvent, QuoteSource, Venue};

/// Session tunables. The credit bound is **(correctness-as-boundedness)**:
/// queues stay finite under bursty chains; everything else sizes pools.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct SessionConfig {
    /// Submission workers draining the fan-out pool.
    #[field(desc = "Submission workers", min = 1, max = 64)]
    #[builder(default = 4)]
    pub workers: u64,
    /// Queue depth before submitters wait.
    #[field(desc = "Fan-out queue capacity", min = 1, max = 10_000)]
    #[builder(default = 64)]
    pub queue_capacity: u64,
    /// Max submits in flight before the producer blocks
    /// **(correctness-as-boundedness)**.
    #[field(desc = "Max submits in flight", min = 1, max = 10_000)]
    #[builder(default = 16)]
    pub credit_limit: u64,
}

impl protopyne_types::Validate for SessionConfig {
    type Err = String;
    fn validate(&self) -> Result<(), Self::Err> {
        SessionConfig::validate(self)
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

impl SessionConfig {
    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), String> {
        if self.workers < 1 {
            return Err("workers must be at least 1".to_owned());
        }
        if self.credit_limit < 1 {
            return Err("credit_limit must be at least 1".to_owned());
        }
        Ok(())
    }
}

impl WorkUnit for SessionConfig {
    fn name(&self) -> &str {
        "venue_session_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        protopyne_types::check_for_execute(self, "session config invalid")?;
        Ok(protopyne_types::valid_output("session config valid"))
    }
}

impl_component!(SessionConfig);

/// One fan-out job: an intent plus the quote it was priced against.
#[derive(Debug, Clone)]
pub struct SubmitJob {
    /// What to submit.
    pub intent: OrderIntent,
    /// Quote the fill prices against.
    pub quote: Quote,
    /// Decision time.
    pub at: DateTime<Utc>,
    /// Whether this job closes or reduces exposure (exits always route,
    /// even while halted); entries need [`TradingState::Enabled`].
    pub is_exit: bool,
}

/// The supervised session: feed scope, credit pair, fan-out pool, venue
/// registry.
///
/// **Explicit behavior change (execution routing):** construction takes a
/// [`VenueRegistry`] instead of an `Arc<PaperVenue>`. Single-venue callers
/// pass `Arc::new(VenueRegistry::single(paper))` and behave identically — a
/// second venue is otherwise impossible without forking the session.
pub struct TradingSession {
    scope: Scope,
    credit_tx: CreditSender,
    credit_rx: CreditReceiver,
    pool: ResultPool<SubmitJob, OrderRecord, VenueError>,
    registry: Arc<VenueRegistry>,
    storage: VenueStorage,
    stream_breaker: Option<Arc<StreamBreaker>>,
    state: StateHandle,
}

impl TradingSession {
    /// Boot a session: config validation, credit pair, and a result pool
    /// whose workers submit through the registry (which routes each intent
    /// to its venue). No task spawns here; feeds spawn under the owned
    /// [`Scope`] on demand.
    pub fn boot(
        config: &SessionConfig,
        registry: Arc<VenueRegistry>,
        storage: VenueStorage,
    ) -> Result<Self, VenueError> {
        config
            .validate()
            .map_err(VenueError::Invalid)?;
        // Bounded intake, two interlocking caps (both validated 1..=10_000):
        // the credit pair admits at most `credit_limit` submits in flight
        // (the producer waits for a bump instead of queueing), and the pool
        // queue holds at most `queue_capacity` jobs (overflow refuses `Full`
        // instead of growing). Keep `credit_limit` at or under
        // `queue_capacity` and `Full` stays unreachable.
        let (credit_tx, credit_rx) = flow::new(CreditSpec {
            initial: config.credit_limit as usize,
            more_after: 1,
        });
        let worker_registry = Arc::clone(&registry);
        let pool = ResultPool::new(
            fluent_concurrency::tokio_runtime(),
            config.workers as usize,
            config.queue_capacity as usize,
            move |job: SubmitJob| {
                let registry = Arc::clone(&worker_registry);
                async move { registry.submit(job.intent, &job.quote, job.at).await }
            },
        );
        Ok(Self {
            scope: Scope::new(),
            credit_tx,
            credit_rx,
            pool,
            registry,
            storage,
            stream_breaker: None,
            // Sessions boot allowing everything they allowed before the
            // command plane was wired: the gate is a no-op until the first
            // operator command is journaled.
            state: StateHandle::new(TradingState::Enabled),
        })
    }

    /// Whether the producer is currently credit-blocked.
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        self.credit_tx.is_blocked()
    }

    /// Publish one event sequence to N receivers through a single
    /// [`Scope`]-owned pump: the pump owns the only send loop, the returned
    /// sender fans out over `broadcast`, and every receiver subscribes and
    /// filters by symbol/kind itself (see [`MarketEvent::matches`]). Lagged
    /// receivers skip; closed ends the stream — the same policy as the
    /// rehearsal feed. `capacity` bounds the channel; shutdown drains the
    /// pump with the scope. Existing quote flow is untouched: the quote
    /// ladder still resolves through [`QuoteSource`]s.
    pub fn spawn_event_feed(
        &mut self,
        events: Vec<MarketEvent>,
        capacity: usize,
    ) -> broadcast::Sender<MarketEvent> {
        // Bounded fan-out: the ring holds at most `capacity` events
        // (floored at one — a zero-capacity broadcast is a refusal to fan
        // out); lagged receivers skip by policy, the pump never blocks.
        let (tx, _) = broadcast::channel(capacity.max(1));
        let pump = tx.clone();
        self.scope.spawn(async move {
            for event in events {
                if pump.send(event).is_err() {
                    break;
                }
            }
        });
        tx
    }

    /// Attach a supervised stream's breaker: every later [`TradingSession::run_bar`]
    /// prepends the stream-health sensor, so a tripped breaker cancels
    /// stream-dependent bar units through the batch graph. Unattached
    /// sessions run bars exactly as before (default-off wiring).
    pub fn attach_stream_breaker(&mut self, breaker: Arc<StreamBreaker>) {
        self.stream_breaker = Some(breaker);
    }

    /// Borrow the shared trading state: operator commands transition it
    /// through journaled [`CommandUnit`](crate::command::CommandUnit)s, and
    /// every bar plus every submission reads it. The handle shares
    /// ownership, so commands driven outside the session still gate it.
    #[must_use]
    pub fn state_handle(&self) -> &StateHandle {
        &self.state
    }

    /// Share a trading-state handle (e.g. one already driven by an operator
    /// command plane): later bars and submissions gate on it instead of the
    /// boot default.
    pub fn attach_trading_state(&mut self, state: StateHandle) {
        self.state = state;
    }

    /// Spawn a supervised reconnect driver under the owned scope and attach
    /// its breaker for later bars. The caller holds no task handle: `abort`
    /// is the consumer's cancellation lever and `Scope::close` (via
    /// [`TradingSession::shutdown`]) drains the driver. `connect`
    /// establishes one generation per call; each generation pumps through
    /// the transport's message loop. The `out` outbox stays caller-bounded:
    /// the pump `try_send`s (a full outbox skips by counted policy, never
    /// blocks), so size it for the slowest consumer you tolerate losing.
    pub fn spawn_supervised_stream<C, Fut>(
        &mut self,
        stream: &SupervisedStream,
        connect: C,
        abort: StreamAbort,
        out: mpsc::Sender<AccountEvent>,
        transport: Arc<TastytradeTransport>,
    ) where
        C: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = Result<mpsc::Receiver<String>, crate::tastytrade::TransportError>>
            + Send
            + 'static,
    {
        self.stream_breaker = Some(Arc::clone(stream.breaker()));
        let driver = stream.clone();
        self.scope.spawn(async move {
            let _ = driver.run(connect, &abort, out, &transport).await;
        });
    }

    /// Spawn a feed task under the owned scope: closed bars flow into a
    /// bounded channel, backpressuring the producer when full.
    pub fn spawn_feed(&mut self, bars: Vec<Bar>, capacity: usize) -> mpsc::Receiver<Bar> {
        // Bounded pipe: at most `capacity` bars buffer; the pump's send
        // awaits room, so burst history cannot outrun a slow consumer.
        let (tx, rx) = mpsc::channel(capacity);
        self.scope.spawn(async move {
            for bar in bars {
                if tx.send(bar).await.is_err() {
                    break;
                }
            }
        });
        rx
    }

    /// Credit-bounded submission: at most `credit_limit` submits stay in
    /// flight; the producer waits for a bump instead of growing a queue.
    /// The trading state gates first (before any credit is touched):
    /// halted entries refuse with reason, exits always route.
    pub async fn submit_order(
        &self,
        job: SubmitJob,
    ) -> Result<OrderRecord, ResultPoolError<VenueError>> {
        self.state
            .get()
            .check(job.is_exit)
            .map_err(ResultPoolError::Inner)?;
        let outcome = self
            .credit_tx
            .send(|| async { self.pool.submit(job).await })
            .await;
        self.credit_rx.recv();
        outcome
    }

    /// Submit operator close intents through the safe path: journal intent,
    /// dry-run, submit, query-by-ID on ambiguity — the same protocol as any
    /// other order, no privileged order path. Closes are exits, so they
    /// route even while halted. Retries are lookups: resubmitting an intent
    /// returns its original record, never a second order. The caller scopes
    /// the [`VenueStorage`] capability (as in [`safe_submit`]).
    pub async fn submit_close_intents(
        &self,
        journal: &Journal,
        approved: &protopyne_strategy::RiskApproved,
        intents: Vec<OrderIntent>,
        quote: &Quote,
        now: DateTime<Utc>,
    ) -> Result<Vec<OrderRecord>, VenueError> {
        let mut records = Vec::with_capacity(intents.len());
        for intent in intents {
            records.push(
                safe_submit(&self.storage, journal, self.registry.as_ref(), approved, intent, quote, now)
                    .await?,
            );
        }
        Ok(records)
    }

    /// Cancel operator-listed orders through the registry: the venue holding
    /// each id answers; unknown ids refuse with reason. Fail-closed on the
    /// first refusal; already-cancelled ids stay terminal and idempotent, so
    /// retrying the sweep is safe.
    pub async fn cancel_orders(
        &self,
        client_ids: &[String],
        now: DateTime<Utc>,
    ) -> Result<Vec<OrderRecord>, VenueError> {
        let mut records = Vec::with_capacity(client_ids.len());
        for client_id in client_ids {
            records.push(self.registry.cancel(client_id, now).await?);
        }
        Ok(records)
    }

    /// Run one bar's units under containment-only supervision. A failing
    /// unit cancels its dependents; nothing restarts. Units arrive wrapped
    /// in [`Instrumented`] before type erasure. The trading state is
    /// consulted on every bar through the same fail-closed shape as health:
    /// while entries are halted the trading-state sensor runs first, failing
    /// with its reason and cancelling entry-carrying units while exit and
    /// monitor units still complete. Enabled bars run exactly as before
    /// (no sensor is registered, so summaries are byte-identical).
    /// While a stream is attached, the stream-health sensor runs alongside:
    /// a tripped breaker fails that sensor, cancelling stream-dependent
    /// units while independent units still complete.
    pub async fn run_bar(
        &self,
        units: Vec<Arc<dyn Component>>,
    ) -> fluent_concurrency::batch::SupervisedBatchSummary {
        use fluent_concurrency::batch::{SupervisedBatchError, SupervisedBatchEvent};
        let caps = fluent_wvr::CapabilitySet::new().with(self.storage.clone());
        let mut batch =
            fluent_concurrency::batch::SupervisedBatch::new(fluent_concurrency::tokio_runtime(), caps);
        let mut registration_failure: Option<SupervisedBatchError> = None;
        let mut units = units;
        if let Some(breaker) = &self.stream_breaker {
            units.insert(
                0,
                Arc::new(StreamHealthUnit::new(Arc::clone(breaker))) as Arc<dyn Component>,
            );
        }
        if !self.state.get().allows(false) {
            units.insert(
                0,
                Arc::new(TradingStateUnit::new(self.state.clone())) as Arc<dyn Component>,
            );
        }
        for unit in units {
            let label = unit.name().to_owned();
            let wrapped = Instrumented::new(
                ComponentAdapter::new(unit).with_name_override(label.clone()),
                label,
            );
            let erased: Arc<dyn Component> = Arc::new(wrapped);
            if let Err(e) = batch.register(erased) {
                registration_failure = Some(e);
                break;
            }
        }
        let mut summary = batch.await;
        // A bar that could not register every unit must say so in the
        // summary: silently breaking out runs the bar short while reporting
        // clean, which no audit can distinguish from a bar that ran whole.
        if let Some(SupervisedBatchError::DuplicateName(name)) = registration_failure {
            summary.failed.push(SupervisedBatchEvent::Failed {
                name,
                error: WorkError::Execution("bar registration rejected a duplicate unit name".to_owned()),
            });
        }
        summary
    }

    /// Resolve the newest quote through the source ladder: first source
    /// with a quote wins; exhaustion means every feed is dark (fail closed).
    pub async fn resolve_quote(
        sources: Vec<Arc<dyn QuoteSource>>,
        underlying: &str,
    ) -> Option<(Quote, String)> {
        let walk = first_accept_in_order(
            sources,
            |source: Arc<dyn QuoteSource>| {
                let name = source.name().to_owned();
                let found = source.latest(underlying).map(|quote| (quote, name));
                async move { Ok::<Option<(Quote, String)>, VenueError>(found) }
            },
            |_: &VenueError| false,
        )
        .await;
        walk.ok().flatten()
    }

    /// Borrow the registry (reconciliation reads truth through here; the
    /// registry unions truth across its venues in chain order).
    #[must_use]
    pub fn registry(&self) -> &Arc<VenueRegistry> {
        &self.registry
    }

    /// Shut down: close the feed scope, then drain the pool.
    pub async fn shutdown(self) {
        let mut scope = self.scope;
        scope.close().await;
        self.pool.shutdown().await;
    }
}

/// Wrap a synchronous check as a supervised bar unit: `check` runs inside
/// `execute` (sync, non-blocking, no hidden I/O) and its failure cancels
/// dependent units through the batch graph.
#[must_use]
pub fn check_unit(
    inner: Arc<dyn Component>,
    name: &str,
    check: impl Fn() -> Result<String, String> + Send + Sync + 'static,
) -> Arc<dyn Component> {
    let label = name.to_owned();
    let adapter = ComponentAdapter::new(inner)
        .with_name_override(label)
        .with_execute_override(Arc::new(move |ctx: &WorkContext| {
            let mut ctx = ctx.clone();
            match check() {
                Ok(message) => {
                    ctx.set("check.message", message.clone());
                    Ok(WorkOutput::ok(message))
                }
                Err(reason) => Err(WorkError::Execution(reason)),
            }
        }));
    Arc::new(adapter)
}

#[cfg(test)]
#[path = "../tests/session.rs"]
mod tests;
