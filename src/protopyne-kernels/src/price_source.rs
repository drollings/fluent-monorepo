//! Canonical price-source registry: one spelling for every column a
//! smoothing or oscillator may read.
//!
//! Pine legs (`Legs::named`), the `ta_price` kernels, and the terminal frame
//! builders each spelled a subset before; this enum names all eight once and
//! every frame builder reads it. Formulas are plain `f64` arithmetic so `NaN`
//! holes propagate row-locally, never repaired.

use crate::indicator::{nan_hole, BarTick, OwnedOhlcv};

/// Which OHLC-derived column an indicator reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
/// Which OHLC-derived column an indicator reads.
pub enum PriceSource {
    /// Raw close.
    #[default]
    Close,
    /// Raw open.
    Open,
    /// Raw high.
    High,
    /// Raw low.
    Low,
    /// `(high + low) / 2`.
    Hl2,
    /// `(high + low + close) / 3`.
    Hlc3,
    /// `(open + high + low + close) / 4`.
    Ohlc4,
    /// `(high + low + 2 * close) / 4`.
    Wcl,
}

impl PriceSource {
    /// Every variant, in canonical order.
    #[must_use]
    pub fn all() -> [Self; 8] {
        [
            Self::Close,
            Self::Open,
            Self::High,
            Self::Low,
            Self::Hl2,
            Self::Hlc3,
            Self::Ohlc4,
            Self::Wcl,
        ]
    }

    /// Canonical id: also the Pine `Legs::named` spelling.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Close => "close",
            Self::Open => "open",
            Self::High => "high",
            Self::Low => "low",
            Self::Hl2 => "hl2",
            Self::Hlc3 => "hlc3",
            Self::Ohlc4 => "ohlc4",
            Self::Wcl => "wcl",
        }
    }

    /// Human label for pickers and settings.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Close => "Close",
            Self::Open => "Open",
            Self::High => "High",
            Self::Low => "Low",
            Self::Hl2 => "HL2",
            Self::Hlc3 => "HLC3",
            Self::Ohlc4 => "OHLC4",
            Self::Wcl => "WCL",
        }
    }

    /// Parse a canonical name (case-insensitive, surrounding space ignored).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "close" => Some(Self::Close),
            "open" => Some(Self::Open),
            "high" => Some(Self::High),
            "low" => Some(Self::Low),
            "hl2" => Some(Self::Hl2),
            "hlc3" => Some(Self::Hlc3),
            "ohlc4" => Some(Self::Ohlc4),
            "wcl" => Some(Self::Wcl),
            _ => None,
        }
    }

    /// Pine leg spelling this source must agree with.
    #[must_use]
    pub fn pine_leg(self) -> &'static str {
        self.name()
    }

    /// Per-bar scalar: the single truth `Legs::of` shares.
    #[must_use]
    pub fn compute_bar(self, bar: BarTick) -> f64 {
        match self {
            Self::Close => bar.close,
            Self::Open => bar.open,
            Self::High => bar.high,
            Self::Low => bar.low,
            Self::Hl2 => crate::math::midpoint_of(bar.high, bar.low),
            Self::Hlc3 => (bar.high + bar.low + bar.close) / 3.0,
            Self::Ohlc4 => (bar.open + bar.high + bar.low + bar.close) / 4.0,
            Self::Wcl => (bar.high + bar.low + bar.close * 2.0) / 4.0,
        }
    }

    /// Whole-column builder over a frame: length-preserving, hole-preserving.
    #[must_use]
    pub fn column_of(self, frame: &OwnedOhlcv) -> Vec<f64> {
        let len = frame.close.len();
        let at = |leg: &[f64], i: usize| nan_hole(leg.get(i).copied());
        (0..len)
            .map(|i| {
                self.compute_bar(BarTick {
                    open: at(&frame.open, i),
                    high: at(&frame.high, i),
                    low: at(&frame.low, i),
                    close: at(&frame.close, i),
                    volume: at(&frame.volume, i),
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "../tests/price_source.rs"]
mod tests;