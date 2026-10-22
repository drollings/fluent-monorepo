//! `PaperVenue`: the rehearsal venue behind the same trait as live.
//!
//! Fills are spread-aware and deterministic: liquidity filter first (with
//! reasons), then marketability against the touch, then slippage-adverse
//! pricing capped at the limit, then per-contract fees. Non-marketable
//! orders are refused as `WouldRest` — the prototype never rests orders, and
//! says so, rather than pretending. Fault injection makes transport, server,
//! delay, and lost-acknowledgement failures reproducible for tests.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use protopyne_types::{
    ContractId, LegSide, Money, OptionContract, OptionKind, SpreadLegs, SymKey, SymbolIndex,
};

use crate::error::VenueError;
use crate::order::{
    CorporateEvent, CorporateEventArgs, CorporateOutcome, Fill, OrderIntent, OrderRecord,
    OrderSide, OrderStatus, Position,
};
use crate::position::{update_from_fill, PositionFill, PositionSide, SpreadPosition};
use crate::quote::{FeeSchedule, LiquidityFilter, Quote, Slippage, DEFAULT_SLIPPAGE_BPS};
use crate::venue::{DryRunOutcome, LegExecution, LegQuoteSource, Venue};

/// Injected failure, consumed FIFO. Tests script venue behavior without any
/// network: every fault is explicit, ordered, and drained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Connection drops before any verdict; nothing is recorded.
    DropConnection,
    /// Venue answers 5xx; nothing is recorded.
    Status500,
    /// Sleep before deciding, so the caller times out first.
    DelayFill(Duration),
    /// Record the fill, then lose the acknowledgement. Query-by-ID still
    /// finds the order: this is the kill-mid-submit case.
    LoseAck,
}

/// Venue-side books: orders by client ID plus net positions by indexed
/// symbol key. Spread legs additionally track per-contract positions by OCC
/// symbol and the executed legs behind each spread fill, both keyed for
/// query and reconciliation. Only the single-underlying net book uses the
/// session-scoped index: OCC symbols and client IDs stay strings (they are
/// identifiers, not a hot lookup path).
#[derive(Debug, Default)]
struct Book {
    orders: HashMap<String, OrderRecord>,
    positions: HashMap<SymKey, Position>,
    spread: HashMap<String, SpreadPosition>,
    executions: HashMap<String, Vec<LegExecution>>,
    /// Cash credited by dividends, per underlying. Dividends are an
    /// instrument-level credit, not a position mutation.
    dividends: HashMap<String, Money>,
}

/// Configuration for one paper venue.
#[derive(Debug, Clone, bon::Builder)]
#[builder(start_fn = new)]
pub struct PaperConfig {
    /// Commission schedule.
    #[builder(default = FeeSchedule::new().per_contract(Money::new(Decimal::new(65, 2))).build())]
    pub fees: FeeSchedule,
    /// Market-quality gate.
    #[builder(default = LiquidityFilter::new()
        .max_spread_width(Money::new(Decimal::new(50, 0)))
        .min_size(1)
        .max_age_secs(60)
        .build())]
    pub filter: LiquidityFilter,
    /// Adverse-move tolerance **(task-value)**.
    #[builder(default = Slippage::new().bps(DEFAULT_SLIPPAGE_BPS).build())]
    pub slippage: Slippage,
    /// Buying power cap for dry-run checks.
    #[builder(default = Money::new(Decimal::new(100_000, 0)))]
    pub buying_power: Money,
}

/// The rehearsal venue: same trait as live, deterministic fills, no network.
pub struct PaperVenue {
    config: PaperConfig,
    book: Mutex<Book>,
    faults: Mutex<VecDeque<Fault>>,
    leg_quotes: Mutex<Option<Arc<dyn LegQuoteSource>>>,
    /// Session-scoped symbol table backing the net-position book. Owned
    /// here (never global) so parallel venues stay isolated; every public
    /// surface still speaks strings, so keys never leak into serialization.
    symbols: Mutex<SymbolIndex>,
}

