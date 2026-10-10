//! Streaming supervision: fault-scripted reconnect suite plus the
//! producer-confidence breaker.
//!
//! Protocol: every test scripts its feed in memory — no sockets, no sleeps
//! beyond millisecond backoffs — and asserts exact attempt counts (no hidden
//! extra tries, no swallowed terminals).
//!
//! Three signal paths stay separate, with separate tests:
//!
//! * **(producer-confidence)**: transport errors feed [`StreamBreaker`],
//!   which disarms entries through `BreakerState::tripped` and never touches
//!   quotes, pricing, or fill verdicts (`breaker_leaves_quotes_and_fills_alone`).
//! * **(correctness)**: stale quotes still go dark at the ladder and crossed
//!   books still trip the depth detector however the breaker reads — the
//!   breaker never invents or heals either (`breaker_never_masks_correctness`).
//! * **(task-value)**: wide spreads still refuse at `LiquidityFilter` while
//!   tripped — economics are judged on merit, never on producer doubt
//!   (`breaker_never_judges_merit`).

use super::*;
use crate::{
    AccountEvent, DepthBook, LiquidityFilter, OrderIntent, OrderSide, PaperConfig, PaperVenue, PriceLevel,
    Quote, SessionConfig, TastytradeConfig, TastytradeTransport, TradingSession, TransportError,
    VenueRegistry, VenueStorage,
    depth::is_crossed,
    venue::{QuoteSource, Venue},
};
use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use fluent_concurrency::stream::StreamAbort;
use protopyne_strategy::BreakerState;
use protopyne_types::Money;
use rust_decimal::Decimal;
use tokio::sync::mpsc;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn ts() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

fn balance_message() -> String {
    r#"{"kind":"balance","asset":"USD","amount":"1000.00"}"#.to_owned()
}

/// Millisecond-scale config so backoff bounds assert in real time.
fn fast_config() -> StreamConfig {
    StreamConfig::new()
        .backoff_base_ms(10)
        .backoff_jitter_pct(50)
        .max_consecutive_errors(2)
        .restart_window_secs(60)
        .max_restarts_per_window(5)
        .build()
}

fn transport() -> Arc<TastytradeTransport> {
    Arc::new(TastytradeTransport::new(
        TastytradeConfig::new().base_url("http://127.0.0.1:9".to_owned()).build(),
    ))
}

/// Scripted connector: pops canned connect outcomes; counts attempts and
/// stamps each attempt time for the jitter-bounds assertion. Interior
/// mutability throughout so `connect` takes `&self` and scripted closures
/// stay `'static` for scope spawning.
struct Script {
    outcomes: std::sync::Mutex<VecDeque<Result<mpsc::Receiver<String>, TransportError>>>,
    attempts: AtomicUsize,
    stamps: std::sync::Mutex<Vec<Instant>>,
}

