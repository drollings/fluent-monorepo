//! Portfolio-level PnL aggregation over attributed fills.
//!
//! The roll-up the per-position engine cannot do alone: [`BookedFill`]s
//! (journaled fills plus their portfolio/underlying/structure/strategy/grant
//! attribution) fold per attributed book through the venue position engine
//! ([`update_from_fill`](protopyne_venue::update_from_fill)) — no new PnL
//! math anywhere here — and a per-mark valuation fold prices the same fills
//! into unrealized curves. Stats reuse the server metric helpers
//! (`metrics::sharpe/sortino/calmar/win_rate/profit_factor`) over Decimal
//! inputs; greeks stay explicitly unavailable until chain history exists.
//!
//! Conventions: fills sort by `(at, trade_id)` so input order never moves a
//! number; money stays [`Money`] end to end (display `f64` appears only in
//! the `Option<f64>` stat fields, converted exactly like
//! [`build_report`](crate::report::build_report) does); an empty book or a
//! stat with no evidence reports absence (`None`, zero quantities), never an
//! invented number.

use bon::Builder;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;
use std::collections::BTreeMap;

use protopyne_types::Money;
use protopyne_venue::{
    calculate_pnl_unrealised, update_from_fill, OrderSide, PositionFill, PositionSide,
    SpreadPosition,
};

use crate::error::ServerError;
use crate::metrics;

/// Run-level portfolio label for fills executed on the paper book — backtest
/// and paper preview alike. The venue mints no finer book identity, so every
/// run-owned fill lands here; multi-portfolio runs will parameterize the
/// label instead of reusing this.
pub const PAPER_PORTFOLIO: &str = "paper";

/// One fill with its attribution: which portfolio book, instrument,
/// structure, strategy, and grant produced it. The fill fields feed the venue
/// position engine unchanged; the attribution fields only group.
#[derive(Debug, Clone, Builder, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[builder(start_fn = new)]
pub struct BookedFill {
    /// Portfolio book this fill belongs to (run-level label).
    pub portfolio: String,
    /// Instrument this fill belongs to.
    pub underlying: String,
    /// Structure label (proposal structure at decide time).
    pub structure: String,
    /// Strategy that proposed the order.
    pub strategy: String,
    /// Grant that authorized the order.
    pub grant: String,
    /// Fill direction.
    pub side: OrderSide,
    /// Whole contracts filled; must be positive.
    pub quantity: u32,
    /// Execution price per contract.
    pub price: Money,
    /// Commission on this fill.
    pub fee: Money,
    /// Venue-side trade identifier for audit (and fold tie-break).
    pub trade_id: String,
    /// Execution time.
    pub at: DateTime<Utc>,
}

/// Grouping key: one attributed book. Plain strings — these are names that
/// already appear in JSON (orders, grants, configs), never opaque keys.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct BucketKey {
    /// Portfolio book.
    pub portfolio: String,
    /// Instrument.
    pub underlying: String,
    /// Structure label.
    pub structure: String,
    /// Strategy.
    pub strategy: String,
    /// Grant.
    pub grant: String,
}

impl BookedFill {
    /// Grouping key for this fill's book.
    #[must_use]
    pub fn key(&self) -> BucketKey {
        BucketKey {
            portfolio: self.portfolio.clone(),
            underlying: self.underlying.clone(),
            structure: self.structure.clone(),
            strategy: self.strategy.clone(),
            grant: self.grant.clone(),
        }
    }
}

/// One attributed book's closed-plus-open state.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BucketSummary {
    /// Which book this summarizes.
    pub key: BucketKey,
    /// Fills folded into this book.
    pub fills: u64,
    /// Positions fully closed (exact closes and flips).
    pub closed_trades: u64,
    /// Cumulative fee-inclusive realized PnL.
    pub pnl_realised: Money,
    /// Unrealized PnL at the last mark (or the last fill price unmarked).
    pub pnl_unrealised: Money,
    /// Cumulative entry plus exit fees.
    pub fees: Money,
    /// Open absolute quantity (0 when flat).
    pub open_quantity: u32,
    /// Open side (`None` when flat).
    pub open_side: Option<PositionSide>,
}

