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
use std::sync::Mutex;
use std::time::Duration;

use protopyne_types::Money;

use crate::error::VenueError;
use crate::order::{
    CorporateEvent, Fill, OrderIntent, OrderRecord, OrderSide, OrderStatus, Position,
};
use crate::quote::{FeeSchedule, LiquidityFilter, Quote, Slippage, DEFAULT_SLIPPAGE_BPS};
use crate::venue::{DryRunOutcome, Venue};

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

/// Venue-side books: orders by client ID plus net positions by underlying.
#[derive(Debug, Default)]
struct Book {
    orders: HashMap<String, OrderRecord>,
    positions: HashMap<String, Position>,
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
}

impl PaperVenue {
    /// Open a venue with `config`.
    #[must_use]
    pub fn new(config: PaperConfig) -> Self {
        Self {
            config,
            book: Mutex::new(Book::default()),
            faults: Mutex::new(VecDeque::new()),
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

    fn commit(&self, record: OrderRecord, signed_qty: i64, price: Money) {
        let mut book = common_core::sync::lock(&self.book);
        let position = book
            .positions
            .entry(record.intent.underlying.clone())
            .or_insert_with(|| {
                Position::new()
                    .underlying(record.intent.underlying.clone())
                    .quantity(0)
                    .avg_price(Money::zero())
                    .build()
            });
        position.apply(signed_qty, price);
        book.orders.insert(record.intent.client_id.clone(), record);
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

    async fn submit(
        &self,
        intent: OrderIntent,
        quote: &Quote,
        now: DateTime<Utc>,
    ) -> Result<OrderRecord, VenueError> {
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
                let (record, signed) = self.decide(&intent, quote, now)?;
                let price = record.avg_price.unwrap_or(intent.limit_price);
                self.commit(record.clone(), signed, price);
                return Err(VenueError::Transport("acknowledgement lost".to_owned()));
            }
            None => {}
        }
        let (record, signed) = self.decide(&intent, quote, now)?;
        let price = record.avg_price.unwrap_or(intent.limit_price);
        self.commit(record.clone(), signed, price);
        Ok(record)
    }

    fn query(&self, client_id: &str) -> impl Future<Output = Option<OrderRecord>> + Send {
        let record = common_core::sync::lock(&self.book)
            .orders
            .get(client_id)
            .cloned();
        std::future::ready(record)
    }

    fn cancel(
        &self,
        client_id: &str,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<OrderRecord, VenueError>> + Send {
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
        std::future::ready(outcome)
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

    fn apply_corporate_event(
        &self,
        underlying: &str,
        event: CorporateEvent,
        _now: DateTime<Utc>,
    ) -> Result<String, VenueError> {
        let mut book = common_core::sync::lock(&self.book);
        match event {
            CorporateEvent::Expiry | CorporateEvent::Assignment => {
                book.positions.remove(underlying);
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
                let position = book.positions.get_mut(underlying).ok_or_else(|| {
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