impl PaperVenue {
    /// Open a venue with `config`.
    #[must_use]
    pub fn new(config: PaperConfig) -> Self {
        Self {
            config,
            book: Mutex::new(Book::default()),
            faults: Mutex::new(VecDeque::new()),
            leg_quotes: Mutex::new(None),
            symbols: Mutex::new(SymbolIndex::new()),
        }
    }

    /// Borrow the configuration.
    #[must_use]
    pub fn config(&self) -> &PaperConfig {
        &self.config
    }

    /// Queue one injected fault for the next submission.
    pub fn inject(&self, fault: Fault) {
        common_core::sync::lock(&self.faults).push_back(fault);
    }

    /// Install the per-leg market data source for spread evaluation. The
    /// venue owns no feed: without a source, multi-leg intents refuse
    /// fail-closed rather than pricing legs off the single-underlying quote.
    pub fn set_leg_quotes(&mut self, source: Arc<dyn LegQuoteSource>) {
        *common_core::sync::lock(&self.leg_quotes) = Some(source);
    }

    /// Per-contract spread positions by OCC symbol, for reconciliation and
    /// tests. Single-underlying net positions stay on [`positions`](Self::positions).
    #[must_use]
    pub fn spread_positions(&self) -> HashMap<String, SpreadPosition> {
        common_core::sync::lock(&self.book).spread.clone()
    }

    /// Dividend cash booked per underlying, for audit and tests.
    #[must_use]
    pub fn booked_dividends(&self) -> HashMap<String, Money> {
        common_core::sync::lock(&self.book).dividends.clone()
    }

    fn leg_source(&self) -> Option<Arc<dyn LegQuoteSource>> {
        common_core::sync::lock(&self.leg_quotes).clone()
    }

    /// Pending injected faults (for test assertions).
    #[must_use]
    pub fn pending_faults(&self) -> usize {
        common_core::sync::lock(&self.faults).len()
    }

    fn pop_fault(&self) -> Option<Fault> {
        common_core::sync::lock(&self.faults).pop_front()
    }

    fn decide(
        &self,
        intent: &OrderIntent,
        quote: &Quote,
        now: DateTime<Utc>,
    ) -> Result<(OrderRecord, i64), VenueError> {
        let is_buy = matches!(intent.side, OrderSide::Buy);
        self.config
            .filter
            .check(quote, intent.quantity, is_buy, now)
            .map_err(VenueError::Refused)?;
        let touch = if is_buy { quote.ask } else { quote.bid };
        let marketable = if is_buy {
            intent.limit_price.as_decimal() >= touch.as_decimal()
        } else {
            intent.limit_price.as_decimal() <= touch.as_decimal()
        };
        if !marketable {
            return Err(VenueError::Refused(format!(
                "limit {} would rest at touch {touch}",
                intent.limit_price
            )));
        }
        let slipped = self.config.slippage.apply(touch, is_buy);
        let price = if is_buy {
            slipped.as_decimal().min(intent.limit_price.as_decimal())
        } else {
            slipped.as_decimal().max(intent.limit_price.as_decimal())
        };
        let fee = self
            .config
            .fees
            .fee_for(intent.quantity)
            .ok_or_else(|| VenueError::Refused("fee overflow".to_owned()))?;
        let mut record = OrderRecord::pending(intent.clone(), now);
        record.status = OrderStatus::Acknowledged;
        record.apply_fill(
            Fill::new()
                .quantity(intent.quantity)
                .price(Money::new(price))
                .fee(fee)
                .build(),
            now,
        );
        record.status = OrderStatus::Filled;
        let signed = if is_buy {
            i64::from(intent.quantity)
        } else {
            -i64::from(intent.quantity)
        };
        Ok((record, signed))
    }

