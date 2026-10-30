//! Durable instrument identity: markets, scales, and the registration rule.
//!
//! [`Market`] is the store's counterpart to `types :: symbol :: SymbolIndex`
//! entries: a bound-checked identity that refuses instead of evicting. A
//! [`SymbolId`](crate::key::SymbolId) names an instrument across restarts,
//! exports, and reruns — allocated by the registry (`next_symbol_id`) and
//! remembered in its in-memory index. `resolve` reads that index and never
//! creates; `register` is the only creator.
//!
//! This module holds the shared vocabulary — [`Market`], [`AssetClass`],
//! [`PriceScale`], the [`check_register`] rule every registration path
//! applies, and the [`check_scale`] representability check every append
//! path applies — while [`MarketRegistry`](crate::registry::MarketRegistry)
//! holds the persisted table. Nothing here touches a database.

use fluent_wvr::prelude::*;
use fluent_wvr::validate::Validate;
use protopyne_types::{Bar, Timeframe, MAX_DECIMAL_SCALE};
use rust_decimal::Decimal;

use crate::error::{PlanError, StoreError};
use crate::key::SymbolId;

/// Capacity bound of a fresh catalog: far above the single-operator
/// universe while keeping a misbehaving producer from growing the table
/// without bound. Mirrors `types :: symbol :: DEFAULT_MAX_SYMBOLS`.
pub const DEFAULT_MAX_MARKETS: usize = 4_096;

/// What kind of instrument a market row names. Stored as lowercase text;
/// anything else in the column is a corrupt row, refused at
/// materialization rather than defaulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetClass {
    /// Equities and equity ETFs.
    Stock,
    /// Listed options (stored per underlying tape, never as chains).
    Option,
    /// Futures contracts.
    Future,
    /// Digital assets.
    Crypto,
}

impl AssetClass {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stock => "stock",
            Self::Option => "option",
            Self::Future => "future",
            Self::Crypto => "crypto",
        }
    }

    /// Parse a stored spelling. `None` is a corrupt row, never a default.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "stock" => Some(Self::Stock),
            "option" => Some(Self::Option),
            "future" => Some(Self::Future),
            "crypto" => Some(Self::Crypto),
            _ => None,
        }
    }
}

/// Decimal digits a market's prices may carry past the point: the
/// representability bound the append path enforces.
///
/// A price fits when its stored form — [`Decimal::normalize` text, which is
/// what the tape keeps — carries no more fraction digits than this. Trailing
/// zeros carry no information, so `100.50` fits a scale of 1 exactly as
/// `100.5` does; the check never refuses on representation. Construction
/// through [`PriceScale::of`] validates; the builder form exists for the
/// schema-driven surface and must pass [`Validate::validate`] before use —
/// every registration validates, so an unvalidated builder value cannot
/// reach the table.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    bon::Builder,
    FieldAccess,
    Describable,
)]
#[builder(start_fn = new)]
pub struct PriceScale {
    /// Decimal digits a market price may carry past the point.
    #[field(
        desc = "Decimal digits a market price may carry past the point",
        min = 0,
        max = 28
    )]
    #[builder(default = 2)]
    pub digits: u32,
}

impl PriceScale {
    /// Build a validated scale: at most 28 digits, the [`Decimal`] maximum.
    pub fn of(digits: u32) -> Result<Self, String> {
        let scale = Self { digits };
        scale.validate()?;
        Ok(scale)
    }

    /// Whether `value` is representable at this scale. The normalized form
    /// is what storage keeps, so it is also what the check reads — what you
    /// check is what you store.
    #[must_use]
    pub fn fits(&self, value: Decimal) -> bool {
        value.normalize().scale() <= self.digits
    }
}

impl Validate for PriceScale {
    type Err = String;

    fn validate(&self) -> Result<(), Self::Err> {
        // (M8.2) The one 28-digit ceiling lives in `protopyne-types`
        // (`MAX_DECIMAL_SCALE`); this check names it, never a literal.
        if self.digits > MAX_DECIMAL_SCALE {
            return Err(format!(
                "price scale {} exceeds the Decimal maximum of {MAX_DECIMAL_SCALE} digits",
                self.digits
            ));
        }
        Ok(())
    }
}

/// One registered instrument: the durable row behind a [`SymbolId`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Market {
    /// The durable identity allocated at registration.
    pub symbol_id: SymbolId,
    /// The instrument's spelling (e.g. `"SPY"`).
    pub symbol: String,
    /// Where the tape prints (empty when the tape is venue-agnostic).
    pub venue: String,
    /// What kind of instrument this is.
    pub asset_class: AssetClass,
    /// Decimal digits the market's prices may carry. Checked structurally
    /// here; the representability policy is enforced at append.
    pub price_scale: PriceScale,
    /// The bar spacing this tape holds. One spacing per symbol: rows are
    /// keyed `(symbol_id, ts_ms)` with no timeframe column, so two spacings
    /// would collide on the primary key. Coarser spacings fold on read;
    /// finer or non-multiple spacings refuse. Durable from schema V2, so the
    /// read planner knows the stored spacing across restarts.
    pub timeframe: Timeframe,
    /// IANA zone name for session interpretation (`"UTC"` default).
    pub time_zone: String,
}

impl fluent_wvr::validate::Validate for Market {
    type Err = String;

    fn validate(&self) -> Result<(), Self::Err> {
        if self.symbol.is_empty() {
            return Err("market symbol must be nonempty".to_owned());
        }
        if self.time_zone.is_empty() {
            return Err("market time_zone must be nonempty".to_owned());
        }
        Ok(())
    }
}

/// Refuse a bar the market cannot represent: every price must fit the
/// market's scale. The first offender names the refusal, with the symbol,
/// the price as carried, and the scale it failed — never rounded, never
/// truncated, and the reason rides the error, not a log line the caller
/// never sees.
pub(crate) fn check_scale(market: &Market, bar: &Bar) -> Result<(), StoreError> {
    for price in [bar.o, bar.h, bar.l, bar.c] {
        if !market.price_scale.fits(price) {
            return Err(StoreError::PriceScale {
                symbol: market.symbol.clone(),
                value: price,
                scale: market.price_scale.digits,
            });
        }
    }
    Ok(())
}

/// Decide a registration before touching any state, so every registration
/// path refuses identically without two implementations of the rule.
pub(crate) fn check_register(
    symbol: &str,
    venue: &str,
    scale: PriceScale,
    exists: bool,
    count: usize,
    max: usize,
) -> Result<(), StoreError> {
    if symbol.is_empty() {
        return Err(StoreError::Plan(PlanError::BlankSymbol));
    }
    scale.validate().map_err(StoreError::Config)?;
    if exists {
        return Err(StoreError::Duplicate(format!(
            "market already registered for ({symbol:?}, {venue:?})"
        )));
    }
    if count >= max {
        return Err(StoreError::CatalogFull(format!(
            "catalog holds {count} of {max} markets: refusing {symbol:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/common/scale.rs"]
pub(crate) mod test_scale;
