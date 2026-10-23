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
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;
use std::collections::BTreeMap;

use protopyne_types::{GreeksCurve, Money, OptionContract};
use protopyne_venue::{
    calculate_pnl_unrealised, update_from_fill, LegExecution, OrderSide, PositionFill,
    PositionSide, SpreadPosition,
};
use protopyne_view::{
    ConcentrationReport, ConcentrationShare, CONCENTRATION_EVENT_EXPIRY_WEEK,
    CONCENTRATION_EVENT_UNEXPOSED, CONCENTRATION_NO_EXPIRY,
};

use crate::error::ServerError;
use crate::marks::{mark_position, MarkBasis, MarkPoint};
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
    /// Whole units filled; must be positive. A spread fill books its package
    /// units (one instrument of N units), never the leg-contract sum.
    pub quantity: u32,
    /// Execution price per contract.
    pub price: Money,
    /// Commission on this fill.
    pub fee: Money,
    /// Venue-side trade identifier for audit (and fold tie-break).
    pub trade_id: String,
    /// Execution time.
    pub at: DateTime<Utc>,
    /// How this fill's position is marked for unrealized P&L. Defaults to the
    /// fill-price basis so pre-existing fills (and JSON rows) read exactly as
    /// before.
    #[builder(default = MarkBasis::FillPrice)]
    #[serde(default)]
    pub mark_basis: MarkBasis,
    /// The package's executed legs, when the fill was one. Empty for a
    /// legless fill. They carry no economics of their own — they are how the
    /// book marks a package from the chain instead of from a share price.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub legs: Vec<LegExecution>,
}

/// Grouping key: one attributed book. Plain strings — these are names that
/// already appear in JSON (orders, grants, configs), never opaque keys — plus
/// the front expiry, so a roll opens a new book: the book follows the
/// instrument, not the request.
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
    /// Front expiry across the fill's legs, when the fill names any. `None`
    /// for legless fills, which book exactly as before.
    pub expiry: Option<NaiveDate>,
}

