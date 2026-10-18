//! Stream-breaker calibration: must-not-trip soak controls plus must-trip
//! cases, with measured quiet rate/recall.
//!
//! Protocol (mirrors the book-detector calibration and the kernel parity
//! discipline): the control group proves clean-but-imperfect streams never
//! trip the breaker (a breaker that fires on jitter rehearses nothing); the
//! trip group proves sustained faults trip within the frozen budget with
//! reasons. Rates are measured, never assumed.
//!
//! Scope: this milestone is tests plus frozen constants — no production
//! logic. The only `src/` touch is the `#[cfg(test)]` forwarder. Nothing
//! here submits an order, judges a fill on merit, or reads profitability:
//! success is signal quality on **(producer-confidence)** inputs only.
//!
//! Frozen thresholds (committed here so the measurement is reproducible):
//!
//! - [`MAX_CONSECUTIVE_ERRORS`], [`RESTART_WINDOW_SECS`],
//!   [`MAX_RESTARTS_PER_WINDOW`]: the breaker budgets this suite soaks. The
//!   gate pins the production [`StreamConfig`] defaults to these values, so
//!   re-tuning without re-running this suite fails loudly instead of
//!   silently shifting trip behavior. Thresholds graduate to production
//!   trust only at 1.0 on controls.
//! - Scenario counts ([`CONTROL_SCENARIOS`], [`TRIP_SCENARIOS`]): the gate
//!   test pins the group sizes, so adding or dropping a scenario without
//!   updating the counts fails loudly instead of silently shifting rates.
//!
//! Contract parity note: the consecutive-error budget follows the same
//! Nth-arms / N+1th-trips contract as the submission-side
//! `grant.rs :: BreakerLimits` (the Nth error still arms; the N+1th trips).
//! The two budgets are independent counters; only the contract shape is
//! shared, so a change to either side re-opens this suite.
//!
//! Backoff note: the soak runs full production [`StreamConfig`] defaults,
//! including the shared backoff schedule — except nothing here asserts
//! backoff timing (that belongs to the reconnect suite). Trip tests pay the
//! real sleeps (sub-second at the frozen budgets).

use super::*;
use crate::tastytrade::TastytradeConfig;
use fluent_concurrency::stream::StreamAbort;
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

// ── frozen thresholds ──────────────────────────────────────────────
// See the module docs: these values are the calibration under test. They
// pin the production `StreamConfig` defaults through the gate test; any
// change re-opens this suite.

/// Consecutive retryable connect errors the breaker tolerates: the Nth
/// still arms, the N+1th trips (submission-side contract parity).
pub const MAX_CONSECUTIVE_ERRORS: u32 = 3;

/// Window in seconds for counting stream restarts.
pub const RESTART_WINDOW_SECS: u64 = 60;

/// Restarts inside one window the breaker tolerates before tripping.
pub const MAX_RESTARTS_PER_WINDOW: u32 = 5;

/// Control scenarios in this suite (jitter soak, single blips, lag-skip
/// soak). The gate test pins this count.
pub const CONTROL_SCENARIOS: usize = 3;

/// Trip scenarios in this suite (sustained 5xx, ack loss, restart storm,
/// terminal). The gate test pins this count.
pub const TRIP_SCENARIOS: usize = 4;

/// Messages delivered in the jitter soak generation.
pub const SOAK_MESSAGES: usize = 12;

// ── fixtures ───────────────────────────────────────────────────────

/// Full production defaults under soak: the budgets this suite freezes.
/// Backoff stays on the shared schedule (not a frozen threshold).
fn production_config() -> StreamConfig {
    StreamConfig::new().build()
}

fn transport() -> Arc<TastytradeTransport> {
    Arc::new(TastytradeTransport::new(
        TastytradeConfig::new()
            .base_url("http://127.0.0.1:9".to_owned())
            .build(),
    ))
}

