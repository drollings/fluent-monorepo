//! Supervised stream: reconnect with jittered backoff plus a
//! producer-confidence breaker.
//!
//! Three signal paths stay on separate code paths by construction:
//!
//! * **(producer-confidence)**: transport errors feed [`StreamBreaker`],
//!   whose trip merges into `BreakerState::tripped` (entries disarm, exits
//!   flow) and into each bar's [`SupervisedBatch`] through
//!   [`StreamHealthUnit`]. The breaker never alters quotes, pricing, or
//!   fill verdicts.
//! * **(correctness)**: quote age, crossed books, and sequence gaps keep
//!   their existing detectors and refusals; this module neither duplicates
//!   nor heals them.
//! * **(task-value)**: spread economics stay in `LiquidityFilter`; nothing
//!   here judges a trade on merit.
//!
//! Ownership composes the shared primitives: the reconnect driver
//! ([`SupervisedStream::run`]) runs under the caller's [`Scope`] (the
//! session spawns it; tests await it directly), each connection attempt
//! reuses the transport's message pump, retries pace on the shared
//! [`backoff_ms`] schedule, and the consumer holds a [`StreamAbort`] that
//! preempts connects, backoffs, and pumps alike.
//!
//! Why [`backoff_ms`] rather than [`retry_async`]'s loop: the retry
//! helper's bounded-attempt shape cannot express an abortable supervision
//! run (abort must preempt a pending sleep, and the breaker — not an
//! attempt count — is the stop). The schedule is shared; only the loop is
//! local, and the loop does nothing but classify, sleep, and count.
//!
//! [`Scope`]: fluent_concurrency::scope::Scope
//! [`SupervisedBatch`]: fluent_concurrency::batch::SupervisedBatch
//! [`backoff_ms`]: common_core::retry::backoff_ms
//! [`retry_async`]: common_core::retry::retry_async

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::time::Instant;

use common_core::retry::backoff_ms;
use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::prelude::*;
use protopyne_strategy::BreakerState;
use tokio::sync::mpsc;

use crate::tastytrade::{PumpEnd, TastytradeTransport, TransportError};
use crate::venue::AccountEvent;

/// Boundary classifier: envelope error code → terminal reason, or `None`
/// for transient. The terminal set is closed; every arm states its reason,
/// and the set grows only with a stated reason per code (the calibration
/// pins terminal-zero retries per code).
#[must_use]
pub fn terminal_reason(code: &str) -> Option<&'static str> {
    match code {
        "unauthorized" | "forbidden" => Some("terminal credentials failure: retrying cannot authenticate"),
        "bad_request" => Some("terminal request failure: the request itself is invalid"),
        "malformed_auth" => Some("terminal credentials failure: auth material is malformed"),
        "subscription_rejected" => Some(
            "terminal subscription failure: the venue refused the parameters, identical reconnects cannot succeed",
        ),
        _ => None,
    }
}

/// Supervised-stream tunables. The breaker budgets below are initial values:
/// the calibration suite freezes them after the soak suite, so move them
/// only through that suite — never by feel.
///
/// [`StreamConfig::validate`] enforces the bounds fail-closed.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct StreamConfig {
    /// Backoff base in milliseconds between reconnects (shared schedule).
    #[field(desc = "Reconnect backoff base in milliseconds", min = 1, max = 60_000)]
    #[builder(default = crate::tastytrade::RETRY_BACKOFF_BASE_MS)]
    pub backoff_base_ms: u64,
    /// Backoff jitter percent (shared schedule, 0–100).
    #[field(desc = "Reconnect backoff jitter percent", min = 0, max = 100)]
    #[builder(default = crate::tastytrade::RETRY_JITTER_PCT)]
    pub backoff_jitter_pct: u32,
    /// Consecutive retryable connect errors that disarm entries
    /// **(producer-confidence)**. The Nth error still arms; the N+1th
    /// trips — the same contract as the submission-side breaker.
    #[field(desc = "Consecutive stream errors before the breaker trips", min = 1, max = 1_000)]
    #[builder(default = 3)]
    pub max_consecutive_errors: u32,
    /// Window in seconds for counting stream restarts
    /// **(producer-confidence)**.
    #[field(desc = "Restart-rate window in seconds", min = 1, max = 3_600)]
    #[builder(default = 60)]
    pub restart_window_secs: u64,
    /// Restarts inside one window that trip the breaker
    /// **(producer-confidence)**.
    #[field(desc = "Restarts per window before the breaker trips", min = 1, max = 1_000)]
    #[builder(default = 5)]
    pub max_restarts_per_window: u32,
}

