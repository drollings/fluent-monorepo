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
use fluent_wvr::prelude::*;
use std::sync::Arc;
use tokio::sync::mpsc;

use protopyne_types::Bar;

use crate::error::VenueError;
use crate::journal::VenueStorage;
use crate::order::{OrderIntent, OrderRecord};
use crate::paper::PaperVenue;
use crate::quote::Quote;
use crate::venue::{QuoteSource, Venue};

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
        self.validate()
            .map_err(|e| WorkError::Execution(format!("session config invalid: {e}")))?;
        Ok(WorkOutput::ok("session config valid"))
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
}

/// The supervised session: feed scope, credit pair, fan-out pool, venue.
pub struct TradingSession {
    scope: Scope,
    credit_tx: CreditSender,
    credit_rx: CreditReceiver,
    pool: ResultPool<SubmitJob, OrderRecord, VenueError>,
    venue: Arc<PaperVenue>,
    storage: VenueStorage,
}

impl TradingSession {
    /// Boot a session: config validation, credit pair, and a result pool
    /// whose workers submit through the venue. No task spawns here; feeds
    /// spawn under the owned [`Scope`] on demand.
    pub fn boot(
        config: &SessionConfig,
        venue: Arc<PaperVenue>,
        storage: VenueStorage,
    ) -> Result<Self, VenueError> {
        config
            .validate()
            .map_err(VenueError::Invalid)?;
        let (credit_tx, credit_rx) = flow::new(CreditSpec {
            initial: config.credit_limit as usize,
            more_after: 1,
        });
        let worker_venue = Arc::clone(&venue);
        let pool = ResultPool::new(
            fluent_concurrency::tokio_runtime(),
            config.workers as usize,
            config.queue_capacity as usize,
            move |job: SubmitJob| {
                let venue = Arc::clone(&worker_venue);
                async move { venue.submit(job.intent, &job.quote, job.at).await }
            },
        );
        Ok(Self {
            scope: Scope::new(),
            credit_tx,
            credit_rx,
            pool,
            venue,
            storage,
        })
    }

    /// Whether the producer is currently credit-blocked.
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        self.credit_tx.is_blocked()
    }

    /// Spawn a feed task under the owned scope: closed bars flow into a
    /// bounded channel, backpressuring the producer when full.
    pub fn spawn_feed(&mut self, bars: Vec<Bar>, capacity: usize) -> mpsc::Receiver<Bar> {
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
    pub async fn submit_order(
        &self,
        job: SubmitJob,
    ) -> Result<OrderRecord, ResultPoolError<VenueError>> {
        let outcome = self
            .credit_tx
            .send(|| async { self.pool.submit(job).await })
            .await;
        self.credit_rx.recv();
        outcome
    }

    /// Run one bar's units under containment-only supervision. A failing
    /// unit cancels its dependents; nothing restarts. Units arrive wrapped
    /// in [`Instrumented`] before type erasure.
    pub async fn run_bar(
        &self,
        units: Vec<Arc<dyn Component>>,
    ) -> fluent_concurrency::batch::SupervisedBatchSummary {
        use fluent_concurrency::batch::{SupervisedBatchError, SupervisedBatchEvent};
        let caps = fluent_wvr::CapabilitySet::new().with(self.storage.clone());
        let mut batch =
            fluent_concurrency::batch::SupervisedBatch::new(fluent_concurrency::tokio_runtime(), caps);
        let mut registration_failure: Option<SupervisedBatchError> = None;
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

    /// Borrow the venue (reconciliation reads truth through here).
    #[must_use]
    pub fn venue(&self) -> &Arc<PaperVenue> {
        &self.venue
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
