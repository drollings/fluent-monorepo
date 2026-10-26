//! Mandatory risk engine: the non-bypassable gate outside every strategy.
//!
//! Strategy elements shape *what* is proposed; [`RiskEngine`] decides what
//! may proceed, and it cannot be configured away. Approval is a typestate
//! token ([`RiskApproved`]) that only [`RiskEngine::check`] can mint, so a
//! submission path that demands the token cannot skip the gate by
//! construction.

use fluent_wvr::prelude::*;
use fluent_wvr::{Describable, FieldAccess};
use rust_decimal::Decimal;

use chrono::NaiveDate;
use protopyne_types::{Greeks, Money, OptionContract};
use rust_decimal::prelude::ToPrimitive as _;

use crate::proposal::Proposal;

/// Configured risk caps. Every field is a **(correctness)** gate: position
/// and margin limits, staleness and clock checks, the reconciliation gate,
/// limit-only-spread enforcement, and the kill switch.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct RiskLimits {
    /// Maximum contracts (or shares) on any single proposal
    /// **(correctness)**.
    #[field(
        desc = "Maximum contracts on one proposal",
        min = 1,
        max = 1000000,
        axis = "correctness"
    )]
    #[builder(default = 10)]
    pub max_contracts_per_trade: u64,
    /// Maximum committed max loss on one proposal, in account-currency major
    /// units **(correctness)**.
    #[field(
        desc = "Maximum committed loss on one proposal",
        min = 1,
        max = 1000000000,
        axis = "correctness"
    )]
    #[builder(default = 500.0)]
    pub max_loss_per_trade: f64,
    /// Maximum committed notional on one proposal, in account-currency major
    /// units **(correctness)**.
    #[field(
        desc = "Maximum committed notional on one proposal",
        min = 1,
        max = 10000000000,
        axis = "correctness"
    )]
    #[builder(default = 50_000.0)]
    pub max_notional: f64,
    /// Maximum quote age in seconds before data counts as stale
    /// **(correctness)**.
    #[field(
        desc = "Maximum quote age in seconds",
        min = 1,
        max = 3600,
        axis = "correctness"
    )]
    #[builder(default = 60)]
    pub quote_max_age_secs: u64,
    /// Maximum tolerable clock skew in seconds **(correctness)**.
    #[field(
        desc = "Maximum clock skew in seconds",
        min = 1,
        max = 3600,
        axis = "correctness"
    )]
    #[builder(default = 5)]
    pub clock_skew_secs: u64,
    /// Whether spreads must be limit orders **(correctness)**.
    #[field(desc = "Require limit orders for spreads", axis = "correctness")]
    #[builder(default = true)]
    pub limit_only_spreads: bool,
    /// When set, every proposal is refused **(correctness)**.
    #[field(
        desc = "Kill switch: refuse every proposal when set",
        axis = "correctness"
    )]
    #[builder(default = false)]
    pub kill_switch: bool,
    /// Maximum aggregate open risk across the book. `None` leaves the cap
    /// unconfigured, so rehearsal is unaffected **(task-value)**.
    #[field(desc = "Maximum aggregate open risk", axis = "task-value")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_open_risk: Option<f64>,
    /// Maximum absolute net portfolio delta. `None` leaves it unconfigured
    /// **(task-value)**.
    #[field(desc = "Maximum absolute net portfolio delta", axis = "task-value")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_abs_net_delta: Option<f64>,
    /// Minimum buying power that must remain available. `None` leaves it
    /// unconfigured **(task-value)**.
    #[field(desc = "Minimum buying power", axis = "task-value")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_buying_power: Option<f64>,
}