/// One portfolio's rolled-up economics plus its attribution stats.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PortfolioTotals {
    /// Portfolio book.
    pub portfolio: String,
    /// Fills folded across its books.
    pub fills: u64,
    /// Positions fully closed across its books.
    pub closed_trades: u64,
    /// Fee-inclusive realized PnL across its books.
    pub pnl_realised: Money,
    /// Unrealized PnL across its books at the last mark.
    pub pnl_unrealised: Money,
    /// Entry plus exit fees across its books.
    pub fees: Money,
    /// Sum of absolute open quantities across its books.
    pub open_quantity: u32,
    /// Winning closes over decisive closes (`None` before the first close).
    pub win_rate: Option<f64>,
    /// Gross close gains over gross close losses (`None` with no losses).
    pub profit_factor: Option<f64>,
    /// Annualized Sharpe of the per-mark MTM equity curve (`None` flat).
    pub sharpe: Option<f64>,
    /// Annualized Sortino of the same curve (`None` with no shortfall).
    pub sortino: Option<f64>,
    /// Annualized Calmar of the same curve (`None` with no drawdown).
    pub calmar: Option<f64>,
    /// Per-book detail, sorted by key.
    pub buckets: Vec<BucketSummary>,
}

/// Portfolio aggregation output: per-portfolio totals plus the per-mark
/// unrealized curve across all books (in mark order).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PortfolioReport {
    /// Per-portfolio totals, sorted by portfolio name.
    pub portfolios: Vec<PortfolioTotals>,
    /// Per-mark mark-to-market unrealized across all books, in mark order.
    pub unrealized_curve: Vec<Money>,
    /// Why greeks-over-time is unavailable in the prototype.
    pub greeks_note: String,
}

/// One attributed book's fold state: the open engine position plus settled
/// lifetime totals. A close settles its whole position lifetime at once
/// (the exited record carries the lifetime's cumulative realized and fees,
/// partials included), so settled sums plus the open position's running
/// numbers always equal the book — never double-counted, never lost.
struct BookState {
    position: Option<SpreadPosition>,
    fills: u64,
    closed_trades: u64,
    closed_realised: Vec<Decimal>,
    settled_realised: Decimal,
    settled_fees: Decimal,
}

impl BookState {
    fn new() -> Self {
        Self {
            position: None,
            fills: 0,
            closed_trades: 0,
            closed_realised: Vec::new(),
            settled_realised: Decimal::ZERO,
            settled_fees: Decimal::ZERO,
        }
    }

    /// Fold one fill through the venue engine, settling lifetimes on close.
    fn apply(&mut self, fill: &BookedFill) -> Result<(), ServerError> {
        let engine_fill = PositionFill::new()
            .underlying(fill.underlying.clone())
            .side(fill.side)
            .quantity(fill.quantity)
            .price(fill.price)
            .fee(fill.fee)
            .trade_id(fill.trade_id.clone())
            .at(fill.at)
            .build();
        let (position, exited) = update_from_fill(self.position.take(), &engine_fill)
            .map_err(|refused| ServerError::Backtest(refused.to_string()))?;
        self.position = position;
        self.fills += 1;
        if let Some(closed) = exited {
            self.closed_trades += 1;
            self.closed_realised.push(closed.pnl_realised.as_decimal());
            self.settled_realised += closed.pnl_realised.as_decimal();
            self.settled_fees += closed.fees_enter.as_decimal() + closed.fees_exit.as_decimal();
        }
        Ok(())
    }

    /// Running realized: settled lifetimes plus the open position's book.
    fn realised(&self) -> Decimal {
        self.settled_realised
            + self
                .position
                .as_ref()
                .map_or(Decimal::ZERO, |open| open.pnl_realised.as_decimal())
    }

    /// Running fees: settled lifetimes plus the open position's book.
    fn fees(&self) -> Decimal {
        self.settled_fees
            + self.position.as_ref().map_or(Decimal::ZERO, |open| {
                open.fees_enter.as_decimal() + open.fees_exit.as_decimal()
            })
    }

