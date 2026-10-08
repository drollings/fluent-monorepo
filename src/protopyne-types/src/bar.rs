//! Closed-bar market data and corporate-action adjustments.

use bon::Builder;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

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

#[cfg(test)]
#[path = "../tests/bar.rs"]
mod tests;