    fn commit(&self, record: OrderRecord, signed_qty: i64, price: Money) -> Result<(), VenueError> {
        // Fail closed with a reason on an uninterable symbol — never panic:
        // trading paths refuse, they do not unwrap.
        let key = common_core::sync::lock(&self.symbols)
            .intern(&record.intent.underlying)
            .map_err(|e| VenueError::Invalid(e.to_string()))?;
        let mut book = common_core::sync::lock(&self.book);
        let position = book.positions.entry(key).or_insert_with(|| {
            Position::new()
                .underlying(record.intent.underlying.clone())
                .quantity(0)
                .avg_price(Money::zero())
                .build()
        });
        position.apply(signed_qty, price);
        book.orders.insert(record.intent.client_id.clone(), record);
        Ok(())
    }

    /// Decide a multi-leg spread as one atomic package: every leg folds
    /// through the same [`LiquidityFilter::check`](crate::quote::LiquidityFilter::check)
    /// then [`Slippage::apply`](crate::quote::Slippage::apply) as the
    /// single-leg path — same helpers, same constants — and every leg must
    /// pass or the whole spread refuses with per-leg reasons naming the
    /// offending leg. Legs describe fills (long legs buy, short legs sell);
    /// the intent side gates the net package (buy ⇒ net debit within the
    /// limit, sell ⇒ net credit at least the limit). Spread legs carry no
    /// individual limits, so no leg is capped: the net either fits the limit
    /// or the package refuses.
    fn decide_spread(
        &self,
        intent: &OrderIntent,
        spread: &SpreadLegs,
        now: DateTime<Utc>,
    ) -> Result<(OrderRecord, Vec<LegExecution>), VenueError> {
        spread
            .validate()
            .map_err(|e| VenueError::Invalid(e.to_string()))?;
        if intent.quantity == 0 {
            return Err(VenueError::Invalid("quantity must be positive".to_owned()));
        }
        let source = self.leg_source().ok_or_else(|| {
            VenueError::Invalid("spread intent has no leg market data".to_owned())
        })?;
        let mut legs = Vec::with_capacity(spread.legs().len());
        let mut net = Decimal::ZERO;
        for (index, leg) in spread.legs().iter().enumerate() {
            let occ = leg.contract.occ_symbol();
            // Time-parameterized read: a historical source prices the leg as
            // of this submission's bar, a newest-only source answers newest
            // only through its explicit opt-in — the same call serves both,
            // so replay and live cannot diverge here.
            let quote = source
                .leg_quote_at(&leg.contract, now)
                .ok_or_else(|| VenueError::Refused(format!("leg {index} ({occ}): no quote")))?;
            let leg_qty = intent.quantity.checked_mul(leg.ratio).ok_or_else(|| {
                VenueError::Refused(format!("leg {index} ({occ}): quantity overflow"))
            })?;
            let is_buy = leg.side == LegSide::Long;
            self.config
                .filter
                .check(&quote, leg_qty, is_buy, now)
                .map_err(|reason| VenueError::Refused(format!("leg {index} ({occ}): {reason}")))?;
            let touch = if is_buy { quote.ask } else { quote.bid };
            if touch.as_decimal() <= Decimal::ZERO {
                return Err(VenueError::Refused(format!(
                    "leg {index} ({occ}): touch not positive"
                )));
            }
            let slipped = self.config.slippage.apply(touch, is_buy);
            let fee =
                self.config.fees.fee_for(leg_qty).ok_or_else(|| {
                    VenueError::Refused(format!("leg {index} ({occ}): fee overflow"))
                })?;
            let sign = if is_buy {
                Decimal::ONE
            } else {
                Decimal::NEGATIVE_ONE
            };
            net += sign * slipped.as_decimal() * Decimal::from(leg.ratio);
            legs.push(LegExecution {
                contract: protopyne_types::ContractId::new(occ),
                side: if is_buy {
                    OrderSide::Buy
                } else {
                    OrderSide::Sell
                },
                quantity: leg_qty,
                price: slipped,
                fee,
            });
        }
        let limit = intent.limit_price.as_decimal();
        let within_limit = match intent.side {
            OrderSide::Buy => net <= limit,
            OrderSide::Sell => net >= limit,
        };
        if !within_limit {
            return Err(VenueError::Refused(format!(
                "net {net} outside limit {limit} for {} spread",
                intent.side
            )));
        }
        // The record's `filled_quantity` counts package units — the order the
        // venue received. Leg contracts live in the executed leg set, which
        // `executed_legs` returns and the journal rows carry; budgets count
        // leg contracts off that set, never off this field.
        let filled_quantity = intent.quantity;
        let fees_paid = Money::new(legs.iter().map(|leg| leg.fee.as_decimal()).sum());
        let mut record = OrderRecord::pending(intent.clone(), now);
        record.status = OrderStatus::Acknowledged;
        record.apply_fill(
            Fill::new()
                .quantity(filled_quantity)
                .price(Money::new(net))
                .fee(fees_paid)
                .build(),
            now,
        );
        record.status = OrderStatus::Filled;
        Ok((record, legs))
    }

