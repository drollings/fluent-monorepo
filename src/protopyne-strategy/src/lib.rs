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

pub mod activity;
pub mod economics;
pub mod elements;
pub mod error;
pub mod generator;
pub mod grant;
pub mod ledger;
pub mod legs;
pub mod proposal;
pub mod risk;
pub mod signals_kernel;
#[cfg(feature = "pine")]
pub mod signals_pine;
pub mod view;

pub use legs::{LegRequest, LegSelectRefusal, LegSelector, StrikePolicy};

pub use activity::{ChainActivityScore, OptionsActivity, DEFAULT_NEAR_MONEY_BAND};
pub use economics::{spread_economics, EconRefusal, PackageSide, SpreadEconomics};
pub use elements::{
    BuyingPower, EconomicsPolicy, ExplicitStructure, FixedContracts, FixedSignal, HoldLoss,
    LimitMonitor, LossAction, LossMgmt, LossSet, MarkContext, Monitor, MonitorAlert, NullMonitor,
    PercentNetLiq, PositionView, ProfitTarget, ProposeInput, Signal, SignalConfig, SignalInstance,
    SignalOutput, Sizing, SizingCaps, StopLoss, Strategy, StructureRefusal, StructureRequest,
    StructureSelection, StructureSelector, TimeStop,
};
pub use error::StrategyError;
pub use generator::{
    ladder_rung, portfolio_for, rebalance, rsrs_score, utility, Exposure, GenerateInput, Generator,
    GeneratorBuilder, RebalanceMode, RebalanceOutcome, Rung, REPORT_SCORE_WINDOW,
    REPORT_SLOPE_LOOKBACK,
};
pub use grant::{
    evaluate_grant, worst_case_for, BreakerLimits, BreakerState, ComputedWorstCase, Grant,
    GrantBudgets, GrantDecision, GrantMarket, GrantPatch, GrantPermission, GrantScope, GrantUsage,
    PortfolioThresholds, SignalThresholds, TradeThresholds, WorstCaseBasis, WorstCaseUnit,
};
pub use ledger::{
    FillOutcome, FillStatus, GrantLedger, LedgerRefusal, ReleaseReason, Reservation, Settled,
};
pub use proposal::{
    transition, Direction, JournaledTransition, Proposal, ProposalEvent, ProposalSide,
    ProposalState, StructureKind,
};
pub use risk::{
    OrderShape, PortfolioFacts, PortfolioFactsSource, RiskApproved, RiskEngine, RiskLimits,
    RiskReason, RiskRefused, RiskState,
};
#[cfg(feature = "pine")]
pub use signals_pine::{ParityClaim, PineSignal};
pub use view::{ChainSlice, MarketData, MarketView};

#[cfg(test)]
#[path = "../tests/calibration.rs"]
mod calibration_tests;

#[cfg(test)]
#[path = "../tests/validate.rs"]
mod validate_tests;

#[cfg(test)]
#[path = "../tests/layering.rs"]
mod layering_tests;
