//! Series and multi-timeframe context for the evaluation runtime.
//!
//! [`PriceSource`] selects which leg of a bar a series calculation reads —
//! the fix for hardcoded-close substitution: kernel helpers take a source,
//! never a bare assumption. [`SeriesContext`] binds one symbol and timeframe
//! to its validated bars plus the gap policy the runtime interprets them
//! under; [`LtfView`] is the borrowed higher-timeframe slice the v6 runtime
//! threads alongside the chart bars. Money types are untouched here.
//!
//! The stream worker's gap policy (`fluent-concurrency`'s reconnect shape)
//! and this module's [`GapPolicy`] share a name and a shape (sparse tolerates
//! holes, dense requires contiguity) over different domains — sequence-number
//! dedupe there, bar-spacing interpretation here. They stay separate types so
//! neither layer can pass its policy to the other by accident.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::bar::{validate_new_bar, Bar};
use crate::error::TypesError;
use crate::timeframe::Timeframe;
use crate::window::BarWindow;

/// Which leg of a bar a series calculation reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum PriceSource {
    /// Opening price.
    Open,
    /// Highest price.
    High,
    /// Lowest price.
    Low,
    /// Closing price (the common case).
    #[default]
    Close,
    /// `(high + low) / 2`.
    Hl2,
    /// `(high + low + close) / 3`.
    Hlc3,
    /// `(open + high + low + close) / 4`.
    Ohlc4,
}

impl PriceSource {
    /// Every variant, in canonical order.
    #[must_use]
    pub fn all() -> [Self; 7] {
        [
            Self::Open,
            Self::High,
            Self::Low,
            Self::Close,
            Self::Hl2,
            Self::Hlc3,
            Self::Ohlc4,
        ]
    }

    /// Display label for configuration surfaces.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::High => "High",
            Self::Low => "Low",
            Self::Close => "Close",
            Self::Hl2 => "HL/2",
            Self::Hlc3 => "HLC/3",
            Self::Ohlc4 => "OHLC/4",
        }
    }

    /// Read the selected price from four legs. Pure `Decimal` math — never
    /// panics; thirds round to the `Decimal` precision rather than failing.
    #[must_use]
    pub fn compute(self, open: Decimal, high: Decimal, low: Decimal, close: Decimal) -> Decimal {
        match self {
            Self::Open => open,
            Self::High => high,
            Self::Low => low,
            Self::Close => close,
            Self::Hl2 => (high + low) / Decimal::from(2),
            Self::Hlc3 => (high + low + close) / Decimal::from(3),
            Self::Ohlc4 => (open + high + low + close) / Decimal::from(4),
        }
    }

    /// Read the selected price from a closed bar.
    #[must_use]
    pub fn select(self, bar: &Bar) -> Decimal {
        self.compute(bar.o, bar.h, bar.l, bar.c)
    }
}

/// How a series interprets spacing discontinuities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GapPolicy {
    /// Holes are ordinary (filtered sessions, sparse symbols): consumers
    /// read around them, `gaps` stays advisory.
    #[default]
    Sparse,
    /// Holes are data defects: pushing a bar that opens a gap is refused.
    Dense,
}

/// One symbol and timeframe bound to validated bars: the chart-bar context
/// the evaluation runtime threads per run.
#[derive(Debug, Clone)]
pub struct SeriesContext {
    symbol: String,
    window: BarWindow,
    gap_policy: GapPolicy,
}

impl SeriesContext {
    /// Bind a symbol and timeframe under a gap policy. Starts empty; bars
    /// arrive through [`push`](Self::push).
    #[must_use]
    pub fn new(symbol: impl Into<String>, timeframe: Timeframe, gap_policy: GapPolicy) -> Self {
        Self {
            symbol: symbol.into(),
            window: BarWindow::new(timeframe),
            gap_policy,
        }
    }

    /// Symbol this context covers.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Bar spacing this context is checked against.
    #[must_use]
    pub fn timeframe(&self) -> Timeframe {
        self.window.timeframe()
    }

    /// Gap policy this context enforces.
    #[must_use]
    pub fn gap_policy(&self) -> GapPolicy {
        self.gap_policy
    }

    /// Append a closed bar: OHLC structure and timestamp order always
    /// checked; under [`GapPolicy::Dense`] a bar that opens a spacing gap is
    /// refused as well.
    pub fn push(&mut self, bar: Bar) -> Result<(), TypesError> {
        validate_new_bar(self.window.bars().last(), &bar)?;
        if self.gap_policy == GapPolicy::Dense {
            if let Some(last) = self.window.bars().last() {
                let step = self.window.timeframe().total_seconds() as i64;
                if (bar.ts - last.ts).num_seconds() != step {
                    return Err(TypesError::InvalidBar(format!(
                        "dense series refuses a spacing gap: {} -> {} (expected {step}s)",
                        last.ts, bar.ts
                    )));
                }
            }
        }
        self.window.push(bar).expect("just-validated bar pushes");
        Ok(())
    }

    /// All stored bars in arrival order.
    #[must_use]
    pub fn bars(&self) -> &[Bar] {
        self.window.bars()
    }

    /// Number of stored bars.
    #[must_use]
    pub fn len(&self) -> usize {
        self.window.len()
    }

    /// Whether no bars are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.window.is_empty()
    }

    /// Borrow a higher-timeframe view over the stored bars for LTF readers.
    #[must_use]
    pub fn ltf_view(&self, timeframe: Timeframe) -> LtfView<'_> {
        LtfView {
            timeframe,
            bars: self.window.bars(),
        }
    }

    /// One leg of every stored bar under a price source.
    #[must_use]
    pub fn source_series(&self, source: PriceSource) -> Vec<Decimal> {
        self.window
            .bars()
            .iter()
            .map(|bar| source.select(bar))
            .collect()
    }
}

/// A borrowed higher-timeframe slice over chart bars: the LTF array the v6
/// runtime reads alongside the chart timeframe. No resampling lives here —
/// aggregation is a runtime concern, not a context one.
#[derive(Debug, Clone, Copy)]
pub struct LtfView<'a> {
    timeframe: Timeframe,
    bars: &'a [Bar],
}

impl<'a> LtfView<'a> {
    /// Timeframe this view is read as.
    #[must_use]
    pub fn timeframe(&self) -> Timeframe {
        self.timeframe
    }

    /// Borrowed bars.
    #[must_use]
    pub fn bars(&self) -> &'a [Bar] {
        self.bars
    }

    /// Number of bars in view.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bars.len()
    }

    /// Whether the view is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    /// One leg of every bar under a price source.
    #[must_use]
    pub fn source_series(&self, source: PriceSource) -> Vec<Decimal> {
        self.bars.iter().map(|bar| source.select(bar)).collect()
    }
}

#[cfg(test)]
#[path = "../tests/series.rs"]
mod tests;
