//! Display-purity fence: `Decimal -> f64` values arrive wrapped.
//!
//! [`DisplayOnly`] marks floats that descended from exact `Decimal`s through
//! the display boundary ([`to_display_float`](crate::convert::to_display_float)).
//! The mark is a type, not a comment: the inner value has no `Deref`,
//! `Borrow`, `From`, or serde impl, so nothing exits silently. The only exit
//! for a float is the explicitly named [`DisplayOnly::reveal`], reserved for
//! display-descended consumers (paint geometry, kernel preview frames) —
//! non-display crates must not name it, pinned by the terminal ledger scan.
//! Crossing into a decision denomination
//! ([`Money`](protopyne_types::Money)) goes through
//! [`DisplayOnly::try_into_money`]: unconditional refusal for floats (a float
//! already lost exactness, so no validation can recover it), and a strict
//! validated path for operator-typed decimal text. The fence itself does
//! not serialize, so a wrapped value can never slip through a JSON boundary
//! unwrapped.

use std::convert::Infallible;
use std::str::FromStr;

use protopyne_types::{DecimalScaleError, Money, parse_scale_guarded};
use rust_decimal::Decimal;

/// Fenced display float: provenance for values that came from `Decimal`.
///
/// Construct at the display boundary with [`new`](Self::new); the value
/// travels wrapped through display code and exits only via
/// [`reveal`](Self::reveal). There is deliberately no `Deref`, `Borrow`,
/// `AsRef`, `From`, or serde impl — any new exit must justify itself here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayOnly<T>(T);

impl<T> DisplayOnly<T> {
    /// Wrap a display-descended value at the conversion boundary.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The single named exit: reveal the float for a display-descended
    /// consumer. Non-display crates must not call this — the terminal
    /// ledger scan fails any naming outside the paint crates and the
    /// fence's home crate.
    #[must_use]
    pub fn reveal(self) -> T {
        self.0
    }
}

/// Refusal for display-to-decision crossings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DisplayOnlyError {
    /// A display float was asked to become a decision denomination.
    /// Always returned; the `Result` shape keeps the refusal explicit at
    /// the call site instead of a missing impl. If a legitimate validated
    /// path ever exists, it lands here with an audit trail — not beside it.
    #[error("display floats never convert into decisions")]
    DecisionRefused,
    /// Operator-typed decimal text that denotes no exact money amount.
    /// The reason names the failure class; the offending text itself never
    /// rides the error, so a refusal cannot leak an operator's draft into a
    /// log it was never meant for.
    #[error("operator decimal is not exact money: {0}")]
    InvalidDecimal(&'static str),
}

impl DisplayOnly<f64> {
    /// Refusing crossing into exact money: always `Err`, even for integral
    /// values. Display rounding must never mint a decision denomination. A
    /// float already lost exactness at the boundary, so no validation can
    /// recover it — operator text enters through
    /// [`DisplayOnly<String>`](DisplayOnly) instead.
    pub fn try_into_money(self) -> Result<Money, DisplayOnlyError> {
        Err(DisplayOnlyError::DecisionRefused)
    }
}

impl FromStr for DisplayOnly<String> {
    type Err = Infallible;

    /// Fence entry for operator-typed text: infallible by design, so every
    /// refusal lives in exactly one place — [`try_into_money`](Self::try_into_money).
    /// The boundary rule holds: outside text enters as a string, and only
    /// the validated exit turns it into a decision.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Ok(Self::new(text.to_owned()))
    }
}

impl DisplayOnly<String> {
    /// Validated crossing from operator-typed decimal text into exact money.
    ///
    /// Axis: task-value/correctness, absolutely — a decimal either denotes
    /// an exact money amount or it does not. This is not a tuning knob:
    /// there is no tolerance, no rounding mode, no locale. The text is
    /// trimmed, then held to ASCII digits with at most one point and one
    /// leading sign, then parsed exactly; anything else refuses with its
    /// reason. Negativity is accepted here — whether a field requires
    /// positive is the field's context (a tier bound, a tightening
    /// direction), never money's. Representability is `Decimal`'s maximum
    /// scale: the fractional digit count is checked on the text before
    /// parsing, because the parse itself rounds excess places instead of
    /// refusing them.
    pub fn try_into_money(self) -> Result<Money, DisplayOnlyError> {
        let text = self.0.trim();
        parse_scale_guarded(text).map_err(|refusal| match refusal {
            DecimalScaleError::Empty => DisplayOnlyError::InvalidDecimal("empty decimal text"),
            DecimalScaleError::SignWithoutDigits => {
                DisplayOnlyError::InvalidDecimal("sign without digits")
            }
            DecimalScaleError::InvalidCharacter => {
                DisplayOnlyError::InvalidDecimal("not an ASCII decimal number")
            }
            DecimalScaleError::NoDigits => DisplayOnlyError::InvalidDecimal("no digits"),
            DecimalScaleError::ScaleTooLarge(_) => {
                DisplayOnlyError::InvalidDecimal("decimal scale exceeds 28 places")
            }
        })?;
        let amount: Decimal =
            text.parse().map_err(|_| DisplayOnlyError::InvalidDecimal("decimal unrepresentable"))?;
        Ok(Money::new(amount))
    }
}

#[cfg(test)]
#[path = "../tests/display_only.rs"]
mod tests;