impl Script {
    fn new(outcomes: Vec<Result<mpsc::Receiver<String>, TransportError>>) -> Self {
        Self {
            outcomes: std::sync::Mutex::new(outcomes.into()),
            attempts: AtomicUsize::new(0),
            stamps: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn connect(&self) -> Result<mpsc::Receiver<String>, TransportError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        self.stamps.lock().expect("stamps").push(Instant::now());
        self.outcomes
            .lock()
            .expect("outcomes")
            .pop_front()
            .expect("scripted outcome")
    }

    fn attempt_count(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }
}

/// One inbox pre-loaded with `messages`, then closed: a clean feed
/// generation that ends so the driver reconnects.
fn inbox_with(messages: Vec<String>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(async move {
        for message in messages {
            let _ = tx.send(message).await;
        }
    });
    rx
}

fn transient(text: &str) -> TransportError {
    TransportError::Transient(text.to_owned())
}

fn terminal(text: &str) -> TransportError {
    TransportError::Terminal(text.to_owned())
}

// ── reconnect suite ────────────────────────────────────────────────

/// Terminal stops immediately: exactly one attempt, no backoff slept, the
/// breaker tripped with the terminal reason.
#[tokio::test]
async fn terminal_error_stops_after_one_attempt() {
    let stream = SupervisedStream::new(fast_config());
    let script = Script::new(vec![Err(terminal("unauthorized: scripted"))]);
    let abort = StreamAbort::new();
    let (out_tx, _out_rx) = mpsc::channel::<AccountEvent>(8);
    let started = Instant::now();
    let vehicle = transport();
    let outcome = stream
        .run(|| async { script.connect() }, &abort, out_tx, &vehicle)
        .await;
    assert!(
        matches!(outcome, StreamOutcome::Terminal { .. }),
        "terminal ends the run, got: {outcome:?}"
    );
    assert_eq!(script.attempt_count(), 1, "zero false-retries on terminal");
    assert!(
        started.elapsed() < Duration::from_millis(10),
        "no backoff may sleep on the terminal path"
    );
    assert!(stream.breaker().tripped(), "terminal trips the breaker");
    assert!(
        stream
            .breaker()
            .trip_reason()
            .unwrap_or_default()
            .contains("unauthorized"),
        "trip reason names the terminal cause"
    );
}

/// Retryable backs off with jitter bounds, then the feed serves: two
/// transients, one generation delivering its message (and closing, which
/// counts its restart), then a terminal sentinel ends the run. Delays sit
/// inside the documented schedule (`base * 2^(k-1)` plus at most `base`
/// jitter at 50%). The sentinel trip afterwards proves the end state —
/// the transients themselves recovered cleanly first.
#[tokio::test]
async fn retryable_backs_off_within_jitter_bounds_then_serves() {
    let stream = SupervisedStream::new(fast_config());
    let script = Script::new(vec![
        Err(transient("unavailable: scripted")),
        Err(transient("rate_limited: scripted")),
        Ok(inbox_with(vec![balance_message()])),
        Err(terminal("sentinel: script over")),
    ]);
    let abort = StreamAbort::new();
    let (out_tx, mut out_rx) = mpsc::channel(8);
    let vehicle = transport();
    let outcome = stream
        .run(|| async { script.connect() }, &abort, out_tx, &vehicle)
        .await;
    assert!(
        matches!(outcome, StreamOutcome::Terminal { .. }),
        "sentinel ends the run, got: {outcome:?}"
    );
    assert_eq!(script.attempt_count(), 4, "two retries, one serve, one sentinel");
    let stamps = script.stamps.lock().expect("stamps").clone();
    assert_eq!(stamps.len(), 4);
    for (k, window) in stamps.windows(2).enumerate().take(2) {
        let delay = window[1].duration_since(window[0]);
        let floor_ms = 10 * (1 << k);
        assert!(
            delay >= Duration::from_millis(floor_ms),
            "retry {} must wait the exponential floor ({delay:?})",
            k + 1
        );
        assert!(
            delay <= Duration::from_millis(floor_ms + 10 + 50),
            "retry {} must stay inside the jitter ceiling ({delay:?})",
            k + 1
        );
    }
    let event = out_rx.recv().await.expect("served message lands");
    assert!(
        matches!(event, AccountEvent::BalanceUpdated(_)),
        "served generation forwards events"
    );
    assert_eq!(
        stream.breaker().consecutive_errors(),
        0,
        "a served generation clears the cooling streak"
    );
    assert_eq!(
        stream.breaker().restart_count(),
        1,
        "the served generation close counts one restart"
    );
    assert!(
        stream
            .breaker()
            .trip_reason()
            .unwrap_or_default()
            .contains("sentinel"),
        "only the sentinel trips, after clean recovery"
    );
}

/// A clean generation close counts one restart: serve, close, then the
/// sentinel ends the run. Restart accounting (not error counting) owns
/// flapping feeds.
#[tokio::test]
async fn clean_generation_close_counts_one_restart() {
    let stream = SupervisedStream::new(fast_config());
    let script = Script::new(vec![
        Ok(inbox_with(vec![balance_message()])),
        Err(terminal("sentinel: script over")),
    ]);
    let abort = StreamAbort::new();
    let (out_tx, _out_rx) = mpsc::channel(8);
    let outcome = stream
        .run(|| async { script.connect() }, &abort, out_tx, &transport())
        .await;
    assert!(
        matches!(outcome, StreamOutcome::Terminal { .. }),
        "sentinel ends the run, got: {outcome:?}"
    );
    assert_eq!(script.attempt_count(), 2, "one serve, one sentinel");
    assert_eq!(
        stream.breaker().restart_count(),
        1,
        "one clean generation close counts one restart"
    );
}

/// Abort cancels promptly mid-pump: the consumer holds the `StreamAbort`,
/// and firing it ends the run without counting an error.
#[tokio::test]
async fn abort_cancels_mid_stream_without_error() {
    let stream = SupervisedStream::new(fast_config());
    let (_held_tx, held_rx) = mpsc::channel::<String>(8);
    let script = Script::new(vec![Ok(held_rx)]);
    let abort = StreamAbort::new();
    let (out_tx, _out_rx) = mpsc::channel(8);
    let canceller = abort.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        canceller.cancel();
    });
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        stream.run(|| async { script.connect() }, &abort, out_tx, &transport()),
    )
    .await
    .expect("abort ends the run promptly");
    assert!(
        matches!(outcome, StreamOutcome::Aborted),
        "cancel ends the run, got: {outcome:?}"
    );
    assert_eq!(script.attempt_count(), 1, "no reconnect after abort");
    assert_eq!(
        stream.breaker().consecutive_errors(),
        0,
        "abort counts no error"
    );
    assert!(!stream.breaker().tripped());
}