    /// Commit an executed spread: one [`update_from_fill`](crate::position::update_from_fill)
    /// fold per leg into the per-contract book, tagging each leg position
    /// with the executed structure, plus the order record and the executed
    /// legs for query and journaling. An engine refusal here means the
    /// venue's own fills disagreed with its books — a server fault, never a
    /// silent partial commit.
    fn commit_spread(
        &self,
        record: OrderRecord,
        legs: Vec<LegExecution>,
        structure: &SpreadLegs,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        let client_id = record.intent.client_id.clone();
        let underlying = record.intent.underlying.clone();
        let mut book = common_core::sync::lock(&self.book);
        book.orders.insert(client_id.clone(), record);
        for (index, leg) in legs.iter().enumerate() {
            let key = leg.contract.as_str().to_owned();
            let fill = PositionFill::new()
                .underlying(underlying.clone())
                .side(leg.side)
                .quantity(leg.quantity)
                .price(leg.price)
                .fee(leg.fee)
                .trade_id(format!("{client_id}#leg{index}"))
                .at(now)
                .build();
            let current = book.spread.remove(&key);
            let (next, _exited) = update_from_fill(current, &fill)
                .map_err(|e| VenueError::Server(format!("leg commit refused: {e}")))?;
            if let Some(mut position) = next {
                position.legs = Some(structure.clone());
                book.spread.insert(key, position);
            }
        }
        book.executions.insert(client_id, legs);
        Ok(())
    }

    /// Execute one intent through the matching fill path: the legacy
    /// single-leg decide/commit when no legs are attached, the atomic spread
    /// fold when they are. Fault injection and idempotency stay in
    /// [`submit`](Venue::submit); this only decides and commits.
    fn execute(
        &self,
        intent: &OrderIntent,
        quote: &Quote,
        now: DateTime<Utc>,
    ) -> Result<OrderRecord, VenueError> {
        match &intent.legs {
            None => {
                let (record, signed) = self.decide(intent, quote, now)?;
                let price = record.avg_price.unwrap_or(intent.limit_price);
                self.commit(record.clone(), signed, price)?;
                Ok(record)
            }
            Some(spread) => {
                let (record, legs) = self.decide_spread(intent, spread, now)?;
                self.commit_spread(record.clone(), legs, spread, now)?;
                Ok(record)
            }
        }
    }
}

/// The listed contract behind one per-OCC book entry, when the structure tag
/// carries it. Positions booked before legs were tagged simply have none.
fn contract_of(position: &SpreadPosition, occ: &str) -> Option<OptionContract> {
    position
        .legs
        .as_ref()?
        .legs()
        .iter()
        .find(|leg| leg.contract.occ_symbol() == occ)
        .map(|leg| leg.contract.clone())
}

