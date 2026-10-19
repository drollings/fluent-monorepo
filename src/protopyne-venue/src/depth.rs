//! Session-scoped depth book with dark detectors: structure, never gating.
//!
//! The prototype fills off top-of-book through [`LiquidityFilter`](crate::quote::LiquidityFilter)
//! and [`Slippage`](crate::quote::Slippage), unchanged. The ladders here
//! exist so detectors can verify market structure instead of trusting one
//! level. One [`DepthBook`] per symbol (pure value: `BTreeMap` bid/ask
//! levels plus a `sequence`), held by a [`BookManager`] (one book per
//! [`SymKey`](protopyne_types::SymKey)).
//!
//! Detectors run **dark**: [`is_crossed`] and [`is_stale`] are pure
//! functions returning reasons, and [`BookManager::scan`] collects them as
//! [`DarkFinding`] records for the journal/telemetry. Nothing here gates a
//! fill — wiring detectors into the fill path is explicitly out of scope
//! until the depth calibration suite is green at 1.0 on controls.
//!
//! Axis labels: [`is_crossed`] and sequence-gap refusal are
//! **(correctness)**; no depth-imbalance scoring is offered (had it been,
//! it would be **(task-value)** and must never feed a health gate);
//! nothing in this module is **(producer-confidence)**.
//!
//! **Library-only.** No production caller consumes a depth book today: the
//! manager and its dark detectors are exercised by tests and the calibration
//! suite only, and nothing wires them into the fill or health path.
//!
//! Mid/width math is never duplicated: top-of-book verdicts delegate to
//! [`Quote::spread_width`](crate::quote::Quote::spread_width) (and
//! [`Quote::mid`](crate::quote::Quote::mid) where a mid is needed), and
//! per-leg crossing stays on
//! [`ChainQuote::is_crossed`](protopyne_types::ChainQuote::is_crossed).

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use protopyne_types::{Money, SymKey, SymbolIndex, TypesError};

use crate::error::VenueError;
use crate::market::MarketEvent;
use crate::quote::Quote;

/// Default cap on levels per side: far above the prototype's needs while
/// keeping a misbehaving feed from growing a book without bound.
pub const DEFAULT_MAX_LEVELS: usize = 64;

/// One price level: a price and the size resting there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceLevel {
    /// Limit price at this level (must be positive).
    pub price: Money,
    /// Contracts (or shares) resting at this level.
    pub size: u64,
}

/// One incremental ladder update: level upserts (`size == 0` removes the
/// level) applied atomically at one sequence number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepthDelta {
    /// Bid upserts/removals.
    pub bids: Vec<PriceLevel>,
    /// Ask upserts/removals.
    pub asks: Vec<PriceLevel>,
    /// Feed sequence number; must be exactly one past the book's.
    pub sequence: u64,
    /// Feed timestamp carried with the update.
    pub ts: DateTime<Utc>,
}

/// Sorted two-sided ladder for one symbol. Levels live in `BTreeMap`s keyed
/// by price, so best-first order is structural (best bid is the greatest
/// key, best ask the least) rather than a caller promise. Every mutation
/// validates: stale or gapped sequences, non-positive prices, and
/// over-bound books refuse fail-closed, never by repairing caller data.
#[derive(Debug, Clone)]
pub struct DepthBook {
    underlying: String,
    key: SymKey,
    bids: BTreeMap<Decimal, u64>,
    asks: BTreeMap<Decimal, u64>,
    sequence: u64,
    ts: DateTime<Utc>,
    max_levels: usize,
}

impl DepthBook {
    /// Open an empty book for `underlying`, interning the spelling through
    /// `index`. Empty sides are a market fact (no touch), never "unknown":
    /// detectors stay silent on empties rather than guessing.
    pub fn new(underlying: String, index: &mut SymbolIndex) -> Result<Self, TypesError> {
        let key = index.intern(&underlying)?;
        Ok(Self {
            underlying,
            key,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            sequence: 0,
            ts: Utc::now(),
            max_levels: DEFAULT_MAX_LEVELS,
        })
    }

    /// Cap levels per side (bounds the book; see [`Validate`](protopyne_types::Validate)).
    #[must_use]
    pub fn with_max_levels(mut self, max_levels: usize) -> Self {
        self.max_levels = max_levels;
        self
    }

