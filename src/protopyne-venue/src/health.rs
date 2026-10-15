//! Health gate: trade only on fresh data, a sane clock, a live stream, and
//! a reachable, current store.
//!
//! Every bound here is **(correctness)**: quote age, stream state, clock
//! skew, and store liveness decide whether a decision is valid, never whether
//! it is attractive. A closed gate refuses with reasons; it never degrades
//! into a warning that trading continues past.

use bon::Builder;
use serde::{Deserialize, Serialize};

use protopyne_store::StoreHealth;

use crate::error::VenueError;

/// Observed health facts. State, not opinion: callers fill these from the
/// feed, the clock, the connection supervisor, and the store's coverage
/// probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthState {
    /// Age of the newest quote in seconds.
    pub quote_age_secs: u64,
    /// Observed clock skew in seconds.
    pub clock_skew_secs: u64,
    /// Whether the market-data stream is connected.
    pub stream_connected: bool,
    /// Latest store liveness reading. `Default` is no reading, which is no
    /// objection: a state that never observed the store evaluates exactly
    /// as it did before the store gate existed.
    pub store: StoreHealth,
}

/// Health bounds. All five are **(correctness)** gates.
#[derive(Debug, Clone, Copy, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct HealthLimits {
    /// Maximum quote age in seconds **(correctness)**.
    #[builder(default = 60)]
    pub max_quote_age_secs: u64,
    /// Maximum clock skew in seconds **(correctness)**.
    #[builder(default = 5)]
    pub max_clock_skew_secs: u64,
    /// Maximum age of the newest committed bar in seconds
    /// **(correctness)**. Mirrors the quote-age horizon: one staleness
    /// definition for the decision's inputs, whether they arrive over the
    /// feed or out of the store.
    #[builder(default = 60)]
    pub max_store_watermark_age_secs: u64,
    /// Maximum unresolved store holes tolerated **(correctness)**. Zero:
    /// any hole the last classification reported halts entries until the
    /// repair pass closes it.
    #[builder(default = 0)]
    pub max_open_gaps: u64,
}

/// Whether the platform may trade right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthVerdict {
    /// All gates pass.
    Trade,
    /// At least one gate fails, with every reason.
    Halt {
        /// Every failed gate.
        reasons: Vec<String>,
    },
}

impl HealthVerdict {
    /// Whether trading may proceed.
    #[must_use]
    pub fn may_trade(&self) -> bool {
        matches!(self, Self::Trade)
    }
}

/// Evaluate the health gate. Pure function: same facts, same verdict.
#[must_use]
pub fn may_trade(state: &HealthState, limits: &HealthLimits) -> HealthVerdict {
    let mut reasons = Vec::new();
    if state.quote_age_secs > limits.max_quote_age_secs {
        reasons.push(format!(
            "quote age {}s above maximum {}s",
            state.quote_age_secs, limits.max_quote_age_secs
        ));
    }
    if state.clock_skew_secs > limits.max_clock_skew_secs {
        reasons.push(format!(
            "clock skew {}s above maximum {}s",
            state.clock_skew_secs, limits.max_clock_skew_secs
        ));
    }
    if !state.stream_connected {
        reasons.push("market-data stream disconnected".to_owned());
    }
    if !state.store.reachable {
        reasons.push("price store unreachable".to_owned());
    }
    if let Some(age) = state.store.watermark_age_secs {
        if age > limits.max_store_watermark_age_secs {
            reasons.push(format!(
                "store watermark age {}s above maximum {}s",
                age, limits.max_store_watermark_age_secs
            ));
        }
    }
    if state.store.open_gaps > limits.max_open_gaps {
        reasons.push(format!(
            "store reports {} open gaps above maximum {}",
            state.store.open_gaps, limits.max_open_gaps
        ));
    }
    if reasons.is_empty() {
        HealthVerdict::Trade
    } else {
        HealthVerdict::Halt { reasons }
    }
}

/// Evaluate the gate for one intent, or refuse with a [`VenueError`] for the
/// submit path. Exits always route: a halted book must stay manageable, so a
/// halt — store outage or otherwise — never strands an open position.
/// Entries refuse with every halting reason. The fail-closed enforcement
/// twin of [`TradingState::check`](crate::command::TradingState::check):
/// both take the intent's exit-ness and both route exits past a halt.
pub fn check_trade(
    state: &HealthState,
    limits: &HealthLimits,
    is_exit: bool,
) -> Result<(), VenueError> {
    if is_exit {
        return Ok(());
    }
    match may_trade(state, limits) {
        HealthVerdict::Trade => Ok(()),
        HealthVerdict::Halt { reasons } => {
            Err(VenueError::Health(format!("halted: {}", reasons.join("; "))))
        }
    }
}

#[cfg(test)]
#[path = "../tests/health.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/health_store_gate.rs"]
mod tests_store_gate;

#[cfg(test)]
#[path = "../tests/health_exits_survive_store_halt.rs"]
mod tests_exits_survive_store_halt;