/// Close one per-OCC option book at `price`; answer the booked leg and its
/// realized P&L. A missing book is `None` (expiry covers whatever is open).
fn close_option_leg(
    book: &mut Book,
    occ: &str,
    price: Money,
    at: DateTime<Utc>,
    tag: &str,
) -> Result<Option<(LegExecution, Money)>, VenueError> {
    let Some(position) = book.spread.remove(occ) else {
        // NOT-A-LADDER: no open spread reads as none, not a candidate walk.
        return Ok(None);
    };
    let side = match position.side {
        PositionSide::Long => OrderSide::Sell,
        PositionSide::Short => OrderSide::Buy,
    };
    let quantity = position.quantity_abs;
    let fill = PositionFill::new()
        .underlying(position.underlying.clone())
        .side(side)
        .quantity(quantity)
        .price(price)
        .fee(Money::zero())
        .trade_id(format!("{tag}:{occ}"))
        .at(at)
        .build();
    let (next, exited) = update_from_fill(Some(position), &fill)
        .map_err(|e| VenueError::Invalid(format!("{tag} close refused for {occ}: {e}")))?;
    if let Some(remaining) = next {
        // Never lose a book: restore the remainder, then refuse.
        book.spread.insert(occ.to_owned(), remaining);
        return Err(VenueError::Invalid(format!(
            "{tag} left {occ} open after a full close"
        )));
    }
    let realised = exited.map_or_else(Money::zero, |closed| closed.pnl_realised);
    Ok(Some((
        LegExecution {
            contract: ContractId::new(occ),
            side,
            quantity,
            price,
            fee: Money::zero(),
        },
        realised,
    )))
}

/// `Expiry`: close every due option book of the instrument at intrinsic.
fn expire_at_intrinsic(
    book: &mut Book,
    action: &CorporateEventArgs,
) -> Result<CorporateOutcome, VenueError> {
    let today = action.at.date_naive();
    let intrinsic = action.amount.unwrap_or_else(Money::zero);
    if let Some(named) = &action.contract {
        if named.expiry > today {
            return Err(VenueError::Invalid(format!(
                "expiry at {today} is before contract expiry {}",
                named.expiry
            )));
        }
    }
    let due: Vec<String> = book
        .spread
        .iter()
        .filter_map(|(occ, position)| contract_of(position, occ))
        .filter(|contract| contract.underlying == action.underlying)
        .filter(|contract| match &action.contract {
            Some(named) => contract.occ_symbol() == named.occ_symbol(),
            None => contract.expiry <= today,
        })
        .map(|contract| contract.occ_symbol())
        .collect();
    if due.is_empty() {
        if let Some(named) = &action.contract {
            return Err(VenueError::Invalid(format!(
                "no position in {}",
                named.occ_symbol()
            )));
        }
        return Ok(CorporateOutcome {
            note: format!("expiry on {}: no due option legs", action.underlying),
            ..CorporateOutcome::default()
        });
    }
    let mut legs = Vec::with_capacity(due.len());
    let mut affected = Vec::with_capacity(due.len());
    let mut realised = Decimal::ZERO;
    for occ in due {
        if let Some((leg, pnl)) = close_option_leg(book, &occ, intrinsic, action.at, "expiry")? {
            affected.push(leg.contract.clone());
            realised += pnl.as_decimal();
            legs.push(leg);
        }
    }
    Ok(CorporateOutcome {
        note: format!(
            "expiry on {}: {} option leg(s) closed at intrinsic {intrinsic}",
            action.underlying,
            legs.len()
        ),
        legs,
        stock_leg: None,
        cash: None,
        affected,
        realised: Some(Money::new(realised)),
        reconcile: false,
    })
}

