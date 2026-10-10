#![forbid(unsafe_code)]

//! Base market-data vocabulary: timeframes, closed bars, money newtypes.
//!
//! This crate holds the money/time foundations every other prototype crate
//! builds on. It contains data definitions and parsing only; no indicator,
//! strategy, venue, or server logic lives here.

pub mod bar;
pub mod chain;
pub mod error;
pub mod money;
pub mod series;
pub mod timeframe;
pub mod warmup;
pub mod window;

pub use bar::{validate_new_bar, Adjustment, Bar};
pub use chain::{
    ChainQuote, ContractId, Greeks, LegSide, OptionContract, OptionKind, Portfolio, PortfolioId,
    SpreadLeg, SpreadLegs, MAX_SPREAD_LEGS,
};
pub use error::TypesError;
pub use money::{Money, Qty};
pub use series::{GapPolicy, LtfView, PriceSource, SeriesContext};
pub use timeframe::Timeframe;
pub use warmup::{adjust_fetch_bounds, trim_indicator_output, warmup_headroom, AdjustedBounds};
pub use window::{BarGap, BarWindow};