    /// Unrealized at `mark` when open and marked, else the engine's own
    /// last-fill refresh (or zero flat). A closed book holds no exposure,
    /// so its unrealized is always zero — never a stale print.
    fn unrealised(&self, mark: Option<Money>) -> Decimal {
        match (&self.position, mark) {
            (Some(open), Some(price)) => calculate_pnl_unrealised(
                open.side,
                open.price_entry_average,
                open.quantity_abs,
                open.quantity_abs_max,
                open.fees_enter,
                price,
            )
            .as_decimal(),
            (Some(open), None) => open.pnl_unrealised.as_decimal(),
            (None, _) => Decimal::ZERO,
        }
    }

    /// Point-in-time summary: unrealized re-marks when a price is given.
    fn snapshot(&self, key: &BucketKey, mark: Option<Money>) -> BucketSummary {
        BucketSummary {
            key: key.clone(),
            fills: self.fills,
            closed_trades: self.closed_trades,
            pnl_realised: Money::new(self.realised()),
            pnl_unrealised: Money::new(self.unrealised(mark)),
            fees: Money::new(self.fees()),
            open_quantity: self.position.as_ref().map_or(0, |open| open.quantity_abs),
            open_side: self.position.as_ref().map(|open| open.side),
        }
    }
}

/// Fills folded per attributed book, retaining input order independence.
/// Build with [`aggregate`]; read buckets any time, or price the whole fold
/// at marks with [`Aggregation::summarize`].
pub struct Aggregation {
    fills: Vec<BookedFill>,
    books: BTreeMap<BucketKey, BookState>,
}

/// Fold attributed fills into per-book engine positions.
///
/// Fills sort by `(at, trade_id)` first, so caller order never moves a
/// number. Each book folds chronologically through `update_from_fill`; a
/// corrupt fill (zero quantity) refuses fail-closed with the venue's reason.
pub fn aggregate(fills: &[BookedFill]) -> Result<Aggregation, ServerError> {
    let mut ordered = fills.to_vec();
    ordered.sort_by(|left, right| (left.at, &left.trade_id).cmp(&(right.at, &right.trade_id)));
    let mut books: BTreeMap<BucketKey, BookState> = BTreeMap::new();
    for fill in &ordered {
        books
            .entry(fill.key())
            .or_insert_with(BookState::new)
            .apply(fill)?;
    }
    Ok(Aggregation {
        fills: ordered,
        books,
    })
}

impl Aggregation {
    /// Per-book summaries with unrealized at the last fill price (no marks).
    /// Buckets sort by key, so output order is deterministic.
    #[must_use]
    pub fn buckets(&self) -> Vec<BucketSummary> {
        self.books
            .iter()
            .map(|(key, book)| book.snapshot(key, None))
            .collect()
    }