/// `Assignment`/`Exercise`: close the identified option leg at intrinsic and
/// add the generated stock leg to the net book. The net position is never
/// deleted; the book is flagged for broker reconciliation.
fn assign_option(
    book: &mut Book,
    action: &CorporateEventArgs,
    stock_key: SymKey,
) -> Result<CorporateOutcome, VenueError> {
    let contract = action.contract.as_ref().ok_or_else(|| {
        VenueError::Invalid(format!(
            "{} needs the identified option contract",
            action.event.label()
        ))
    })?;
    if contract.multiplier == 0 {
        return Err(VenueError::Invalid(
            "contract multiplier must be positive, got 0".to_owned(),
        ));
    }
    let occ = contract.occ_symbol();
    let (option_side, contracts) = {
        let position = book
            .spread
            .get(&occ)
            .ok_or_else(|| VenueError::Invalid(format!("no position in {occ}")))?;
        (position.side, position.quantity_abs)
    };
    let shares = contracts
        .checked_mul(contract.multiplier)
        .ok_or_else(|| VenueError::Invalid(format!("assignment of {occ} overflows shares")))?;
    let stock_side = match (option_side, contract.kind) {
        // Short calls and long puts deliver shares to the counterparty;
        // short puts and long calls take delivery.
        (PositionSide::Short, OptionKind::Call) | (PositionSide::Long, OptionKind::Put) => {
            OrderSide::Sell
        }
        (PositionSide::Short, OptionKind::Put) | (PositionSide::Long, OptionKind::Call) => {
            OrderSide::Buy
        }
    };
    let intrinsic = action.amount.unwrap_or_else(Money::zero);
    let (leg, realised) = close_option_leg(book, &occ, intrinsic, action.at, "assignment")?
        .ok_or_else(|| VenueError::Invalid(format!("no position in {occ}")))?;
    let signed = match stock_side {
        OrderSide::Buy => i64::from(shares),
        OrderSide::Sell => -i64::from(shares),
    };
    let position = book.positions.entry(stock_key).or_insert_with(|| {
        Position::new()
            .underlying(action.underlying.clone())
            .quantity(0)
            .avg_price(Money::zero())
            .build()
    });
    position.apply(signed, contract.strike);
    let stock_leg = LegExecution {
        contract: ContractId::new(action.underlying.clone()),
        side: stock_side,
        quantity: shares,
        price: contract.strike,
        fee: Money::zero(),
    };
    let affected = vec![
        leg.contract.clone(),
        ContractId::new(action.underlying.clone()),
    ];
    Ok(CorporateOutcome {
        note: format!(
            "{} {occ}: option leg closed at {intrinsic}, {stock_side} {shares} shares at {}",
            action.event.label(),
            contract.strike
        ),
        legs: vec![leg],
        stock_leg: Some(stock_leg),
        cash: None,
        affected,
        realised: Some(realised),
        reconcile: true,
    })
}

/// `Dividend`: book cash per held share against the instrument and name the
/// option legs the ex-dividend mark exposes. No position is mutated.
fn book_dividend(
    book: &mut Book,
    action: &CorporateEventArgs,
    stock_key: Option<SymKey>,
) -> Result<CorporateOutcome, VenueError> {
    let per_share = action.amount.ok_or_else(|| {
        VenueError::Invalid("dividend needs the cash amount per share".to_owned())
    })?;
    let shares = stock_key
        .and_then(|key| book.positions.get(&key))
        .map_or(0_i64, |position| position.quantity);
    let cash = Money::new(per_share.as_decimal() * Decimal::from(shares.abs()));
    if shares != 0 {
        let booked = book
            .dividends
            .entry(action.underlying.clone())
            .or_insert_with(Money::zero);
        *booked = Money::new(booked.as_decimal() + cash.as_decimal());
    }
    let affected: Vec<ContractId> = book
        .spread
        .iter()
        .filter_map(|(occ, position)| {
            let contract = contract_of(position, occ)?;
            (contract.underlying == action.underlying).then(|| ContractId::new(occ.clone()))
        })
        .collect();
    Ok(CorporateOutcome {
        note: format!(
            "dividend {per_share}/share on {}: {cash} booked across {} shares, {} option leg(s) marked",
            action.underlying,
            shares.abs(),
            affected.len()
        ),
        legs: Vec::new(),
        stock_leg: None,
        cash: Some(cash),
        affected,
        realised: None,
        reconcile: false,
    })
}