    /// Canonical spelling (serialization-safe; keys never serialize — this
    /// type deliberately has no `Serialize` impl).
    #[must_use]
    pub fn underlying(&self) -> &str {
        &self.underlying
    }

    /// Indexed key for hot-path comparisons.
    #[must_use]
    pub fn key(&self) -> SymKey {
        self.key
    }

    /// Re-intern through an index (idempotent: same spelling, same key).
    pub fn symbol_key(&self, index: &mut SymbolIndex) -> Result<SymKey, TypesError> {
        index.intern(&self.underlying)
    }

    /// Last applied sequence number.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Level cap per side.
    #[must_use]
    pub fn max_levels(&self) -> usize {
        self.max_levels
    }

    /// Level counts per side (bids, asks).
    #[must_use]
    pub fn level_counts(&self) -> (usize, usize) {
        (self.bids.len(), self.asks.len())
    }

    /// Replace both ladders wholesale at `sequence`: the snapshot path.
    /// Refuses stale sequences, duplicate prices within one snapshot,
    /// non-positive prices, and over-bound books.
    pub fn snapshot_replace(
        &mut self,
        bids: &[PriceLevel],
        asks: &[PriceLevel],
        sequence: u64,
        ts: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        refuse_stale(&self.underlying, self.sequence, sequence)?;
        let bids = collect_side(bids, &self.underlying, "bid", self.max_levels)?;
        let asks = collect_side(asks, &self.underlying, "ask", self.max_levels)?;
        self.bids = bids;
        self.asks = asks;
        self.sequence = sequence;
        self.ts = ts;
        Ok(())
    }

    /// Apply one incremental [`DepthDelta`]: the delta sequence must be
    /// exactly one past the book's — a gap means the book missed a feed
    /// update and must re-snapshot rather than limp on. Each entry upserts
    /// its price (`size == 0` removes the level); the whole delta applies
    /// atomically, so a refusal commits nothing.
    pub fn apply_delta(&mut self, delta: &DepthDelta) -> Result<(), VenueError> {
        if delta.sequence != self.sequence + 1 {
            return Err(VenueError::Invalid(format!(
                "sequence gap for {}: book at {}, delta at {}",
                self.underlying, self.sequence, delta.sequence
            )));
        }
        let mut bids = self.bids.clone();
        let mut asks = self.asks.clone();
        for level in &delta.bids {
            upsert(&mut bids, level, &self.underlying, "bid")?;
        }
        for level in &delta.asks {
            upsert(&mut asks, level, &self.underlying, "ask")?;
        }
        if bids.len() > self.max_levels || asks.len() > self.max_levels {
            return Err(VenueError::Invalid(format!(
                "book for {} exceeds {} levels per side after delta {}",
                self.underlying, self.max_levels, delta.sequence
            )));
        }
        self.bids = bids;
        self.asks = asks;
        self.sequence = delta.sequence;
        self.ts = delta.ts;
        Ok(())
    }

    /// Top of book as a [`Quote`]: `None` when either side is empty (a book
    /// with no touch invents none). This is the hot accessor detectors
    /// share with any future fill-path use — mid/width math comes from
    /// [`Quote`], never from a second implementation here.
    #[must_use]
    pub fn top(&self) -> Option<Quote> {
        let (bid_price, bid_size) = self.bids.iter().next_back()?;
        let (ask_price, ask_size) = self.asks.iter().next()?;
        Some(
            Quote::new()
                .bid(Money::new(*bid_price))
                .ask(Money::new(*ask_price))
                .bid_size(*bid_size)
                .ask_size(*ask_size)
                .ts(self.ts)
                .build(),
        )
    }
}

fn refuse_stale(underlying: &str, current: u64, next: u64) -> Result<(), VenueError> {
    if next < current {
        return Err(VenueError::Invalid(format!(
            "stale book update for {underlying}: sequence {next} behind {current}"
        )));
    }
    Ok(())
}

