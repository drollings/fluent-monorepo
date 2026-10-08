#![forbid(unsafe_code)]

//! Backtest runner and HTTP surface over the strategy/venue stack.
//!
//! This milestone owns CSV ingest for versioned bar history; the backtest
//! venue, reports, and the `axum` surface arrive in later milestones.

pub mod backtest;
pub mod csv;
pub mod error;
pub mod report;
pub mod runner;
pub mod service;
pub mod transpile;

pub use backtest::{BacktestConfig, BacktestVenue, load_bars_single, load_bars_with_fallback, quote_for_bar};
pub use csv::{load_bars_csv, parse_bars_csv};
pub use error::ServerError;
pub use report::{BacktestReport, RunManifest, build_report, capital_efficiency, count_unquotable_bars, max_drawdown, worst_step_loss};
pub use runner::{BacktestRun, BarDecision, RunnerConfig, SweepOutcome, ReplayStep, build_replay, complete_replay, decide_and_submit, equity_curve_for, expire_position, newest_quote, run_backtest, run_paper_decisions, run_sweep, snapshot_positions, walk_forward_split};
pub use service::{AppState, ArmResponse, CandleRow, ErrorBody, OverlaySeries, PortfolioView, PreviewParams, ProposalView, ServerConfig, ServerHandle, WorstCase, serve};
pub use transpile::{Binding, CalibrationTally, Hair, Length, Lowered, Refusal, SpikeOutcome, calibrate, compile_spike, evaluate_lowered};
