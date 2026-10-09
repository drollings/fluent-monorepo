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
pub struct Bar {
    /// Close timestamp of the bar.
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
}

impl Bar {
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
