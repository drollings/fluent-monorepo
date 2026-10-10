//! Normalized market subscription model: one event enum over the existing
//! vocabulary, one pump, one fan-out.
//!
//! The session used to move only top-of-book [`Quote`]s (plus a separate
//! bar channel); every new feed kind would have grown its own spawn loop
//! and its own fan-out. [`MarketEvent`] is the single subscription surface
//! instead: four additive variants reusing the existing payload vocabulary
//! ([`Quote`], [`Fill`], [`Bar`] — no new price/fill/candle types), one
//! [`Scope`](fluent_concurrency::scope::Scope)-owned pump per session, one
//! `broadcast` fan-out, per-receiver filtering by symbol and kind.
//!
//! Note on placement: the roadmap sketches this enum in `protopyne-types`,
//! but [`Quote`] and [`Fill`] live in this crate and the workspace layers
//! one way (`types → … → venue`), so the enum lives here where its payloads
//! live. No payload is duplicated and no dependency inverts.
//!
//! Axis labels: subscription plumbing is pure infrastructure — no variant
//! here is a **(correctness)**, **(task-value)**, or
//! **(producer-confidence)** signal. Detectors and gates read these events;
//! the events themselves judge nothing.

use serde::{Deserialize, Serialize};

use protopyne_types::{Bar, SymKey, SymbolIndex, TypesError};

use crate::order::Fill;
use crate::quote::Quote;

/// Subscription kind of one [`MarketEvent`]: what the receiver asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MarketEventKind {
    /// Top-of-book quote.
    Quote,
    /// One executed trade print.
    Trade,
    /// One order-book delta (sequence carrier until the depth book lands).
    BookDelta,
    /// One closed bar.
    Candle,
}

/// One market fact for one symbol, published by the session pump and
/// filtered per receiver. Payloads are the existing venue/types vocabulary;
/// the enum adds only the (symbol, kind) routing header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MarketEvent {
    /// Newest top-of-book for `underlying`.
    Quote {
        /// Symbol this quote is for.
        underlying: String,
        /// The quote itself.
        quote: Quote,
    },
    /// One executed trade print for `underlying`.
    Trade {
        /// Symbol this print is for.
        underlying: String,
        /// The print itself.
        fill: Fill,
    },
    /// One order-book delta marker for `underlying`. Carries the sequence
    /// number only; full depth application arrives with the depth book and
    /// must not be read as book state today.
    BookDelta {
        /// Symbol this delta is for.
        underlying: String,
        /// Monotonic sequence number from the feed.
        sequence: u64,
    },
    /// One closed bar for `underlying`.
    Candle {
        /// Symbol this bar is for.
        underlying: String,
        /// The closed bar itself.
        bar: Bar,
    },
}

impl MarketEvent {
    /// Symbol this event is for.
    #[must_use]
    pub fn symbol(&self) -> &str {
        match self {
            Self::Quote { underlying, .. }
            | Self::Trade { underlying, .. }
            | Self::BookDelta { underlying, .. }
            | Self::Candle { underlying, .. } => underlying,
        }
    }

    /// Subscription kind of this event.
    #[must_use]
    pub fn kind(&self) -> MarketEventKind {
        match self {
            Self::Quote { .. } => MarketEventKind::Quote,
            Self::Trade { .. } => MarketEventKind::Trade,
            Self::BookDelta { .. } => MarketEventKind::BookDelta,
            Self::Candle { .. } => MarketEventKind::Candle,
        }
    }

    /// Per-receiver filter: `None` on either axis accepts everything on
    /// that axis. Receivers subscribe to the shared fan-out and keep only
    /// what matches — the pump never filters per subscriber.
    #[must_use]
    pub fn matches(&self, symbol: Option<&str>, kind: Option<MarketEventKind>) -> bool {
        if let Some(want) = symbol {
            if self.symbol() != want {
                return false;
            }
        }
        if let Some(want) = kind {
            if self.kind() != want {
                return false;
            }
        }
        true
    }

    /// Key this event's symbol through a scoped index: the receiver-side
    /// bridge to the indexed world, so symbol comparison on the consume
    /// path is a `Copy`-key comparison after one interning per symbol.
    pub fn symbol_key(&self, index: &mut SymbolIndex) -> Result<SymKey, TypesError> {
        index.intern(self.symbol())
    }
}

#[cfg(test)]
#[path = "../tests/market.rs"]
mod tests;
