//! Tastytrade account reads: balances, orders, and deals over the existing
//! authenticated venue. The mirror remains the sync truth; this adapter
//! serves the neutral read surface and names every refusal.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use protopyne_types::{ContractId, Money};
use rust_decimal::Decimal;

use crate::account::{
    AccountError, AccountFuture, AccountRange, AccountReader, AccountSummary, BrokerDeal,
};
use crate::broker::BrokerCapabilities;
use crate::live::TastytradeVenue;
use crate::order::{Fill, OrderRecord, OrderSide};
use crate::venue::Venue;

/// Account reads over a [`TastytradeVenue`].
pub struct TastytradeAccountReader {
    venue: Arc<TastytradeVenue>,
    account: String,
    currency: String,
}

impl TastytradeAccountReader {
    /// Bind a reader to one authenticated venue account.
    #[must_use]
    pub fn new(venue: Arc<TastytradeVenue>, account: impl Into<String>) -> Self {
        Self {
            venue,
            account: account.into(),
            currency: "USD".to_owned(),
        }
    }

    async fn get(&self, path: &str) -> Result<serde_json::Value, AccountError> {
        let lease = self
            .venue
            .token_manager()
            .lease()
            .await
            .map_err(|error| AccountError::Terminal(error.to_string()))?;
        let url = format!(
            "{}/accounts/{}{}",
            self.venue
                .transport()
                .config()
                .base_url
                .trim_end_matches('/'),
            self.account,
            path
        );
        let raw = self
            .venue
            .net()
            .http_get_with_headers(&url, &[("Authorization", lease.credential().expose())])
            .await
            .map_err(|error| AccountError::Transient(error.to_string()))?;
        let wire: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|error| AccountError::Malformed(format!("response is not JSON: {error}")))?;
        if let Some(error) = crate::tastytrade::envelope_error(&wire) {
            return Err(match error {
                crate::tastytrade::TransportError::Terminal(reason) => {
                    AccountError::Terminal(reason)
                }
                crate::tastytrade::TransportError::Transient(reason) => {
                    AccountError::Transient(reason)
                }
                crate::tastytrade::TransportError::Rest(reason) => AccountError::Transient(reason),
            });
        }
        Ok(wire)
    }
}

impl AccountReader for TastytradeAccountReader {
    fn capabilities(&self) -> BrokerCapabilities {
        BrokerCapabilities {
            streams: false,
            accounts: true,
            orders: true,
            deals: true,
            auth_refresh: true,
            native_bars: false,
        }
    }

    fn accounts(&self) -> AccountFuture<'_, Result<Vec<String>, AccountError>> {
        Box::pin(async move { Ok(vec![self.account.clone()]) })
    }

    fn summary<'a>(
        &'a self,
        account: &'a str,
    ) -> AccountFuture<'a, Result<AccountSummary, AccountError>> {
        Box::pin(async move {
            if account != self.account {
                return Err(AccountError::Terminal(format!(
                    "unknown account {account:?}: this reader serves {:?}",
                    self.account
                )));
            }
            let wire = self.get("/balances").await?;
            let (net_liquid, buying_power, margin_used) =
                summary_from_balances(&wire, &self.currency)?;
            Ok(AccountSummary {
                account: self.account.clone(),
                venue: "tastytrade".to_owned(),
                currency: self.currency.clone(),
                net_liquid,
                buying_power,
                margin_used,
                observed_at: Utc::now(),
            })
        })
    }

    fn orders<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<OrderRecord>, AccountError>> {
        Box::pin(async move {
            if account != self.account {
                return Err(AccountError::Terminal(format!(
                    "unknown account {account:?}"
                )));
            }
            Ok(range.window(
                self.venue
                    .orders()
                    .into_iter()
                    .map(|record| (record.updated_at, record)),
            ))
        })
    }

    fn open_orders<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<OrderRecord>, AccountError>> {
        Box::pin(async move {
            if account != self.account {
                return Err(AccountError::Terminal(format!(
                    "unknown account {account:?}"
                )));
            }
            Ok(range.window(
                self.venue
                    .fetch_open_orders()
                    .into_iter()
                    .map(|record| (record.updated_at, record)),
            ))
        })
    }

    fn deals<'a>(
        &'a self,
        account: &'a str,
        range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<BrokerDeal>, AccountError>> {
        Box::pin(async move {
            if account != self.account {
                return Err(AccountError::Terminal(format!(
                    "unknown account {account:?}"
                )));
            }
            let wire = self.get("/transactions").await?;
            deals_from_transactions(&wire, &self.account, range)
        })
    }
}

