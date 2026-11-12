//! Closed-bar market data and corporate-action adjustments.

use bon::Builder;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::TypesError;
use crate::money::Money;

/// Corporate-action adjustment attached to a closed bar.
#[derive(Debug, Clone, PartialEq, Builder, Serialize, Deserialize)]
#[builder(start_fn = new)]
/// Corporate-action adjustment attached to a closed bar.
pub struct Adjustment {
    /// Split factor applied at this bar (e.g. `2` for a 2-for-1 split).
    #[serde(default)]
    pub split_factor: Option<Decimal>,
    /// Cash dividend paid at this bar.
    #[serde(default)]
    pub dividend: Option<Money>,
}

/// A single closed bar. Only closed bars enter the decision path; open
/// (still-forming) candles are never represented here.
#[derive(Debug, Clone, PartialEq, Builder, Serialize, Deserialize)]
#[builder(start_fn = new)]
/// A single closed bar. Only closed bars enter the decision path; open
/// (still-forming) candles are never represented here.
pub struct Bar {
    /// Bucket-open timestamp: the instant the bar's interval begins.
    ///
    /// The interval a bar summarizes ends at
    /// [`confirmed_at`](Self::confirmed_at) when its producer knows the close.
    /// Read this through [`open_time`](Self::open_time) when the distinction
    /// matters — every visibility predicate in the kernel layer compares
    /// against the open, never against a presumed close.
    pub ts: DateTime<Utc>,
    /// Open price.
    pub o: Decimal,
    /// High price.
    pub h: Decimal,
    /// Low price.
    pub l: Decimal,
    /// Close price.
    pub c: Decimal,
    /// Traded volume in shares/contracts.
    pub volume: u64,
    /// Corporate-action adjustment, if any.
    #[serde(default)]
    pub adj: Option<Adjustment>,
    /// The instant this bar's bucket closed, when its producer knows it.
    ///
    /// [`None`] means the bar is not known to be closed — either the producer
    /// did not record the close or the bucket is still forming. Never part of
    /// the wire shape: a serialized bar carries its open stamp and OHLC only.
    #[serde(skip)]
    pub confirmed_at: Option<DateTime<Utc>>,
}

impl fluent_wvr::validate::Validate for Bar {
    type Err = TypesError;
    fn validate(&self) -> Result<(), Self::Err> {
        Bar::validate(self)
    }
}

impl Bar {
    /// The instant this bar's bucket opens.
    #[must_use]
    pub fn open_time(&self) -> DateTime<Utc> {
        self.ts
    }

    /// The instant this bar's bucket closed, when its producer knows it.
    ///
    /// A consumer that must not act on a still-forming bucket treats [`None`]
    /// as "not closed" rather than assuming a duration.
    #[must_use]
    pub fn confirmed_at(&self) -> Option<DateTime<Utc>> {
        self.confirmed_at
    }

    /// Check the OHLC structural invariants: `high` covers open, close, and
    /// low; `low` sits under open, close, and high. Volume needs no check —
    /// it is a `u64`, so negative volume is unrepresentable by construction.
    pub fn validate(&self) -> Result<(), TypesError> {
        if self.h < self.o || self.h < self.c || self.h < self.l {
            return Err(TypesError::InvalidBar(format!(
                "high ({}) is less than open ({}), close ({}), or low ({})",
                self.h, self.o, self.c, self.l
            )));
        }
        if self.l > self.o || self.l > self.c || self.l > self.h {
            return Err(TypesError::InvalidBar(format!(
                "low ({}) is greater than open ({}), close ({}), or high ({})",
                self.l, self.o, self.c, self.h
            )));
        }
        Ok(())
    }
}

/// One decimal price leg as an `f64` hole (`NaN` when unrepresentable):
/// the one spelling for the `to_f64().unwrap_or(NAN)` / `nan_hole(...)`
/// price-leg conversions across the kernel layer (indicator legs, bar
/// frames, research frames, Pine legs, IV quotes). Volume legs
/// (`u64 as f64`) are not decimals and stay out of scope (M3.1).
#[must_use]
/// One decimal price leg as an `f64` hole (`NaN` when unrepresentable):
/// the one spelling for the `to_f64().unwrap_or(NAN)` / `nan_hole(...)`
/// price-leg conversions across the kernel layer (indicator legs, bar
/// frames, research frames, Pine legs, IV quotes). Volume legs
/// (`u64 as f64`) are not decimals and stay out of scope (M3.1).
pub fn bar_leg_as_f64(leg: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive as _;
    leg.to_f64().unwrap_or(f64::NAN)
}

/// Validate one new bar against its predecessor for the streaming feed path:
/// OHLC structure first, then strictly increasing timestamps (duplicates
/// included — an equal timestamp is a reorder, never a new bar).
pub fn validate_new_bar(last: Option<&Bar>, new: &Bar) -> Result<(), TypesError> {
    new.validate()?;
    if let Some(prev) = last {
        if new.ts <= prev.ts {
            return Err(TypesError::InvalidBar(format!(
                "bars must arrive in strictly increasing order: {} <= {}",
                new.ts, prev.ts
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/bar.rs"]
mod tests;