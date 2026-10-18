#![forbid(unsafe_code)]

//! Backtest runner and HTTP surface over the strategy/venue stack.
//!
//! File ingest for versioned bar history, the backtest venue, the report
//! suite, the `axum` surface, and a store-backed history arm.

pub mod backtest;
pub mod chain_history;
pub mod csv;
pub mod error;
pub mod gateway;
pub mod history;
pub mod history_root;
pub mod metrics;
pub mod parquet;
pub mod portfolio;
pub mod replay_session;
pub mod report;
pub mod runner;
pub mod service;
pub mod transpile;

pub use backtest::{
    load_history, quote_for_bar, BacktestConfig, BacktestVenue, HistorySource, LoadedHistory,
};
pub use csv::parse_bars_csv;
pub use error::ServerError;
pub use gateway::ServerGateway;
pub use history::{format_of, load_bars_candidates, load_bars_file, HistoryFormat};
pub use history_root::{
    load_history_path, HistoryCache, HistoryRoot, SkippedFile, TickerLoad, DEFAULT_TICKER,
    HISTORY_ROOT_BASE,
};
pub use metrics::{
    calmar, drawdown_fraction, mean, population_std, profit_factor, sharpe, sortino, step_returns,
    win_rate, TRADING_DAYS_PER_YEAR,
};
pub use parquet::fold_frames;
pub use portfolio::{
    aggregate, Aggregation, BookedFill, BucketKey, BucketSummary, PortfolioReport, PortfolioTotals,
    PAPER_PORTFOLIO,
};
pub use replay_session::{ChainPolicy, ReplayDriver, ReplayRegistry, ReplaySession};
pub use report::{
    build_report, capital_efficiency, count_unquotable_bars, cum_return, history_source_limit,
    log_to_simple_return, max_drawdown, render_tearsheet, simple_to_log_return,
    uncertified_coverage_limit, worst_step_loss, BacktestReport, HistoryKind, RunManifest,
    ABSENT_CAPABILITIES,
};
pub use runner::{
    build_replay, checked_order_for, complete_replay, decide_and_submit, equity_curve_for,
    expire_position, report_rsrs, run_backtest, run_backtest_from_source, run_multi_backtest,
    run_paper_decisions, run_sweep, side_for_direction, snapshot_positions, triple_barrier,
    walk_forward_split, BacktestRun, BarDecision, BarHealth, CheckedOrder, MultiBacktestRun,
    MultiRunManifest, MultiSymbolInput, ReplayStep, RunnerConfig, SweepOutcome,
};
pub use service::{
    open_tape_reader, serve, serve_with_tape, tape_serve, timeframe_of_label, AppState, ErrorBody,
    PreviewParams, PreviewResponse, ServerConfig, ServerHandle, TapeMiss, TapeMissKind, TapeReader,
    TapeServe, TapeServeSnapshot, DEFAULT_REPLAY_CHECKPOINT_EVERY, DEFAULT_TAPE_LIMIT,
};
pub use transpile::{
    calibrate, compile_spike, evaluate_lowered, Binding, CalibrationTally, Hair, Length, Lowered,
    Refusal, SpikeOutcome,
};

#[cfg(test)]
#[path = "../tests/validate.rs"]
mod validate_tests;

#[cfg(test)]
#[path = "../tests/common/tape.rs"]
pub(crate) mod common_tape;