impl fluent_wvr::validate::Validate for RiskLimits {
    type Err = String;
    /// Fail-closed self-consistency check shared by supervision
    /// (`execute`) and the decision path (`RiskEngine::check`).
    fn validate(&self) -> Result<(), Self::Err> {
        if self.max_contracts_per_trade < 1 {
            return Err("max_contracts_per_trade must be at least 1".to_owned());
        }
        if !(self.max_loss_per_trade > 0.0 && self.max_loss_per_trade.is_finite()) {
            return Err("max_loss_per_trade must be a positive finite cap".to_owned());
        }
        if !(self.max_notional > 0.0 && self.max_notional.is_finite()) {
            return Err("max_notional must be a positive finite cap".to_owned());
        }
        if self.quote_max_age_secs < 1 || self.quote_max_age_secs > 3_600 {
            return Err("quote_max_age_secs must be within [1, 3600]".to_owned());
        }
        if self.clock_skew_secs < 1 || self.clock_skew_secs > 3_600 {
            return Err("clock_skew_secs must be within [1, 3600]".to_owned());
        }
        for (name, cap) in [
            ("max_open_risk", self.max_open_risk),
            ("max_abs_net_delta", self.max_abs_net_delta),
            ("min_buying_power", self.min_buying_power),
        ] {
            if let Some(cap) = cap {
                if !cap.is_finite() || cap < 0.0 {
                    return Err(format!("{name} must be a non-negative finite cap"));
                }
            }
        }
        Ok(())
    }
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self::new().build()
    }
}

impl WorkUnit for RiskLimits {
    fn name(&self) -> &str {
        "risk_limits"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        fluent_wvr::validate::check_for_execute(self, "risk limits invalid")?;
        Ok(fluent_wvr::validate::valid_output("risk limits valid"))
    }
}

impl RiskLimits {
    /// Whether any aggregate cap is configured. Aggregate caps need a
    /// portfolio facts producer, so construction refuses them until one is
    /// attached.
    #[must_use]
    pub fn has_aggregate_caps(&self) -> bool {
        self.max_open_risk.is_some()
            || self.max_abs_net_delta.is_some()
            || self.min_buying_power.is_some()
    }

    /// Open-risk cap as [`Decimal`], or `None` when unconfigured. Same
    /// exact-string round-trip as [`RiskLimits::loss_cap`].
    #[must_use]
    pub fn open_risk_cap(&self) -> Option<Decimal> {
        self.max_open_risk
            .and_then(|cap| cap.to_string().parse().ok())
    }

    /// Buying-power floor as [`Decimal`], or `None` when unconfigured.
    #[must_use]
    pub fn buying_power_floor(&self) -> Option<Decimal> {
        self.min_buying_power
            .and_then(|cap| cap.to_string().parse().ok())
    }

    /// Loss cap as [`Decimal`], or `None` when the configured float has no
    /// decimal reading (the check then fails closed).
    ///
    /// Conversion goes through the decimal string round-trip, so a
    /// configured cap means exactly what it says (`19.99` is `19.99`, not
    /// the binary retain artifact). The `f64` field shapes (serde +
    /// `FieldAccess`) are untouched: only the comparison reads exactly.
    #[must_use]
    pub fn loss_cap(&self) -> Option<Decimal> {
        self.max_loss_per_trade.to_string().parse().ok()
    }

    /// Notional cap as [`Decimal`], or `None` when the configured float has
    /// no decimal reading (the check then fails closed). Same round-trip
    /// rule as [`RiskLimits::loss_cap`].
    #[must_use]
    pub fn notional_cap(&self) -> Option<Decimal> {
        self.max_notional.to_string().parse().ok()
    }
}

impl_component!(RiskLimits);

