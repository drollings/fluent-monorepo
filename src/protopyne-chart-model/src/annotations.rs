//! Domain-keyed annotations: `(bar_timestamp, price)` identity, no pixels.
//!
//! An annotation names a bar close time (epoch seconds, the same clock as
//! [`ChartRow::time`](crate::convert::ChartRow::time)) plus an exact
//! [`Decimal`](rust_decimal::Decimal) price. Screen coordinates are never
//! stored: the paint edge resolves keys to pixels per frame through its
//! viewport, so annotations survive pan, zoom, and resize by construction —
//! the key does not move when the pixels do. Styles are display-level theme
//! roles, never decisions.

use rust_decimal::Decimal;

use crate::convert::to_display_float;

/// Domain identity of one annotation: when plus at what exact price.
///
/// Both legs are `Copy` and `Eq`, so keys compare by value and travel
/// cheaply; serialization keeps the price exact (no float rounding).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AnnotationKey {
    /// Bar close time as epoch seconds (the [`ChartRow`](crate::convert::ChartRow) clock).
    pub bar_timestamp: i64,
    /// Exact annotated price.
    pub price: Decimal,
}

impl AnnotationKey {
    /// Display float for the paint edge. `Decimal` legs are always finite,
    /// so this maps today; the `Option` keeps the display-boundary refusal
    /// shape for unrepresentable futures instead of substituting.
    #[must_use]
    pub fn price_float(self) -> Option<f64> {
        to_display_float(self.price)
    }

    /// One key from display-grade parts: a bar close time plus a chart
    /// float. This is the single sanctioned display→key crossing — the
    /// paint edge and the terminal bridge both key annotations off floats
    /// they read (a clicked close, a projected price), and the key type
    /// owns the conversion rule rather than each caller re-deciding it.
    /// `None` when the price is unrepresentable (non-finite): the caller
    /// creates nothing instead of pinning a substituted value. Keys from
    /// exact decimals use the struct literal directly.
    #[must_use]
    pub fn from_display(bar_timestamp: i64, price: f64) -> Option<Self> {
        Some(Self {
            bar_timestamp,
            price: Decimal::from_f64_retain(price)?,
        })
    }
}

/// Display role of an annotation marker: which theme key paints it.
///
/// These are paint roles (`bull`/`bear`/neutral), not trade semantics —
/// the marker records that something is worth looking at, never what to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AnnotationStyle {
    /// Neutral readout marker.
    Note,
    /// Up-flavored marker.
    Bull,
    /// Down-flavored marker.
    Bear,
}

/// One operator annotation: a domain key plus a label and display role.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Annotation {
    /// When and at what price.
    pub key: AnnotationKey,
    /// Human label; display text only.
    pub label: String,
    /// Display role.
    pub style: AnnotationStyle,
}

#[cfg(test)]
#[path = "../tests/annotations.rs"]
mod tests;
