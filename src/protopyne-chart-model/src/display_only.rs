//! Display-purity fence: `Decimal -> f64` values arrive wrapped.
//!
//! [`DisplayOnly`] marks floats that descended from exact `Decimal`s through
//! the display boundary ([`to_display_float`](crate::convert::to_display_float)).
//! The mark is a type, not a comment: the inner value has no `Deref`,
//! `Borrow`, `From`, or serde impl, so nothing exits silently. The only exit
//! is the explicitly named [`DisplayOnly::reveal`], reserved for
//! display-descended consumers (paint geometry, kernel preview frames) —
//! non-display crates must not name it, pinned by the terminal ledger scan.
//! Crossing into a decision denomination
//! ([`Money`](protopyne_types::Money)) goes through
//! [`DisplayOnly::try_into_money`], which refuses unconditionally: display
//! floats never become decisions, even when integral. The fence itself does
//! not serialize, so a wrapped value can never slip through a JSON boundary
//! unwrapped.

use protopyne_types::Money;

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
}

impl DisplayOnly<f64> {
    /// Refusing crossing into exact money: always `Err`, even for integral
    /// values. Display rounding must never mint a decision denomination.
    pub fn try_into_money(self) -> Result<Money, DisplayOnlyError> {
        Err(DisplayOnlyError::DecisionRefused)
    }
}

#[cfg(test)]
#[path = "../tests/display_only.rs"]
mod tests;