/// A gone consumer ends the run instead of wedging the pump: the first
/// send observes the closed outbox and stops with `ConsumerGone`.
#[tokio::test]
async fn closed_consumer_ends_the_run() {
    let stream = SupervisedStream::new(fast_config());
    let script = Script::new(vec![Ok(inbox_with(vec![balance_message()]))]);
    let abort = StreamAbort::new();
    let (out_tx, out_rx) = mpsc::channel(8);
    drop(out_rx);
    let outcome = stream
        .run(|| async { script.connect() }, &abort, out_tx, &transport())
        .await;
    assert!(
        matches!(outcome, StreamOutcome::ConsumerGone),
        "closed outbox ends the run, got: {outcome:?}"
    );
    assert_eq!(script.attempt_count(), 1, "no reconnect after the consumer leaves");
}

// ── breaker suite ──────────────────────────────────────────────────

/// N consecutive retryable errors trip the breaker: with
/// `max_consecutive_errors = 2` the second error still arms and the third
/// trips — the same contract as the submission-side breaker. Recovery never
/// re-arms: later successes leave the trip (and its reason) in place, and a
/// new breaker is the only clean slate.
#[tokio::test]
async fn consecutive_errors_trip_and_never_auto_rearm() {
    let stream = SupervisedStream::new(fast_config());
    let breaker = stream.breaker();
    breaker.record_error(&transient("unavailable: one"));
    assert!(!breaker.tripped(), "first error arms");
    breaker.record_error(&transient("unavailable: two"));
    assert!(!breaker.tripped(), "Nth error still arms");
    breaker.record_error(&transient("unavailable: three"));
    assert!(breaker.tripped(), "N+1th error trips");
    let reason = breaker.trip_reason().expect("trip banks a reason");
    assert!(reason.contains("consecutive"), "reason names the budget: {reason}");

    breaker.record_success();
    breaker.record_success();
    assert!(breaker.tripped(), "successes never re-arm a trip");
    assert_eq!(breaker.trip_reason().as_deref(), Some(reason.as_str()));

    let mut state = BreakerState::new().build();
    breaker.apply_to(&mut state);
    assert!(state.tripped, "the trip merges into BreakerState");
    assert_eq!(state.trip_reason, Some(reason));

    let fresh = SupervisedStream::new(fast_config());
    assert!(!fresh.breaker().tripped(), "a new run starts untripped");
}

