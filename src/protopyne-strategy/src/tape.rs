//! A point-in-time window that cannot see the future.
//!
//! Every decision component reads a `Tape`: the bars, the batched legs, and
//! the closes, all bounded by one index. Reads past that index return `None`,
//! so look-ahead is a refusal at the type's edge rather than a convention a
//! reviewer has to remember. The window advances with [`Tape::next`] without
//! re-slicing or allocating.

use std::sync::Arc;

use protopyne_kernels::lite::LiteLegs;
use protopyne_types::Bar;

/// One bar's view of a closed tape.
#[derive(Debug, Clone, Copy)]
pub struct Tape<'a> {
    bars: &'a [Bar],
    legs: &'a LiteLegs,
    closes: &'a [f64],
    at: usize,
}

impl<'a> Tape<'a> {
    /// Bind a tape at `at`.
    ///
    /// Refuses an index past the last bar, a legs batch shorter than the
    /// bars, or a closes window shorter than the bars: every one of those
    /// would make an aligned read silently wrong.
    #[must_use]
    pub fn new(bars: &'a [Bar], legs: &'a LiteLegs, closes: &'a [f64], at: usize) -> Option<Self> {
        if at >= bars.len() || legs.len() < bars.len() || closes.len() < bars.len() {
            return None;
        }
        Some(Self {
            bars,
            legs,
            closes,
            at,
        })
    }

    /// The only bar index this tape allows.
    #[must_use]
    pub fn at(&self) -> usize {
        self.at
    }

    /// The bar at `i`, or `None` when `i` is past the tape's cursor.
    #[must_use]
    pub fn bar(&self, i: usize) -> Option<&'a Bar> {
        if i > self.at {
            return None;
        }
        self.bars.get(i)
    }

    /// The close at `i`, or `None` when `i` is past the tape's cursor.
    #[must_use]
    pub fn close(&self, i: usize) -> Option<f64> {
        if i > self.at {
            return None;
        }
        self.closes.get(i).copied()
    }

    /// The batched legs behind the tape.
    #[must_use]
    pub fn legs(&self) -> &'a LiteLegs {
        self.legs
    }

    /// The visible bars, oldest first: exactly `bars[..=at]`.
    #[must_use]
    pub fn window(&self) -> &'a [Bar] {
        &self.bars[..=self.at]
    }

    /// The visible closes, oldest first.
    #[must_use]
    pub fn closes(&self) -> &'a [f64] {
        &self.closes[..=self.at]
    }

    /// Advance one bar, or `None` at the end of the tape.
    #[must_use]
    pub fn next(&self) -> Option<Tape<'a>> {
        if self.at + 1 >= self.bars.len() {
            return None;
        }
        Some(Tape {
            bars: self.bars,
            legs: self.legs,
            closes: self.closes,
            at: self.at + 1,
        })
    }
}

/// An owned, shareable tape binding: the bars, the batched legs, and the
/// cursor, behind `Arc`s so a per-bar snapshot crosses a `WorkContext`
/// channel without borrowing the decision loop's frame.
///
/// A [`Tape`] borrows; a `TapeSnapshot` owns. [`TapeSnapshot::view`]
/// rebinds the borrow-checked window, refusing exactly what [`Tape::new`]
/// refuses, so the snapshot can never smuggle an out-of-bounds cursor past
/// the look-ahead fence.
#[derive(Debug, Clone)]
pub struct TapeSnapshot {
    /// Closed bars behind the cursor.
    pub bars: Arc<Vec<Bar>>,
    /// Batched indicator legs aligned with `bars`.
    pub legs: Arc<LiteLegs>,
    /// Cursor index into both.
    pub at: usize,
}

impl TapeSnapshot {
    /// Bind owned bars and legs at `at`. Bounds are checked by [`view`](Self::view), not here.
    #[must_use]
    pub fn new(bars: Arc<Vec<Bar>>, legs: Arc<LiteLegs>, at: usize) -> Self {
        Self { bars, legs, at }
    }

    /// Rebind the borrow-checked window over the owned bindings.
    #[must_use]
    pub fn view(&self) -> Option<Tape<'_>> {
        Tape::new(&self.bars, &self.legs, &self.legs.close, self.at)
    }
}

#[cfg(test)]
pub(crate) fn sine_fixture(len: usize) -> (Vec<Bar>, LiteLegs) {
    use chrono::{TimeZone as _, Utc};
    use protopyne_kernels::lite::{lite_legs, LiteParams};
    use rust_decimal::Decimal;
    let bars: Vec<Bar> = (0..len)
        .map(|index| {
            let close = 100.0 + (index as f64 / 5.0).sin() * 4.0 + index as f64 * 0.05;
            let value = Decimal::from_f64_retain(close).expect("finite");
            Bar::new()
                .ts(Utc
                    .with_ymd_and_hms(2026, 1, 5, 15, 0, 0)
                    .single()
                    .expect("time")
                    + chrono::Duration::minutes(index as i64))
                .o(value)
                .h(Decimal::from_f64_retain(close + 0.5).expect("finite"))
                .l(Decimal::from_f64_retain(close - 0.5).expect("finite"))
                .c(value)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let legs = lite_legs(&bars, &LiteParams::default(), 14).expect("legs");
    (bars, legs)
}

#[cfg(test)]
#[path = "../tests/tape.rs"]
mod tests;
