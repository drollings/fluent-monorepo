#![forbid(unsafe_code)]

//! Headless chart model: display conversion, chart taxonomy, sessions,
//! date ranges, and pure bar transforms.
//!
//! Everything here is `egui`-free by construction: it operates on
//! [`protopyne_types::Bar`] (`Decimal`-native) and produces plain data the
//! paint edge consumes. Display `f64` appears only at the conversion
//! boundary in [`convert`], never flows back into decisions.

pub mod annotations;
pub mod chart_type;
pub mod convert;
pub mod display_only;
pub mod error;
pub mod range;
pub mod session;
pub mod transform;

pub use annotations::{Annotation, AnnotationKey, AnnotationStyle};
pub use chart_type::ChartType;
pub use convert::{row_of, rows_of, to_display_float, ChartBars, ChartRow};
pub use display_only::{DisplayOnly, DisplayOnlyError};
pub use error::ChartModelError;
pub use range::{DateRange, MONTH_SECONDS};
pub use session::{
    find_session_breaks, session_breaks_between, CompositeSessionProvider, DailySessionProvider,
    EtSessionProvider, MonthlySessionProvider, SessionBreak, SessionBreakType, SessionProvider,
    WeeklySessionProvider,
};
pub use transform::{heikin_ashi, heikin_rows_of, renko_bricks};

#[cfg(test)]
#[path = "../tests/budget.rs"]
mod budget_tests;