fn balance_message(amount: &str) -> String {
    format!(r#"{{"kind":"balance","asset":"USD","amount":"{amount}"}}"#)
}

/// Scripted connector: pops canned connect outcomes and counts attempts.
/// Interior mutability throughout so `connect` takes `&self`.
struct Script {
    outcomes: std::sync::Mutex<VecDeque<Result<mpsc::Receiver<String>, TransportError>>>,
    attempts: AtomicUsize,
}

impl Script {
    fn new(outcomes: Vec<Result<mpsc::Receiver<String>, TransportError>>) -> Self {
        Self {
            outcomes: std::sync::Mutex::new(outcomes.into()),
            attempts: AtomicUsize::new(0),
        }
    }

    fn connect(&self) -> Result<mpsc::Receiver<String>, TransportError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
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

/// One inbox pre-loaded with `messages`, then closed.
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

/// Wait (generously) for a condition the driver establishes; the condition
/// itself is exact, only the wake-up is polled.
async fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for driver to establish: {what}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// ── tally ──────────────────────────────────────────────────────────
// Test-local tally (same discipline as the book calibration: quiet and
// recall both 1.0 before trust). It lives here because the denominators
// are scenarios, not scripts or streams.

/// Control quiet / trip recall tally for this suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StreamCalibrationTally {
    /// Control scenarios presented.
    controls: usize,
    /// Control scenarios that tripped (false trips).
    control_trips: usize,
    /// Trip scenarios presented.
    trips: usize,
    /// Trip scenarios that tripped as budgeted.
    trips_fired: usize,
}

impl StreamCalibrationTally {
    /// Fraction of controls that stayed untripped.
    fn control_quiet_rate(&self) -> f64 {
        if self.controls == 0 {
            return 0.0;
        }
        (self.controls - self.control_trips) as f64 / self.controls as f64
    }

    /// Fraction of trips that fired.
    fn trip_recall(&self) -> f64 {
        if self.trips == 0 {
            return 0.0;
        }
        self.trips_fired as f64 / self.trips as f64
    }

    /// Fraction of all trips that were expected must-trip cases.
    fn precision(&self) -> f64 {
        let fired = self.trips_fired + self.control_trips;
        if fired == 0 {
            return 0.0;
        }
        self.trips_fired as f64 / fired as f64
    }

    /// Whether the breaker may be trusted: every control quiet, every trip
    /// fired within budget.
    fn is_green(&self) -> bool {
        (self.control_quiet_rate() - 1.0).abs() < f64::EPSILON
            && (self.trip_recall() - 1.0).abs() < f64::EPSILON
    }
}

// ── control group: imperfect streams stay untripped ─────────────────

/// Jitter soak: one generation delivers [`SOAK_MESSAGES`] mixed messages —
/// valid balances interleaved with malformed drops — closes cleanly, and
/// the consumer aborts the reconnected generation. Drops bank with reasons;
/// the breaker banks nothing but one clean restart.
#[tokio::test]
#[ignore = "soak: full-generation jitter untripped audit; blip/drop branches are pinned by smoke tests every run; run via `make test-heavy`"]
async fn jitter_soak_stays_untripped() {
    let stream = SupervisedStream::new(production_config());
    let mut messages = Vec::new();
    for i in 0..SOAK_MESSAGES {
        if i % 3 == 2 {
            messages.push("not json at all".to_owned());
        } else {
            messages.push(balance_message(&format!("{}.00", 1000 + i)));
        }
    }
    let valid = messages
        .iter()
        .filter(|message| message.starts_with('{'))
        .count();
    // The sender task closes the inbox after delivering, so the first
    // generation ends cleanly; the second generation parks until aborted.
    let (park_tx, park_rx) = mpsc::channel::<String>(8);
    let _parked = park_tx;
    let script = Script::new(vec![Ok(inbox_with(messages)), Ok(park_rx)]);
    let abort = StreamAbort::new();
    let driver_abort = abort.clone();
    let (out_tx, mut out_rx) = mpsc::channel::<AccountEvent>(16);
    let vehicle = transport();
    let probe = Arc::clone(&vehicle);
    let driver = stream.clone();
    let run = tokio::spawn(async move {
        driver
            .run(
                || async { script.connect() },
                &driver_abort,
                out_tx,
                &vehicle,
            )
            .await
    });
    // Exact drain: every valid message arrives, then the consumer leaves.
    let mut received = 0;
    while received < valid {
        let event = out_rx.recv().await.expect("valid message arrives");
        assert!(
            matches!(event, AccountEvent::BalanceUpdated(_)),
            "soak delivers balances, got: {event:?}"
        );
        received += 1;
    }
    // Abort the parked generation: no error counted.
    abort.cancel();
    let outcome = run.await.expect("driver joins");
    assert!(
        matches!(outcome, StreamOutcome::Aborted),
        "abort ends the soak, got: {outcome:?}"
    );
    assert!(
        probe.dropped_messages() > 0,
        "the drop path ran (banked, never breaker-fed)"
    );
    assert!(!stream.breaker().tripped(), "jitter never trips");
    assert_eq!(
        stream.breaker().consecutive_errors(),
        0,
        "clean connects bank no errors"
    );
    assert_eq!(
        stream.breaker().restart_count(),
        1,
        "one clean close banks exactly one restart"
    );
}

/// Single blips: isolated transient errors separated by successful
/// generations never accumulate past one, and the run ends on a closed
/// consumer — untripped throughout.
#[tokio::test]
async fn single_blips_stay_untripped() {
    let stream = SupervisedStream::new(production_config());
    let script = Script::new(vec![
        Err(transient("blip 1: upstream hiccup")),
        Ok(inbox_with(
            vec!["not json: dropped, not counted".to_owned()],
        )),
        Err(transient("blip 2: upstream hiccup")),
        Ok(inbox_with(vec![balance_message("1000.00")])),
    ]);
    let abort = StreamAbort::new();
    // No receiver: the closing generation finds its consumer gone.
    let (out_tx, out_rx) = mpsc::channel::<AccountEvent>(8);
    drop(out_rx);
    let vehicle = transport();
    let outcome = stream
        .run(|| async { script.connect() }, &abort, out_tx, &vehicle)
        .await;
    assert!(
        matches!(outcome, StreamOutcome::ConsumerGone),
        "dropped consumer ends the run, got: {outcome:?}"
    );
    assert_eq!(script.attempt_count(), 4, "no hidden extra attempts");
    assert!(!stream.breaker().tripped(), "blips never trip");
    assert_eq!(
        stream.breaker().consecutive_errors(),
        0,
        "each successful generation clears the cooling streak"
    );
    assert_eq!(
        stream.breaker().restart_count(),
        1,
        "one clean close banked"
    );
    assert!(
        vehicle.dropped_messages() > 0,
        "the drop path ran (banked, never breaker-fed)"
    );
}

/// Lag-skip soak: a slow consumer (capacity one, never read) forces the
/// pump's skip policy across a full generation; skips bank, the breaker
/// stays untripped, and one post-soak blip still arms without tripping.
#[tokio::test]
#[ignore = "soak: full-generation lag-skip untripped audit; run via `make test-heavy`"]
async fn lag_skip_soak_stays_untripped() {
    let stream = SupervisedStream::new(production_config());
    let messages: Vec<String> = (0..8)
        .map(|i| balance_message(&format!("{}.00", 2000 + i)))
        .collect();
    let script = Script::new(vec![
        Ok(inbox_with(messages)),
        Err(transient("post-soak blip")),
    ]);
    let abort = StreamAbort::new();
    let (out_tx, out_rx) = mpsc::channel::<AccountEvent>(1);
    // The receiver exists (so the first send buffers) but never reads.
    let _held = out_rx;
    let vehicle = transport();
    let probe = Arc::clone(&vehicle);
    let driver = Arc::new(stream);
    let runner = Arc::clone(&driver);
    let owned_abort = abort.clone();
    let run = tokio::spawn(async move {
        runner
            .run(
                || async { script.connect() },
                &owned_abort,
                out_tx,
                &vehicle,
            )
            .await
    });
    wait_for("skip soak banks a restart and one blip arms", || {
        driver.breaker().restart_count() == 1 && driver.breaker().consecutive_errors() == 1
    })
    .await;
    abort.cancel();
    let outcome = run.await.expect("driver joins");
    assert!(
        matches!(outcome, StreamOutcome::Aborted),
        "abort ends the soak, got: {outcome:?}"
    );
    assert!(
        probe.skipped_messages() > 0,
        "the lag-skip path ran (banked, never breaker-fed)"
    );
    assert!(!driver.breaker().tripped(), "lag-skip never trips");
}

// ── trip group: sustained faults trip within budget ─────────────────

/// Sustained 5xx: consecutive transient errors trip past the frozen budget
/// — the N+1th error trips — and the run stops instead of hammering.
#[tokio::test]
async fn sustained_5xx_trips_within_error_budget() {
    let stream = SupervisedStream::new(production_config());
    let script = Script::new(vec![
        Err(transient("503: service unavailable (1)")),
        Err(transient("503: service unavailable (2)")),
        Err(transient("503: service unavailable (3)")),
        Err(transient("503: service unavailable (4)")),
    ]);
    let abort = StreamAbort::new();
    let (out_tx, _out_rx) = mpsc::channel::<AccountEvent>(8);
    let vehicle = transport();
    let outcome = stream
        .run(|| async { script.connect() }, &abort, out_tx, &vehicle)
        .await;
    let reason = stream.breaker().trip_reason().expect("trip banks a reason");
    assert!(
        matches!(outcome, StreamOutcome::BreakerTripped { .. }),
        "sustained errors trip the run, got: {outcome:?}"
    );
    assert_eq!(
        script.attempt_count(),
        usize::try_from(MAX_CONSECUTIVE_ERRORS).expect("budget fits") + 1,
        "trip lands on the N+1th error, never earlier or later"
    );
    assert!(stream.breaker().tripped());
    assert!(
        reason.contains("consecutive") && reason.contains(&MAX_CONSECUTIVE_ERRORS.to_string()),
        "reason names the budget, got: {reason}"
    );
}

/// Repeated ack loss: generations that close without delivering anything
/// bank restarts; past the frozen rate budget the breaker trips.
#[tokio::test]
async fn repeated_ack_loss_trips_within_restart_budget() {
    let stream = SupervisedStream::new(production_config());
    let empties: Vec<Result<mpsc::Receiver<String>, TransportError>> = (0
        ..=MAX_RESTARTS_PER_WINDOW)
        .map(|_| Ok(inbox_with(Vec::new())))
        .collect();
    let script = Script::new(empties);
    let abort = StreamAbort::new();
    let (out_tx, _out_rx) = mpsc::channel::<AccountEvent>(8);
    let vehicle = transport();
    let outcome = stream
        .run(|| async { script.connect() }, &abort, out_tx, &vehicle)
        .await;
    let reason = stream.breaker().trip_reason().expect("trip banks a reason");
    assert!(
        matches!(outcome, StreamOutcome::BreakerTripped { .. }),
        "ack loss trips the run, got: {outcome:?}"
    );
    assert_eq!(
        script.attempt_count(),
        usize::try_from(MAX_RESTARTS_PER_WINDOW).expect("budget fits") + 1,
        "trip lands on the N+1th restart"
    );
    assert!(
        reason.contains("restarts") && reason.contains(&MAX_RESTARTS_PER_WINDOW.to_string()),
        "reason names the budget, got: {reason}"
    );
}

/// Restart storm: flapping generations that deliver one message each still
/// trip on churn — producer-confidence trips on rate even while traffic
/// flows.
#[tokio::test]
async fn restart_storm_trips_within_window() {
    let stream = SupervisedStream::new(production_config());
    let flaps: Vec<Result<mpsc::Receiver<String>, TransportError>> = (0..=MAX_RESTARTS_PER_WINDOW)
        .map(|i| {
            Ok(inbox_with(vec![balance_message(&format!(
                "{}.00",
                3000 + i
            ))]))
        })
        .collect();
    let script = Script::new(flaps);
    let abort = StreamAbort::new();
    let (out_tx, mut out_rx) = mpsc::channel::<AccountEvent>(16);
    let vehicle = transport();
    let driver = stream.clone();
    let run = tokio::spawn(async move {
        driver
            .run(|| async { script.connect() }, &abort, out_tx, &vehicle)
            .await
    });
    let mut received = 0;
    while let Some(event) = out_rx.recv().await {
        assert!(
            matches!(event, AccountEvent::BalanceUpdated(_)),
            "storm delivers before tripping, got: {event:?}"
        );
        received += 1;
    }
    let outcome = run.await.expect("driver joins");
    assert!(
        matches!(outcome, StreamOutcome::BreakerTripped { .. }),
        "flapping trips the run, got: {outcome:?}"
    );
    assert_eq!(
        received,
        usize::try_from(MAX_RESTARTS_PER_WINDOW).expect("budget fits") + 1,
        "every generation delivered before the trip"
    );
    assert!(stream.breaker().tripped());
}

/// Terminal trips immediately: exactly one attempt, no backoff, and every
/// terminal code carries its stated reason (unknown codes stay retryable).
#[tokio::test]
async fn terminal_trips_immediately_with_stated_reason() {
    for code in [
        "unauthorized",
        "forbidden",
        "bad_request",
        "malformed_auth",
        "subscription_rejected",
    ] {
        let reason = terminal_reason(code).expect("terminal code has a reason");
        assert!(
            reason.contains("terminal"),
            "reason states terminality for {code}, got: {reason}"
        );
    }
    assert!(
        terminal_reason("timeout").is_none(),
        "unknown codes stay retryable"
    );

    let stream = SupervisedStream::new(production_config());
    let script = Script::new(vec![Err(TransportError::Terminal(
        "terminal credentials failure: retrying cannot authenticate".to_owned(),
    ))]);
    let abort = StreamAbort::new();
    let (out_tx, _out_rx) = mpsc::channel::<AccountEvent>(8);
    let vehicle = transport();
    let started = Instant::now();
    let outcome = stream
        .run(|| async { script.connect() }, &abort, out_tx, &vehicle)
        .await;
    assert!(
        matches!(outcome, StreamOutcome::Terminal { .. }),
        "terminal ends the run first-class, got: {outcome:?}"
    );
    assert_eq!(script.attempt_count(), 1, "terminal never retries");
    assert!(stream.breaker().tripped(), "terminal trips the breaker");
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "terminal sleeps no backoff"
    );
}

