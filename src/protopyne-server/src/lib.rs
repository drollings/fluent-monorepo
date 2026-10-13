#![forbid(unsafe_code)]

//! Backtest runner and HTTP surface over the strategy/venue stack.
//!
//! This milestone owns CSV ingest for versioned bar history; the backtest
//! venue, reports, and the `axum` surface arrive in later milestones.

pub mod backtest;
pub mod csv;
pub mod error;
pub mod gateway;
pub mod metrics;
pub mod portfolio;
pub mod report;
pub mod runner;
pub mod service;
pub mod transpile;

pub use backtest::{BacktestConfig, BacktestVenue, HistorySource, load_bars_single, load_bars_with_fallback, load_history, quote_for_bar};
pub use csv::{load_bars_csv, parse_bars_csv};
pub use error::ServerError;
pub use metrics::{TRADING_DAYS_PER_YEAR, calmar, drawdown_fraction, mean, population_std, profit_factor, sharpe, sortino, step_returns, win_rate};
pub use portfolio::{Aggregation, BookedFill, BucketKey, BucketSummary, PAPER_PORTFOLIO, PortfolioReport, PortfolioTotals, aggregate};
pub use report::{BacktestReport, RunManifest, build_report, capital_efficiency, count_unquotable_bars, cum_return, log_to_simple_return, max_drawdown, render_tearsheet, simple_to_log_return, worst_step_loss};
pub use runner::{BacktestRun, BarDecision, BarHealth, CheckedOrder, MultiBacktestRun, MultiRunManifest, MultiSymbolInput, RunnerConfig, SweepOutcome, ReplayStep, build_replay, checked_order_for, complete_replay, decide_and_submit, equity_curve_for, expire_position, report_rsrs, run_backtest, run_multi_backtest, run_paper_decisions, run_sweep, side_for_direction, snapshot_positions, triple_barrier, walk_forward_split};
pub use gateway::ServerGateway;
pub use service::{AppState, ErrorBody, PreviewParams, PreviewResponse, ServerConfig, ServerHandle, serve};
pub use transpile::{Binding, CalibrationTally, Hair, Length, Lowered, Refusal, SpikeOutcome, calibrate, compile_spike, evaluate_lowered};

#[cfg(test)]
#[path = "../tests/validate.rs"]
mod validate_tests;
