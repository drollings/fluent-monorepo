//! `Decimal`-backed money and quantity newtypes.
//!
//! All monetary values in the workspace use [`rust_decimal::Decimal`];
//! these newtypes keep currency amounts (`Money`) distinct from contract or
//! share counts (`Qty`) at the type level while sharing the representation.

use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::TypesError;

/// A monetary amount in account currency.
///
/// Serializes through `Decimal`'s own shape (unchanged, so manifests stay
/// byte-identical); deserializes strictly — strings route through the
/// scale-guarded [`FromStr`](Money), integers convert exactly, and floats
/// refuse, so no serde path rounds excess precision into a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
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

/// Fraction digits in decimal text, ignoring a single leading sign.
/// `None` when the text is not plain ASCII digits with at most one point —
/// callers that need strict shape validation keep their own refusal, this
/// only answers the scale question so excess precision refuses instead of
/// rounding into a decision.
fn fraction_digits(text: &str) -> Option<u32> {
    let body = text.strip_prefix('+').or_else(|| text.strip_prefix('-')).unwrap_or(text);
    let mut points = 0;
    let mut fraction: u32 = 0;
    let mut digits = 0;
    for ch in body.chars() {
        match ch {
            '0'..='9' => {
                digits += 1;
                if points > 0 {
                    fraction += 1;
                }
            }
            '.' if points == 0 => points += 1,
            _ => return None,
        }
    }
    if digits == 0 { None } else { Some(fraction) }
}

impl FromStr for Money {
    type Err = TypesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let text = s.trim();
        // `Decimal` rounds excess scale instead of refusing it, so the scale
        // is checked on the text first: a 29-place decimal refuses here
        // exactly as the operator-text fence refuses it, never rounding into
        // a decision.
        if fraction_digits(text).is_some_and(|scale| scale > 28) {
            return Err(TypesError::InvalidMoney(format!("invalid money value: {s}")));
        }
        text.parse::<Decimal>()
            .map(Self)
            .map_err(|_| TypesError::InvalidMoney(format!("invalid money value: {s}")))
    }
}

/// A share or contract count. Never mixed with [`Money`] by construction.
///
/// Same strict-deserialization contract as [`Money`]: serialization is
/// `Decimal`'s own shape, deserialization never rounds into a count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
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

impl<'de> Deserialize<'de> for Money {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MoneyVisitor;
        impl serde::de::Visitor<'_> for MoneyVisitor {
            type Value = Money;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("exact decimal money as a string, or an integer")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Money, E> {
                Money::from_str(value).map_err(|_| E::custom(format!("invalid money value: {value}")))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Money, E> {
                self.visit_str(&value)
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Money, E> {
                Ok(Money(Decimal::from(value)))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Money, E> {
                Ok(Money(Decimal::from(value)))
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Money, E> {
                Err(E::custom(format!("invalid money value: {value}")))
            }
        }
        deserializer.deserialize_any(MoneyVisitor)
    }
}

impl FromStr for Qty {
    type Err = TypesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let text = s.trim();
        // Same scale guard as money: excess precision refuses rather than
        // rounding into a count.
        if fraction_digits(text).is_some_and(|scale| scale > 28) {
            return Err(TypesError::InvalidQuantity(format!("invalid quantity value: {s}")));
        }
        text.parse::<Decimal>()
            .map(Self)
            .map_err(|_| TypesError::InvalidQuantity(format!("invalid quantity value: {s}")))
    }
}

impl<'de> Deserialize<'de> for Qty {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct QtyVisitor;
        impl serde::de::Visitor<'_> for QtyVisitor {
            type Value = Qty;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("exact decimal quantity as a string, or an integer")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Qty, E> {
                Qty::from_str(value).map_err(|_| E::custom(format!("invalid quantity value: {value}")))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Qty, E> {
                self.visit_str(&value)
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Qty, E> {
                Ok(Qty(Decimal::from(value)))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Qty, E> {
                Ok(Qty(Decimal::from(value)))
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Qty, E> {
                Err(E::custom(format!("invalid quantity value: {value}")))
            }
        }
        deserializer.deserialize_any(QtyVisitor)
    }
}
