//! Chart visualization taxonomy: the twenty views the terminal can name.
//!
//! Re-expressed in monorepo idioms from the reference charting application
//! (never imported): [`ChartType::all`] is the picker order and
//! [`ChartType::name`] the label. Which transform a view applies is decided
//! by the terminal's dispatch, not by predicates here.

use std::fmt;

/// How a view renders OHLCV data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
/// How a view renders OHLCV data.
pub enum ChartType {
    /// Classic open/high/low/close ticks.
    Bars,
    /// Japanese candlesticks.
    #[default]
    Candles,
    /// Candles hollow on up bars, filled on down bars.
    HollowCandles,
    /// Candles whose width reflects volume.
    VolumeCandles,
    /// Close-price polyline.
    Line,
    /// Close-price polyline with a marker per point.
    LineWithMarkers,
    /// Close-price polyline held flat between bars.
    StepLine,
    /// Filled region under the close line.
    Area,
    /// Filled region spanning high/low/close.
    HlcArea,
    /// Filled region split around a baseline level.
    Baseline,
    /// Vertical high-low segments.
    HighLow,
    /// Time-independent range bars.
    Range,
    /// Price-movement bricks without a time axis.
    Renko,
    /// Trend-reversal lines without a time axis.
    Kagi,
    /// Consecutive-line reversal chart.
    LineBreak,
    /// Smoothed heikin-ashi candles.
    Heikin,
    /// X-and-O columns.
    PointAndFigure,
    /// Per-price-level volume footprint.
    VolumeFootprint,
    /// Time-price-opportunity market profile.
    TimePriceOpportunity,
    /// Volume aggregated by session.
    SessionVolume,
}

impl ChartType {
    /// Every chart type in picker order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::Bars,
            Self::Candles,
            Self::HollowCandles,
            Self::VolumeCandles,
            Self::Line,
            Self::LineWithMarkers,
            Self::StepLine,
            Self::Area,
            Self::HlcArea,
            Self::Baseline,
            Self::HighLow,
            Self::VolumeFootprint,
            Self::TimePriceOpportunity,
            Self::SessionVolume,
            Self::LineBreak,
            Self::Kagi,
            Self::Range,
            Self::PointAndFigure,
            Self::Renko,
            Self::Heikin,
        ]
    }

    /// Short display name for pickers.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Bars => "Bars",
            Self::Candles => "Candles",
            Self::HollowCandles => "Hollow candles",
            Self::VolumeCandles => "Volume candles",
            Self::Line => "Line",
            Self::LineWithMarkers => "Line with markers",
            Self::StepLine => "Step line",
            Self::Area => "Area",
            Self::HlcArea => "HLC area",
            Self::Baseline => "Baseline",
            Self::HighLow => "High-low",
            Self::VolumeFootprint => "Volume footprint",
            Self::TimePriceOpportunity => "Time Price Opportunity",
            Self::SessionVolume => "Session volume",
            Self::LineBreak => "Line break",
            Self::Kagi => "Kagi",
            Self::Range => "Range",
            Self::PointAndFigure => "Point & Figure",
            Self::Renko => "Renko",
            Self::Heikin => "Heikin Ashi",
        }
    }
}

impl fmt::Display for ChartType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
#[path = "../tests/chart_type.rs"]
mod tests;