/// `Split`: adjust the net stock position and every option book of the
/// instrument. Options are re-based as `factor` contracts at `1/factor` the
/// per-share basis (the 2-for-1 OCC representation), so the strike and the
/// contract count move while each adjusted contract still delivers the
/// standard multiplier.
fn split_instrument(
    book: &mut Book,
    action: &CorporateEventArgs,
    stock_key: Option<SymKey>,
    factor: u32,
) -> Result<CorporateOutcome, VenueError> {
    if factor == 0 {
        return Err(VenueError::Invalid(
            "split factor must be new shares per old share (forward splits only); got 0".to_owned(),
        ));
    }
    let divisor = Decimal::from(factor);
    let mut remap: Vec<(String, String)> = Vec::new();
    let mut affected = Vec::new();
    for (occ, position) in &book.spread {
        let Some(contract) = contract_of(position, occ) else {
            continue;
        };
        if contract.underlying != action.underlying {
            continue;
        }
        let next = contract
            .adjusted_for_split(factor)
            .map_err(|e| VenueError::Invalid(e.to_string()))?;
        affected.push(ContractId::new(occ.clone()));
        affected.push(ContractId::new(next.occ_symbol()));
        remap.push((occ.clone(), next.occ_symbol()));
    }
    let mut stock_note = String::new();
    if let Some(position) = stock_key.and_then(|key| book.positions.get_mut(&key)) {
        position.quantity = position
            .quantity
            .checked_mul(i64::from(factor))
            .ok_or_else(|| {
                VenueError::Invalid(format!(
                    "split {factor}-for-1 overflows position in {}",
                    action.underlying
                ))
            })?;
        position.avg_price = Money::new(position.avg_price.as_decimal() / divisor);
        stock_note = format!(", stock {} shares", position.quantity);
    }
    if remap.is_empty() && stock_note.is_empty() {
        return Err(VenueError::Invalid(format!(
            "no position in {}",
            action.underlying
        )));
    }
    for (old, new) in remap {
        let Some(mut position) = book.spread.remove(&old) else {
            continue;
        };
        position.quantity_abs = position
            .quantity_abs
            .checked_mul(factor)
            .ok_or_else(|| VenueError::Invalid(format!("split {factor}-for-1 overflows {old}")))?;
        position.quantity_abs_max = position
            .quantity_abs_max
            .checked_mul(factor)
            .ok_or_else(|| VenueError::Invalid(format!("split {factor}-for-1 overflows {old}")))?;
        position.price_entry_average =
            Money::new(position.price_entry_average.as_decimal() / divisor);
        if let Some(legs) = &position.legs {
            position.legs = Some(
                legs.adjusted_for_split(&action.underlying, factor)
                    .map_err(|e| VenueError::Invalid(e.to_string()))?,
            );
        }
        book.spread.insert(new, position);
    }
    Ok(CorporateOutcome {
        note: format!(
            "split {factor}-for-1 on {}: option strike /{factor}, contracts ×{factor}{stock_note}",
            action.underlying
        ),
        legs: Vec::new(),
        stock_leg: None,
        cash: None,
        affected,
        realised: None,
        reconcile: false,
    })
}

impl Venue for PaperVenue {
    fn dry_run(&self, intent: &OrderIntent, _quote: &Quote) -> Result<DryRunOutcome, VenueError> {
        if intent.quantity == 0 {
            return Err(VenueError::Invalid("quantity must be positive".to_owned()));
        }
        let notional = intent.capital_requirement.map_or_else(
            || intent.limit_price.as_decimal() * Decimal::from(intent.quantity),
            Money::as_decimal,
        );
        if notional > self.config.buying_power.as_decimal() {
            return Err(VenueError::Refused(format!(
                "notional {notional} exceeds buying power {}",
                self.config.buying_power
            )));
        }
        Ok(DryRunOutcome {
            allowed: true,
            reserved_notional: Money::new(notional),
        })
    }

    fn fee_schedule(&self) -> FeeSchedule {
        self.config.fees
    }