impl BookedFill {
    /// Grouping key for this fill's book. The expiry is the earliest leg
    /// expiry parsed from the legs' OCC symbols (the nearest event dominates
    /// concentration); a fill that names no legs is dateless. An unparseable
    /// contract refuses fail-closed with the trade id named — like a corrupt
    /// fill, it never books silently.
    pub fn key(&self) -> Result<BucketKey, ServerError> {
        let mut expiry: Option<NaiveDate> = None;
        for leg in &self.legs {
            let contract =
                OptionContract::parse_occ_symbol(leg.contract.as_str()).map_err(|refused| {
                    ServerError::Backtest(format!(
                        "fill {} names an unparseable contract {}: {refused}",
                        self.trade_id,
                        leg.contract.as_str()
                    ))
                })?;
            expiry = Some(expiry.map_or(contract.expiry, |earliest: NaiveDate| {
                earliest.min(contract.expiry)
            }));
        }
        Ok(BucketKey {
            portfolio: self.portfolio.clone(),
            underlying: self.underlying.clone(),
            structure: self.structure.clone(),
            strategy: self.strategy.clone(),
            grant: self.grant.clone(),
            expiry,
        })
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
    /// Unrealized PnL at the last mark (or the last fill price unmarked);
    /// `None` — never zero — when no mark exists for this book.
    pub pnl_unrealised: Option<Money>,
    /// Cumulative entry plus exit fees.
    pub fees: Money,
    /// Open absolute quantity (0 when flat).
    pub open_quantity: u32,
    /// Open side (`None` when flat).
    pub open_side: Option<PositionSide>,
    /// How this book's open position is marked for unrealized P&L.
    pub mark_basis: MarkBasis,
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
    /// Unrealized PnL across its books at the last mark; `None` — never zero —
    /// when any open book has no mark.
    pub pnl_unrealised: Option<Money>,
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
    /// Fitted volatility curve, when a surface feed is configured. `None`
    /// until then — the note above says why, never an invented curve.
    /// `#[serde(default)]` so payloads written before the field existed
    /// still decode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub greeks_curve: Option<GreeksCurve>,
    /// Concentration over the fold's open books, projected from the same
    /// fold the totals read — never a second fold. `#[serde(default)]` so
    /// payloads written before the field existed still decode.
    #[serde(default)]
    pub concentration: ConcentrationReport,
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
    /// How this book's open position is marked (the newest fill's basis).
    mark_basis: MarkBasis,
    /// The newest fill's executed legs, so a package book marks from the
    /// chain. Empty for a legless book, which marks off the tape price.
    legs: Vec<LegExecution>,
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
            mark_basis: MarkBasis::FillPrice,
            legs: Vec::new(),
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
        // The newest fill carries the book's mark basis and legs; a book
        // folded from marked fills marks, one folded from absent fills stays
        // absent.
        self.mark_basis = fill.mark_basis;
        self.legs.clone_from(&fill.legs);
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
    /// last-fill refresh (or zero flat). `None` when the book has no mark
    /// basis at all: absence is reported, never zero. A closed book holds no
    /// exposure, so its unrealized is always some zero — never a stale print.
    ///
    /// A package has no share price to ride the tape on, so a legged book
    /// marks off its own legs through [`mark_position`] at the mark's
    /// instant; a leg the chain cannot answer falls back to the book's own
    /// fill-price refresh, which is the fallback its basis names.
    fn unrealised(&self, mark: Option<MarkPoint<'_>>) -> Option<Decimal> {
        if self.mark_basis.is_absent() {
            return None;
        }
        Some(match (&self.position, mark) {
            (Some(open), Some(mark)) if !self.legs.is_empty() => {
                let (per_unit, _) =
                    mark_position(&self.legs, open.quantity_abs, mark.chain, mark.at);
                per_unit.map_or(open.pnl_unrealised.as_decimal(), |price| {
                    calculate_pnl_unrealised(
                        open.side,
                        open.price_entry_average,
                        open.quantity_abs,
                        open.quantity_abs_max,
                        open.fees_enter,
                        price,
                    )
                    .as_decimal()
                })
            }
            (Some(open), Some(mark)) => calculate_pnl_unrealised(
                open.side,
                open.price_entry_average,
                open.quantity_abs,
                open.quantity_abs_max,
                open.fees_enter,
                mark.price,
            )
            .as_decimal(),
            (Some(open), None) => open.pnl_unrealised.as_decimal(),
            (None, _) => Decimal::ZERO,
        })
    }

    /// Point-in-time summary: unrealized re-marks when a mark is given.
    fn snapshot(&self, key: &BucketKey, mark: Option<MarkPoint<'_>>) -> BucketSummary {
        BucketSummary {
            key: key.clone(),
            fills: self.fills,
            closed_trades: self.closed_trades,
            pnl_realised: Money::new(self.realised()),
            pnl_unrealised: self.unrealised(mark).map(Money::new),
            fees: Money::new(self.fees()),
            open_quantity: self.position.as_ref().map_or(0, |open| open.quantity_abs),
            open_side: self.position.as_ref().map(|open| open.side),
            mark_basis: self.mark_basis,
        }
    }
}

/// One open book position as the aggregate sees it: which underlying, which
/// side, how many whole units, and the engine's volume-weighted entry price.
/// Flat books are omitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPosition {
    /// Underlying this book trades.
    pub underlying: String,
    /// Open direction.
    pub side: PositionSide,
    /// Open absolute quantity.
    pub quantity: u32,
    /// Volume-weighted entry price.
    pub price_entry_average: Money,
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
            .entry(fill.key()?)
            .or_insert_with(BookState::new)
            .apply(fill)?;
    }
    Ok(Aggregation {
        fills: ordered,
        books,
    })
}

/// One partition of the concentration report: per-bucket notionals become
/// shares of the total. Inputs arrive sorted (B-tree order, or dated with
/// dateless last); a zero total reports zero shares rather than dividing by
/// zero.
fn shares(buckets: Vec<(String, Decimal)>, total: Decimal) -> Vec<ConcentrationShare> {
    buckets
        .into_iter()
        .map(|(key, notional)| {
            let share = if total.is_zero() {
                0.0
            } else {
                (notional / total).to_f64().unwrap_or(0.0)
            };
            ConcentrationShare {
                key,
                notional: Money::new(notional),
                share,
            }
        })
        .collect()
}

