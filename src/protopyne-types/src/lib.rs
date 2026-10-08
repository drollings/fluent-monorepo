#![forbid(unsafe_code)]

//! Base market-data vocabulary: timeframes, closed bars, money newtypes.
//!
//! This crate holds the money/time foundations every other prototype crate
//! builds on. It contains data definitions and parsing only; no indicator,
//! strategy, venue, or server logic lives here.

pub mod bar;
pub mod error;
pub mod money;
pub mod timeframe;
pub mod warmup;
pub mod window;

pub use bar::{Adjustment, Bar};
pub use error::TypesError;
pub use money::{Money, Qty};
pub use timeframe::Timeframe;
pub use warmup::{adjust_fetch_bounds, trim_indicator_output, warmup_headroom, AdjustedBounds};
pub use window::{BarGap, BarWindow};
