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

use protopyne_types::{LegSide, Money, SpreadLegs, SymKey, SymbolIndex};

use crate::error::VenueError;
use crate::order::{
    CorporateEvent, Fill, OrderIntent, OrderRecord, OrderSide, OrderStatus, Position,
};
use crate::position::{PositionFill, SpreadPosition, update_from_fill};
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
        let source = self
            .leg_source()
            .ok_or_else(|| VenueError::Invalid("spread intent has no leg market data".to_owned()))?;
        let mut legs = Vec::with_capacity(spread.legs().len());
        let mut net = Decimal::ZERO;
        for (index, leg) in spread.legs().iter().enumerate() {
            let occ = leg.contract.occ_symbol();
            let quote = source.leg_quote(&leg.contract).ok_or_else(|| {
                VenueError::Refused(format!("leg {index} ({occ}): no quote"))
            })?;
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
            let fee = self.config.fees.fee_for(leg_qty).ok_or_else(|| {
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
        let filled_quantity: u32 = legs
            .iter()
            .map(|leg| leg.quantity)
            .try_fold(0_u32, |total, qty| {
                total
                    .checked_add(qty)
                    .ok_or_else(|| VenueError::Refused("filled quantity overflow".to_owned()))
            })?;
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
            let (next, _exited) = update_from_fill(current, &fill).map_err(|e| {
                VenueError::Server(format!("leg commit refused: {e}"))
            })?;
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

impl Venue for PaperVenue {
    fn dry_run(&self, intent: &OrderIntent, _quote: &Quote) -> Result<DryRunOutcome, VenueError> {
        let notional = intent.limit_price.as_decimal() * Decimal::from(intent.quantity);
        if intent.quantity == 0 {
            return Err(VenueError::Invalid("quantity must be positive".to_owned()));
        }
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
            let record = book.orders.get_mut(client_id).ok_or_else(|| {
                VenueError::Invalid(format!("unknown order {client_id}"))
            })?;
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
        underlying: &str,
        event: CorporateEvent,
        _now: DateTime<Utc>,
    ) -> Result<String, VenueError> {
        // Reads resolve without inserting: an unknown underlying is absence,
        // and absence closes (expiry) or refuses (split) exactly as before.
        let key = common_core::sync::lock(&self.symbols).lookup(underlying);
        let mut book = common_core::sync::lock(&self.book);
        match event {
            CorporateEvent::Expiry | CorporateEvent::Assignment => {
                if let Some(key) = key {
                    book.positions.remove(&key);
                }
                Ok(format!("{event:?} on {underlying}: position closed for review"))
            }
            CorporateEvent::Dividend(amount) => Ok(format!(
                "dividend {amount} on {underlying}: cash booked outside position"
            )),
            CorporateEvent::Split(factor) => {
                if factor == 0 {
                    return Err(VenueError::Invalid(
                        "split factor must be new shares per old share (forward splits only); got 0".to_owned(),
                    ));
                }
                let Some(key) = key else {
                    return Err(VenueError::Invalid(format!("no position in {underlying}")));
                };
                let position = book.positions.get_mut(&key).ok_or_else(|| {
                    VenueError::Invalid(format!("no position in {underlying}"))
                })?;
                position.quantity = position.quantity.checked_mul(i64::from(factor)).ok_or_else(|| {
                    VenueError::Invalid(format!("split {factor}-for-1 overflows position in {underlying}"))
                })?;
                let price = position.avg_price.as_decimal() / Decimal::from(factor);
                position.avg_price = Money::new(price);
                Ok(format!("split {factor}-for-1 on {underlying}"))
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/paper.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/paper_legs.rs"]
mod leg_tests;