impl Aggregation {
    /// Open positions across the fold's books, in bucket-key order. Flat
    /// books are omitted; each entry carries the engine's volume-weighted
    /// entry average so a caller never re-derives it.
    #[must_use]
    pub fn open_positions(&self) -> Vec<OpenPosition> {
        self.books
            .iter()
            .filter_map(|(key, book)| {
                let open = book.position.as_ref()?;
                Some(OpenPosition {
                    underlying: key.underlying.clone(),
                    side: open.side,
                    quantity: open.quantity_abs,
                    price_entry_average: open.price_entry_average,
                })
            })
            .collect()
    }

    /// Per-book summaries with unrealized at the last fill price (no marks).
    /// Buckets sort by key, so output order is deterministic.
    #[must_use]
    pub fn buckets(&self) -> Vec<BucketSummary> {
        self.books
            .iter()
            .map(|(key, book)| book.snapshot(key, None))
            .collect()
    }

    /// Days before expiry that put a book in the event window: the expiry
    /// week. Only expiry proximity feeds this — the fold carries no
    /// corporate-action calendar, so ex-dividend windowing waits for one.
    pub const EVENT_WINDOW_DAYS: i64 = 7;

    /// Concentration over the fold's open books: three partitions of the
    /// same open notional (quantity × entry average), projected from this
    /// fold's books — never a second fold. Flat books contribute nothing;
    /// an empty fold reports empty partitions and a zero total.
    ///
    /// The event partition reads expiry against the newest fill's date:
    /// `expiry-week` for books expiring within the window (inclusive),
    /// `unexposed` for everything else, including dateless books.
    #[must_use]
    pub fn concentration(&self) -> ConcentrationReport {
        let mut entries: Vec<(String, Option<NaiveDate>, Decimal)> = Vec::new();
        for (key, book) in &self.books {
            let Some(open) = &book.position else {
                continue;
            };
            let notional = Decimal::from(open.quantity_abs) * open.price_entry_average.as_decimal();
            entries.push((key.underlying.clone(), key.expiry, notional));
        }
        let total = entries
            .iter()
            .map(|(_, _, notional)| *notional)
            .sum::<Decimal>();
        let as_of = self.fills.last().map(|fill| fill.at.date_naive());
        let mut by_underlying: BTreeMap<&str, Decimal> = BTreeMap::new();
        let mut by_expiry: BTreeMap<Option<NaiveDate>, Decimal> = BTreeMap::new();
        let mut by_event: BTreeMap<&str, Decimal> = BTreeMap::new();
        for (underlying, expiry, notional) in &entries {
            *by_underlying.entry(underlying.as_str()).or_default() += *notional;
            *by_expiry.entry(*expiry).or_default() += *notional;
            let in_window = as_of.zip(*expiry).is_some_and(|(at, exp)| {
                let days = (exp - at).num_days();
                (0..=Self::EVENT_WINDOW_DAYS).contains(&days)
            });
            *by_event
                .entry(if in_window {
                    CONCENTRATION_EVENT_EXPIRY_WEEK
                } else {
                    CONCENTRATION_EVENT_UNEXPOSED
                })
                .or_default() += *notional;
        }
        let mut by_expiry: Vec<(String, Decimal)> = by_expiry
            .into_iter()
            .map(|(expiry, notional)| {
                (
                    expiry.map_or_else(
                        || CONCENTRATION_NO_EXPIRY.to_owned(),
                        |date| date.to_string(),
                    ),
                    notional,
                )
            })
            .collect();
        by_expiry.sort_by(|left, right| {
            // Dated ascending, dateless last: ISO dates sort lexicographically.
            (left.0 == CONCENTRATION_NO_EXPIRY, &left.0)
                .cmp(&(right.0 == CONCENTRATION_NO_EXPIRY, &right.0))
        });
        ConcentrationReport {
            by_underlying: shares(
                by_underlying
                    .into_iter()
                    .map(|(key, notional)| (key.to_owned(), notional))
                    .collect(),
                total,
            ),
            by_expiry: shares(by_expiry, total),
            by_event: shares(
                by_event
                    .into_iter()
                    .map(|(key, notional)| (key.to_owned(), notional))
                    .collect(),
                total,
            ),
            total_notional: Money::new(total),
        }
    }

