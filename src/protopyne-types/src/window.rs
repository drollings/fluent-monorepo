//! Closed-bar window with gap detection.
//!
//! The window stores closed bars only: a [`Bar`](crate::bar::Bar) is by
//! construction a finished bar, so no open-candle filtering happens here.
//! Pushes must arrive in strictly increasing timestamp order; anything else
//! is a typed error, never a silent reorder.

use bon::Builder;
use chrono::{DateTime, Duration, Utc};

use crate::bar::Bar;
use crate::error::TypesError;
use crate::timeframe::Timeframe;

/// One detected discontinuity in an otherwise ordered bar sequence.
#[derive(Debug, Clone, PartialEq, Builder)]
#[builder(start_fn = new)]
pub struct BarGap {
    /// Position in the window where the discontinuity was observed.
    pub at_index: usize,
    /// Timestamp the missing bar(s) should have carried.
    pub expected_ts: DateTime<Utc>,
    /// Timestamp actually observed at `at_index`.
    pub actual_ts: DateTime<Utc>,
    /// Whole bars missing between the predecessor and `actual_ts`.
    pub missing_bars: u64,
}

/// Ordered store of closed bars for one symbol and timeframe.
#[derive(Debug, Clone)]
pub struct BarWindow {
    timeframe: Timeframe,
    bars: Vec<Bar>,
}

impl BarWindow {
    /// Create an empty window for the given bar spacing.
    #[must_use]
    pub fn new(timeframe: Timeframe) -> Self {
        Self {
            timeframe,
            bars: Vec::new(),
        }
    }

    /// Bar spacing this window is checked against.
    #[must_use]
    pub fn timeframe(&self) -> Timeframe {
        self.timeframe
    }

    /// Append a closed bar. Runs the full construction check — OHLC
    /// structure plus strictly increasing timestamps — so gaps are always
    /// detected explicitly via [`gaps`](Self::gaps), never hidden by
    /// reordering, and structurally invalid bars never enter the decision
    /// path.
    pub fn push(&mut self, bar: Bar) -> Result<(), TypesError> {
        crate::bar::validate_new_bar(self.bars.last(), &bar)?;
        self.bars.push(bar);
        Ok(())
    }

    /// All stored bars in arrival order.
    #[must_use]
    pub fn bars(&self) -> &[Bar] {
        &self.bars
    }

    /// Number of stored bars.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bars.len()
    }

    /// Whether no bars are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    /// Scan for spacing discontinuities. Every consecutive pair whose
    /// distance differs from the timeframe step yields one [`BarGap`].
    #[must_use]
    pub fn gaps(&self) -> Vec<BarGap> {
        let step_secs = self.timeframe.total_seconds() as i64;
        let mut out = Vec::new();
        for (index, pair) in self.bars.windows(2).enumerate() {
            let prev = &pair[0];
            let current = &pair[1];
            let diff_secs = (current.ts - prev.ts).num_seconds();
            if diff_secs != step_secs {
                let missing = diff_secs.div_euclid(step_secs).saturating_sub(1).max(0) as u64;
                // Timestamps near the edges of the representable range cannot
                // overflow in market data; fall back to the observed stamp so
                // gap reporting itself never panics.
                let expected_ts = prev
                    .ts
                    .checked_add_signed(Duration::seconds(step_secs))
                    .unwrap_or(current.ts);
                out.push(
                    BarGap::new()
                        .at_index(index + 1)
                        .expected_ts(expected_ts)
                        .actual_ts(current.ts)
                        .missing_bars(missing)
                        .build(),
                );
            }
        }
        out
    }

    /// Whether [`gaps`](Self::gaps) is non-empty.
    #[must_use]
    pub fn has_gaps(&self) -> bool {
        let step_secs = self.timeframe.total_seconds() as i64;
        self.bars
            .windows(2)
            .any(|pair| (pair[1].ts - pair[0].ts).num_seconds() != step_secs)
    }
}

#[cfg(test)]
#[path = "../tests/window.rs"]
mod tests;
