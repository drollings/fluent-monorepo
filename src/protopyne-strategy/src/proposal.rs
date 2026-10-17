//! Proposals and their journaled lifecycle.
//!
//! A [`Proposal`] is the single unit of tradable intent: one strategy asks
//! to trade one structure on one underlying at one limit. Its [`ProposalState`]
//! moves only through [`transition`], and [`Proposal::apply`] records every
//! move as a [`JournaledTransition`] the caller persists before acting on it.

use bon::Builder;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

use protopyne_types::{Money, Qty};

use crate::error::StrategyError;

/// Direction of the proposed trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    /// Buy exposure / bullish position.
    Long,
    /// Sell exposure / bearish position.
    Short,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Long => f.write_str("long"),
            Self::Short => f.write_str("short"),
        }
    }
}

/// Which side of the order lifecycle a proposal belongs to. Entry, exit, and
/// roll are separate permissions on a grant: exit-only operation is far lower
/// risk than entry, so the side is explicit on every proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProposalSide {
    /// Open new exposure.
    Entry,
    /// Close or reduce existing exposure.
    Exit,
    /// Close one position and open another as a single multi-leg order.
    Roll,
}

impl fmt::Display for ProposalSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Entry => f.write_str("entry"),
            Self::Exit => f.write_str("exit"),
            Self::Roll => f.write_str("roll"),
        }
    }
}

/// Option structure of a proposal. Every variant except [`Self::NakedShort`]
/// carries bounded loss by construction; naked short option exposure is
/// excluded from the initial grant envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StructureKind {
    /// Single stock or single option leg.
    Single,
    /// Vertical spread (two legs, same expiry).
    Vertical,
    /// Calendar spread (same strikes, different expiries).
    Calendar,
    /// Iron condor (two paired verticals).
    Condor,
    /// Uncovered short option with unbounded loss.
    NakedShort,
}

impl StructureKind {
    /// Whether loss is bounded by construction. Only [`Self::NakedShort`]
    /// answers `false`; grants admit defined-risk structures only.
    #[must_use]
    pub fn is_defined_risk(self) -> bool {
        !matches!(self, Self::NakedShort)
    }
}

/// Lifecycle state of a proposal.
///
/// ```text
/// Proposed -> RiskChecked -> AutoApproved | PendingHuman
///   -> Approved | Rejected | Expired -> Submitted -> Working
///   -> Filled | Partial | Cancelled
/// ```
///
/// An approved-but-unsubmitted proposal may also cancel directly: withdrawing
/// approval before submission (entries disarmed, quantity unrepresentable)
/// terminalizes without journaling a submission that never happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProposalState {
    /// Freshly created, not yet risk-checked.
    Proposed,
    /// Passed the mandatory risk engine, awaiting routing to approval.
    RiskChecked,
    /// Covered by a live grant; no human step required.
    AutoApproved,
    /// Waiting for an explicit human decision.
    PendingHuman,
    /// Human-approved, ready to submit.
    Approved,
    /// Refused by risk, grant, or human. Terminal.
    Rejected,
    /// Approval window lapsed before submission. Terminal.
    Expired,
    /// Handed to a venue, acknowledgement pending.
    Submitted,
    /// Acknowledged by the venue, working.
    Working,
    /// Fully filled. Terminal.
    Filled,
    /// Partially filled, remainder dead. Terminal.
    Partial,
    /// Cancelled before completion. Terminal.
    Cancelled,
}

/// Events that advance the proposal lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProposalEvent {
    /// Risk engine approved the proposal.
    PassRisk,
    /// Risk engine refused the proposal.
    FailRisk,
    /// A live grant covers the proposal.
    AutoApprove,
    /// No grant covers the proposal; a human must decide.
    RequireHuman,
    /// Human approved.
    Approve,
    /// Human, policy, or venue rejected.
    Reject,
    /// Approval window lapsed.
    Expire,
    /// Submit an approved proposal to a venue.
    Submit,
    /// Venue acknowledged the order.
    StartWork,
    /// Venue reported a full fill.
    Fill,
    /// Venue reported a partial fill with nothing workable left.
    PartialFill,
    /// Cancel before completion.
    Cancel,
}