/// Observed portfolio facts the aggregate caps read. No default reads as
/// healthy: an unobserved bundle carries `observed == false` and refuses
/// every configured aggregate cap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PortfolioFacts {
    /// Whether the numbers below are an observation. `false` is a
    /// producer-health refusal, never a pass.
    pub observed: bool,
    /// Sum of live committed max loss over open positions.
    pub open_risk: Money,
    /// Net portfolio greeks, when a greeks feed exists. The delta cap reads
    /// through [`net_delta`](Self::net_delta), so the call site is unchanged.
    pub net_greeks: Option<Greeks>,
    /// Available buying power, when the venue reports it.
    pub buying_power: Option<Money>,
    /// Net liquidation value, when an equity source reports it. Sizing
    /// policies that need it (fraction-of-net-liq) size zero until one
    /// does — the venue book alone reports no equity line.
    pub net_liq: Option<Money>,
    /// Equity peak the drawdown breaker falls from, when an equity source
    /// reports one. `None` means the calendar of the book's high-water mark
    /// is unknown — a configured drawdown cap then refuses as unobserved,
    /// never passes.
    pub equity_peak: Option<Money>,
    /// Current equity the drawdown breaker judges, when an equity source
    /// reports it. Same absence rule as the peak: no reading, no pass.
    pub equity_now: Option<Money>,
}

impl PortfolioFacts {
    /// An unobserved bundle: no numbers, not healthy.
    #[must_use]
    pub fn unobserved() -> Self {
        Self {
            observed: false,
            open_risk: Money::zero(),
            net_greeks: None,
            buying_power: None,
            net_liq: None,
            equity_peak: None,
            equity_now: None,
        }
    }

    /// An observed bundle. Every observed bundle states its net liquidation
    /// value, even when no equity source reported one (`None`). The equity
    /// pair stays `None` until a source attaches it with [`with_equity`](Self::with_equity).
    #[must_use]
    pub fn observed(
        open_risk: Money,
        net_greeks: Option<Greeks>,
        buying_power: Option<Money>,
        net_liq: Option<Money>,
    ) -> Self {
        Self {
            observed: true,
            open_risk,
            net_greeks,
            buying_power,
            net_liq,
            equity_peak: None,
            equity_now: None,
        }
    }

    /// Net portfolio delta for the delta cap: the carried greeks' delta, or
    /// `None` when no greeks feed supplied one. Preserves the call site the
    /// rename would otherwise move.
    #[must_use]
    pub fn net_delta(&self) -> Option<f64> {
        self.net_greeks.map(|greeks| greeks.delta)
    }

    /// Attach the equity pair a drawdown cap is judged against. `None` on
    /// either side means the source has no reading — the breaker refuses as
    /// unobserved rather than inventing one.
    #[must_use]
    pub fn with_equity(mut self, peak: Option<Money>, now: Option<Money>) -> Self {
        self.equity_peak = peak;
        self.equity_now = now;
        self
    }
}

/// Source of observed [`PortfolioFacts`]: the venue book, a paper book, or a
/// test stub. A session that configures aggregate caps must have one
/// attached, so an unobserved aggregate cap can never fail open.
pub trait PortfolioFactsSource: Send + Sync {
    /// Observe the portfolio facts at the decision instant.
    fn facts(&self) -> PortfolioFacts;
}

/// Source of per-contract greeks: a live feed, a fitted surface, or a
/// computed wrapper — mirroring [`PortfolioFactsSource`], so which Greeks
/// implementation prices the delta cap is a wiring decision, not an engine
/// edit. A session that configures [`RiskLimits::max_abs_net_delta`] must
/// have one attached, so a missing delta refuses as unobserved rather than
/// passing.
pub trait GreeksSource: Send + Sync {
    /// Greeks for `contract`, or `None` when this source has none.
    fn greeks(&self, contract: &OptionContract) -> Option<Greeks>;
}

/// Rehearsal-grade computed greeks: the kernels closed form at one spot,
/// rate, and volatility, dated `as_of` (ACT/365 year fractions). A live feed
/// supplies per-contract surfaces instead; this wrapper exists so the delta
/// cap has a deterministic producer in rehearsal and calibration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComputedGreeks {
    /// Underlying spot the closed form prices against.
    pub spot: f64,
    /// Continuously-compounded risk-free rate.
    pub rate: f64,
    /// Implied volatility every contract is priced at.
    pub volatility: f64,
    /// Date the year fractions measure from.
    pub as_of: NaiveDate,
}

