//! Service composition root: the HTTP surface was split out of this module
//! into [`crate::boot`] (config, shared state, serve, durable tape),
//! [`crate::routes`] (the axum handlers), [`crate::views`] (every shared view
//! builder the routes and the gateway read), and [`crate::rehearsal`] (the one
//! fixed signal/grant composition).
//!
//! This module is the compatibility index that re-exports them, so existing
//! `crate::service::…` callers — the gateway and the crate's tests — compile
//! unchanged. New code should reach for the owning module directly.

pub use crate::boot::{
    open_tape_reader, serve, serve_with_tape, tape_serve, timeframe_of_label, AppState,
    ServerConfig, ServerHandle, TapeMiss, TapeMissKind, TapeReader, TapeServe, TapeServeSnapshot,
    DEFAULT_TAPE_LIMIT,
};
pub use crate::routes::{ErrorBody, OperatorParams, PreviewParams, PreviewResponse, RevokeParams};
pub use crate::views::DEFAULT_REPLAY_CHECKPOINT_EVERY;

// The gateway and the crate's service tests reach these through
// `crate::service::…` / `use super::*`; the split moved them to their owning
// modules, so re-export them here rather than rewriting every caller.
#[allow(unused_imports)]
pub(crate) use {
    crate::backtest::BacktestConfig,
    crate::boot::{Shared, TapeServeStats},
    crate::chain_history::{ChainFeedConfig, HistoricalChainSource},
    crate::error::ServerError,
    crate::gateway::FallbackLatch,
    crate::history_root::HistoryRoot,
    crate::rehearsal::replay_grant_for,
    crate::replay_session::ReplayRegistry,
    crate::runner::{
        build_replay, complete_replay, run_backtest, side_for_direction, BacktestRun, BarDecision,
        RunnerConfig,
    },
    crate::views::{
        align_overlay, apply_arm, bar_frame, booked_portfolio_of, chain_units, chart_bars_of,
        decide_journaled, deny_histogram_of, direction_tag, grant_tiers_of, health_view,
        journal_rows_of, journaled_proposals_of, live_grant, overlay_series_of, parse_patch,
        pending_proposals_of, portfolio_of_booked, portfolio_of_decisions, preview_arm_of,
        proposal_state_tag, publish_run, replay_view_of, revoke_grant, structure_legs,
        structure_tag, trading_state_tag, worst_case_of,
    },
    fluent_concurrency::pool::Limiter,
    protopyne_chart_model::rows_of,
    protopyne_kernels::{
        BbIndicator, EmaIndicator, Indicator, Ohlcv, ParamSet, SmaIndicator, TrimaIndicator,
    },
    protopyne_store::{
        FoldPolicy, MarketTape, PlanError, ReadRequest as TapeReadRequest, StoreError, StoreHealth,
        StoreStorage, TimeRange,
    },
    protopyne_strategy::{
        BreakerLimits, Direction, FixedSignal, Grant, GrantBudgets, GrantPermission, GrantScope,
        PortfolioThresholds, ProposalEvent, ProposalState, RiskEngine, RiskLimits,
        SignalThresholds, Strategy, StructureKind, TradeThresholds,
    },
    protopyne_types::{Bar, ChainQuote, Money, Qty, Timeframe},
    protopyne_venue::{
        replay_grant, serving_allows_entries, serving_verdict, update_from_fill, HealthVerdict,
        Journal, Position, PositionFill, PositionSide, SpreadPosition, StateHandle, TradingState,
        VenueStorage, COMMAND_ARM_GRANT_KIND, COMMAND_REVOKE_GRANT_KIND, OPERATOR_CLIENT_ID,
    },
    protopyne_view::{
        ArmResponse, ChartBars, CommandStatus, DenyCount, HealthView, JournalRowView,
        OverlaySeries, PortfolioView, PositionView, ProposalView, ReplayStepView, ReplayView,
        WorstCase,
    },
    std::sync::{Arc, Mutex},
};

#[cfg(test)]
#[path = "../tests/api.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/views_golden.rs"]
mod views_golden_tests;

#[cfg(test)]
#[path = "../tests/health_view_store.rs"]
mod health_view_store_tests;

#[cfg(test)]
#[path = "../tests/store_halt_end_to_end.rs"]
mod store_halt_end_to_end_tests;

#[cfg(test)]
#[path = "../tests/trading_status.rs"]
mod trading_status_tests;

#[cfg(test)]
#[path = "../tests/labels.rs"]
mod labels_tests;

#[cfg(test)]
#[path = "../tests/tape_boot.rs"]
mod tape_boot_tests;

#[cfg(test)]
#[path = "../tests/chart_contract.rs"]
mod chart_contract_tests;

#[cfg(test)]
#[path = "../tests/arm_worst_case.rs"]
mod arm_worst_case_tests;

#[cfg(test)]
#[path = "../tests/arm_journal.rs"]
mod arm_journal_tests;
