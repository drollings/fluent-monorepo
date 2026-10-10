#![forbid(unsafe_code)]

//! Journaled paper-trading venue: one `Venue` trait for paper now, sandbox
//! later, and backtest over the same fill code.
//!
//! The safety case is ordering, not cleverness: journal the intent before
//! any venue call, gate risk before journaling, retry through query-by-ID
//! (never blind resubmit), reconcile against venue truth, and halt entries
//! on any disagreement. Supervision composes [`Scope`](fluent_concurrency::scope::Scope),
//! [`SupervisedBatch`](fluent_concurrency::batch::SupervisedBatch),
//! [`ResultPool`](fluent_concurrency::pool::ResultPool), credit flow, and
//! [`first_accept_in_order`](fluent_concurrency::ladder::first_accept_in_order).

pub mod command;
pub mod error;pub mod health;
pub mod journal;
pub mod order;
pub mod paper;
pub mod position;
pub mod quote;
pub mod reconcile;
pub mod session;
pub mod submit;
pub mod tastytrade;
pub mod venue;

pub use command::{
    cancel_all_ids, close_all_intents, CommandKind, CommandUnit, LegGroup, PortfolioRegistry,
    PortfolioView, ReplicaOrder, StateHandle, StateReplica, TradingState,
};
pub use error::VenueError;
pub use health::{check_trade, may_trade, HealthLimits, HealthState, HealthVerdict};
pub use journal::{Journal, JournalEvent, ReplayedState, VenueStorage, LEG_FILL_KIND};
pub use order::{
    client_id_for, qty_of, ActiveOrderState, CorporateEvent, Fill, OrderIntent, OrderRecord,
    OrderSide, OrderStatus, Position, TimeInForce,
};
pub use paper::{Fault, PaperConfig, PaperVenue};
pub use position::{
    calculate_pnl_realised, calculate_pnl_unrealised, calculate_price_entry_average,
    update_from_fill, PositionExited, PositionFill, PositionSide, SpreadPosition,
};
pub use quote::{FeeSchedule, LiquidityFilter, Quote, Slippage, DEFAULT_SLIPPAGE_BPS};
pub use reconcile::{journal_leg_net, journal_net, reconcile, reconcile_legs, ReconcileGate, ReconcileMismatch, ReconcileReport};
pub use session::{check_unit, SessionConfig, SubmitJob, TradingSession};
pub use submit::{safe_submit, SUBMIT_RETRIES, SUBMIT_TIMEOUT};
pub use tastytrade::{
    RETRY_BACKOFF_BASE_MS, RETRY_JITTER_PCT, RETRY_MAX_ATTEMPTS, TastytradeConfig,
    TastytradeTransport, TransportError, transform_account_event, transform_chain_quote,
    transform_quote, validate_subscription,
};
pub use venue::{AccountEvent, AccountSnapshot, BalanceEntry, DryRunOutcome, LegExecution, LegQuoteSource, QuoteSource, Venue};

#[cfg(test)]
#[path = "../tests/calibration.rs"]
mod calibration_tests;

#[cfg(test)]
#[path = "../tests/spread_calibration.rs"]
mod spread_calibration_tests;
