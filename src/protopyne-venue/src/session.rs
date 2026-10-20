//! Supervised trading session: feeds, per-bar supervision, operator closes.
//!
//! All concurrency composes monorepo primitives — never raw `tokio::spawn`
//! outside scope/batch, never ambient I/O outside capability-gated
//! entry points:
//!
//! - [`Scope`] owns feed tasks; dropping without close panics by design.
//! - [`SupervisedBatch`] runs each bar's units with containment-only
//!   supervision (a failing freshness check cancels its dependents; nothing
//!   restarts).
//! - [`first_accept_in_order`] resolves quotes through the source ladder:
//!   primary first, stale-quote fallback last, trading blind never.

use chrono::{DateTime, Utc};
use fluent_concurrency::ladder::first_accept_in_order;
use fluent_concurrency::scope::Scope;
use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::prelude::*;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

use protopyne_types::Bar;
use protopyne_types::SeriesContext;

use protopyne_strategy::{PortfolioFactsSource, RiskLimits};

use crate::backfill::{BackfillHandle, BackfillSource, BackfillUnit};
use crate::command::{StateHandle, TradingState, TradingStateUnit};
use crate::error::VenueError;

use crate::journal::{Journal, VenueStorage};
use crate::market::MarketEvent;
use crate::order::{OrderIntent, OrderRecord};
use crate::quote::Quote;
use crate::reading::GateReading;
use crate::reconcile::{ReconcileHandle, ReconcileSource, ReconcileUnit};
use crate::registry::VenueRegistry;
use crate::stream::{StreamBreaker, StreamHealthUnit, SupervisedStream};
use crate::submit::safe_submit;
use crate::tastytrade::TastytradeTransport;
use crate::venue::{AccountEvent, QuoteSource, Venue};

