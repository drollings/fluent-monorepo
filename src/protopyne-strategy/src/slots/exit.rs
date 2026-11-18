//! Exit intents: what a rule asks the runner to unwind, and at what price.
//!
//! A rule never submits. It returns [`ExitIntent`]s; the runner prices them,
//! submits through the shared decision body, and books the venue's fill back
//! into the position manager. This keeps one exit authority and one
//! submission authority, exactly as entries split the signal from the venue.
//!
//! The default [`ExitRule::review_exits`] maps the legacy
//! [`LossMgmt::review`] vocabulary onto whole-position market intents, so
//! every shipped loss element keeps its exact behaviour with no code change.
//!
//! [`LossExitRule`] is the canonical bridge from any [`LossMgmt`] to the
//! intent shape. The run-owned manager builds its intents directly from the
//! strategy dials rather than dispatching through this adapter, so it has no
//! production caller; it stays because it is the only in-tree exercise of the
//! default mapping (pinned by its suite) and the extension point for rules
//! that keep the legacy vocabulary.

use std::sync::Arc;

use protopyne_types::{MarkBasis, Money};

use crate::elements::{LossMgmt, MarkContext, PositionView};
use crate::tape::Tape;

/// How much of the position one intent unwinds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExitQuantity {
    /// The whole remaining position.
    All,
    /// A fraction of the remaining position, in `(0, 1]`.
    Fraction(f64),
    /// An explicit unit count.
    Units(u32),
}

/// Where the exit rests.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RestingPrice {
    /// Marketable at the touch (a signal exit).
    Market,
    /// A resting price (a stop or target).
    At(Money),
}

/// One requested unwind.
#[derive(Debug, Clone, PartialEq)]
pub struct ExitIntent {
    /// Machine-readable kind (`"stop"`, `"take-profit"`, `"time-stop"`,
    /// `"session-flatten"`, …).
    pub kind: String,
    /// Why the intent exists, for the trade and skip lines.
    pub reason: String,
    /// How much to unwind.
    pub quantity: ExitQuantity,
    /// Where to rest.
    pub price: RestingPrice,
}

impl ExitIntent {
    /// A whole-position market exit.
    #[must_use]
    pub fn close(kind: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            reason: reason.into(),
            quantity: ExitQuantity::All,
            price: RestingPrice::Market,
        }
    }

    /// A resting whole-position exit.
    #[must_use]
    pub fn resting(kind: impl Into<String>, reason: impl Into<String>, price: Money) -> Self {
        Self {
            kind: kind.into(),
            reason: reason.into(),
            quantity: ExitQuantity::All,
            price: RestingPrice::At(price),
        }
    }

    /// A partial market exit.
    #[must_use]
    pub fn partial_units(
        kind: impl Into<String>,
        reason: impl Into<String>,
        units: u32,
        price: RestingPrice,
    ) -> Self {
        Self {
            kind: kind.into(),
            reason: reason.into(),
            quantity: ExitQuantity::Units(units),
            price,
        }
    }
}

/// Everything an exit rule sees at one bar.
pub struct ExitContext<'a> {
    /// The bounded tape at this bar.
    pub tape: &'a Tape<'a>,
    /// The open position as the loss elements see it.
    pub position: PositionView<'a>,
    /// Per-unit mark (the bar close).
    pub mark: Money,
    /// Intrabar high.
    pub high: Money,
    /// Intrabar low.
    pub low: Money,
    /// Best favourable price seen since entry.
    pub best: Money,
    /// Bars held, entry to this bar inclusive.
    pub bars_held: u64,
    /// Whether the mark is beyond entry in the position's favour.
    pub in_profit: bool,
}

/// One exit authority.
pub trait ExitRule: Send + Sync {
    /// Rule name for attribution.
    fn name(&self) -> &str;

    /// The legacy loss rule this exit rule wraps, when it wraps one. The
    /// default [`review_exits`](Self::review_exits) reads it; a rule with
    /// its own quantity/price logic leaves it `None` and overrides the body.
    fn loss(&self) -> Option<&dyn LossMgmt> {
        None
    }

    /// Propose exits for this bar, in firing order. The default maps every
    /// legacy [`LossAction`](crate::elements::LossAction) to a whole-position
    /// market intent, preserving the installed loss set exactly.
    fn review_exits(&self, context: &ExitContext<'_>) -> Vec<ExitIntent> {
        let Some(loss) = self.loss() else {
            return Vec::new();
        };
        loss.review(
            &context.position,
            &MarkContext {
                basis: MarkBasis::FillPrice,
                mark: Some(context.mark),
            },
        )
        .into_iter()
        .map(|action| ExitIntent::close(action.kind, action.reason))
        .collect()
    }
}

/// The adapter that gives any [`LossMgmt`] the exit-intent shape.
#[derive(Clone)]
pub struct LossExitRule {
    inner: Arc<dyn LossMgmt>,
}

impl LossExitRule {
    /// Wrap one legacy loss rule.
    #[must_use]
    pub fn new(inner: Arc<dyn LossMgmt>) -> Self {
        Self { inner }
    }
}

impl ExitRule for LossExitRule {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn loss(&self) -> Option<&dyn LossMgmt> {
        Some(self.inner.as_ref())
    }
}

/// One monitor observation, kept beside the exit authority so a gate alert
/// never becomes an order by accident.
pub use crate::elements::Monitor;

#[cfg(test)]
#[path = "../../tests/exit.rs"]
mod tests;
