//! Strategy elements: one required signal plus swappable optional pieces.
//!
//! A strategy is a small declarative composition. The [`Signal`] is the only
//! required element; [`StructureSelector`], [`Sizing`], [`LossMgmt`], and
//! [`Monitor`] each ship a default so a strategy author overrides only what
//! the idea needs. [`Strategy::propose`] runs signal, structure, and sizing
//! in that order and emits a [`Proposal`](crate::proposal::Proposal) the
//! risk engine and grant evaluator then gate.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use protopyne_types::{Money, Qty};

use crate::error::StrategyError;
use crate::proposal::{Direction, Proposal, ProposalSide, ProposalState, StructureKind};

/// What a signal concluded on closed bars: a direction and two scores in
/// `[0, 1]`. Scores feed the grant's signal tier **(task-value)**.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalOutput {
    /// Direction of exposure.
    pub direction: Direction,
    /// Indicator conviction in `[0, 1]`.
    pub conviction: f64,
    /// Options-activity score in `[0, 1]`.
    pub activity_score: f64,
}

/// The only required strategy element: indicator and options-activity logic
/// evaluated on closed bars. Implementations are pure over their input
/// window: no I/O, no clock reads.
pub trait Signal: Send + Sync {
    /// Element name for attribution (which strategy produced a position).
    fn name(&self) -> &str;
    /// Evaluate closed-bar closes, oldest first, into a direction and scores.
    fn evaluate(&self, closes: &[f64]) -> Result<SignalOutput, StrategyError>;
}

/// Placeholder signal returning fixed scores. Useful for rehearsal and for
/// tests that target risk/grant behavior rather than indicator logic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FixedSignal {
    direction: Direction,
    conviction: f64,
    activity_score: f64,
}

impl FixedSignal {
    /// Build a fixed signal. Scores must sit in `[0, 1]`; anything else is
    /// refused rather than clamped, so misconfiguration stays loud.
    pub fn new(
        direction: Direction,
        conviction: f64,
        activity_score: f64,
    ) -> Result<Self, StrategyError> {
        if !(0.0..=1.0).contains(&conviction) {
            return Err(StrategyError::InvalidElement(format!(
                "conviction {conviction} outside [0, 1]"
            )));
        }
        if !(0.0..=1.0).contains(&activity_score) {
            return Err(StrategyError::InvalidElement(format!(
                "activity score {activity_score} outside [0, 1]"
            )));
        }
        Ok(Self {
            direction,
            conviction,
            activity_score,
        })
    }
}

impl Signal for FixedSignal {
    fn name(&self) -> &str {
        "fixed"
    }

    fn evaluate(&self, closes: &[f64]) -> Result<SignalOutput, StrategyError> {
        if closes.is_empty() {
            return Err(StrategyError::InvalidElement(
                "signal needs at least one closed bar".to_owned(),
            ));
        }
        Ok(SignalOutput {
            direction: self.direction,
            conviction: self.conviction,
            activity_score: self.activity_score,
        })
    }
}

/// Chooses instrument structure, strikes, and DTE policy. Optional: defaults
/// to an explicit, caller-supplied structure.
pub trait StructureSelector: Send + Sync {
    /// Pick the structure for this signal.
    fn select(&self, signal: &SignalOutput) -> StructureKind;
}

/// Structure selector that always returns one explicit structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExplicitStructure {
    /// The structure every proposal uses.
    pub structure: StructureKind,
}

impl StructureSelector for ExplicitStructure {
    fn select(&self, _signal: &SignalOutput) -> StructureKind {
        self.structure
    }
}

/// Determines quantity. Optional: defaults to a fixed contract count.
/// Sizing can only reduce what risk permits, never exceed it — the engine
/// re-checks every quantity, so an aggressive sizer is contained.
pub trait Sizing: Send + Sync {
    /// Contracts (or shares) for this signal.
    fn size(&self, signal: &SignalOutput) -> Qty;
}

/// Fixed-count sizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedContracts {
    /// Contracts on every proposal.
    pub contracts: u32,
}

impl Sizing for FixedContracts {
    fn size(&self, _signal: &SignalOutput) -> Qty {
        Qty::new(rust_decimal::Decimal::from(self.contracts))
    }
}

/// What loss management proposes for a live position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LossAction {
    /// Machine-readable kind (e.g. `"stop"`, `"profit-target"`, `"roll"`).
    pub kind: String,
    /// Why the action is proposed.
    pub reason: String,
}

/// Defines how a position is protected and unwound. Optional: defaults to
/// holding (no action). Protective exits rest at the broker wherever it
/// supports them, so they survive an outage of this platform.
pub trait LossMgmt: Send + Sync {
    /// Propose an action given unrealized P&L and the configured stop level.
    /// Returns `None` to hold.
    fn review(&self, unrealized: Money, stop_level: Money) -> Option<LossAction>;
}

/// Loss management that never acts (hold every position).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HoldLoss;

impl LossMgmt for HoldLoss {
    fn review(&self, _unrealized: Money, _stop_level: Money) -> Option<LossAction> {
        None
    }
}

/// A read-only monitoring observation. Monitors may propose actions, but only
/// an approval or an armed grant can turn one into an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorAlert {
    /// Portfolio the observation belongs to.
    pub portfolio: String,
    /// What was observed.
    pub message: String,
}

