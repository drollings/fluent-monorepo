#![forbid(unsafe_code)]

//! Base market-data vocabulary: timeframes, closed bars, money newtypes.
//!
//! This crate holds the money/time foundations every other prototype crate
//! builds on. It contains data definitions and parsing only; no indicator,
//! strategy, venue, or server logic lives here.

pub mod bar;
pub mod chain;
pub mod epoch;
pub mod error;
pub mod fold;
pub mod mark;
pub mod money;
pub mod series;
pub mod session;
pub mod symbol;
pub mod timeframe;
pub mod warmup;
pub mod window;

pub use bar::{validate_new_bar, Adjustment, Bar};
pub use chain::{
    ChainQuote, ContractId, Greeks, GreeksCurve, GreeksPoint, LegSide, OptionContract, OptionKind,
    Portfolio, PortfolioId, SpreadLeg, SpreadLegs, DEFAULT_CONTRACT_MULTIPLIER, MAX_SPREAD_LEGS,
};
pub use epoch::EpochMs;
pub use error::TypesError;
pub use fold::{BucketAccum, FoldEdges};
pub use mark::MarkBasis;
pub use money::{parse_scale_guarded, DecimalScaleError, Money, Qty, MAX_DECIMAL_SCALE};
pub use series::{GapPolicy, LtfView, PriceSource, SeriesContext};
pub use session::{et_offset_ms, is_dst_et, SessionCalendar, TradingSession};
pub use symbol::{SymKey, SymbolIndex, DEFAULT_MAX_SYMBOLS};
pub use timeframe::{Timeframe, FOLD_ORIGIN_MS, WEEK_FOLD_ORIGIN_MS};
pub use warmup::{adjust_fetch_bounds, trim_indicator_output, warmup_headroom, AdjustedBounds};
pub use window::{BarGap, BarWindow};

#[cfg(test)]
#[path = "../tests/validate.rs"]
mod validate_tests;
