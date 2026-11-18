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

    /// Finite `f64` reading, or `None` when unrepresentable or non-finite
    /// (M6.3: the one home for the strike conversions in `risk.rs` and
    /// `activity.rs`, which differ only in the positivity check the caller
    /// keeps).
    #[must_use]
    pub fn as_finite_f64(self) -> Option<f64> {
        use rust_decimal::prelude::ToPrimitive as _;
        self.0.to_f64().filter(|value| value.is_finite())
    }

    /// Lossy `f64` read for f64-native paths (equity marks, stop distances):
    /// retains the binary value; unrepresentable reads `ZERO` (fail-closed
    /// for sizing/curve math, never a panic). Risk bounds must use the
    /// string round-trip instead — exact, not retained (M3.2).
    #[must_use]
    pub fn from_f64_or_zero(value: f64) -> Self {
        Self(Decimal::from_f64_retain(value).unwrap_or(Decimal::ZERO))
    }

    /// The smaller of two amounts: a budget is a hard ceiling (M3.2: the
    /// one home for `grant.rs`'s former bespoke `cheaper`).
    #[must_use]
    pub fn min(self, other: Self) -> Self {
        Self(self.0.min(other.0))
    }

    /// The larger of two amounts.
    #[must_use]
    pub fn max(self, other: Self) -> Self {
        Self(self.0.max(other.0))
    }

    /// Saturating addition: same result as `+` absent overflow (overflow
    /// previously panicked inside `Decimal`; unreachable at market
    /// magnitudes — now saturates at `Decimal::MAX` instead).
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Subtraction floored at zero: money never goes negative, so
    /// `a < b` reads `ZERO` rather than a negative amount (M3.2: exact
    /// match for the ledger's former bespoke `sub`).
    #[must_use]
    pub fn saturating_sub(self, other: Self) -> Self {
        Self((self.0 - other.0).max(Decimal::ZERO))
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

/// Maximum fraction digits accepted before parsing a decimal.
pub const MAX_DECIMAL_SCALE: u32 = 28;

/// Shaped refusal from decimal-text scale validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecimalScaleError {
    /// Text contains no characters.
    #[error("empty decimal text")]
    Empty,
    /// Text contains only a sign.
    #[error("sign without digits")]
    SignWithoutDigits,
    /// Text contains a non-digit other than one decimal point.
    #[error("not an ASCII decimal number")]
    InvalidCharacter,
    /// Text contains no digits.
    #[error("no digits")]
    NoDigits,
    /// Text contains more than [`MAX_DECIMAL_SCALE`] fraction digits.
    #[error("decimal scale exceeds {MAX_DECIMAL_SCALE} places")]
    ScaleTooLarge(u32),
}

/// Count fraction digits in trimmed text, refusing excess scale before
/// `Decimal` can round it. Shape failures keep their own refusal; callers map
/// that refusal into their own error type.
pub fn parse_scale_guarded(text: &str) -> Result<u32, DecimalScaleError> {
    if text.is_empty() {
        return Err(DecimalScaleError::Empty);
    }
    let body = text
        .strip_prefix('+')
        .or_else(|| text.strip_prefix('-'))
        .unwrap_or(text);
    if body.is_empty() {
        return Err(DecimalScaleError::SignWithoutDigits);
    }
    let mut points = 0;
    let mut fraction: u32 = 0;
    let mut digits = 0;
    for ch in body.chars() {
        match ch {
            '0'..='9' => {
                digits += 1;
                if points > 0 {
                    fraction = fraction.saturating_add(1);
                }
            }
            '.' if points == 0 => points += 1,
            _ => return Err(DecimalScaleError::InvalidCharacter),
        }
    }
    if digits == 0 {
        return Err(DecimalScaleError::NoDigits);
    }
    if fraction > MAX_DECIMAL_SCALE {
        return Err(DecimalScaleError::ScaleTooLarge(fraction));
    }
    Ok(fraction)
}

impl FromStr for Money {
    type Err = TypesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let text = s.trim();
        // `Decimal` rounds excess scale instead of refusing it, so the scale
        // is checked on the text first: over-scale text refuses here exactly
        // as the operator-text fence refuses it, never rounding into a
        // decision.
        if matches!(
            parse_scale_guarded(text),
            Err(DecimalScaleError::ScaleTooLarge(_))
        ) {
            return Err(TypesError::InvalidMoney(format!(
                "invalid money value: {s}"
            )));
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
                Money::from_str(value)
                    .map_err(|_| E::custom(format!("invalid money value: {value}")))
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
        if matches!(
            parse_scale_guarded(text),
            Err(DecimalScaleError::ScaleTooLarge(_))
        ) {
            return Err(TypesError::InvalidQuantity(format!(
                "invalid quantity value: {s}"
            )));
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
                Qty::from_str(value)
                    .map_err(|_| E::custom(format!("invalid quantity value: {value}")))
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

#[cfg(test)]
#[path = "../tests/money.rs"]
mod tests;
