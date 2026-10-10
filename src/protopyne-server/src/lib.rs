#![forbid(unsafe_code)]

//! Backtest runner and HTTP surface over the strategy/venue stack.
//!
//! This milestone owns CSV ingest for versioned bar history; the backtest
//! venue, reports, and the `axum` surface arrive in later milestones.

pub mod backtest;
pub mod csv;
pub mod error;
pub mod metrics;
pub mod report;
pub mod runner;
pub mod service;
pub mod transpile;

pub use backtest::{BacktestConfig, BacktestVenue, load_bars_single, load_bars_with_fallback, quote_for_bar};
pub use csv::{load_bars_csv, parse_bars_csv};
pub use error::ServerError;
pub use metrics::{TRADING_DAYS_PER_YEAR, calmar, drawdown_fraction, mean, population_std, profit_factor, sharpe, sortino, step_returns, win_rate};
pub use report::{BacktestReport, RunManifest, build_report, capital_efficiency, count_unquotable_bars, cum_return, log_to_simple_return, max_drawdown, simple_to_log_return, worst_step_loss};
pub use runner::{BacktestRun, BarDecision, BarHealth, CheckedOrder, RunnerConfig, SweepOutcome, ReplayStep, build_replay, checked_order_for, complete_replay, decide_and_submit, equity_curve_for, expire_position, report_rsrs, run_backtest, run_paper_decisions, run_sweep, snapshot_positions, triple_barrier, walk_forward_split};
pub use service::{AppState, ArmResponse, CandleRow, ErrorBody, OverlaySeries, PortfolioView, PreviewParams, ProposalView, ServerConfig, ServerHandle, WorstCase, serve};
pub use transpile::{Binding, CalibrationTally, Hair, Length, Lowered, Refusal, SpikeOutcome, calibrate, compile_spike, evaluate_lowered};
