//! Per-run drawings: labels and lines a script emits while evaluating.
//!
//! Drawings are collected on the [`RunCtx`](crate::context::RunCtx) and die
//! with the run. They never cross into venue or journal types.

use bon::Builder;
use serde::{Deserialize, Serialize};

/// What a drawing marks on the chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// What a drawing marks on the chart.
pub enum DrawingKind {
    /// Text anchored at a bar and price.
    Label,
    /// Segment anchored at a bar and price.
    Line,
}

/// One chart annotation emitted during evaluation.
#[derive(Debug, Clone, PartialEq, Builder, Serialize, Deserialize)]
#[builder(start_fn = new)]
/// One chart annotation emitted during evaluation.
pub struct Drawing {
    /// Label or line.
    pub kind: DrawingKind,
    /// Cursor position the drawing anchors to.
    pub bar_index: usize,
    /// Price the drawing anchors to.
    pub price: f64,
    /// Annotation text.
    pub text: String,
}

#[cfg(test)]
#[path = "../tests/drawing.rs"]
mod tests;