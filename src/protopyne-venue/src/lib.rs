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

pub mod error;
pub mod health;
pub mod journal;
pub mod order;
pub mod paper;
pub mod quote;
pub mod reconcile;
pub mod session;
pub mod submit;
pub mod venue;

pub use error::VenueError;
pub use health::{check_trade, may_trade, HealthLimits, HealthState, HealthVerdict};
pub use journal::{Journal, JournalEvent, ReplayedState, VenueStorage};
pub use order::{
    client_id_for, qty_of, CorporateEvent, Fill, OrderIntent, OrderRecord, OrderSide, OrderStatus,
    Position,
};
pub use paper::{Fault, PaperConfig, PaperVenue};
pub use quote::{FeeSchedule, LiquidityFilter, Quote, Slippage, DEFAULT_SLIPPAGE_BPS};
pub use reconcile::{journal_net, reconcile, ReconcileGate, ReconcileMismatch, ReconcileReport};
pub use session::{check_unit, SessionConfig, SubmitJob, TradingSession};
pub use submit::{safe_submit, SUBMIT_RETRIES, SUBMIT_TIMEOUT};
pub use venue::{DryRunOutcome, QuoteSource, Venue};

#[cfg(test)]
#[path = "../tests/calibration.rs"]
mod calibration_tests;