impl ComputedGreeks {
    /// Years from `as_of` to `expiry` (ACT/365), or `None` for a dead or
    /// dateless expiry — an expired contract has no greeks.
    fn years_to(&self, expiry: NaiveDate) -> Option<f64> {
        let days = (expiry - self.as_of).num_days();
        if days <= 0 {
            return None;
        }
        let days = u32::try_from(days).ok()?;
        Some(f64::from(days) / 365.0)
    }
}

impl GreeksSource for ComputedGreeks {
    fn greeks(&self, contract: &OptionContract) -> Option<Greeks> {
        if !(self.spot > 0.0
            && self.spot.is_finite()
            && self.rate.is_finite()
            && self.volatility > 0.0
            && self.volatility.is_finite())
        {
            return None;
        }
        let strike = Self::contract_strike(contract)?;
        let years = self.years_to(contract.expiry)?;
        let priced = protopyne_kernels::bs_greeks(
            contract.kind,
            self.spot,
            strike,
            years,
            self.rate,
            self.volatility,
        );
        if !(priced.delta.is_finite()
            && priced.gamma.is_finite()
            && priced.theta.is_finite()
            && priced.vega.is_finite())
        {
            return None;
        }
        Some(priced.into())
    }
}

impl ComputedGreeks {
    /// Strike per share as a positive finite `f64`, or `None` when the
    /// contract names no priceable strike.
    fn contract_strike(contract: &OptionContract) -> Option<f64> {
        let strike = contract.strike.as_decimal().to_f64()?;
        (strike > 0.0 && strike.is_finite()).then_some(strike)
    }
}

/// The shape of the order that will actually be submitted, derived from the
/// intent rather than asserted by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderShape {
    /// Whether the order is a multi-leg spread.
    pub is_spread: bool,
    /// Whether the order is a limit order.
    pub is_limit: bool,
}

impl OrderShape {
    /// Build a shape explicitly.
    #[must_use]
    pub const fn new(is_spread: bool, is_limit: bool) -> Self {
        Self {
            is_spread,
            is_limit,
        }
    }

    /// Derive a shape from any source convertible into one (an order intent
    /// implements the conversion beside its own vocabulary).
    #[must_use]
    pub fn of<T: Into<Self>>(source: T) -> Self {
        source.into()
    }
}

/// Per-decision venue and market facts the engine checks a proposal against.
/// This is observed state (quote age, reconciliation health, portfolio
/// facts), never strategy opinion: the gate fails closed whenever any fact
/// is unhealthy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RiskState {
    /// Age of the newest quote in seconds.
    pub quote_age_secs: u64,
    /// Observed clock skew in seconds.
    pub clock_skew_secs: u64,
    /// Whether the venue reconciliation loop reports agreement.
    pub reconciliation_healthy: bool,
    /// Observed portfolio facts for the aggregate caps.
    pub facts: PortfolioFacts,
}

/// Why the risk engine refused a proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskReason {
    /// Kill switch is engaged.
    KillSwitch,
    /// Reconciliation disagrees with venue truth.
    ReconciliationHalt,
    /// Quantity exceeds the per-trade contract cap.
    OverContractCap,
    /// Committed loss exceeds the per-trade loss cap.
    OverLossCap,
    /// Committed notional exceeds the per-trade notional cap.
    OverNotionalCap,
    /// Quote is older than the staleness gate.
    StaleQuote,
    /// Clock skew exceeds tolerance.
    ClockSkew,
    /// A spread would trade without a limit price.
    MarketOrderForSpread,
    /// Aggregate open risk exceeds the configured cap.
    OverOpenRisk,
    /// Absolute net delta exceeds the configured cap.
    OverNetDelta,
    /// Buying power sits below the configured floor.
    InsufficientBuyingPower,
    /// An aggregate cap is configured but the facts bundle is not observed
    /// (or the component the cap needs is missing).
    PortfolioFactsUnavailable,
    /// Quantity or configured cap is unrepresentable; fail closed.
    Unrepresentable,
}

