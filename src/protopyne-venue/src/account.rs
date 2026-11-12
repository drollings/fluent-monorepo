//! Broker account/order/deal reads: one neutral, object-safe surface.
//!
//! Execution stays on [`Venue`](crate::venue::Venue); this module adds only
//! display/accounting reads. Every method has an explicit refusal for a
//! surface a broker does not implement, so an unsupported read never looks
//! like an empty answer.

use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};
use protopyne_types::{ContractId, Money};
use serde::{Deserialize, Serialize};

use crate::broker::BrokerCapabilities;
use crate::order::{Fill, OrderRecord, OrderSide};

/// One account's headline facts at an observed instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One account's headline facts at an observed instant.
pub struct AccountSummary {
    /// Broker account identifier.
    pub account: String,
    /// Venue that answered.
    pub venue: String,
    /// Account currency tag.
    pub currency: String,
    /// Net liquidation value.
    pub net_liquid: Money,
    /// Buying power.
    pub buying_power: Money,
    /// Margin currently used.
    pub margin_used: Money,
    /// When the broker read was observed.
    pub observed_at: DateTime<Utc>,
}

/// One broker deal: an execution fact, wrapping the existing [`Fill`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One broker deal: an execution fact, wrapping the existing [`Fill`].
pub struct BrokerDeal {
    /// Broker deal/execution identifier.
    pub deal_id: String,
    /// Client identifier the originating order carried, when known.
    pub client_id: Option<String>,
    /// Broker order identifier, when known.
    pub order_id: Option<String>,
    /// Executed contract, when the deal names one.
    pub contract: Option<ContractId>,
    /// Underlying symbol.
    pub underlying: String,
    /// The fill's side.
    pub side: OrderSide,
    /// The fill itself: quantity, side, price, fee.
    pub fill: Fill,
    /// When the broker executed the deal.
    pub executed_at: DateTime<Utc>,
    /// When this read observed the deal.
    pub observed_at: DateTime<Utc>,
}

/// Optional read window and page bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
/// Optional read window and page bound.
pub struct AccountRange {
    /// Inclusive execution timestamp lower bound.
    pub start: Option<DateTime<Utc>>,
    /// Exclusive execution timestamp upper bound.
    pub end: Option<DateTime<Utc>>,
    /// Maximum rows to return.
    pub limit: Option<usize>,
}

impl AccountRange {
    /// Unbounded read.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// Whether `at` falls inside the window.
    #[must_use]
    pub fn contains(&self, at: DateTime<Utc>) -> bool {
        self.start.is_none_or(|start| at >= start) && self.end.is_none_or(|end| at < end)
    }

    /// Apply the window and limit to an ascending row list.
    #[must_use]
    pub fn window<T>(&self, rows: impl IntoIterator<Item = (DateTime<Utc>, T)>) -> Vec<T> {
        let mut kept: Vec<T> = rows
            .into_iter()
            .filter(|(at, _)| self.contains(*at))
            .map(|(_, row)| row)
            .collect();
        if let Some(limit) = self.limit {
            kept.truncate(limit);
        }
        kept
    }
}

/// Account-read failure with the same terminal/transient split as the rest
/// of the venue.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
/// Account-read failure with the same terminal/transient split as the rest
/// of the venue.
pub enum AccountError {
    /// The broker does not implement this surface.
    #[error("unsupported broker read: {0}")]
    Unsupported(String),
    /// A terminal broker refusal.
    #[error("terminal account read failure: {0}")]
    Terminal(String),
    /// A retryable transport/venue failure.
    #[error("transient account read failure: {0}")]
    Transient(String),
    /// A payload that cannot be interpreted.
    #[error("malformed account payload: {0}")]
    Malformed(String),
}

impl AccountError {
    /// Whether attempting again may help.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

/// Boxed future for object-safe account reads.
pub type AccountFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Read-only broker account surface. A broker that cannot answer a method
/// returns [`AccountError::Unsupported`], never an empty list.
pub trait AccountReader: Send + Sync {
    /// Which read surfaces this broker implements.
    fn capabilities(&self) -> BrokerCapabilities;

    /// Account identifiers served by this broker.
    fn accounts(&self) -> AccountFuture<'_, Result<Vec<String>, AccountError>>;

    /// Headline facts for one account.
    fn summary<'a>(
        &'a self,
        account: &'a str,
    ) -> AccountFuture<'a, Result<AccountSummary, AccountError>>;

    /// Orders, optionally windowed.
    fn orders<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<OrderRecord>, AccountError>>;

    /// Working orders, optionally windowed.
    fn open_orders<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<OrderRecord>, AccountError>>;

    /// Executions (deals), optionally windowed.
    fn deals<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<BrokerDeal>, AccountError>>;
}