fn collect_side(
    levels: &[PriceLevel],
    underlying: &str,
    side: &str,
    max_levels: usize,
) -> Result<BTreeMap<Decimal, u64>, VenueError> {
    if levels.len() > max_levels {
        return Err(VenueError::Invalid(format!(
            "book for {underlying} exceeds {max_levels} {side} levels (got {})",
            levels.len()
        )));
    }
    let mut map = BTreeMap::new();
    for level in levels {
        let price = level.price.as_decimal();
        if price <= Decimal::ZERO {
            return Err(VenueError::Invalid(format!(
                "book for {underlying}: {side} level is not positive"
            )));
        }
        if map.insert(price, level.size).is_some() {
            return Err(VenueError::Invalid(format!(
                "book for {underlying}: duplicate {side} price {price} in one snapshot"
            )));
        }
    }
    Ok(map)
}

fn upsert(
    side: &mut BTreeMap<Decimal, u64>,
    level: &PriceLevel,
    underlying: &str,
    side_name: &str,
) -> Result<(), VenueError> {
    let price = level.price.as_decimal();
    if price <= Decimal::ZERO {
        return Err(VenueError::Invalid(format!(
            "book for {underlying}: {side_name} level is not positive"
        )));
    }
    if level.size == 0 {
        side.remove(&price);
    } else {
        side.insert(price, level.size);
    }
    Ok(())
}

impl protopyne_types::Validate for DepthBook {
    type Err = String;
    fn validate(&self) -> Result<(), Self::Err> {
        if self.max_levels < 1 {
            return Err("max_levels must be at least 1".to_owned());
        }
        if self.bids.len() > self.max_levels || self.asks.len() > self.max_levels {
            return Err(format!(
                "book for {} exceeds {} levels per side",
                self.underlying, self.max_levels
            ));
        }
        Ok(())
    }
}

/// Whether the book's top is crossed or locked (`bid >= ask`):
/// **(correctness)** — a crossed top has no valid mid. Pure function
/// returning the reason; `None` means clean. An empty side is not a cross
/// (it is dark, owned by no detector here), so empties pass silent.
/// Width math delegates to [`Quote::spread_width`]: `None` there is the
/// cross verdict here.
#[must_use]
pub fn is_crossed(book: &DepthBook) -> Option<String> {
    let top = book.top()?;
    if top.spread_width().is_none() {
        Some(format!(
            "crossed book for {}: bid {} at or above ask {}",
            book.underlying(),
            top.bid,
            top.ask
        ))
    } else {
        None
    }
}

/// Whether the book is older than `max_age_secs` at `now`:
/// **(correctness)** — an old book describes a dead market. Pure function
/// returning the reason; `None` means fresh.
#[must_use]
pub fn is_stale(book: &DepthBook, now: DateTime<Utc>, max_age_secs: u64) -> Option<String> {
    let age = now.signed_duration_since(book.ts).num_seconds().max(0) as u64;
    if age > max_age_secs {
        Some(format!(
            "stale book for {}: age {age}s above maximum {max_age_secs}s",
            book.underlying()
        ))
    } else {
        None
    }
}

/// One dark-detector trip, recorded for the journal/telemetry. The book
/// keeps trading without it: findings never gate fills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DarkFinding {
    /// Canonical symbol spelling (strings serialize; keys never do).
    pub symbol: String,
    /// Detector reason (`crossed …` / `stale …`).
    pub reason: String,
    /// Book sequence the finding was taken at.
    pub sequence: u64,
}

/// One book per [`SymKey`](protopyne_types::SymKey) behind a session-owned
/// [`SymbolIndex`](protopyne_types::SymbolIndex): the only depth-book home,
/// never a second instrument index, never global state. Books open on
/// snapshot, never on a mid-stream delta: a [`MarketEvent::BookDelta`] for
/// an unknown symbol refuses (the feed must snapshot first), and heartbeats
/// advance the marker only when consecutive — a gap refuses so the book
/// re-snapshots rather than limping on.
#[derive(Debug)]
pub struct BookManager {
    books: HashMap<SymKey, DepthBook>,
    index: SymbolIndex,
}

impl BookManager {
    /// Open an empty manager with its own scoped index.
    #[must_use]
    pub fn new() -> Self {
        Self {
            books: HashMap::new(),
            index: SymbolIndex::new(),
        }
    }

    /// Borrow a book by spelling. Unknown spellings are absence (`None`),
    /// never an implicit insert — reads must not grow the table.
    #[must_use]
    pub fn book(&self, symbol: &str) -> Option<&DepthBook> {
        let key = self.index.lookup(symbol)?;
        self.books.get(&key)
    }