/// A refused proposal with every failed gate collected for audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskRefused {
    /// Every failed gate, in check order.
    pub reasons: Vec<RiskReason>,
    /// Human-readable summary of the failures.
    pub detail: String,
}

/// Proof that [`RiskEngine::check`] approved a proposal. The only
/// constructor is the engine's own check, so submission code that demands
/// this token cannot bypass the gate. Carries the approved proposal id so a
/// token cannot be reused for a different proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskApproved {
    proposal_id: String,
}

impl RiskApproved {
    /// The id of the proposal this approval was minted for.
    #[must_use]
    pub fn proposal_id(&self) -> &str {
        &self.proposal_id
    }
}

/// The mandatory, non-bypassable risk gate. Holds its [`RiskLimits`]; the
/// decision itself ([`check`](Self::check)) is a synchronous, non-blocking,
/// side-effect-free function of proposal, limits, and observed state.
pub struct RiskEngine {
    limits: RiskLimits,
    loss_cap: Option<Decimal>,
    notional_cap: Option<Decimal>,
    open_risk_cap: Option<Decimal>,
    buying_power_floor: Option<Decimal>,
}

impl RiskEngine {
    /// Bind an engine to its limits, parsing both caps once. A cap with no
    /// decimal reading stores as `None`, and every later check fails closed.
    ///
    /// Construction itself is unchecked: a [`RiskLimits`] built through the
    /// builder or serde may violate its declared bounds until validated.
    /// Every owner must either call [`try_new`](Self::try_new) or run
    /// [`check`](Self::check) (which validates on every proposal) before
    /// trusting the engine — an unchecked value can exist but can never
    /// approve.
    #[must_use]
    pub fn new(limits: RiskLimits) -> Self {
        let loss_cap = limits.loss_cap();
        let notional_cap = limits.notional_cap();
        let open_risk_cap = limits.open_risk_cap();
        let buying_power_floor = limits.buying_power_floor();
        Self {
            limits,
            loss_cap,
            notional_cap,
            open_risk_cap,
            buying_power_floor,
        }
    }

    /// Bind an engine to pre-validated limits, refusing out-of-bound
    /// configuration at construction instead of at the first check. A limit
    /// set that configures aggregate caps also refuses here: those caps need
    /// a portfolio facts producer, which is attached at session
    /// construction, not at engine construction.
    pub fn try_new(limits: RiskLimits) -> Result<Self, String> {
        limits.validate()?;
        if limits.has_aggregate_caps() {
            return Err(
                "aggregate caps need a portfolio facts producer; attach one at session \
                 construction"
                    .to_owned(),
            );
        }
        Ok(Self::new(limits))
    }

    /// Borrow the configured limits. Hot-path reads go through here.
    #[must_use]
    pub fn limits(&self) -> &RiskLimits {
        &self.limits
    }

    /// The aggregate block, ordered after the per-trade caps. Every arm is
    /// inert unless its cap is configured; a configured cap whose fact is
    /// unobserved (or missing, or non-finite) refuses as a producer-health
    /// problem rather than passing.
    fn check_aggregate_caps(&self, state: &RiskState, reasons: &mut Vec<RiskReason>) {
        let facts = &state.facts;
        if let Some(cap) = self.open_risk_cap {
            if facts.observed {
                if facts.open_risk.as_decimal() > cap {
                    reasons.push(RiskReason::OverOpenRisk);
                }
            } else {
                reasons.push(RiskReason::PortfolioFactsUnavailable);
            }
        }
        if let Some(cap) = self.limits.max_abs_net_delta {
            if facts.observed {
                match facts.net_delta() {
                    Some(delta) if delta.is_finite() => {
                        if delta.abs() > cap {
                            reasons.push(RiskReason::OverNetDelta);
                        }
                    }
                    _ => reasons.push(RiskReason::PortfolioFactsUnavailable),
                }
            } else {
                reasons.push(RiskReason::PortfolioFactsUnavailable);
            }
        }
        if let Some(floor) = self.buying_power_floor {
            if facts.observed {
                match facts.buying_power {
                    Some(power) if power.as_decimal() >= floor => {}
                    Some(_) => reasons.push(RiskReason::InsufficientBuyingPower),
                    None => reasons.push(RiskReason::PortfolioFactsUnavailable),
                }
            } else {
                reasons.push(RiskReason::PortfolioFactsUnavailable);
            }
        }
    }