impl protopyne_types::Validate for StreamConfig {
    type Err = String;
    fn validate(&self) -> Result<(), Self::Err> {
        StreamConfig::validate(self)
    }
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

impl StreamConfig {
    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), String> {
        if self.backoff_base_ms < 1 {
            return Err("backoff_base_ms must be at least 1".to_owned());
        }
        if self.backoff_jitter_pct > 100 {
            return Err("backoff_jitter_pct must be at most 100".to_owned());
        }
        if self.max_consecutive_errors < 1 {
            return Err("max_consecutive_errors must be at least 1".to_owned());
        }
        if self.restart_window_secs < 1 {
            return Err("restart_window_secs must be at least 1".to_owned());
        }
        if self.max_restarts_per_window < 1 {
            return Err("max_restarts_per_window must be at least 1".to_owned());
        }
        Ok(())
    }
}

impl WorkUnit for StreamConfig {
    fn name(&self) -> &str {
        "venue_stream_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        protopyne_types::check_for_execute(self, "stream config invalid")?;
        Ok(protopyne_types::valid_output("stream config valid"))
    }
}

impl_component!(StreamConfig);

/// Asset the stream-health sensor provides: bar units that must not run
/// while the stream is tripped declare it in `depends`.
pub const STREAM_HEALTH_ASSET: &str = "stream.health";

/// Producer-confidence breaker over one stream: consecutive connect-error
/// and restart-rate counters with a sticky trip. Trips merge into
/// [`BreakerState`] (entries disarm, exits flow) and into bar batches
/// through [`StreamHealthUnit`]. There is deliberately no `reset`: recovery
/// re-arms only through a fresh instance (a new run), never automatically.
#[derive(Debug)]
pub struct StreamBreaker {
    config: StreamConfig,
    consecutive_errors: AtomicU32,
    restarts: Mutex<VecDeque<Instant>>,
    tripped: AtomicBool,
    trip_reason: Mutex<Option<String>>,
}

impl StreamBreaker {
    /// Open a breaker over `config`. Starts untripped with cold counters.
    #[must_use]
    pub fn new(config: StreamConfig) -> Self {
        Self {
            config,
            consecutive_errors: AtomicU32::new(0),
            restarts: Mutex::new(VecDeque::new()),
            tripped: AtomicBool::new(false),
            trip_reason: Mutex::new(None),
        }
    }

    /// Consecutive retryable connect errors so far (resets on success).
    #[must_use]
    pub fn consecutive_errors(&self) -> u32 {
        self.consecutive_errors.load(Ordering::SeqCst)
    }

    /// Restarts inside the current window.
    #[must_use]
    pub fn restart_count(&self) -> usize {
        common_core::sync::lock(&self.restarts).len()
    }