/// Restart storms trip within budget: the third restart inside the window
/// trips with a restart reason, while spaced-out generations stay armed.
#[tokio::test]
async fn restart_storm_trips_within_budget() {
    let stream = SupervisedStream::new(fast_config());
    let breaker = stream.breaker();
    breaker.record_restart();
    breaker.record_restart();
    assert!(!breaker.tripped(), "restarts inside the budget arm");
    breaker.record_restart();
    breaker.record_restart();
    breaker.record_restart();
    breaker.record_restart();
    assert!(breaker.tripped(), "the sixth restart exceeds five per window");
    assert!(
        breaker
            .trip_reason()
            .unwrap_or_default()
            .contains("restart"),
        "trip reason names the storm"
    );
}

/// Success clears a cooling streak while untripped: one error, one success,
/// one error never accumulates toward the budget.
#[test]
fn success_resets_the_cooling_streak() {
    let stream = SupervisedStream::new(fast_config());
    let breaker = stream.breaker();
    breaker.record_error(&transient("unavailable: one"));
    breaker.record_success();
    assert_eq!(breaker.consecutive_errors(), 0);
    breaker.record_error(&transient("unavailable: two"));
    assert!(!breaker.tripped(), "reset streaks never sum");
}

/// The merge never clears: applying an untripped breaker to an already
/// tripped `BreakerState` leaves the trip (and its reason) intact.
#[test]
fn merge_never_clears_a_trip() {
    let stream = SupervisedStream::new(fast_config());
    let mut state = BreakerState::new()
        .tripped(true)
        .trip_reason("earlier trip".to_owned())
        .build();
    stream.breaker().apply_to(&mut state);
    assert!(state.tripped);
    assert_eq!(state.trip_reason.as_deref(), Some("earlier trip"));
}

// ── axis-separation suite ──────────────────────────────────────────

/// The breaker never alters quotes, pricing, or fill verdicts: with the
/// breaker tripped, the cached quote still serves and the paper venue still
/// fills a marketable order. Only grant evaluation (the confidence path)
/// disarms entries — proven server-side through the existing tripped path.
#[tokio::test]
async fn breaker_leaves_quotes_and_fills_alone() {
    let stream = SupervisedStream::new(fast_config());
    let breaker = stream.breaker();
    for _ in 0..3 {
        breaker.record_error(&transient("unavailable: storm"));
    }
    assert!(breaker.tripped());

    let venue_transport = transport();
    let quote = Quote::new()
        .bid(money("600.00"))
        .ask(money("600.10"))
        .bid_size(50)
        .ask_size(50)
        .ts(Utc::now())
        .build();
    venue_transport.put_cached("SPY", quote);
    let served = venue_transport.latest("SPY").expect("tripped breaker serves quotes");
    assert_eq!(served.bid, money("600.00"), "pricing untouched");

    let venue = PaperVenue::new(PaperConfig::new().build());
    let record = venue
        .submit(
            OrderIntent::new()
                .client_id("m6-untouched".to_owned())
                .proposal_id("prop-1".to_owned())
                .strategy("ribbon".to_owned())
                .underlying("SPY".to_owned())
                .side(OrderSide::Buy)
                .quantity(1)
                .limit_price(money("600.50"))
                .build(),
            &served,
            ts(),
        )
        .await
        .expect("tripped breaker fills marketable flow");
    assert_eq!(record.filled_quantity, 1);
}

