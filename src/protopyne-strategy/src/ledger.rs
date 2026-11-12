//! The grant ledger: one reserve/consume/release implementation over the
//! pure [`evaluate_grant`] rules.
//!
//! Budgets are hard exposure bounds, so authorization is an atomic
//! reservation, not a stateless predicate: two concurrent callers can never
//! both consume the last unit. A [`Reservation`] holds pending counters until
//! the venue verdict settles it; dropping one without settling releases its
//! pending budget (RAII), so a lost verdict never leaks or silently consumes
//! exposure.
//!
//! Breaker streaks live here too, per grant, but stay a separate counter:
//! budgets are task-value, breaker state is producer-confidence, and the two
//! are never conflated into one verdict.

use std::fmt;
use std::sync::Mutex;

use chrono::{DateTime, Utc};

use protopyne_types::Money;
use rust_decimal::Decimal;

use crate::grant::{
    evaluate_grant, evaluate_grant_merit, BreakerState, Grant, GrantDecision, GrantMarket,
    GrantUsage,
};
use crate::proposal::Proposal;
use crate::risk::PortfolioFacts;

/// What the drawdown arm concluded over one facts reading: inert (no cap, or
/// a cap that can never be a threshold), within (configured and holding),
/// breached (configured and strictly past — the ledger trips), or unobserved
/// (configured but the book's equity is unknown — refuse, never pass).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// What the drawdown arm concluded over one facts reading: inert (no cap, or
/// a cap that can never be a threshold), within (configured and holding),
/// breached (configured and strictly past — the ledger trips), or unobserved
/// (configured but the book's equity is unknown — refuse, never pass).
pub enum DrawdownStatus {
    /// No drawdown cap configured, or a garbage cap that arming refused.
    Inert,
    /// Configured and holding at this fraction.
    Within(Decimal),
    /// Configured and strictly past the cap.
    Breached(Decimal),
    /// Configured but the equity reading is missing: refuse, never latch.
    Unobserved,
}

/// Why a grant could not be installed.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Why a grant could not be installed.
pub enum LedgerRefusal {
    /// An adopted grant would widen authority; only tightening is allowed.
    NotTightening,
}

impl fmt::Display for LedgerRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotTightening => f.write_str("a live grant can only be tightened"),
        }
    }
}

impl std::error::Error for LedgerRefusal {}

/// Why a reservation was released instead of settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Why a reservation was released instead of settled.
pub enum ReleaseReason {
    /// The order was never handed to the venue.
    Unsubmitted,
    /// The reservation was explicitly abandoned.
    Abandoned,
}

/// What settling a reservation did to the budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// What settling a reservation did to the budgets.
pub enum Settled {
    /// The committed budget was consumed.
    Consumed,
    /// The reservation stays pending until query-by-ID resolves.
    Retained,
}

/// The venue verdict that settles a reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The venue verdict that settles a reservation.
pub enum FillStatus {
    /// The order filled in full.
    Filled,
    /// The order filled in part.
    Partial,
    /// The venue permanently refused the order.
    Refused,
    /// The outcome is unknown; query-by-ID must resolve it.
    Retryable,
}

/// The settled facts a fill contributes to the budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The settled facts a fill contributes to the budgets.
pub struct FillOutcome {
    /// Venue verdict.
    pub status: FillStatus,
    /// Contracts actually filled.
    pub contracts: u32,
    /// Committed max loss, consumed on a fill.
    pub loss: Money,
    /// Committed notional, consumed on a fill.
    pub notional: Money,
}

struct LedgerState {
    grant: Grant,
    usage: GrantUsage,
    pending: GrantUsage,
    breakers: BreakerState,
}

impl LedgerState {
    fn effective(&self) -> GrantUsage {
        self.usage.plus(&self.pending)
    }
}

/// One run's or session's authorization state: the immutable grant plus its
/// consumed budgets, pending reservations, and breaker streak.
pub struct GrantLedger {
    state: Mutex<LedgerState>,
}

