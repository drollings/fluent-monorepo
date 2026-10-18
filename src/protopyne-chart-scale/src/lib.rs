#![forbid(unsafe_code)]

//! Headless scale engine: price modes, ranges, bar/price mapping, ticks.
//!
//! Pure math, `egui`-free: pixel geometry enters only as `f32`/`f64`
//! dimensions supplied by the paint edge. Degenerate inputs (zero height,
//! non-positive log bounds, inverted ranges) refuse fail-closed instead of
//! emitting substitute coordinates.

pub mod error;
pub mod mapping;
pub mod ticks;

pub use error::ScaleError;
pub use mapping::{PriceRange, PriceScale, PriceScaleMode};
pub use ticks::{nice_ticks, Tick};