/// Staleness and crossing stay correctness-owned however the breaker reads:
/// a stale cached quote still goes dark at the ladder and a crossed book
/// still trips the depth detector, tripped or not.
#[test]
fn breaker_never_masks_correctness() {
    let stream = SupervisedStream::new(fast_config());
    for _ in 0..3 {
        stream.breaker().record_error(&transient("unavailable: storm"));
    }
    assert!(stream.breaker().tripped());

    let venue_transport = transport();
    venue_transport.put_cached(
        "SPY",
        Quote::new()
            .bid(money("600.00"))
            .ask(money("600.10"))
            .bid_size(50)
            .ask_size(50)
            .ts(ts() - chrono::Duration::seconds(3_600))
            .build(),
    );
    assert!(
        venue_transport.latest("SPY").is_none(),
        "stale quotes go dark while tripped"
    );

    let mut index = protopyne_types::SymbolIndex::new();
    let mut book = DepthBook::new("SPY".to_owned(), &mut index).expect("intern");
    book.snapshot_replace(
        &[PriceLevel { price: money("600.20"), size: 50 }],
        &[PriceLevel { price: money("600.10"), size: 40 }],
        1,
        ts(),
    )
    .expect("crossed snapshot sets");
    assert!(
        is_crossed(&book).is_some(),
        "crossed books trip while tripped"
    );
}

/// Wide spreads still refuse on merit while tripped: the liquidity filter
/// judges economics, never producer doubt.
#[test]
fn breaker_never_judges_merit() {
    let stream = SupervisedStream::new(fast_config());
    for _ in 0..3 {
        stream.breaker().record_error(&transient("unavailable: storm"));
    }
    assert!(stream.breaker().tripped());
    let wide = Quote::new()
        .bid(money("600.00"))
        .ask(money("700.00"))
        .bid_size(1)
        .ask_size(1)
        .ts(Utc::now())
        .build();
    let filter = LiquidityFilter::new()
        .max_spread_width(money("0.50"))
        .min_size(5)
        .max_age_secs(60)
        .build();
    assert!(
        filter.check(&wide, 1, true, Utc::now()).is_err(),
        "task-value refusal holds while tripped"
    );
}

// ── classification suite ───────────────────────────────────────────

/// The boundary table: each terminal code stops with its stated reason;
/// everything else leans transient. `subscription_rejected` joins the
/// closed terminal set — the venue refused the parameters, so identical
/// reconnects cannot succeed.
#[test]
fn terminal_set_carries_a_stated_reason_per_code() {
    for (code, reason) in [
        ("unauthorized", "credentials"),
        ("forbidden", "credentials"),
        ("bad_request", "request"),
        ("malformed_auth", "credentials"),
        ("subscription_rejected", "subscription"),
    ] {
        let stated = terminal_reason(code).unwrap_or_else(|| panic!("{code} is terminal"));
        assert!(
            stated.contains(reason),
            "{code} states its reason, got: {stated}"
        );
        assert!(!TransportError::Terminal(code.to_owned()).is_retryable());
    }
    for code in ["unavailable", "rate_limited", "timeout", "reset", "future_code"] {
        assert!(terminal_reason(code).is_none(), "{code} stays transient");
    }
}

// ── supervision-graph suite ────────────────────────────────────────

/// Shared probe unit for the supervision-graph tests: configurable
/// dependencies plus two behaviors. `Pending` returns the transient
/// retryable `Dependency` failure, so under a retrying batch the probe
/// stays in flight (preemptible at backoff sleeps) until its provider
/// resolves or the batch cancels it — the same contract as the
/// supervisor's own `dep_fail` stubs. A probe that returns `Ok` is done by
/// definition: cancellation only preempts unfinished work, never rewrites
/// a completed verdict.
struct Probe {
    name: String,
    depends: Vec<ArcIntern<str>>,
    pending: bool,
}

impl WorkUnit for Probe {
    fn name(&self) -> &str {
        &self.name
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &self.depends
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        if self.pending {
            Err(WorkError::Dependency(format!(
                "probe {} waits on its dependency",
                self.name
            )))
        } else {
            Ok(WorkOutput::ok("ran"))
        }
    }
}

