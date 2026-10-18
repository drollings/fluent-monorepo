#![forbid(unsafe_code)]

//! Strategy elements, mandatory risk checks, and the grant evaluator.
//!
//! A [`Strategy`](elements::Strategy) composes one required [`Signal`](elements::Signal)
//! with swappable optional elements into a [`Proposal`](proposal::Proposal).
//! The [`RiskEngine`](risk::RiskEngine) gates every proposal and mints a
//! [`RiskApproved`](risk::RiskApproved) token only on approval, and the pure
//! [`evaluate_grant`](grant::evaluate_grant) function decides whether a live
//! [`Grant`](grant::Grant) covers the proposal. Backtest, paper, and live
//! share this code and differ only at the venue boundary.

pub mod elements;
pub mod error;
pub mod generator;
pub mod grant;
pub mod legs;
pub mod proposal;
pub mod risk;

pub use legs::{LegSelectRefusal, LegSelector};

pub use elements::{
    ExplicitStructure, FixedContracts, FixedSignal, HoldLoss, LimitMonitor, LossAction, LossMgmt,
    Monitor, MonitorAlert, NullMonitor, ProposeInput, Signal, SignalOutput, Sizing, Strategy,
    StructureSelector,
};
pub use error::StrategyError;
pub use generator::{
    ladder_rung, portfolio_for, rebalance, rsrs_score, utility, Exposure, GenerateInput, Generator,
    GeneratorBuilder, RebalanceMode, RebalanceOutcome, Rung, REPORT_SCORE_WINDOW,
    REPORT_SLOPE_LOOKBACK,
};
pub use grant::{
    evaluate_grant, BreakerLimits, BreakerState, Grant, GrantBudgets, GrantDecision, GrantMarket,
    GrantPatch, GrantPermission, GrantScope, GrantUsage, PortfolioThresholds, SignalThresholds,
    TradeThresholds,
};
pub use proposal::{
    transition, Direction, JournaledTransition, Proposal, ProposalEvent, ProposalSide,
    ProposalState, StructureKind,
};
pub use risk::{RiskApproved, RiskEngine, RiskLimits, RiskReason, RiskRefused, RiskState};

#[cfg(test)]
#[path = "../tests/calibration.rs"]
mod calibration_tests;

#[cfg(test)]
#[path = "../tests/validate.rs"]
mod validate_tests;