    /// Price the fold at `marks` (`(time, price)` per mark, in any order):
    /// per-bucket unrealized re-marks at the last mark, the unrealized curve
    /// values each mark over fills at or before it, and the per-mark MTM
    /// equity (realized-so-far plus unrealized) feeds the return-based stats
    /// with the same annualization convention `build_report` uses.
    ///
    /// Refuses fail-closed when the fold spans more than one underlying: one
    /// price series cannot mark two instruments, and guessing is worse than
    /// refusing.
    pub fn summarize(
        &self,
        marks: &[(DateTime<Utc>, Money)],
    ) -> Result<PortfolioReport, ServerError> {
        let mut underlyings: Vec<&str> = self
            .fills
            .iter()
            .map(|fill| fill.underlying.as_str())
            .collect();
        underlyings.sort_unstable();
        underlyings.dedup();
        if underlyings.len() > 1 {
            return Err(ServerError::Backtest(format!(
                "one mark series cannot price {} underlyings; summarize one underlying at a time",
                underlyings.len()
            )));
        }
        let mut ordered: Vec<(DateTime<Utc>, Money)> = marks.to_vec();
        ordered.sort_by_key(|mark| mark.0);

        // One incremental walk: fills drain into live books as each mark's
        // time passes them, so the whole curve costs one fold, not one per
        // mark. Per-portfolio equity accumulates alongside the global curve.
        let mut live: BTreeMap<BucketKey, BookState> = BTreeMap::new();
        let mut cursor = 0;
        let mut curve = Vec::with_capacity(ordered.len());
        let mut equity: BTreeMap<String, Vec<Decimal>> = BTreeMap::new();
        for (at, price) in &ordered {
            while cursor < self.fills.len() && self.fills[cursor].at <= *at {
                let fill = &self.fills[cursor];
                live.entry(fill.key())
                    .or_insert_with(BookState::new)
                    .apply(fill)?;
                cursor += 1;
            }
            let mut mark_total = Decimal::ZERO;
            let mut by_portfolio: BTreeMap<&str, Decimal> = BTreeMap::new();
            for (key, book) in &live {
                let point = book.realised() + book.unrealised(Some(*price));
                mark_total += book.unrealised(Some(*price));
                *by_portfolio.entry(key.portfolio.as_str()).or_default() += point;
            }
            curve.push(Money::new(mark_total));
            for (portfolio, point) in by_portfolio {
                equity.entry(portfolio.to_owned()).or_default().push(point);
            }
        }

        // Totals read the full fold, not the mark walk: with no marks the
        // walk never runs, but the books still closed and settled.
        let last_mark = ordered.last().map(|(_, price)| *price);
        let mut totals: BTreeMap<&str, PortfolioTotals> = BTreeMap::new();
        for (key, book) in &self.books {
            let entry = totals
                .entry(key.portfolio.as_str())
                .or_insert_with(|| PortfolioTotals {
                    portfolio: key.portfolio.clone(),
                    fills: 0,
                    closed_trades: 0,
                    pnl_realised: Money::zero(),
                    pnl_unrealised: Money::zero(),
                    fees: Money::zero(),
                    open_quantity: 0,
                    win_rate: None,
                    profit_factor: None,
                    sharpe: None,
                    sortino: None,
                    calmar: None,
                    buckets: Vec::new(),
                });
            let snapshot = book.snapshot(key, last_mark);
            entry.fills += snapshot.fills;
            entry.closed_trades += snapshot.closed_trades;
            entry.pnl_realised =
                Money::new(entry.pnl_realised.as_decimal() + snapshot.pnl_realised.as_decimal());
            entry.pnl_unrealised = Money::new(
                entry.pnl_unrealised.as_decimal() + snapshot.pnl_unrealised.as_decimal(),
            );
            entry.fees = Money::new(entry.fees.as_decimal() + snapshot.fees.as_decimal());
            entry.open_quantity += snapshot.open_quantity;
            entry.buckets.push(snapshot);
        }
        let annual = Decimal::from(metrics::TRADING_DAYS_PER_YEAR);
        for (portfolio, entry) in &mut totals {
            let closed: Vec<Decimal> = self
                .books
                .iter()
                .filter(|(key, _)| key.portfolio.as_str() == *portfolio)
                .flat_map(|(_, book)| book.closed_realised.iter().copied())
                .collect();
            let curve_decimals: Vec<Decimal> =
                equity.get(*portfolio).map_or_else(Vec::new, Clone::clone);
            let steps = metrics::step_returns(&curve_decimals);
            entry.win_rate = metrics::win_rate(&closed).and_then(|value| value.to_f64());
            entry.profit_factor = metrics::profit_factor(&closed).and_then(|value| value.to_f64());
            entry.sharpe =
                metrics::sharpe(&steps, Decimal::ZERO, annual).and_then(|value| value.to_f64());
            entry.sortino =
                metrics::sortino(&steps, Decimal::ZERO, annual).and_then(|value| value.to_f64());
            entry.calmar = metrics::calmar(&curve_decimals, Decimal::ZERO, annual)
                .and_then(|value| value.to_f64());
        }
        Ok(PortfolioReport {
            portfolios: totals.into_values().collect(),
            unrealized_curve: curve,
            greeks_note: crate::report::BacktestReport::unavailable_greeks_note(),
        })
    }
}

#[cfg(test)]
#[path = "../tests/portfolio.rs"]
mod tests;