    fn submit<'a>(
        &'a self,
        intent: OrderIntent,
        quote: &'a Quote,
        now: DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(record) = self.query(&intent.client_id).await {
                return Ok(record);
            }
            match self.pop_fault() {
                Some(Fault::DropConnection) => {
                    return Err(VenueError::Transport("connection dropped".to_owned()));
                }
                Some(Fault::Status500) => {
                    return Err(VenueError::Server("status 500".to_owned()));
                }
                Some(Fault::DelayFill(delay)) => {
                    // NOT-A-BACKOFF (M15): fault-injection delay only.
                    tokio::time::sleep(delay).await;
                }
                Some(Fault::LoseAck) => {
                    self.execute(&intent, quote, now)?;
                    return Err(VenueError::Transport("acknowledgement lost".to_owned()));
                }
                None => {}
            }
            self.execute(&intent, quote, now)
        })
    }

    fn query<'a>(
        &'a self,
        client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<OrderRecord>> + Send + 'a>> {
        let record = common_core::sync::lock(&self.book)
            .orders
            .get(client_id)
            .cloned();
        Box::pin(std::future::ready(record))
    }

    fn cancel<'a>(
        &'a self,
        client_id: &'a str,
        now: DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        // Cancelling a terminal order is a caller error, never a silent
        // no-op: quiet success would let a dead-order retry loop believe it
        // acted.
        let outcome = (|| {
            let mut book = common_core::sync::lock(&self.book);
            let record = book
                .orders
                .get_mut(client_id)
                .ok_or_else(|| VenueError::Invalid(format!("unknown order {client_id}")))?;
            if !record.status.is_active() {
                let status = record.status;
                return Err(VenueError::Invalid(format!(
                    "cancel refused: order {client_id} already terminal ({status:?})"
                )));
            }
            record.status = OrderStatus::Cancelled;
            record.updated_at = now;
            Ok(record.clone())
        })();
        Box::pin(std::future::ready(outcome))
    }

    fn positions(&self) -> Vec<Position> {
        common_core::sync::lock(&self.book)
            .positions
            .values()
            .cloned()
            .collect()
    }

    fn open_spread_positions(&self) -> Option<Vec<SpreadPosition>> {
        Some(self.spread_positions().into_values().collect())
    }

    fn chain_source(&self) -> Option<Arc<dyn LegQuoteSource>> {
        self.leg_source()
    }

    fn orders(&self) -> Vec<OrderRecord> {
        common_core::sync::lock(&self.book)
            .orders
            .values()
            .cloned()
            .collect()
    }

    fn executed_legs(&self, client_id: &str) -> Vec<LegExecution> {
        common_core::sync::lock(&self.book)
            .executions
            .get(client_id)
            .cloned()
            .unwrap_or_default()
    }

    fn apply_corporate_event(
        &self,
        action: &CorporateEventArgs,
    ) -> Result<CorporateOutcome, VenueError> {
        // Resolve the net-book key before the book lock, in `commit`'s order
        // (symbols then book). Reads never insert; an assignment must intern
        // because it always lands a stock leg.
        let stock_key = match action.event {
            CorporateEvent::Expiry => None,
            CorporateEvent::Assignment | CorporateEvent::Exercise => Some(
                common_core::sync::lock(&self.symbols)
                    .intern(&action.underlying)
                    .map_err(|e| VenueError::Invalid(e.to_string()))?,
            ),
            CorporateEvent::Dividend | CorporateEvent::Split(_) => {
                common_core::sync::lock(&self.symbols).lookup(&action.underlying)
            }
        };
        let mut book = common_core::sync::lock(&self.book);
        match action.event {
            CorporateEvent::Expiry => expire_at_intrinsic(&mut book, action),
            CorporateEvent::Assignment | CorporateEvent::Exercise => {
                let key = stock_key.ok_or_else(|| {
                    VenueError::Invalid(format!("no symbol table entry for {}", action.underlying))
                })?;
                assign_option(&mut book, action, key)
            }
            CorporateEvent::Dividend => book_dividend(&mut book, action, stock_key),
            CorporateEvent::Split(factor) => split_instrument(&mut book, action, stock_key, factor),
        }
    }
}

#[cfg(test)]
#[path = "../tests/paper.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/paper_legs.rs"]
mod leg_tests;

#[cfg(test)]
#[path = "../tests/corporate_actions.rs"]
mod corporate_action_tests;