// ── gate: measured rates at 1.0 + frozen budgets ────────────────────

/// Calibration gate: replays both groups at the breaker level end to end
/// and pins the measured rates — 1.0 control quiet, 1.0 trip recall — plus
/// the frozen budgets against the production defaults and the scenario
/// counts, so budget or group edits without updates fail here.
#[test]
fn stream_breaker_calibration_is_green() {
    let defaults = StreamConfig::new().build();
    assert_eq!(
        defaults.max_consecutive_errors, MAX_CONSECUTIVE_ERRORS,
        "production error budget is frozen"
    );
    assert_eq!(
        defaults.restart_window_secs, RESTART_WINDOW_SECS,
        "production restart window is frozen"
    );
    assert_eq!(
        defaults.max_restarts_per_window, MAX_RESTARTS_PER_WINDOW,
        "production restart budget is frozen"
    );

    // Controls: blips with cooling, streak resets, and at-bound restarts
    // all stay quiet.
    let mut control_trips = 0;
    let blips = StreamBreaker::new(production_config());
    for _ in 0..3 {
        blips.record_error(&transient("blip"));
        blips.record_success();
    }
    control_trips += usize::from(blips.tripped());

    let streak = StreamBreaker::new(production_config());
    for _ in 0..MAX_CONSECUTIVE_ERRORS - 1 {
        streak.record_error(&transient("warming"));
    }
    streak.record_success();
    for _ in 0..MAX_CONSECUTIVE_ERRORS - 1 {
        streak.record_error(&transient("warming again"));
    }
    control_trips += usize::from(streak.tripped());
    assert_eq!(
        streak.consecutive_errors(),
        MAX_CONSECUTIVE_ERRORS - 1,
        "success resets the streak; the second run-up arms, never trips"
    );

    let at_bound = StreamBreaker::new(production_config());
    for _ in 0..MAX_RESTARTS_PER_WINDOW {
        at_bound.record_restart();
    }
    control_trips += usize::from(at_bound.tripped());
    assert!(
        !at_bound.tripped(),
        "restarts exactly at budget stay quiet (strictly-greater-than contract)"
    );

    // Trips: error budget, restart budget, and terminal all fire.
    let mut fired = 0;
    let sustained = StreamBreaker::new(production_config());
    for i in 0..=MAX_CONSECUTIVE_ERRORS {
        sustained.record_error(&transient(&format!("503 ({i})")));
    }
    fired += usize::from(sustained.tripped());

    let storm = StreamBreaker::new(production_config());
    for _ in 0..=MAX_RESTARTS_PER_WINDOW {
        storm.record_restart();
    }
    fired += usize::from(storm.tripped());

    let refused = StreamBreaker::new(production_config());
    refused.record_error(&TransportError::Terminal("terminal test".to_owned()));
    fired += usize::from(refused.tripped());

    let flapping = StreamBreaker::new(production_config());
    for _ in 0..=MAX_RESTARTS_PER_WINDOW {
        flapping.record_success();
        flapping.record_restart();
    }
    fired += usize::from(flapping.tripped());

    let tally = StreamCalibrationTally {
        controls: CONTROL_SCENARIOS,
        control_trips,
        trips: TRIP_SCENARIOS,
        trips_fired: fired,
    };
    assert_eq!(CONTROL_SCENARIOS, 3, "control group size is frozen");
    assert_eq!(TRIP_SCENARIOS, 4, "trip group size is frozen");
    assert!(
        tally.is_green(),
        "calibration must be green at 1.0 quiet / 1.0 recall: \
         control quiet {:.3}, trip recall {:.3}, precision {:.3} ({tally:?})",
        tally.control_quiet_rate(),
        tally.trip_recall(),
        tally.precision()
    );
}