/// Observes positions, portfolios, and health. Optional, read-only, and
/// independent of strategy logic: defaults to silence.
pub trait Monitor: Send + Sync {
    /// Observe aggregate open risk against its cap; emit alerts, if any.
    fn observe(&self, open_risk: Money, cap: Money) -> Vec<MonitorAlert>;
}

/// Monitor that never alerts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NullMonitor;

impl Monitor for NullMonitor {
    fn observe(&self, _open_risk: Money, _cap: Money) -> Vec<MonitorAlert> {
        Vec::new()
    }
}

/// Threshold breach monitor: alerts when open risk crosses its cap.
/// The alert is a proposal input, never an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitMonitor {
    /// Portfolio under watch.
    pub portfolio: String,
}

impl Monitor for LimitMonitor {
    fn observe(&self, open_risk: Money, cap: Money) -> Vec<MonitorAlert> {
        if open_risk.as_decimal() > cap.as_decimal() {
            vec![MonitorAlert {
                portfolio: self.portfolio.clone(),
                message: format!("open risk {open_risk} above cap {cap}"),
            }]
        } else {
            Vec::new()
        }
    }
}

/// Economics the caller supplies for a proposal. Signal, structure, and
/// sizing fill in direction, structure, and quantity; price, loss, notional,
/// and expiry come from the caller's quote and structure math.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProposeInput<'a> {
    /// Strategy name for attribution.
    pub strategy: &'a str,
    /// Underlying symbol.
    pub underlying: &'a str,
    /// Entry, exit, or roll.
    pub side: ProposalSide,
    /// Closed-bar closes, oldest first, for the signal.
    pub closes: &'a [f64],
    /// Limit price per contract/share.
    pub limit_price: Money,
    /// Worst-case loss if the position goes to zero value.
    pub max_loss: Money,
    /// Committed notional.
    pub notional: Money,
    /// Days to expiry of the front leg.
    pub dte_days: u32,
    /// Proposal creation time.
    pub created_at: DateTime<Utc>,
}

/// A strategy: one required signal plus independently swappable optional
/// elements. Backtest, paper, and (later) live share this composition and
/// differ only at the venue boundary.
pub struct Strategy {
    signal: Arc<dyn Signal>,
    structure: Arc<dyn StructureSelector>,
    sizing: Arc<dyn Sizing>,
    loss: Arc<dyn LossMgmt>,
    monitor: Arc<dyn Monitor>,
}

impl Strategy {
    /// Compose a strategy around a signal; optionals take their defaults
    /// (explicit vertical, one fixed contract, hold, silence).
    #[must_use]
    pub fn with_signal(signal: Arc<dyn Signal>) -> Self {
        Self {
            signal,
            structure: Arc::new(ExplicitStructure {
                structure: StructureKind::Vertical,
            }),
            sizing: Arc::new(FixedContracts { contracts: 1 }),
            loss: Arc::new(HoldLoss),
            monitor: Arc::new(NullMonitor),
        }
    }

    /// Override the structure selector.
    #[must_use]
    pub fn with_structure(mut self, structure: Arc<dyn StructureSelector>) -> Self {
        self.structure = structure;
        self
    }

    /// Override sizing.
    #[must_use]
    pub fn with_sizing(mut self, sizing: Arc<dyn Sizing>) -> Self {
        self.sizing = sizing;
        self
    }

    /// Override loss management.
    #[must_use]
    pub fn with_loss(mut self, loss: Arc<dyn LossMgmt>) -> Self {
        self.loss = loss;
        self
    }

    /// Override monitoring.
    #[must_use]
    pub fn with_monitor(mut self, monitor: Arc<dyn Monitor>) -> Self {
        self.monitor = monitor;
        self
    }

    /// Signal name for attribution.
    #[must_use]
    pub fn signal_name(&self) -> &str {
        self.signal.name()
    }

    /// Run signal, structure, and sizing over closed bars and emit a
    /// [`Proposal`] in state [`Proposed`](ProposalState::Proposed).
    pub fn propose(&self, id: &str, input: ProposeInput<'_>) -> Result<Proposal, StrategyError> {
        let output = self.signal.evaluate(input.closes)?;
        let structure = self.structure.select(&output);
        let quantity = self.sizing.size(&output);
        Ok(Proposal::new()
            .id(id.to_owned())
            .strategy(input.strategy.to_owned())
            .underlying(input.underlying.to_owned())
            .direction(output.direction)
            .side(input.side)
            .structure(structure)
            .quantity(quantity)
            .limit_price(input.limit_price)
            .max_loss(input.max_loss)
            .notional(input.notional)
            .dte_days(input.dte_days)
            .conviction(output.conviction)
            .activity_score(output.activity_score)
            .state(ProposalState::Proposed)
            .created_at(input.created_at)
            .build())
    }

    /// Ask loss management for a protective action.
    #[must_use]
    pub fn review_position(&self, unrealized: Money, stop_level: Money) -> Option<LossAction> {
        self.loss.review(unrealized, stop_level)
    }

    /// Ask the monitor for read-only observations.
    #[must_use]
    pub fn observe(&self, open_risk: Money, cap: Money) -> Vec<MonitorAlert> {
        self.monitor.observe(open_risk, cap)
    }
}

#[cfg(test)]
#[path = "../tests/elements.rs"]
mod tests;