impl GrantLedger {
    /// Install a grant and open an empty ledger.
    #[must_use]
    pub fn new(grant: Grant) -> Self {
        let valid_from = grant.valid_from;
        Self {
            state: Mutex::new(LedgerState {
                grant,
                usage: GrantUsage::new().last_attested(valid_from).build(),
                pending: GrantUsage::new().build(),
                breakers: BreakerState::new().build(),
            }),
        }
    }

    /// Install a grant with a pre-seeded breaker snapshot (replay/test
    /// resume paths).
    #[must_use]
    pub fn with_breakers(grant: Grant, breakers: BreakerState) -> Self {
        let valid_from = grant.valid_from;
        Self {
            state: Mutex::new(LedgerState {
                grant,
                usage: GrantUsage::new().last_attested(valid_from).build(),
                pending: GrantUsage::new().build(),
                breakers,
            }),
        }
    }

    /// Record an operator attestation at `at`: the usage's last-attested
    /// instant moves forward to `at`, never backwards, and budgets do not
    /// move at all — attesting twice consumes nothing. The loop calls this
    /// for every journaled `Attest` row it executes.
    pub fn observe_attested(&self, at: DateTime<Utc>) {
        let mut state = common_core::sync::lock(&self.state);
        if state.usage.last_attested.is_none_or(|known| at > known) {
            state.usage.last_attested = Some(at);
        }
    }

    /// Adopt a new grant only when it tightens the installed one. Loosening
    /// needs a new grant and a fresh confirmation; it never mutates this
    /// ledger.
    pub fn adopt(&self, grant: Grant) -> Result<(), LedgerRefusal> {
        let mut state = common_core::sync::lock(&self.state);
        if !grant.tightens(&state.grant) {
            return Err(LedgerRefusal::NotTightening);
        }
        state.grant = grant;
        Ok(())
    }

    /// The installed grant.
    #[must_use]
    pub fn grant(&self) -> Grant {
        common_core::sync::lock(&self.state).grant.clone()
    }

    /// Consumed budgets (pending reservations excluded).
    #[must_use]
    pub fn usage(&self) -> GrantUsage {
        common_core::sync::lock(&self.state).usage
    }

    /// Reserved-but-unsettled budgets.
    #[must_use]
    pub fn pending(&self) -> GrantUsage {
        common_core::sync::lock(&self.state).pending
    }

    /// The breaker snapshot (producer-confidence, never budget truth).
    #[must_use]
    pub fn breakers(&self) -> BreakerState {
        common_core::sync::lock(&self.state).breakers.clone()
    }

    /// Evaluate the grant without reserving: the merit verdict over consumed
    /// plus pending budgets. Breaker input is excluded, so a tripped breaker
    /// never rewrites the trade's recorded merit; attestation is excluded
    /// for the same reason, so a lapsed cadence never rewrites it either.
    #[must_use]
    pub fn evaluate(
        &self,
        proposal: &Proposal,
        market: &GrantMarket,
        now: DateTime<Utc>,
    ) -> GrantDecision {
        let state = common_core::sync::lock(&self.state);
        evaluate_grant_merit(
            proposal,
            &state.grant,
            &state.effective(),
            market,
            &clean_breakers(),
            now,
        )
    }