    /// Whether the breaker has tripped. Sticky: once true, stays true.
    #[must_use]
    pub fn tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }

    /// Why the breaker tripped, if it did.
    #[must_use]
    pub fn trip_reason(&self) -> Option<String> {
        common_core::sync::lock(&self.trip_reason).clone()
    }

    fn trip(&self, reason: String) {
        *common_core::sync::lock(&self.trip_reason) = Some(reason);
        self.tripped.store(true, Ordering::SeqCst);
    }

    /// Record one established generation: clears the cooling streak while
    /// untripped; never re-arms a trip.
    pub fn record_success(&self) {
        if !self.tripped() {
            self.consecutive_errors.store(0, Ordering::SeqCst);
        }
    }

    /// Record one failed connect: terminal errors trip immediately with
    /// their reason; retryable errors accumulate and trip past the budget.
    /// Tripped breakers bank nothing further.
    pub fn record_error(&self, error: &TransportError) {
        if self.tripped() {
            return;
        }
        if let TransportError::Terminal(reason) = error {
            self.trip(format!("terminal stream error: {reason}"));
            return;
        }
        let consecutive = self.consecutive_errors.fetch_add(1, Ordering::SeqCst) + 1;
        if consecutive > self.config.max_consecutive_errors {
            self.trip(format!(
                "{} consecutive stream errors exceed max {}",
                consecutive, self.config.max_consecutive_errors
            ));
        }
    }

    /// Record one generation close (clean or faulted): restarts inside the
    /// window accumulate and trip past the budget. Tripped breakers bank
    /// nothing further.
    pub fn record_restart(&self) {
        if self.tripped() {
            return;
        }
        let window = std::time::Duration::from_secs(self.config.restart_window_secs);
        let now = Instant::now();
        let mut restarts = common_core::sync::lock(&self.restarts);
        while restarts
            .front()
            .is_some_and(|first| now.duration_since(*first) > window)
        {
            restarts.pop_front();
        }
        restarts.push_back(now);
        if restarts.len() as u32 > self.config.max_restarts_per_window {
            self.trip(format!(
                "{} restarts inside {}s exceed max {}",
                restarts.len(),
                self.config.restart_window_secs,
                self.config.max_restarts_per_window
            ));
        }
    }

    /// Merge a trip into `state`: sets `tripped` with the banked reason.
    /// Never clears — an untripped breaker leaves an already-tripped state
    /// exactly as it found it.
    pub fn apply_to(&self, state: &mut BreakerState) {
        if self.tripped() {
            state.tripped = true;
            state.trip_reason = self.trip_reason();
        }
    }
}

/// How a supervised-stream run ended. Every variant is a stop, never a
/// repair: the run reports; the caller (or a fresh run) decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamOutcome {
    /// The consumer's [`StreamAbort`] fired: no error counted.
    Aborted,
    /// The consumer is gone (outbox closed): the pump would wedge, so the
    /// run stops instead.
    ConsumerGone,
    /// A terminal transport error ended the run first-class, with its
    /// reason intact.
    Terminal {
        /// Terminal reason from the boundary classifier.
        reason: String,
    },
    /// The breaker tripped: entries disarm through the merged
    /// `BreakerState`, and the run stops rather than hammering the venue.
    BreakerTripped {
        /// Banked breaker reason.
        reason: String,
    },
}

/// One supervised stream: configuration plus its shared breaker. Clone is
/// cheap (config plus one `Arc` bump) so the driver task and the consumer
/// observe the same trip.
#[derive(Debug, Clone)]
pub struct SupervisedStream {
    config: StreamConfig,
    breaker: Arc<StreamBreaker>,
}

impl SupervisedStream {
    /// Open a stream over `config` with a cold breaker.
    #[must_use]
    pub fn new(config: StreamConfig) -> Self {
        Self {
            breaker: Arc::new(StreamBreaker::new(config.clone())),
            config,
        }
    }

    /// Borrow the shared breaker: the consumer merges trips into its
    /// `BreakerState` and attaches it to bar batches from here.
    #[must_use]
    pub fn breaker(&self) -> &Arc<StreamBreaker> {
        &self.breaker
    }

