//! Quotes, the liquidity filter, and the slippage model.
//!
//! Every fill starts from a two-sided quote. The [`LiquidityFilter`] refuses
//! illiquid, wide, or stale markets with reasons (a **(task-value)** gate for
//! width/size, **(correctness)** for age); [`Slippage`] then moves the touch
//! price adversely within the caller's tolerance. The default tolerance is
//! frozen by the slippage-sweep calibration, not by intuition.

use bon::Builder;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use protopyne_types::Money;

/// Default slippage tolerance in basis points, frozen by calibration:
/// the sweep shows 10 bps keeps adverse cost under a tenth of a typical
/// spread while filling every liquid marketable order.
pub const DEFAULT_SLIPPAGE_BPS: u32 = 10;

/// A two-sided quote at one instant.
#[derive(Debug, Clone, Copy, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct Quote {
    /// Best bid.
    pub bid: Money,
    /// Best ask.
    pub ask: Money,
    /// Contracts (or shares) available at the bid.
    pub bid_size: u64,
    /// Contracts (or shares) available at the ask.
    pub ask_size: u64,
    /// Quote timestamp.
    pub ts: DateTime<Utc>,
}

impl Quote {
    /// Mid price, or `None` when crossed/invalid (bid >= ask fails closed).
    #[must_use]
    pub fn mid(&self) -> Option<Money> {
        if self.ask.as_decimal() <= self.bid.as_decimal() {
            return None;
        }
        let two = Decimal::from(2);
        let mid = (self.bid.as_decimal() + self.ask.as_decimal()) / two;
        if mid.is_zero() || mid.is_sign_negative() {
            None
        } else {
            Some(Money::new(mid))
        }
    }

    /// Spread width, or `None` when crossed/invalid.
    #[must_use]
    pub fn spread_width(&self) -> Option<Money> {
        if self.ask.as_decimal() <= self.bid.as_decimal() {
            return None;
        }
        Some(Money::new(self.ask.as_decimal() - self.bid.as_decimal()))
    }

    /// Age in seconds against `now`, saturating on clock skew.
    #[must_use]
    pub fn age_secs(&self, now: DateTime<Utc>) -> u64 {
        now.signed_duration_since(self.ts).num_seconds().max(0) as u64
    }
}

/// Minimum market quality for a fill. Width and size bound economics
/// (**(task-value)**); age bounds validity (**(correctness)**).
#[derive(Debug, Clone, Copy, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct LiquidityFilter {
    /// Maximum acceptable spread width **(task-value)**.
    pub max_spread_width: Money,
    /// Minimum size on the trading side **(task-value)**.
    pub min_size: u64,
    /// Maximum quote age in seconds **(correctness)**.
    pub max_age_secs: u64,
}

impl LiquidityFilter {
    /// Accept or refuse `quote` for `contracts` on `is_buy`, collecting no
    /// partial state: the first failure explains the refusal.
    pub fn check(
        &self,
        quote: &Quote,
        contracts: u32,
        is_buy: bool,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        let width = quote
            .spread_width()
            .ok_or_else(|| "crossed or invalid quote".to_owned())?;
        if width.as_decimal() > self.max_spread_width.as_decimal() {
            return Err(format!(
                "spread {width} above maximum {}",
                self.max_spread_width
            ));
        }
        let size = if is_buy {
            quote.ask_size
        } else {
            quote.bid_size
        };
        if size < self.min_size {
            return Err(format!("size {size} below minimum {}", self.min_size));
        }
        if u64::from(contracts) > size {
            return Err(format!("order for {contracts} exceeds quoted size {size}"));
        }
        if quote.age_secs(now) > self.max_age_secs {
            return Err(format!(
                "quote age {}s above maximum {}s",
                quote.age_secs(now),
                self.max_age_secs
            ));
        }
        Ok(())
    }
}

/// Adverse price move applied to the touch, in basis points of price.
/// Axis: **(task-value)** — slippage tolerance prices adverse selection,
/// never producer health.
#[derive(Debug, Clone, Copy, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct Slippage {
    /// Basis points of adverse move (10_000 = 100%).
    pub bps: u32,
}

impl Slippage {
    /// Move `touch` adversely for the aggressor side: buys pay up, sells
    /// accept less.
    #[must_use]
    pub fn apply(&self, touch: Money, is_buy: bool) -> Money {
        let basis = Decimal::from(10_000);
        let rate = Decimal::from(self.bps) / basis;
        let moved = if is_buy {
            touch.as_decimal() * (Decimal::ONE + rate)
        } else {
            touch.as_decimal() * (Decimal::ONE - rate)
        };
        if moved.is_sign_negative() || moved.is_zero() {
            touch
        } else {
            Money::new(moved)
        }
    }
}

/// Per-contract commission schedule.
#[derive(Debug, Clone, Copy, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct FeeSchedule {
    /// Commission per contract (or share).
    pub per_contract: Money,
}

impl FeeSchedule {
    /// Commission for `contracts`, or `None` on arithmetic overflow.
    #[must_use]
    pub fn fee_for(&self, contracts: u32) -> Option<Money> {
        self.per_contract
            .as_decimal()
            .checked_mul(Decimal::from(contracts))
            .map(Money::new)
    }
}

#[cfg(test)]
#[path = "../tests/quote.rs"]
mod tests;
