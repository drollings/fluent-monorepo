//! Minimal consecutive-failure breaker with cooldown.
//!
//! Only transport failure (`Unavailable`) counts toward the trip: a permanent
//! rejection proves the endpoint is alive, and a decode failure is a
//! task-value outcome, not endpoint health. No new pool, no new retry — the
//! existing `retry_async` schedule stays the single backoff.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::decision::DecisionError;
use common_core::sync::lock;

struct BreakerState {
    consecutive: u32,
    open_until: Option<Instant>,
    /// A post-cooldown trial is in flight: concurrent sends wait it out
    /// instead of bursting the recovering endpoint.
    half_open: bool,
}

/// Short-circuits sends to a dead endpoint without sending.
///
/// ```rust
/// use fluent_llm::decision::DecisionBreaker;
///
/// let breaker = DecisionBreaker::new(2, 60_000).expect("breaker");
/// breaker.record_transport_failure();
/// assert!(!breaker.is_open());
/// breaker.record_transport_failure();
/// assert!(breaker.is_open());
/// ```
pub struct DecisionBreaker {
    threshold: u32,
    cooldown: Duration,
    state: Mutex<BreakerState>,
}

impl DecisionBreaker {
    /// Trip after `threshold` consecutive transport failures; stay open for
    /// `cooldown_ms`. `threshold` must be ≥ 1.
    pub fn new(threshold: u32, cooldown_ms: u64) -> Result<Self, String> {
        if threshold == 0 {
            return Err("breaker threshold must be >= 1".to_string());
        }
        Ok(Self {
            threshold,
            cooldown: Duration::from_millis(cooldown_ms),
            state: Mutex::new(BreakerState {
                consecutive: 0,
                open_until: None,
                half_open: false,
            }),
        })
    }

    /// `true` while the breaker is open (a send must be skipped). An
    /// in-flight half-open trial also reads open: one trial at a time.
    #[must_use]
    pub fn is_open(&self) -> bool {
        let state = lock(&self.state);
        if state.half_open {
            return true;
        }
        state.open_until.is_some_and(|until| Instant::now() < until)
    }

    /// Gate a send: `Unavailable` when open, without sending. The first
    /// call past the cooldown takes the single half-open trial.
    ///
    /// ```rust
    /// use fluent_llm::decision::DecisionBreaker;
    ///
    /// let breaker = DecisionBreaker::new(1, 60_000).expect("breaker");
    /// assert!(breaker.check().is_ok());
    /// assert!(!breaker.is_open());
    /// ```
    pub fn check(&self) -> Result<(), DecisionError> {
        let mut state = lock(&self.state);
        if state.open_until.is_some_and(|until| Instant::now() < until) {
            return Err(DecisionError::Unavailable("breaker open".to_string()));
        }
        if state.half_open {
            return Err(DecisionError::Unavailable(
                "half-open trial in flight".to_string(),
            ));
        }
        if state.open_until.is_some() {
            state.half_open = true;
            state.open_until = None;
        }
        Ok(())
    }

    /// A transport failure counts toward the trip (and resolves a trial).
    pub fn record_transport_failure(&self) {
        let mut state = lock(&self.state);
        state.half_open = false;
        state.consecutive += 1;
        if state.consecutive >= self.threshold {
            state.open_until = Some(Instant::now() + self.cooldown);
        }
    }

    /// A permanent rejection proves the endpoint answers: reset the count,
    /// never trip (and resolve a trial the same way a success would).
    pub fn record_permanent(&self) {
        let mut state = lock(&self.state);
        state.consecutive = 0;
        state.half_open = false;
    }

    /// A success clears the count and closes the breaker.
    pub fn record_success(&self) {
        let mut state = lock(&self.state);
        state.consecutive = 0;
        state.open_until = None;
        state.half_open = false;
    }
}
