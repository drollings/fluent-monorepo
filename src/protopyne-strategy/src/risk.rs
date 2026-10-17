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
    #[field(desc = "Maximum contracts on one proposal", min = 1, max = 1000000)]
    #[builder(default = 10)]
    pub max_contracts_per_trade: u64,
    /// Maximum committed max loss on one proposal, in account-currency major
    /// units **(correctness)**.
    #[field(desc = "Maximum committed loss on one proposal", min = 1, max = 1000000000)]
    #[builder(default = 500.0)]
    pub max_loss_per_trade: f64,
    /// Maximum committed notional on one proposal, in account-currency major
    /// units **(correctness)**.
    #[field(desc = "Maximum committed notional on one proposal", min = 1, max = 10000000000)]
    #[builder(default = 50_000.0)]
    pub max_notional: f64,
    /// Maximum quote age in seconds before data counts as stale
    /// **(correctness)**.
    #[field(desc = "Maximum quote age in seconds", min = 1, max = 3600)]
    #[builder(default = 60)]
    pub quote_max_age_secs: u64,
    /// Maximum tolerable clock skew in seconds **(correctness)**.
    #[field(desc = "Maximum clock skew in seconds", min = 1, max = 3600)]
    #[builder(default = 5)]
    pub clock_skew_secs: u64,
    /// Whether spreads must be limit orders **(correctness)**.
    #[field(desc = "Require limit orders for spreads")]
    #[builder(default = true)]
    pub limit_only_spreads: bool,
    /// When set, every proposal is refused **(correctness)**.
    #[field(desc = "Kill switch: refuse every proposal when set")]
    #[builder(default = false)]
    pub kill_switch: bool,
}

impl protopyne_types::Validate for RiskLimits {
    type Err = String;
    fn validate(&self) -> Result<(), Self::Err> {
        RiskLimits::validate(self)
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
        protopyne_types::check_for_execute(self, "risk limits invalid")?;
        Ok(protopyne_types::valid_output("risk limits valid"))
    }
}

impl RiskLimits {
    /// Fail-closed self-consistency check shared by supervision
    /// (`execute`) and the decision path (`RiskEngine::check`).
    pub fn validate(&self) -> Result<(), String> {
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
        Ok(())
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

/// Per-decision venue and market facts the engine checks a proposal against.
/// This is observed state (quote age, reconciliation health), never strategy
/// opinion: the gate fails closed whenever any fact is unhealthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiskState {
    /// Age of the newest quote in seconds.
    pub quote_age_secs: u64,
    /// Observed clock skew in seconds.
    pub clock_skew_secs: u64,
    /// Whether the venue reconciliation loop reports agreement.
    pub reconciliation_healthy: bool,
    /// Whether the order would be submitted as a limit order.
    pub is_limit_order: bool,
    /// Whether the order is a multi-leg spread.
    pub is_spread: bool,
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
}

impl RiskEngine {
    /// Bind an engine to its limits, parsing both caps once. A cap with no
    /// decimal reading stores as `None`, and every later check fails closed.
    #[must_use]
    pub fn new(limits: RiskLimits) -> Self {
        let loss_cap = limits.loss_cap();
        let notional_cap = limits.notional_cap();
        Self { limits, loss_cap, notional_cap }
    }

    /// Borrow the configured limits. Hot-path reads go through here.
    #[must_use]
    pub fn limits(&self) -> &RiskLimits {
        &self.limits
    }

    /// Check one proposal against limits and observed state. Approvals mint
    /// a [`RiskApproved`] token; refusals collect every failed gate.
    pub fn check(&self, proposal: &Proposal, state: &RiskState) -> Result<RiskApproved, RiskRefused> {
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
        if state.is_spread && self.limits.limit_only_spreads && !state.is_limit_order {
            reasons.push(RiskReason::MarketOrderForSpread);
        }
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
        protopyne_types::check_for_execute(&self.limits, "risk limits invalid")?;
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