fn summary_from_balances(
    wire: &serde_json::Value,
    currency: &str,
) -> Result<(Money, Money, Money), AccountError> {
    let items = wire
        .get("data")
        .and_then(|data| data.get("items"))
        .and_then(serde_json::Value::as_array)
        .or_else(|| wire.get("items").and_then(serde_json::Value::as_array))
        .ok_or_else(|| AccountError::Malformed("balance wire names no items".to_owned()))?;
    let field = |item: &serde_json::Value, name: &str| -> Option<Decimal> {
        parse_decimal(item.get(name)?)
    };
    let line = items
        .iter()
        .find(|item| {
            item.get("currency")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| value == currency)
        })
        .or_else(|| items.first())
        .ok_or_else(|| AccountError::Malformed("balance wire holds no lines".to_owned()))?;
    let net_liquid = field(line, "net-liquid-balance")
        .or_else(|| field(line, "net-liquid"))
        .unwrap_or(Decimal::ZERO);
    let buying_power = field(line, "derivative-buying-power")
        .or_else(|| field(line, "buying-power"))
        .unwrap_or(net_liquid);
    let margin_used = field(line, "maintenance-requirement")
        .or_else(|| field(line, "margin-used"))
        .unwrap_or(Decimal::ZERO);
    Ok((
        Money::new(net_liquid),
        Money::new(buying_power),
        Money::new(margin_used),
    ))
}

fn deals_from_transactions(
    wire: &serde_json::Value,
    account: &str,
    range: AccountRange,
) -> Result<Vec<BrokerDeal>, AccountError> {
    let items = wire
        .get("data")
        .and_then(|data| data.get("items"))
        .and_then(serde_json::Value::as_array)
        .or_else(|| wire.get("items").and_then(serde_json::Value::as_array))
        .ok_or_else(|| AccountError::Malformed("transaction wire names no items".to_owned()))?;
    let observed_at = Utc::now();
    let mut rows = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let executed_at = parse_time(item, "executed-at")
            .or_else(|| parse_time(item, "transaction-date"))
            .unwrap_or(observed_at);
        let Some(price) = parse_money(item, "price") else {
            continue;
        };
        let quantity = item
            .get("quantity")
            .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
            .unwrap_or(0)
            .unsigned_abs() as u32;
        let side = match item
            .get("action")
            .or_else(|| item.get("side"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("buy")
            .to_ascii_lowercase()
            .as_str()
        {
            "sell" | "sell_to_open" | "sell_to_close" => OrderSide::Sell,
            _ => OrderSide::Buy,
        };
        let fill = Fill {
            quantity,
            price,
            fee: parse_money(item, "commission").unwrap_or_else(Money::zero),
        };
        rows.push((
            executed_at,
            BrokerDeal {
                deal_id: item
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(|| format!("{account}-{index}"), str::to_owned),
                client_id: item
                    .get("client-order-id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                order_id: item
                    .get("order-id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                contract: item
                    .get("symbol")
                    .and_then(serde_json::Value::as_str)
                    .map(|symbol| ContractId::new(symbol.to_owned())),
                underlying: item
                    .get("underlying-symbol")
                    .or_else(|| item.get("symbol"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                fill,
                side,
                executed_at,
                observed_at,
            },
        ));
    }
    Ok(range.window(rows))
}

fn parse_time(item: &serde_json::Value, name: &str) -> Option<DateTime<Utc>> {
    let text = item.get(name)?.as_str()?;
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn parse_money(item: &serde_json::Value, name: &str) -> Option<Money> {
    parse_decimal(item.get(name)?).map(Money::new)
}

fn parse_decimal(value: &serde_json::Value) -> Option<Decimal> {
    if let Some(text) = value.as_str() {
        return text.parse::<Decimal>().ok();
    }
    value.as_f64().and_then(Decimal::from_f64_retain)
}

#[cfg(test)]
#[path = "../tests/tastytrade_account.rs"]
mod tests_account;
