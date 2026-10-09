#![forbid(unsafe_code)]

//! Headless chart model: display conversion, chart taxonomy, sessions,
//! date ranges, and pure bar transforms.
//!
//! Everything here is `egui`-free by construction: it operates on
//! [`protopyne_types::Bar`] (`Decimal`-native) and produces plain data the
//! paint edge consumes. Display `f64` appears only at the conversion
//! boundary in [`convert`], never flows back into decisions.

pub mod chart_type;
pub mod convert;
pub mod error;
pub mod range;
pub mod session;
pub mod transform;

pub use chart_type::{ChartType, ChartTypeCategory};
pub use convert::{rows_of, row_of, to_display_float, ChartRow, ChartRows};
pub use error::ChartModelError;
pub use range::DateRange;
pub use session::{
    CompositeSessionProvider, DailySessionProvider, MonthlySessionProvider, SessionBreak,
    SessionBreakType, SessionProvider, WeeklySessionProvider, find_session_breaks,
};
pub use transform::{heikin_ashi, renko_bricks};