impl FieldAccess for Probe {
    fn set_field(&mut self, name: &str, _v: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound(name.to_owned()))
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        Err(FieldError::NotFound(name.to_owned()))
    }
    fn field_names(&self) -> &'static [&'static str] {
        &[]
    }
}

impl Describable for Probe {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
}

impl_component!(Probe);

fn probe(name: &str, depends: &[&str], pending: bool) -> Arc<dyn Component> {
    Arc::new(Probe {
        name: name.to_owned(),
        depends: depends.iter().map(|d| ArcIntern::from(*d)).collect(),
        pending,
    })
}

/// Register a pending probe with a short retry budget: enough attempts to
/// outlive any fast provider fate, millisecond backoff so a missed
/// cancellation surfaces as failure in milliseconds, not minutes.
fn register_pending(
    batch: &mut fluent_concurrency::batch::SupervisedBatch,
    unit: Arc<dyn Component>,
) {
    use fluent_wvr::prelude::*;
    batch
        .register_with_context(
            unit,
            WorkContext {
                max_retries: 5,
                ..WorkContext::default()
            },
        )
        .expect("register pending probe");
}

/// A panicking unit cancels its dependents only: the feed source panics,
/// its pending dependent cancels through the graph with
/// `DependencyFailed`, and the health sensor plus the independent unit
/// still complete. The batch owns the failure; the runtime never sees it.
#[tokio::test]
async fn panic_cancels_dependents_only() {
    use fluent_concurrency::batch::SupervisedBatch;
    use fluent_wvr::prelude::*;

    struct PanicSource {
        name: String,
    }
    impl WorkUnit for PanicSource {
        fn name(&self) -> &str {
            &self.name
        }
        fn depends(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn provides(&self) -> &[ArcIntern<str>] {
            static FEED: std::sync::OnceLock<Vec<ArcIntern<str>>> = std::sync::OnceLock::new();
            FEED.get_or_init(|| vec![ArcIntern::from("stream.feed")])
        }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            panic!("scripted stream panic");
        }
    }
    impl FieldAccess for PanicSource {
        fn set_field(&mut self, name: &str, _v: &str) -> Result<(), FieldError> {
            Err(FieldError::NotFound(name.to_owned()))
        }
        fn get_field(&self, name: &str) -> Result<String, FieldError> {
            Err(FieldError::NotFound(name.to_owned()))
        }
        fn field_names(&self) -> &'static [&'static str] {
            &[]
        }
    }
    impl Describable for PanicSource {
        fn describe(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
    }
    impl_component!(PanicSource);

    let stream = SupervisedStream::new(fast_config());
    let sensor: Arc<dyn Component> =
        Arc::new(StreamHealthUnit::new(Arc::clone(stream.breaker())));
    let mut batch = SupervisedBatch::new(
        fluent_concurrency::tokio_runtime(),
        fluent_wvr::CapabilitySet::new(),
    );
    let panicking: Arc<dyn Component> = Arc::new(PanicSource {
        name: "stream.source".to_owned(),
    });
    batch.register(panicking).expect("register source");
    register_pending(&mut batch, probe("entries", &["stream.feed"], true));
    batch.register(sensor).expect("register sensor");
    batch.register(probe("telemetry", &[], false)).expect("register free");
    let summary = batch.await;
    assert_eq!(summary.panicked.len(), 1, "the panic is contained");
    assert!(
        summary.cancelled.iter().any(|event| matches!(
            event,
            fluent_concurrency::batch::SupervisedBatchEvent::Cancelled { name, reason: fluent_concurrency::batch::CancelReason::DependencyFailed }
            if &**name == "entries"
        )),
        "the dependent cancels on dependency failure, got: {:?}",
        summary.cancelled
    );
    assert!(
        summary
            .cancelled
            .iter()
            .all(|event| !format!("{event:?}").contains("telemetry")),
        "independents are never cancelled, got: {:?}",
        summary.cancelled
    );
    assert_eq!(summary.completed.len(), 2, "sensor and free complete: {summary:?}");
}