    /// Open-or-replace one book by full snapshot. This is the bootstrap
    /// path: unknown symbols open here, known symbols re-snapshot.
    pub fn apply_snapshot(
        &mut self,
        symbol: &str,
        bids: &[PriceLevel],
        asks: &[PriceLevel],
        sequence: u64,
        ts: DateTime<Utc>,
    ) -> Result<SymKey, VenueError> {
        let key = self
            .index
            .intern(symbol)
            .map_err(|e| VenueError::Invalid(e.to_string()))?;
        if let Some(book) = self.books.get_mut(&key) {
            book.snapshot_replace(bids, asks, sequence, ts)?;
        } else {
            let mut book = DepthBook::new(symbol.to_owned(), &mut self.index)
                .map_err(|e| VenueError::Invalid(e.to_string()))?;
            debug_assert_eq!(book.key(), key, "same index interns one key");
            book.snapshot_replace(bids, asks, sequence, ts)?;
            self.books.insert(key, book);
        }
        Ok(key)
    }

    /// Fold one [`MarketEvent`] into the managed books: `BookDelta`
    /// heartbeats advance a known book's marker when consecutive (gap or
    /// unknown symbol refuses); every other kind refuses — callers route by
    /// kind first, and a misrouted event is a caller bug, never a silent
    /// skip.
    pub fn apply_event(&mut self, event: &MarketEvent) -> Result<(), VenueError> {
        let (underlying, sequence) = match event {
            MarketEvent::BookDelta {
                underlying,
                sequence,
            } => (underlying, *sequence),
            _ => {
                return Err(VenueError::Invalid(
                    "book manager advances on book deltas only".to_owned(),
                ));
            }
        };
        let key = self.index.lookup(underlying).ok_or_else(|| {
            VenueError::Invalid(format!(
                "no book for {underlying}: open with a snapshot first"
            ))
        })?;
        let book = self.books.get_mut(&key).ok_or_else(|| {
            VenueError::Invalid(format!(
                "no book for {underlying}: open with a snapshot first"
            ))
        })?;
        if sequence != book.sequence + 1 {
            return Err(VenueError::Invalid(format!(
                "sequence gap for {underlying}: book at {}, delta at {sequence}",
                book.sequence
            )));
        }
        book.sequence = sequence;
        Ok(())
    }

    /// Record-only detector sweep over every managed book: collects
    /// [`DarkFinding`]s (crossed tops, stale books) sorted by symbol for
    /// stable journaling. Sweeping changes nothing — books and fills are
    /// untouched, so this is safe to call on any cadence.
    #[must_use]
    pub fn scan(&self, now: DateTime<Utc>, max_age_secs: u64) -> Vec<DarkFinding> {
        let mut findings = Vec::new();
        for book in self.books.values() {
            if let Some(reason) = is_crossed(book) {
                findings.push(DarkFinding {
                    symbol: book.underlying().to_owned(),
                    reason,
                    sequence: book.sequence(),
                });
            }
            if let Some(reason) = is_stale(book, now, max_age_secs) {
                findings.push(DarkFinding {
                    symbol: book.underlying().to_owned(),
                    reason,
                    sequence: book.sequence(),
                });
            }
        }
        findings.sort_by(|left, right| {
            left.symbol
                .cmp(&right.symbol)
                .then_with(|| left.reason.cmp(&right.reason))
        });
        findings
    }

    /// Count of managed books.
    #[must_use]
    pub fn len(&self) -> usize {
        self.books.len()
    }

    /// Whether any book is managed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.books.is_empty()
    }
}

impl Default for BookManager {
    fn default() -> Self {
        Self::new()
    }
}

impl protopyne_types::Validate for BookManager {
    type Err = String;
    fn validate(&self) -> Result<(), Self::Err> {
        protopyne_types::Validate::validate(&self.index)
            .map_err(|e| format!("book manager index invalid: {e}"))?;
        for book in self.books.values() {
            protopyne_types::Validate::validate(book)
                .map_err(|e| format!("book manager book invalid: {e}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/depth.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/book_calibration.rs"]
mod book_calibration_tests;