    /// Price the fold at `marks` ([`MarkPoint`] per mark, in any order):
    /// per-bucket unrealized re-marks at the last mark, the unrealized curve
    /// values each mark over fills at or before it, and the per-mark MTM
    /// equity (realized-so-far plus unrealized) feeds the return-based stats
    /// with the same annualization convention `build_report` uses. Each mark
    /// carries the run's chain, so a legged book marks from it and a legless
    /// one off the tape price.
    ///
    /// Refuses fail-closed when the fold spans more than one underlying: one
    /// price series cannot mark two instruments, and guessing is worse than
    /// refusing.
    pub fn summarize(&self, marks: &[MarkPoint<'_>]) -> Result<PortfolioReport, ServerError> {
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
        let mut ordered: Vec<MarkPoint<'_>> = marks.to_vec();
        ordered.sort_by_key(|mark| mark.at);

        // One incremental walk: fills drain into live books as each mark's
        // time passes them, so the whole curve costs one fold, not one per
        // mark. Per-portfolio equity accumulates alongside the global curve.
        let mut live: BTreeMap<BucketKey, BookState> = BTreeMap::new();
        let mut cursor = 0;
        let mut curve = Vec::with_capacity(ordered.len());
        let mut equity: BTreeMap<String, Vec<Decimal>> = BTreeMap::new();
        for mark in &ordered {
            while cursor < self.fills.len() && self.fills[cursor].at <= mark.at {
                let fill = &self.fills[cursor];
                live.entry(fill.key()?)
                    .or_insert_with(BookState::new)
                    .apply(fill)?;
                cursor += 1;
            }
            let mut mark_total = Decimal::ZERO;
            let mut by_portfolio: BTreeMap<&str, Decimal> = BTreeMap::new();
            for (key, book) in &live {
                // An absent mark contributes nothing to the curve and its
                // bucket reports `None` (totals go absent); this is a
                // fail-honest guard, never a silent zero. No production book
                // is absent today.
                let unrealized = book.unrealised(Some(*mark)).unwrap_or(Decimal::ZERO);
                mark_total += unrealized;
                let point = book.realised() + unrealized;
                *by_portfolio.entry(key.portfolio.as_str()).or_default() += point;
            }
            curve.push(Money::new(mark_total));
            for (portfolio, point) in by_portfolio {
                equity.entry(portfolio.to_owned()).or_default().push(point);
            }
        }

        // Totals read the full fold, not the mark walk: with no marks the
        // walk never runs, but the books still closed and settled.
        let last_mark = ordered.last().copied();
        let mut totals: BTreeMap<&str, PortfolioTotals> = BTreeMap::new();
        for (key, book) in &self.books {
            let entry = totals
                .entry(key.portfolio.as_str())
                .or_insert_with(|| PortfolioTotals {
                    portfolio: key.portfolio.clone(),
                    fills: 0,
                    closed_trades: 0,
                    pnl_realised: Money::zero(),
                    pnl_unrealised: Some(Money::zero()),
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
            entry.pnl_unrealised = match (entry.pnl_unrealised, snapshot.pnl_unrealised) {
                (Some(total), Some(part)) => {
                    Some(Money::new(total.as_decimal() + part.as_decimal()))
                }
                // Any unmarked bucket makes the total absent: a partial sum
                // would read as a smaller loss than reality.
                _ => None,
            };
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
            greeks_note: crate::report::BacktestReport::unavailable_greeks_note(
                ordered.iter().any(|mark| !mark.chain.is_empty()),
            ),
            greeks_curve: None,
            concentration: self.concentration(),
        })
    }
}

#[cfg(test)]
#[path = "../tests/portfolio.rs"]
mod tests;
