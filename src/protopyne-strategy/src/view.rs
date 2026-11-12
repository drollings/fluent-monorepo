//! Point-in-time decision input.
//!
//! A [`MarketView`] is a borrowed, read-only bundle the caller assembles from
//! the data it already holds: a window of closed bars, a point-in-time option
//! chain, the DTE policy, and the entry instant. Signals read it instead of
//! receiving ever-longer parameter lists, and the chain arrives as a
//! [`ChainSlice`] whose construction refuses any quote stamped after the view
//! instant — a caller that sliced time wrongly gets a refusal, never a
//! silently borrowed future.
//!
//! The slice carries [`ChainQuote`] only. Money never crosses back through a
//! signal: a signal returns direction and unit-interval scores, and a separate
//! economics primitive converts quotes to [`Money`](protopyne_types::Money).

use chrono::{DateTime, Utc};

use protopyne_types::{Bar, ChainQuote, Timeframe};

use crate::error::StrategyError;

/// A caller-assembled point-in-time view of one underlying's decision inputs.
///
/// `bars` and `closes` are the same window: the caller derives the closes
/// once (the signal's fast path) rather than re-extracting them per call.
#[derive(Debug, Clone, Copy)]
/// A caller-assembled point-in-time view of one underlying's decision inputs.
/// 
/// `bars` and `closes` are the same window: the caller derives the closes
/// once (the signal's fast path) rather than re-extracting them per call.
pub struct MarketData<'a> {
    /// Closed bars, oldest first.
    pub bars: &'a [Bar],
    /// Point-in-time chain, when the caller has one. `None` and an empty
    /// slice both mean "no chain", never a zero-filled chain.
    pub chain: Option<ChainSlice<'a>>,
    /// Days to expiry the structure policy targets.
    pub dte_days: u32,
    /// Spacing of [`bars`](Self::bars).
    pub timeframe: Timeframe,
}

impl MarketData<'_> {
    /// A bar-less view: no bars, no chain, no spacing to interpret. Used by
    /// the economics-only proposal path, which takes its closes directly
    /// from [`ProposeInput`](crate::elements::ProposeInput). The timeframe is
    /// nominal because there is no bar whose span it could describe.
    #[must_use]
    pub const fn empty(dte_days: u32) -> Self {
        Self {
            bars: &[],
            chain: None,
            dte_days,
            timeframe: Timeframe::D1,
        }
    }
}

/// What a signal sees, with the view instant that bounds it.
#[derive(Debug, Clone, Copy)]
/// What a signal sees, with the view instant that bounds it.
pub struct MarketView<'a> {
    /// The caller's data window.
    pub data: &'a MarketData<'a>,
    /// Closes derived once from [`MarketData::bars`], oldest first.
    pub closes: &'a [f64],
    /// The instant this view is valid as of. No quote later than this may be
    /// read through [`MarketData::chain`].
    pub as_of: DateTime<Utc>,
}

impl MarketView<'_> {
    /// Chain quotes observable at this view's instant, if a chain is present.
    pub fn chain_quotes(&self) -> impl Iterator<Item = &ChainQuote> {
        self.data
            .chain
            .as_ref()
            .into_iter()
            .flat_map(|slice| slice.quotes_at_or_before(self.as_of))
    }
}

/// A point-in-time slice of one underlying's option chain.
///
/// Construction refuses a quote stamped after `as_of`, so a caller bug
/// (`chain_snapshot` for the wrong instant) cannot leak the future into a
/// decision. [`quotes_at_or_before`](Self::quotes_at_or_before) re-filters
/// defensively at read time as well.
#[derive(Debug, Clone, Copy)]
/// A point-in-time slice of one underlying's option chain.
/// 
/// Construction refuses a quote stamped after `as_of`, so a caller bug
/// (`chain_snapshot` for the wrong instant) cannot leak the future into a
/// decision. [`quotes_at_or_before`](Self::quotes_at_or_before) re-filters
/// defensively at read time as well.
pub struct ChainSlice<'a> {
    quotes: &'a [ChainQuote],
    as_of: DateTime<Utc>,
    underlying: &'a str,
}

impl<'a> ChainSlice<'a> {
    /// Bind `quotes` to `as_of` for `underlying`. Refuses when any quote is
    /// stamped strictly after `as_of`; an empty slice is valid (it is simply
    /// no chain, not an error).
    pub fn new(
        quotes: &'a [ChainQuote],
        as_of: DateTime<Utc>,
        underlying: &'a str,
    ) -> Result<Self, StrategyError> {
        if let Some(future) = quotes.iter().find(|quote| quote.timestamp > as_of) {
            return Err(StrategyError::InvalidElement(format!(
                "chain quote for {} is stamped {}, after the view instant {as_of}",
                future.contract.strike, future.timestamp
            )));
        }
        Ok(Self {
            quotes,
            as_of,
            underlying,
        })
    }

    /// Every quote held, in source order.
    #[must_use]
    pub fn quotes(&self) -> &'a [ChainQuote] {
        self.quotes
    }

    /// The instant this slice is valid as of.
    #[must_use]
    pub fn as_of(&self) -> DateTime<Utc> {
        self.as_of
    }

    /// The underlying this slice was taken for.
    #[must_use]
    pub fn underlying(&self) -> &'a str {
        self.underlying
    }

    /// Whether the slice holds no quotes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.quotes.is_empty()
    }

    /// Quotes stamped at-or-before `at`, in source order. `at` later than the
    /// slice instant still cannot observe a future quote: the filter is
    /// bounded by the quotes the slice actually holds.
    pub fn quotes_at_or_before(&self, at: DateTime<Utc>) -> impl Iterator<Item = &ChainQuote> {
        self.quotes
            .iter()
            .filter(move |quote| quote.timestamp <= at)
    }
}

#[cfg(test)]
#[path = "../tests/view.rs"]
mod tests;