    /// Run the reconnect loop until the abort fires, the consumer leaves, a
    /// terminal error lands, or the breaker trips. `connect` establishes one
    /// generation (a wire-message inbox); each generation pumps through the
    /// transport's message loop, so parsing behavior is identical to the
    /// unsupervised stream — only supervision changes. The `out` outbox and
    /// every inbox stay caller-bounded (this loop constructs no channels); a
    /// full outbox skips by counted policy, never blocks.
    pub async fn run<C, Fut>(
        &self,
        mut connect: C,
        abort: &StreamAbort,
        out: mpsc::Sender<AccountEvent>,
        transport: &Arc<TastytradeTransport>,
    ) -> StreamOutcome
    where
        C: FnMut() -> Fut + Send,
        Fut: Future<Output = Result<mpsc::Receiver<String>, TransportError>> + Send,
    {
        transport.note_stream_started();
        let _guard = StreamRunGuard { transport };
        loop {
            if abort.is_cancelled() {
                return StreamOutcome::Aborted;
            }
            let inbox = tokio::select! {
                connected = connect() => Some(connected),
                () = abort.cancelled() => None,
            };
            let Some(connected) = inbox else {
                return StreamOutcome::Aborted;
            };
            match connected {
                Err(error) => {
                    let terminal = !error.is_retryable();
                    self.breaker.record_error(&error);
                    if terminal {
                        return StreamOutcome::Terminal {
                            reason: error.to_string(),
                        };
                    }
                    if self.breaker.tripped() {
                        return StreamOutcome::BreakerTripped {
                            reason: self
                                .breaker
                                .trip_reason()
                                .unwrap_or_else(|| "stream breaker tripped".to_owned()),
                        };
                    }
                    // Just incremented above and still untripped, so the
                    // streak is at least one: the schedule's 1-based attempt.
                    let delay = backoff_ms(
                        self.config.backoff_base_ms,
                        self.breaker.consecutive_errors(),
                        self.config.backoff_jitter_pct,
                    );
                    tokio::select! {
                        () = tokio::time::sleep(std::time::Duration::from_millis(delay)) => {},
                        () = abort.cancelled() => return StreamOutcome::Aborted,
                    }
                }
                Ok(inbox) => {
                    self.breaker.record_success();
                    match transport
                        .pump_account_events(inbox, out.clone(), abort)
                        .await
                    {
                        PumpEnd::InboxClosed => {
                            self.breaker.record_restart();
                            if self.breaker.tripped() {
                                return StreamOutcome::BreakerTripped {
                                    reason: self
                                        .breaker
                                        .trip_reason()
                                        .unwrap_or_else(|| "stream breaker tripped".to_owned()),
                                };
                            }
                        }
                        PumpEnd::OutboxClosed => return StreamOutcome::ConsumerGone,
                        PumpEnd::Aborted => return StreamOutcome::Aborted,
                    }
                }
            }
        }
    }
}

/// Keeps the transport's live-task accounting over a supervised run, the
/// same way the unsupervised stream guards its spawned task.
struct StreamRunGuard<'a> {
    transport: &'a Arc<TastytradeTransport>,
}

impl Drop for StreamRunGuard<'_> {
    fn drop(&mut self) {
        self.transport.note_stream_ended();
    }
}

/// Supervision sensor for bar batches: reports the shared breaker into a
/// [`SupervisedBatch`] as pass/fail. Failing cancels stream-dependent bar
/// units through the graph; independent units still complete. The unit is
/// synchronous and side-effect free (one atomic load), honoring the
/// `execute` purity contract.
pub struct StreamHealthUnit {
    breaker: Arc<StreamBreaker>,
    provides: Vec<ArcIntern<str>>,
}

impl StreamHealthUnit {
    /// Build the session's sensor over `breaker`. The unit name is fixed
    /// (`stream.health.sensor`) so bars cannot register it twice by
    /// accident; dependents key on the [`STREAM_HEALTH_ASSET`] asset.
    #[must_use]
    pub fn new(breaker: Arc<StreamBreaker>) -> Self {
        Self {
            breaker,
            provides: vec![ArcIntern::from(STREAM_HEALTH_ASSET)],
        }
    }
}

impl WorkUnit for StreamHealthUnit {
    fn name(&self) -> &str {
        "stream.health.sensor"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        if self.breaker.tripped() {
            Err(WorkError::Execution(
                self.breaker
                    .trip_reason()
                    .unwrap_or_else(|| "stream breaker tripped".to_owned()),
            ))
        } else {
            Ok(WorkOutput::ok("stream healthy"))
        }
    }
}

impl_fieldless!(StreamHealthUnit);

impl Describable for StreamHealthUnit {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "description": "Stream breaker supervision sensor: fails while the attached stream breaker is tripped",
        })
    }
}

impl_component!(StreamHealthUnit);

#[cfg(test)]
#[path = "../tests/stream.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/stream_calibration.rs"]
mod stream_calibration_tests;