/// Advance `from` by `event`, or explain why the move is illegal.
///
/// This is a pure function over values: no I/O, no clock reads, no hidden
/// state. Illegal moves are errors, never silent no-ops, so a journal can
/// never record a transition that did not happen.
pub fn transition(
    from: ProposalState,
    event: ProposalEvent,
) -> Result<ProposalState, StrategyError> {
    use ProposalEvent as E;
    use ProposalState as S;
    let next = match (from, event) {
        (S::Proposed, E::PassRisk) => S::RiskChecked,
        (S::Proposed | S::RiskChecked, E::FailRisk)
        | (S::PendingHuman | S::Submitted, E::Reject) => S::Rejected,
        (S::RiskChecked, E::AutoApprove) => S::AutoApproved,
        (S::RiskChecked, E::RequireHuman) => S::PendingHuman,
        (S::PendingHuman, E::Approve) => S::Approved,
        (S::PendingHuman | S::AutoApproved | S::Approved, E::Expire) => S::Expired,
        (S::AutoApproved | S::Approved, E::Submit) => S::Submitted,
        (S::Submitted, E::StartWork) => S::Working,
        (S::Submitted | S::Working | S::AutoApproved | S::Approved, E::Cancel) => {
            S::Cancelled
        }
        (S::Working, E::Fill) => S::Filled,
        (S::Working, E::PartialFill) => S::Partial,
        _ => {
            return Err(StrategyError::InvalidTransition(format!(
                "{from:?} does not accept {event:?}"
            )));
        }
    };
    Ok(next)
}

/// One recorded lifecycle move. The caller persists the entry before acting
/// on the new state, so replay rebuilds the exact proposal history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournaledTransition {
    /// Proposal this entry belongs to.
    pub proposal_id: String,
    /// State before the event.
    pub from: ProposalState,
    /// State after the event.
    pub to: ProposalState,
    /// Human- or machine-readable reason for the move.
    pub reason: String,
    /// Time the move was applied.
    pub at: DateTime<Utc>,
}

/// The single unit of tradable intent.
///
/// A proposal carries its own economics (`limit_price`, `max_loss`,
/// `notional`) so risk and grant checks never re-derive numbers from a
/// strategy handle: the checked values are the submitted values.
#[derive(Debug, Clone, Builder, PartialEq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct Proposal {
    /// Stable external identifier, also used as the venue client key.
    pub id: String,
    /// Strategy that produced this proposal.
    pub strategy: String,
    /// Underlying symbol.
    pub underlying: String,
    /// Direction of exposure.
    pub direction: Direction,
    /// Which side of the lifecycle this is (entry, exit, roll).
    pub side: ProposalSide,
    /// Option structure.
    pub structure: StructureKind,
    /// Contracts (or shares) requested.
    pub quantity: Qty,
    /// Limit price per contract/share. Spreads are limit-only.
    pub limit_price: Money,
    /// Worst-case loss if the position goes to zero value.
    pub max_loss: Money,
    /// Committed notional for budget accounting.
    pub notional: Money,
    /// Days to expiry of the structure's front leg.
    pub dte_days: u32,
    /// Signal conviction in `[0, 1]`.
    pub conviction: f64,
    /// Options-activity score in `[0, 1]`.
    pub activity_score: f64,
    /// Current lifecycle state.
    #[builder(default = ProposalState::Proposed)]
    pub state: ProposalState,
    /// Creation time.
    pub created_at: DateTime<Utc>,
}

impl Proposal {
    /// Whole contracts requested, or `None` when the quantity is fractional
    /// or out of range. Budget and risk checks fail closed on `None` rather
    /// than truncating: a fractional quantity is a data error, not one
    /// contract.
    #[must_use]
    pub fn contracts(&self) -> Option<u32> {
        use rust_decimal::prelude::ToPrimitive as _;
        let amount = self.quantity.as_decimal();
        if amount.fract() != Decimal::ZERO {
            return None;
        }
        amount.to_u32()
    }

    /// Apply `event` to this proposal and return the journal entry the
    /// caller must persist before acting on the new state.
    pub fn apply(
        &mut self,
        event: ProposalEvent,
        reason: impl Into<String>,
        at: DateTime<Utc>,
    ) -> Result<JournaledTransition, StrategyError> {
        let from = self.state;
        let to = transition(from, event)?;
        self.state = to;
        Ok(JournaledTransition {
            proposal_id: self.id.clone(),
            from,
            to,
            reason: reason.into(),
            at,
        })
    }
}

#[cfg(test)]
#[path = "../tests/proposal.rs"]
mod tests;
