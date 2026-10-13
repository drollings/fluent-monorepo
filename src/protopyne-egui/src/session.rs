//! Session-break overlays over closed bars.
//!
//! Composes the canonical break scan with the generic viewport's index
//! mapping: each returned line sits exactly on its break bar's x pixel, so
//! bar indices and gridlines can never drift apart.

use fluent_egui::{BreakLine, Viewport};
use protopyne_chart_model::{SessionProvider, find_session_breaks};
use protopyne_types::Bar;

/// Session-break gridlines over closed bars.
///
/// Composes the canonical break scan with this viewport's index mapping:
/// each returned line sits exactly on its break bar's x pixel.
#[must_use]
pub fn break_lines(viewport: &Viewport, bars: &[Bar], provider: &dyn SessionProvider) -> Vec<BreakLine> {
    find_session_breaks(bars, provider)
        .into_iter()
        .map(|(index, _)| BreakLine { index, x: viewport.index_to_x(index) })
        .collect()
}

#[cfg(test)]
#[path = "../tests/session.rs"]
mod tests;