    /// Check one proposal against limits, the derived order shape, and
    /// observed state. Approvals mint a [`RiskApproved`] token; refusals
    /// collect every failed gate.
    pub fn check(
        &self,
        proposal: &Proposal,
        shape: OrderShape,
        state: &RiskState,
    ) -> Result<RiskApproved, RiskRefused> {
        let mut reasons: Vec<RiskReason> = Vec::new();

        if self.limits.kill_switch {
            reasons.push(RiskReason::KillSwitch);
        }
        if !state.reconciliation_healthy {
            reasons.push(RiskReason::ReconciliationHalt);
        }
        match proposal.contracts() {
            Some(contracts) => {
                if u64::from(contracts) > self.limits.max_contracts_per_trade {
                    reasons.push(RiskReason::OverContractCap);
                }
            }
            None => reasons.push(RiskReason::Unrepresentable),
        }
        match self.loss_cap {
            Some(cap) => {
                if proposal.max_loss.as_decimal() > cap {
                    reasons.push(RiskReason::OverLossCap);
                }
            }
            None => reasons.push(RiskReason::Unrepresentable),
        }
        match self.notional_cap {
            Some(cap) => {
                if proposal.notional.as_decimal() > cap {
                    reasons.push(RiskReason::OverNotionalCap);
                }
            }
            None => reasons.push(RiskReason::Unrepresentable),
        }
        if state.quote_age_secs > self.limits.quote_max_age_secs {
            reasons.push(RiskReason::StaleQuote);
        }
        if state.clock_skew_secs > self.limits.clock_skew_secs {
            reasons.push(RiskReason::ClockSkew);
        }
        if shape.is_spread && self.limits.limit_only_spreads && !shape.is_limit {
            reasons.push(RiskReason::MarketOrderForSpread);
        }
        self.check_aggregate_caps(state, &mut reasons);
        if self.limits.validate().is_err() {
            reasons.push(RiskReason::Unrepresentable);
        }

        if reasons.is_empty() {
            Ok(RiskApproved {
                proposal_id: proposal.id.clone(),
            })
        } else {
            let detail = format!("risk refused {} gate(s): {reasons:?}", reasons.len());
            Err(RiskRefused { reasons, detail })
        }
    }
}

impl WorkUnit for RiskEngine {
    fn name(&self) -> &str {
        "risk_engine"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        fluent_wvr::validate::check_for_execute(&self.limits, "risk limits invalid")?;
        Ok(WorkOutput::ok(format!(
            "risk engine ready (max_contracts_per_trade={})",
            self.limits.max_contracts_per_trade
        )))
    }
}

impl FieldAccess for RiskEngine {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        self.limits.set_field(name, value)?;
        self.loss_cap = self.limits.loss_cap();
        self.notional_cap = self.limits.notional_cap();
        Ok(())
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        self.limits.get_field(name)
    }

    fn field_names(&self) -> &'static [&'static str] {
        self.limits.field_names()
    }
}

impl Describable for RiskEngine {
    fn describe(&self) -> serde_json::Value {
        self.limits.describe()
    }
}

impl_component!(RiskEngine);

#[cfg(test)]
#[path = "../tests/risk.rs"]
mod tests;