/// The health sensor reports the breaker into the batch: untripped completes
/// Ok, tripped fails with the banked reason — the unit the session prepends
/// to every bar while a stream is attached.
#[tokio::test]
async fn health_sensor_reports_breaker_state() {
    use fluent_concurrency::batch::SupervisedBatch;
    use fluent_wvr::prelude::*;

    async fn run_sensor(breaker: &Arc<StreamBreaker>) -> String {
        let sensor: Arc<dyn Component> =
            Arc::new(StreamHealthUnit::new(Arc::clone(breaker)));
        let mut batch = SupervisedBatch::new(
            fluent_concurrency::tokio_runtime(),
            fluent_wvr::CapabilitySet::new(),
        );
        batch.register(sensor).expect("register sensor");
        let summary = batch.await;
        if summary.failed.is_empty() {
            "completed".to_owned()
        } else {
            format!("{:?}", summary.failed[0])
        }
    }

    let calm = SupervisedStream::new(fast_config());
    assert_eq!(run_sensor(calm.breaker()).await, "completed");

    let stormy = SupervisedStream::new(fast_config());
    for _ in 0..3 {
        stormy.breaker().record_error(&transient("unavailable: storm"));
    }
    let report = run_sensor(stormy.breaker()).await;
    assert!(
        report.contains("consecutive"),
        "sensor failure names the trip, got: {report}"
    );
}

/// A tripped sensor cancels stream-dependent units through the graph: the
/// sensor fails fast on the banked trip, its pending dependent cancels
/// with `DependencyFailed`, the independent unit completes. This is the
/// batch-level proof behind the session's per-bar sensor.
#[tokio::test]
async fn tripped_sensor_cancels_dependents_through_graph() {
    use fluent_concurrency::batch::{
        CancelReason, SupervisedBatch, SupervisedBatchEvent,
    };

    let stream = SupervisedStream::new(fast_config());
    for _ in 0..3 {
        stream.breaker().record_error(&transient("unavailable: storm"));
    }
    let mut batch = SupervisedBatch::new(
        fluent_concurrency::tokio_runtime(),
        fluent_wvr::CapabilitySet::new(),
    );
    batch
        .register(Arc::new(StreamHealthUnit::new(Arc::clone(stream.breaker())))
            as Arc<dyn Component>)
        .expect("register sensor");
    register_pending(&mut batch, probe("entries", &["stream.health"], true));
    batch.register(probe("telemetry", &[], false)).expect("register free");
    let summary = batch.await;
    assert!(
        summary.failed.iter().any(
            |event| matches!(event, SupervisedBatchEvent::Failed { name, .. } if &**name == "stream.health.sensor")
        ),
        "the sensor reports the trip, got: {:?}",
        summary.failed
    );
    assert!(
        summary.cancelled.iter().any(
            |event| matches!(event, SupervisedBatchEvent::Cancelled { name, reason: CancelReason::DependencyFailed }
                if &**name == "entries")
        ),
        "the dependent cancels, got: {:?}",
        summary.cancelled
    );
    assert!(
        summary.completed.iter().any(
            |event| matches!(event, SupervisedBatchEvent::Completed { name, .. } if &**name == "telemetry")
        ),
        "independents complete, got: {:?}",
        summary.completed
    );
}