/// Session tunables: risk caps plus the shutdown drain bound.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct SessionConfig {
    /// Risk caps the session boots against. Aggregate caps require an
    /// attached [`PortfolioFactsSource`], so a session that configures one
    /// with no producer refuses at construction.
    #[field(skip)]
    #[serde(default)]
    #[builder(default)]
    pub risk_limits: RiskLimits,
    /// Shutdown drain bound in seconds: closing the feed scope must complete
    /// inside it, so a kill mid-loop never hangs shutdown behind a slow pump.
    #[field(desc = "Shutdown drain timeout in seconds", min = 1, max = 3600)]
    #[builder(default = 30)]
    pub shutdown_timeout_secs: u64,
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
        if self.shutdown_timeout_secs < 1 {
            return Err("shutdown_timeout_secs must be at least 1".to_owned());
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

/// The supervised session: feed scope plus the venue registry.
///
/// Orders reach the venue through exactly one path — the safe protocol
/// ([`safe_submit`], journal intent → dry-run → submit → query-by-ID),
/// driven either by the runner's pipeline verdict or by the operator close
/// path below. Both demand the risk token; there is no token-free order
/// path left in the session.
///
/// **Explicit behavior change (execution routing):** construction takes a
/// [`VenueRegistry`] instead of an `Arc<PaperVenue>`. Single-venue callers
/// pass `Arc::new(VenueRegistry::single(paper))` and behave identically — a
/// second venue is otherwise impossible without forking the session.
pub struct TradingSession {
    scope: Scope,
    registry: Arc<VenueRegistry>,
    storage: VenueStorage,
    stream_breaker: Option<Arc<StreamBreaker>>,
    credential: Option<crate::auth::AuthHandle>,
    reconcile: Option<ReconcileSensor>,
    backfill: Option<BackfillSensor>,
    state: StateHandle,
    portfolio_facts: Option<Arc<dyn PortfolioFactsSource>>,
    /// Shutdown drain bound: closing the feed scope must complete inside it.
    shutdown_timeout_secs: u64,
}

/// The session-owned reconciliation sensor and its shared gate. Attached
/// only where the venue can diverge from the journal; a session with no
/// attached sensor runs every bar exactly as before.
struct ReconcileSensor {
    unit: Arc<dyn Component>,
    handle: ReconcileHandle,
}

/// The session-owned backfill sensor and its shared gate. Attached only
/// where bars arrive over a supervised stream that can drop; a session
/// with no attached sensor runs every bar exactly as before. The concrete
/// unit is retained (not just the erased component) so the feed pump can
/// hand each reconnected bar to [`BackfillUnit::note_bar`] before any
/// consumer reads it.
struct BackfillSensor {
    unit: Arc<BackfillUnit>,
    handle: BackfillHandle,
}

impl TradingSession {
    /// Boot a session: config validation plus the owned feed scope. No task
    /// spawns here; feeds spawn under the owned [`Scope`] on demand.
    pub fn boot(
        config: &SessionConfig,
        registry: Arc<VenueRegistry>,
        storage: VenueStorage,
        portfolio_facts: Option<Arc<dyn PortfolioFactsSource>>,
    ) -> Result<Self, VenueError> {
        config.validate().map_err(VenueError::Invalid)?;
        // Construction-time fail-closed: an aggregate cap with no facts
        // producer could only fail open at the first check, so the session
        // refuses to exist instead.
        if config.risk_limits.has_aggregate_caps() && portfolio_facts.is_none() {
            return Err(VenueError::Invalid(
                "aggregate risk caps need a portfolio facts producer".to_owned(),
            ));
        }
        Ok(Self {
            scope: Scope::new(),
            registry,
            storage,
            stream_breaker: None,
            credential: None,
            reconcile: None,
            backfill: None,
            portfolio_facts,
            shutdown_timeout_secs: config.shutdown_timeout_secs,
            // Sessions boot allowing everything they allowed before the
            // command plane was wired: the gate is a no-op until the first
            // operator command is journaled.
            state: StateHandle::new(TradingState::Enabled),
        })
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

    /// The attached account-stream breaker, when one is installed. The live
    /// boot reports its producer set through here.
    #[must_use]
    pub fn stream_breaker(&self) -> Option<&Arc<StreamBreaker>> {
        self.stream_breaker.as_ref()
    }

    /// Share a trading-state handle (e.g. one already driven by an operator
    /// command plane): later bars and submissions gate on it instead of the
    /// boot default.
    pub fn attach_trading_state(&mut self, state: StateHandle) {
        self.state = state;
    }

    /// Attach the portfolio facts producer the aggregate risk caps read.
    /// A session booted with aggregate caps must have one; attaching one
    /// later satisfies a session that booted without caps.
    pub fn attach_portfolio_facts(&mut self, facts: Arc<dyn PortfolioFactsSource>) {
        self.portfolio_facts = Some(facts);
    }

    /// The attached portfolio facts producer, when one is installed.
    #[must_use]
    pub fn portfolio_facts(&self) -> Option<&Arc<dyn PortfolioFactsSource>> {
        self.portfolio_facts.as_ref()
    }

    /// Attach the reconciliation sensor: every later [`TradingSession::run_bar`]
    /// runs one compare of the journal's book against venue truth before the
    /// bar's other units, so a mismatch cancels entry-carrying units through
    /// the batch graph while exits stay independent. Attach only where the
    /// venue can diverge from the journal; a paper/backtest session that
    /// leaves it detached runs exactly as before. Returns the shared gate the
    /// submission path reads.
    pub fn attach_reconcile(&mut self, source: Arc<dyn ReconcileSource>) -> ReconcileHandle {
        let handle = ReconcileHandle::new();
        let unit: Arc<dyn Component> = Arc::new(ReconcileUnit::new(source, handle.clone()));
        self.reconcile = Some(ReconcileSensor {
            unit,
            handle: handle.clone(),
        });
        handle
    }

    /// The attached reconciliation gate, if any. Submissions read it through
    /// [`ReconcileHandle::check_entries`]; a detached session returns `None`
    /// and gates exactly as before.
    #[must_use]
    pub fn reconcile_handle(&self) -> Option<&ReconcileHandle> {
        self.reconcile.as_ref().map(|sensor| &sensor.handle)
    }

    /// Attach the backfill sensor: every later [`TradingSession::run_bar`]
    /// runs one resync pass before the bar's other units, so an unspliced
    /// hole cancels entry-carrying units through the batch graph while exits
    /// stay independent. Attach only where bars arrive over a stream that
    /// can drop; a session that leaves it detached runs exactly as before.
    /// Returns the shared gate the submission path reads. Reconnecting bars
    /// reach the sensor through [`BackfillUnit::note_bar`], which the feed
    /// owner calls; the bar units themselves only observe the gate.
    pub fn attach_backfill(
        &mut self,
        ctx: SeriesContext,
        sources: Vec<Arc<dyn BackfillSource>>,
        warmup: usize,
    ) -> BackfillHandle {
        let handle = BackfillHandle::new();
        let unit = Arc::new(BackfillUnit::new(ctx, sources, handle.clone(), warmup));
        self.backfill = Some(BackfillSensor {
            unit,
            handle: handle.clone(),
        });
        handle
    }

    /// The attached backfill gate, if any. Submissions read it through
    /// [`BackfillHandle::check_entries`]; a detached session returns `None`
    /// and gates exactly as before.
    #[must_use]
    pub fn backfill_handle(&self) -> Option<&BackfillHandle> {
        self.backfill.as_ref().map(|sensor| &sensor.handle)
    }

    /// One bar's producer-confidence reading over this session's attached
    /// gates: the trading state plus whichever reconcile, backfill, and
    /// stream gates are attached. Detached gates are declared at
    /// construction (allowed); an attached gate that never answered refuses
    /// entries. The loop consumes this every bar; exits always route.
    #[must_use]
    pub fn gate_reading(&self) -> GateReading {
        GateReading::assemble_with_credential(
            self.state.get(),
            self.reconcile.as_ref().map(|sensor| &sensor.handle),
            self.backfill.as_ref().map(|sensor| &sensor.handle),
            self.stream_breaker.as_deref(),
            self.credential.as_ref(),
        )
    }

    /// Attach a credential publication: bars read its validity as a gate.
    pub fn attach_credential(&mut self, handle: crate::auth::AuthHandle) {
        self.credential = Some(handle);
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
    /// bounded channel, backpressuring the producer when full. When a
    /// backfill sensor is attached, each bar enters it through
    /// [`BackfillUnit::note_bar`] before any consumer reads it, so a forward
    /// gap suspends entries ahead of the hole — the feed is the sensor's
    /// caller, and an unattached session pumps exactly as before.
    pub fn spawn_feed(&mut self, bars: Vec<Bar>, capacity: usize) -> mpsc::Receiver<Bar> {
        // Bounded pipe: at most `capacity` bars buffer; the pump's send
        // awaits room, so burst history cannot outrun a slow consumer.
        let (tx, rx) = mpsc::channel(capacity);
        let sensor = self
            .backfill
            .as_ref()
            .map(|sensor| Arc::clone(&sensor.unit));
        self.scope.spawn(async move {
            for bar in bars {
                if let Some(unit) = &sensor {
                    // A stale redelivery refuses without touching state; the
                    // pump still forwards it, since the sensor — not the
                    // pipe — owns gap truth.
                    let _ = unit.note_bar(bar.clone());
                }
                if tx.send(bar).await.is_err() {
                    break;
                }
            }
        });
        rx
    }

    /// Collect newly tripped or halted alerts from the attached gates into
    /// `sink`: the breaker trip and the kill-switch transition alert on the
    /// edge through `cursor`, banked reconciliation and backfill lines drain
    /// exactly once. Unattached gates are unwatched. The production poll
    /// entry point — tests and the soak harness drive the same function.
    pub fn poll_alerts(
        &self,
        cursor: &mut crate::alert::AlertCursor,
        sink: &dyn crate::alert::AlertSink,
        now: DateTime<Utc>,
    ) -> Result<usize, VenueError> {
        let watch = crate::alert::GateWatch {
            breaker: self.stream_breaker.as_deref(),
            reconcile: self.reconcile.as_ref().map(|sensor| &sensor.handle),
            backfill: self.backfill.as_ref().map(|sensor| &sensor.handle),
            state: Some(&self.state),
        };
        crate::alert::poll_alerts(cursor, &watch, sink, now)
    }

    /// Spawn the out-of-band alert poller under the owned scope: every
    /// `interval` it collects newly tripped or halted gates through
    /// [`poll_alerts`](Self::poll_alerts) and delivers them through `sink`
    /// (usually a [`FileSink`](crate::alert::FileSink) the operator tails).
    /// The cursor lives in the task, so edges alert once no matter how many
    /// polls observe the level. A failing sink reports loudly to stderr and
    /// the cadence continues — never a retry of the failed send, never a
    /// gate on a trade. `shutdown` drains the task with the feed scope.
    pub fn spawn_alert_poller(
        &mut self,
        sink: Arc<dyn crate::alert::AlertSink>,
        interval: std::time::Duration,
    ) {
        let breaker = self.stream_breaker.clone();
        let reconcile = self.reconcile.as_ref().map(|sensor| sensor.handle.clone());
        let backfill = self.backfill.as_ref().map(|sensor| sensor.handle.clone());
        let state = self.state.clone();
        self.scope.spawn(async move {
            let mut cursor = crate::alert::AlertCursor::new();
            loop {
                tokio::time::sleep(interval).await;
                let watch = crate::alert::GateWatch {
                    breaker: breaker.as_deref(),
                    reconcile: reconcile.as_ref(),
                    backfill: backfill.as_ref(),
                    state: Some(&state),
                };
                if let Err(error) =
                    crate::alert::poll_alerts(&mut cursor, &watch, sink.as_ref(), Utc::now())
                {
                    eprintln!("alert delivery refused: {error}");
                }
            }
        });
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
                safe_submit(
                    &self.storage,
                    journal,
                    self.registry.as_ref(),
                    approved,
                    intent,
                    quote,
                    now,
                )
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
        let mut batch = fluent_concurrency::batch::SupervisedBatch::new(
            fluent_concurrency::tokio_runtime(),
            caps,
        );
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
        if let Some(sensor) = &self.reconcile {
            units.insert(0, Arc::clone(&sensor.unit));
        }
        if let Some(sensor) = &self.backfill {
            let erased: Arc<dyn Component> = sensor.unit.clone();
            units.insert(0, erased);
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
                error: WorkError::Execution(
                    "bar registration rejected a duplicate unit name".to_owned(),
                ),
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

    /// Shut down: close the feed scope inside the configured timeout. A kill
    /// mid-loop must not hang shutdown behind a slow pump, so the timeout
    /// bounds the wait and the return says which it was: `true` drained
    /// clean (no task outlives the call), `false` timed out (stragglers
    /// finish or die with the runtime — the caller reports loudly, the
    /// session never swallows it).
    pub async fn shutdown(self) -> bool {
        let bound = std::time::Duration::from_secs(self.shutdown_timeout_secs);
        tokio::time::timeout(bound, async {
            let mut scope = self.scope;
            scope.close().await;
        })
        .await
        .is_ok()
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