    /// Atomically evaluate and reserve this proposal's worst-case budgets.
    /// The reservation drops back to free unless it is settled or retained.
    pub fn reserve(
        &self,
        proposal: &Proposal,
        market: &GrantMarket,
        now: DateTime<Utc>,
    ) -> Result<Reservation<'_>, GrantDecision> {
        let mut state = common_core::sync::lock(&self.state);
        let decision = evaluate_grant(
            proposal,
            &state.grant,
            &state.effective(),
            market,
            &clean_breakers(),
            now,
        );
        if !decision.is_allow() {
            return Err(decision);
        }
        let commit = GrantUsage::new()
            .used_trades(1)
            .used_contracts(proposal.leg_contracts().unwrap_or(0))
            .used_loss(proposal.max_loss)
            .used_notional(proposal.notional)
            .build();
        state.pending = state.pending.plus(&commit);
        Ok(Reservation {
            ledger: self,
            commit,
            settled: false,
        })
    }

    /// Book an operator close fill: the same budget accounting as a settled
    /// entry fill (trades, contracts, notional consumed; no new loss
    /// committed — the position's loss was booked at entry), without a prior
    /// reservation. Operator closes are exits, never grant-gated entries, so
    /// there is no claim to settle — only a fill to book. Consumed budgets
    /// stay monotonic: a close never un-spends the lifetime tally; the
    /// at-rest exposure drops out of the venue-book reading instead.
    pub fn record_operator_fill(&self, outcome: &FillOutcome) -> Settled {
        let mut state = common_core::sync::lock(&self.state);
        apply_fill(&mut state.usage, outcome)
    }
    /// Record a venue verdict in the producer-confidence counter. Only
    /// retryable failures feed the streak; a permanent refusal speaks to the
    /// trade, never to the venue's health.
    pub fn observe_error(&self, retryable: bool) {
        if !retryable {
            return;
        }
        let mut state = common_core::sync::lock(&self.state);
        state.breakers.consecutive_errors = state.breakers.consecutive_errors.saturating_add(1);
        if !state.breakers.tripped
            && state.breakers.consecutive_errors > state.grant.breakers.max_consecutive_errors
        {
            state.breakers.tripped = true;
            state.breakers.trip_reason = Some(format!(
                "{} consecutive venue errors exceed max {}",
                state.breakers.consecutive_errors, state.grant.breakers.max_consecutive_errors,
            ));
        }
    }

    /// Judge the book's equity against the grant's drawdown cap and report
    /// the arm's status. A breach trips exactly like the error streak — the
    /// flag latches and the reason banks once, so the run stays disarmed and
    /// the trip reads the same on every later evaluation. Anything else
    /// leaves the breaker state untouched: `Unobserved` refuses the current
    /// evaluation without latching (the feed may recover), and `Inert` and
    /// `Within` move nothing.
    pub fn observe_drawdown(&self, facts: &PortfolioFacts) -> DrawdownStatus {
        let mut state = common_core::sync::lock(&self.state);
        let Some(cap) = state.grant.breakers.max_portfolio_drawdown_fraction else {
            return DrawdownStatus::Inert;
        };
        if !(cap > 0.0 && cap <= 1.0 && cap.is_finite()) {
            return DrawdownStatus::Inert;
        }
        let (Some(peak), Some(now)) = (facts.equity_peak, facts.equity_now) else {
            return DrawdownStatus::Unobserved;
        };
        if !facts.observed {
            return DrawdownStatus::Unobserved;
        }
        let Some(fraction) =
            protopyne_kernels::drawdown_of_peak(peak.as_decimal(), now.as_decimal())
        else {
            return DrawdownStatus::Unobserved;
        };
        let Some(limit) = Decimal::from_f64_retain(cap) else {
            return DrawdownStatus::Inert;
        };
        if fraction <= limit {
            return DrawdownStatus::Within(fraction);
        }
        if !state.breakers.tripped {
            state.breakers.tripped = true;
            state.breakers.trip_reason =
                Some(format!("portfolio drawdown {fraction} past max {cap}"));
        }
        DrawdownStatus::Breached(fraction)
    }

    /// Clear a cooling streak after a successful venue call. A tripped
    /// breaker stays tripped for the run.
    pub fn observe_success(&self) {
        let mut state = common_core::sync::lock(&self.state);
        if !state.breakers.tripped {
            state.breakers.consecutive_errors = 0;
        }
    }
}

fn clean_breakers() -> BreakerState {
    BreakerState::new().build()
}