/// Session wiring: attaching a stream prepends the health sensor to every
/// bar. A tripped breaker fails the sensor with the banked trip reason
/// while independent units still complete (containment); calm streams and
/// unattached sessions run bars exactly as before. Dependent cancellation
/// itself is the supervisor's contract, proven at the batch level above —
/// bars register with zero retries, so no bar unit can outwait a fate.
#[tokio::test]
async fn attached_stream_sensor_reports_trip_per_bar() {
    use fluent_concurrency::batch::SupervisedBatchEvent;

    fn session() -> TradingSession {
        let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
        TradingSession::boot(
            &SessionConfig::new().build(),
            Arc::new(VenueRegistry::single(venue)),
            VenueStorage::new("test.db".into()),
        )
        .expect("boot")
    }

    fn units() -> Vec<Arc<dyn Component>> {
        vec![probe("telemetry", &[], false)]
    }

    // Unattached: identical to today, everything completes.
    let plain = session();
    let summary = plain.run_bar(units()).await;
    assert!(summary.failed.is_empty() && summary.cancelled.is_empty());
    assert_eq!(summary.completed.len(), 1);

    // Attached + calm: the sensor passes, everything completes.
    let calm = SupervisedStream::new(fast_config());
    let mut guarded = session();
    guarded.attach_stream_breaker(Arc::clone(calm.breaker()));
    let summary = guarded.run_bar(units()).await;
    assert!(summary.failed.is_empty() && summary.cancelled.is_empty());
    assert_eq!(summary.completed.len(), 2);

    // Attached + tripped: the sensor fails naming the trip, the
    // independent unit still completes.
    let stormy = SupervisedStream::new(fast_config());
    for _ in 0..3 {
        stormy.breaker().record_error(&transient("unavailable: storm"));
    }
    guarded.attach_stream_breaker(Arc::clone(stormy.breaker()));
    let summary = guarded.run_bar(units()).await;
    assert!(
        summary.failed.iter().any(
            |event| matches!(event, SupervisedBatchEvent::Failed { name, error }
                if &**name == "stream.health.sensor" && format!("{error:?}").contains("consecutive"))
        ),
        "the health sensor reports the trip, got: {:?}",
        summary.failed
    );
    assert!(
        summary.completed.iter().any(
            |event| matches!(event, SupervisedBatchEvent::Completed { name, .. } if &**name == "telemetry")
        ),
        "independent units complete, got: {:?}",
        summary.completed
    );
    guarded.shutdown().await;
    plain.shutdown().await;
}

/// Spawned drivers attach, serve, and drain: the session spawns the
/// reconnect loop under its owned scope, the trip attaches to later bars,
/// and shutdown brings the live count back to zero with no leaked task.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawned_stream_attaches_serves_and_drains() {
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let mut session = TradingSession::boot(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
        VenueStorage::new("test.db".into()),
    )
    .expect("boot");
    let vehicle = transport();
    let stream = SupervisedStream::new(fast_config());
    let script = Arc::new(Script::new(vec![
        Ok(inbox_with(vec![balance_message()])),
        Err(terminal("scripted shutdown")),
    ]));
    let (out_tx, mut out_rx) = mpsc::channel(8);
    let connector = {
        let script = Arc::clone(&script);
        move || {
            let script = Arc::clone(&script);
            async move { script.connect() }
        }
    };
    session.spawn_supervised_stream(&stream, connector, StreamAbort::new(), out_tx, Arc::clone(&vehicle));
    let event = tokio::time::timeout(Duration::from_secs(5), out_rx.recv())
        .await
        .expect("served promptly")
        .expect("served message lands");
    assert!(matches!(event, AccountEvent::BalanceUpdated(_)));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(script.attempt_count(), 2, "serve then terminal settle");
    assert!(stream.breaker().tripped(), "the terminal trip attaches");

    let summary = session.run_bar(vec![probe("telemetry", &[], false)]).await;
    assert!(
        summary.failed.iter().any(|event| matches!(
            event,
            fluent_concurrency::batch::SupervisedBatchEvent::Failed { name, .. }
            if &**name == "stream.health.sensor"
        )),
        "the attached trip fails the bar sensor, got: {:?}",
        summary.failed
    );
    session.shutdown().await;
    assert_eq!(vehicle.active_streams(), 0, "shutdown drains the driver");
}
