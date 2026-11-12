//! Options-activity scoring: an optional, task-value input to a strategy.
//!
//! A signal may carry a constant activity score; an operator who wants the
//! chain to rank tradability instead installs one [`OptionsActivity`] source
//! on the strategy. The source reads only the point-in-time view, so it can
//! never see a quote after the decision instant.
//!
//! [`ChainActivityScore`] is a bounded proxy, not a calibrated trigger:
//!
//! - **book imbalance** across near-the-money strikes — `|bid size − ask
//!   size| / total size`, one-sided books scoring highest;
//! - **open-interest concentration** across those strikes, normalized
//!   Herfindahl (evenly spread scores `0`, one strike scores `1`).
//!
//! Size and open interest aggregate every held expiry at a strike; the
//! point-in-time slice keeps the window honest. The two halves are equally
//! weighted. The score returns `None` whenever the chain cannot support a
//! reading: no chain, fewer than two strikes, a crossed quote, no size at
//! all, or a newest quote older than the evidence bound (a frozen tape is
//! not evidence of activity now). `None` leaves the signal's own
//! `activity_score` in place, so an absent or unreadable chain never silently
//! becomes a favourable score. No shipped strategy installs the source; the
//! operator opts in explicitly.

use std::collections::{BTreeMap, BTreeSet};

use rust_decimal::Decimal;

use protopyne_types::ChainQuote;

use crate::elements::SignalOutput;
use crate::error::StrategyError;
use crate::view::MarketView;

/// Reads point-in-time market data into an options-activity score in
/// `[0, 1]` **(task-value)**.
///
/// `None` means the inputs cannot support a reading; the strategy then keeps
/// the signal's own score. An `Err` means the source itself is unusable and
/// the proposal refuses.
pub trait OptionsActivity: Send + Sync {
    /// Score the view, if the chain can support a reading.
    fn score(&self, view: &MarketView<'_>) -> Result<Option<f64>, StrategyError>;
}

/// Default near-the-money band: strikes within ten percent of the last close.
pub const DEFAULT_NEAR_MONEY_BAND: f64 = 0.10;

/// Default evidence age: a quote older than fifteen minutes is not evidence
/// of activity at the decision instant.
pub const DEFAULT_MAX_QUOTE_AGE_SECS: i64 = 15 * 60;

/// Bounded chain-activity proxy over near-the-money strikes.
#[derive(Debug, Clone, Copy, PartialEq)]
/// Bounded chain-activity proxy over near-the-money strikes.
pub struct ChainActivityScore {
    near_money_band: f64,
    max_quote_age_secs: i64,
}

impl Default for ChainActivityScore {
    fn default() -> Self {
        Self {
            near_money_band: DEFAULT_NEAR_MONEY_BAND,
            max_quote_age_secs: DEFAULT_MAX_QUOTE_AGE_SECS,
        }
    }
}

impl ChainActivityScore {
    /// The default proxy (ten-percent near-the-money band).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A proxy with a caller-chosen near-the-money band, as a fraction of
    /// the last close. Refuses a non-finite or out-of-`(0, 1]` band rather
    /// than clamping.
    pub fn with_band(near_money_band: f64) -> Result<Self, StrategyError> {
        if !near_money_band.is_finite() || near_money_band <= 0.0 || near_money_band > 1.0 {
            return Err(StrategyError::InvalidElement(format!(
                "near-money band must be finite in (0, 1], got {near_money_band}"
            )));
        }
        Ok(Self {
            near_money_band,
            ..Self::default()
        })
    }

    /// Override how old the newest quote may be, relative to the view
    /// instant, and still count as evidence of activity.
    pub fn with_max_quote_age_secs(
        mut self,
        max_quote_age_secs: i64,
    ) -> Result<Self, StrategyError> {
        if max_quote_age_secs <= 0 {
            return Err(StrategyError::InvalidElement(format!(
                "max quote age must be positive, got {max_quote_age_secs}"
            )));
        }
        self.max_quote_age_secs = max_quote_age_secs;
        Ok(self)
    }

    /// The configured near-the-money band.
    #[must_use]
    pub fn near_money_band(&self) -> f64 {
        self.near_money_band
    }

