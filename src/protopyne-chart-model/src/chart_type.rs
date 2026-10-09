//! Chart visualization taxonomy: which transform, if any, a view applies.
//!
//! Re-expressed in monorepo idioms from the reference charting application
//! (never imported): the variant set covers the same twenty views so future
//! work can name them, while predicates ([`ChartType::transforms_data`]
//! et al.) drive the headless transform dispatch in [`crate::transform`].

use std::fmt;

/// How a view renders OHLCV data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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

/// Families used to group chart types in pickers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChartTypeCategory {
    /// Direct OHLC pictures.
    Standard,
    /// Single-value lines.
    LineBased,
    /// Filled areas.
    AreaBased,
    /// Japanese techniques.
    Japanese,
    /// Price-based ranges.
    RangeBased,
    /// Professional overlays.
    Advanced,
}

impl ChartTypeCategory {
    /// Short display name for pickers.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::LineBased => "Line",
            Self::AreaBased => "Area",
            Self::Japanese => "Japanese",
            Self::RangeBased => "Range",
            Self::Advanced => "Advanced",
        }
    }

    /// Every category in picker order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::Standard,
            Self::LineBased,
            Self::AreaBased,
            Self::Japanese,
            Self::RangeBased,
            Self::Advanced,
        ]
    }
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

    /// Family this chart type belongs to.
    #[must_use]
    pub fn category(self) -> ChartTypeCategory {
        match self {
            Self::Bars | Self::Candles | Self::HollowCandles | Self::VolumeCandles => {
                ChartTypeCategory::Standard
            }
            Self::Line | Self::LineWithMarkers | Self::StepLine => ChartTypeCategory::LineBased,
            Self::Area | Self::HlcArea | Self::Baseline => ChartTypeCategory::AreaBased,
            Self::Renko | Self::Kagi | Self::LineBreak | Self::Heikin | Self::PointAndFigure => {
                ChartTypeCategory::Japanese
            }
            Self::HighLow | Self::Range => ChartTypeCategory::RangeBased,
            Self::VolumeFootprint | Self::TimePriceOpportunity | Self::SessionVolume => {
                ChartTypeCategory::Advanced
            }
        }
    }

    /// Chart types in one family, in picker order.
    #[must_use]
    pub fn in_category(category: ChartTypeCategory) -> Vec<Self> {
        Self::all().iter().copied().filter(|t| t.category() == category).collect()
    }

    /// Whether the view reads full OHLC legs (versus close only).
    #[must_use]
    pub fn uses_ohlc(self) -> bool {
        match self {
            Self::Line | Self::LineWithMarkers | Self::StepLine | Self::Area | Self::Baseline => false,
            Self::Bars
            | Self::Candles
            | Self::HollowCandles
            | Self::VolumeCandles
            | Self::HlcArea
            | Self::HighLow
            | Self::Range
            | Self::Renko
            | Self::Kagi
            | Self::LineBreak
            | Self::Heikin
            | Self::PointAndFigure
            | Self::VolumeFootprint
            | Self::TimePriceOpportunity
            | Self::SessionVolume => true,
        }
    }

    /// Whether volume can be shown alongside this view.
    #[must_use]
    pub fn supports_volume(self) -> bool {
        !matches!(
            self,
            Self::Renko
                | Self::Kagi
                | Self::PointAndFigure
                | Self::Range
                | Self::LineBreak
                | Self::VolumeFootprint
                | Self::SessionVolume
        )
    }

    /// Whether this view cannot render without an extra parameter
    /// (brick size, reversal amount, …).
    #[must_use]
    pub fn requires_parameters(self) -> bool {
        matches!(
            self,
            Self::Renko
                | Self::Kagi
                | Self::Range
                | Self::PointAndFigure
                | Self::LineBreak
                | Self::Baseline
        )
    }

    /// Whether the view drops the time axis (one glyph per price move).
    #[must_use]
    pub fn is_time_independent(self) -> bool {
        matches!(
            self,
            Self::Renko | Self::Kagi | Self::Range | Self::PointAndFigure | Self::LineBreak
        )
    }

    /// Whether the view reshapes input bars before painting.
    #[must_use]
    pub fn transforms_data(self) -> bool {
        matches!(
            self,
            Self::Renko
                | Self::Kagi
                | Self::Range
                | Self::PointAndFigure
                | Self::LineBreak
                | Self::Heikin
        )
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