/// Book one known fill into the consumed budgets: fills consume trades,
/// contracts, loss, and notional; a permanent refusal consumes the attempted
/// trade only. A retryable (unknown) outcome books nothing — the order may
/// still fill, so only a retained reservation (never a bare booking) may
/// carry it.
fn apply_fill(usage: &mut GrantUsage, outcome: &FillOutcome) -> Settled {
    match outcome.status {
        FillStatus::Filled | FillStatus::Partial => {
            usage.used_trades = usage.used_trades.saturating_add(1);
            usage.used_contracts = usage.used_contracts.saturating_add(outcome.contracts);
            usage.used_loss = add(usage.used_loss, outcome.loss);
            usage.used_notional = add(usage.used_notional, outcome.notional);
            Settled::Consumed
        }
        FillStatus::Refused => {
            usage.used_trades = usage.used_trades.saturating_add(1);
            Settled::Consumed
        }
        FillStatus::Retryable => Settled::Retained,
    }
}

/// A pending claim on the grant's budgets. Settle it with the venue verdict
/// or release it; dropping an unsettled reservation releases it.
pub struct Reservation<'a> {
    ledger: &'a GrantLedger,
    commit: GrantUsage,
    settled: bool,
}

impl Reservation<'_> {
    /// Convert the pending claim into consumed budget according to the venue
    /// verdict. `Filled`/`Partial` consume the committed loss and notional;
    /// a permanent refusal consumes the attempted trade only; a retryable
    /// failure keeps the reservation pending until query-by-ID resolves.
    pub fn settle(mut self, outcome: &FillOutcome) -> Settled {
        self.settled = true;
        let mut state = common_core::sync::lock(&self.ledger.state);
        state.pending = state.pending.minus(&self.commit);
        if outcome.status == FillStatus::Retryable {
            state.pending = state.pending.plus(&self.commit);
            return Settled::Retained;
        }
        apply_fill(&mut state.usage, outcome);
        Settled::Consumed
    }

    /// Release the pending claim without consuming budget.
    pub fn release(mut self, _why: ReleaseReason) {
        self.settled = true;
        let mut state = common_core::sync::lock(&self.ledger.state);
        state.pending = state.pending.minus(&self.commit);
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let mut state = common_core::sync::lock(&self.ledger.state);
        state.pending = state.pending.minus(&self.commit);
    }
}

impl GrantUsage {
    /// Consumed plus pending counters. Attestation rides along (the newer
    /// of the two): budgets combine, authority does not reset.
    fn plus(&self, other: &Self) -> Self {
        let mut combined = Self::new()
            .used_trades(self.used_trades.saturating_add(other.used_trades))
            .used_contracts(self.used_contracts.saturating_add(other.used_contracts))
            .used_loss(add(self.used_loss, other.used_loss))
            .used_notional(add(self.used_notional, other.used_notional))
            .build();
        combined.last_attested = max_attested(self.last_attested, other.last_attested);
        combined
    }

    /// Counter-wise saturating subtraction.
    fn minus(&self, other: &Self) -> Self {
        Self::new()
            .used_trades(self.used_trades.saturating_sub(other.used_trades))
            .used_contracts(self.used_contracts.saturating_sub(other.used_contracts))
            .used_loss(sub(self.used_loss, other.used_loss))
            .used_notional(sub(self.used_notional, other.used_notional))
            .build()
    }
}

fn max_attested(
    left: Option<chrono::DateTime<Utc>>,
    right: Option<chrono::DateTime<Utc>>,
) -> Option<chrono::DateTime<Utc>> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(at), None) | (None, Some(at)) => Some(at),
        (None, None) => None,
    }
}

fn add(left: Money, right: Money) -> Money {
    // (M3.2: `Money::saturating_add` matches `+` absent overflow, which is
    // unreachable at market magnitudes.)
    left.saturating_add(right)
}

fn sub(left: Money, right: Money) -> Money {
    // (M3.2: `Money::saturating_sub` floors at zero — exact match.)
    left.saturating_sub(right)
}

#[cfg(test)]
#[path = "../tests/ledger.rs"]
mod tests;