    /// The configured evidence age bound, in seconds.
    #[must_use]
    pub fn max_quote_age_secs(&self) -> i64 {
        self.max_quote_age_secs
    }
}

impl OptionsActivity for ChainActivityScore {
    fn score(&self, view: &MarketView<'_>) -> Result<Option<f64>, StrategyError> {
        let quotes: Vec<&ChainQuote> = view.chain_quotes().collect();
        if quotes.is_empty() {
            return Ok(None);
        }
        // Activity must be observed at or near the decision instant: a frozen
        // tape is not evidence of trading now.
        let newest = quotes
            .iter()
            .map(|quote| quote.timestamp)
            .max()
            .expect("a non-empty quote set has a newest stamp");
        if (view.as_of - newest).num_seconds() > self.max_quote_age_secs {
            return Ok(None);
        }
        // A crossed book has no valid mid and cannot rank activity.
        if quotes.iter().any(|quote| quote.is_crossed()) {
            return Ok(None);
        }
        let spot = view
            .closes
            .last()
            .copied()
            .filter(|close| close.is_finite() && *close > 0.0);
        let considered: Vec<&ChainQuote> = quotes
            .into_iter()
            .filter(|quote| match spot {
                None => true,
                Some(spot) => strike_of(quote)
                    .is_some_and(|strike| (strike - spot).abs() / spot <= self.near_money_band),
            })
            .collect();
        let strikes: BTreeSet<Decimal> = considered
            .iter()
            .map(|quote| quote.contract.strike.as_decimal())
            .collect();
        if strikes.len() < 2 {
            return Ok(None);
        }
        let bid: f64 = considered
            .iter()
            .map(|quote| f64::from(quote.bid_size))
            .sum();
        let ask: f64 = considered
            .iter()
            .map(|quote| f64::from(quote.ask_size))
            .sum();
        let total_size = bid + ask;
        if total_size <= 0.0 {
            return Ok(None);
        }
        let imbalance = (bid - ask).abs() / total_size;
        let concentration = open_interest_concentration(&considered);
        Ok(Some(
            (0.5 * imbalance + 0.5 * concentration).clamp(0.0, 1.0),
        ))
    }
}

/// Normalized Herfindahl concentration of open interest over strikes:
/// `0` when evenly spread, `1` when one strike holds it all. A chain with no
/// open interest contributes nothing to the score.
fn open_interest_concentration(quotes: &[&ChainQuote]) -> f64 {
    let mut by_strike: BTreeMap<Decimal, f64> = BTreeMap::new();
    for quote in quotes {
        let open_interest = quote.open_interest as f64;
        if open_interest <= 0.0 {
            continue;
        }
        *by_strike
            .entry(quote.contract.strike.as_decimal())
            .or_insert(0.0) += open_interest;
    }
    let total: f64 = by_strike.values().sum();
    if total <= 0.0 || by_strike.len() < 2 {
        return 0.0;
    }
    let hhi: f64 = by_strike
        .values()
        .map(|open_interest| (open_interest / total).powi(2))
        .sum();
    let floor = 1.0 / by_strike.len() as f64;
    ((hhi - floor) / (1.0 - floor)).clamp(0.0, 1.0)
}

fn strike_of(quote: &ChainQuote) -> Option<f64> {
    // No positivity check here (unlike `ComputedGreeks::contract_strike`):
    // activity bins by relative position, it never prices the strike.
    quote.contract.strike.as_finite_f64()
}

/// Replace a signal's constant activity score with the source's reading.
/// A source score outside `[0, 1]` is a contract violation and refuses.
pub(crate) fn with_source_score(
    output: SignalOutput,
    view: &MarketView<'_>,
    source: &dyn OptionsActivity,
) -> Result<SignalOutput, StrategyError> {
    let Some(score) = source.score(view)? else {
        return Ok(output);
    };
    if !score.is_finite() || !(0.0..=1.0).contains(&score) {
        return Err(StrategyError::InvalidElement(format!(
            "options-activity source scored {score} outside [0, 1]"
        )));
    }
    Ok(SignalOutput {
        activity_score: score,
        ..output
    })
}

#[cfg(test)]
#[path = "../tests/activity.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/activity_calibration.rs"]
mod calibration_tests;