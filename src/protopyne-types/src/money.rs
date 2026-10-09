//! `Decimal`-backed money and quantity newtypes.
//!
//! All monetary values in the workspace use [`rust_decimal::Decimal`];
//! these newtypes keep currency amounts (`Money`) distinct from contract or
//! share counts (`Qty`) at the type level while sharing the representation.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::TypesError;

/// A monetary amount in account currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Money(pub Decimal);

impl Money {
    /// Wrap a raw decimal amount.
    #[must_use]
    pub fn new(amount: Decimal) -> Self {
        Self(amount)
    }

    /// Zero amount.
    #[must_use]
    pub fn zero() -> Self {
        Self(Decimal::ZERO)
    }

    /// Borrow the underlying decimal.
    #[must_use]
    pub fn as_decimal(self) -> Decimal {
        self.0
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Decimal> for Money {
    fn from(value: Decimal) -> Self {
        Self(value)
    }
}

impl From<i64> for Money {
    fn from(value: i64) -> Self {
        Self(Decimal::from(value))
    }
}

impl FromStr for Money {
    type Err = TypesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<Decimal>()
            .map(Self)
            .map_err(|_| TypesError::InvalidMoney(format!("invalid money value: {s}")))
    }
}

/// A share or contract count. Never mixed with [`Money`] by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Qty(pub Decimal);

impl Qty {
    /// Wrap a raw decimal count.
    #[must_use]
    pub fn new(count: Decimal) -> Self {
        Self(count)
    }

    /// Zero count.
    #[must_use]
    pub fn zero() -> Self {
        Self(Decimal::ZERO)
    }

    /// Borrow the underlying decimal.
    #[must_use]
    pub fn as_decimal(self) -> Decimal {
        self.0
    }
}

impl fmt::Display for Qty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Decimal> for Qty {
    fn from(value: Decimal) -> Self {
        Self(value)
    }
}

impl From<i64> for Qty {
    fn from(value: i64) -> Self {
        Self(Decimal::from(value))
    }
}

impl FromStr for Qty {
    type Err = TypesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<Decimal>()
            .map(Self)
            .map_err(|_| TypesError::InvalidQuantity(format!("invalid quantity value: {s}")))
    }
}
