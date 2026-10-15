//! Durable instrument identity: the market catalog.
//!
//! [`MarketCatalog`] is the store's counterpart to `types :: symbol ::
//! SymbolIndex`: a bound-checked interner that refuses instead of evicting.
//! The differences are what durability requires. A `SymKey` is a run-scoped
//! row handle; a [`SymbolId`](crate::key::SymbolId) names an instrument
//! across restarts, exports, and reruns, so it is allocated by the database
//! (`INTEGER PRIMARY KEY`) and remembered in a private in-memory index that
//! is loaded once at open. `resolve` reads that index and never creates;
//! `register` is the only creator.
//!
//! Every mutating or I/O method gates on the catalog's own permit, minted
//! at open and bound to the catalog's path — the same shape as the
//! journal's `VenueStorage` gate, so a task never granted the store is
//! denied, not defaulted. [`MarketCatalog::resolve`] is the deliberate
//! exception: it reads the in-memory index without touching the database,
//! so there is no effect to gate and it takes no capability.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fluent_wvr::prelude::*;
use protopyne_types::{Timeframe, Validate};
use rusqlite::{Connection, params};
use rust_decimal::Decimal;

use crate::capability::{StoreStorage, check_present};
use crate::error::{PlanError, StoreError};
use crate::key::SymbolId;
use crate::migrate::ensure_schema;

/// Capacity bound of a fresh catalog: far above the single-operator
/// universe while keeping a misbehaving producer from growing the table
/// without bound. Mirrors `types :: symbol :: DEFAULT_MAX_SYMBOLS`.
pub const DEFAULT_MAX_MARKETS: usize = 4_096;

/// What kind of instrument a market row names. Stored as lowercase text;
/// anything else in the column is a corrupt row, refused at
/// materialization rather than defaulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
        if self.digits > 28 {
            return Err(format!(
                "price scale {} exceeds the Decimal maximum of 28 digits",
                self.digits
            ));
        }
        Ok(())
    }
}

/// One registered instrument: the durable row behind a [`SymbolId`].
#[derive(Debug, Clone, PartialEq, Eq)]
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

impl protopyne_types::Validate for Market {
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

/// Materialize one `market` row. Corrupt spellings and out-of-range
/// integers refuse here, at the boundary, so no caller ever sees a
/// defaulted or wrapped market.
pub(crate) fn market_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<Market> {
    fn bad(col: usize, what: String) -> rusqlite::Error {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Text,
            std::io::Error::new(std::io::ErrorKind::InvalidData, what).into(),
        )
    }
    let asset_text: String = row.get(3)?;
    let asset_class = AssetClass::parse(&asset_text)
        .ok_or_else(|| bad(3, format!("unknown asset_class {asset_text:?}")))?;
    let symbol_id = u32::try_from(row.get::<_, i64>(0)?)
        .map_err(|_| bad(0, "symbol_id out of u32 range".to_owned()))?;
    let scale_digits = u32::try_from(row.get::<_, i64>(4)?)
        .map_err(|_| bad(4, "price_scale out of u32 range".to_owned()))?;
    let price_scale = PriceScale::of(scale_digits)
        .map_err(|e| bad(4, format!("stored price_scale invalid: {e}")))?;
    let timeframe_text: String = row.get(6)?;
    let timeframe: Timeframe = timeframe_text
        .parse()
        .map_err(|_| bad(6, format!("unknown timeframe {timeframe_text:?}")))?;
    Ok(Market {
        symbol_id: SymbolId(symbol_id),
        symbol: row.get(1)?,
        venue: row.get(2)?,
        asset_class,
        price_scale,
        timeframe,
        time_zone: row.get(5)?,
    })
}

/// The full `market` row select, shared by every reader so the column order
/// cannot drift between them.
pub(crate) const SELECT_MARKET_SQL: &str =
    "SELECT symbol_id, symbol, venue, asset_class, price_scale, time_zone, timeframe FROM market";

/// Decide a registration before touching the database, so the
/// synchronous core and the async handle refuse identically without two
/// implementations of the rule.
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

/// The durable catalog: one connection, one in-memory index, one bound.
///
/// The index is loaded once at open and updated on every successful
/// `register`. It is a cache over the table, not a second truth: the
/// `UNIQUE (symbol, venue)` constraint backstops concurrent registrations,
/// and a single-writer discipline keeps the cache coherent.
pub struct MarketCatalog {
    conn: Mutex<Connection>,
    entries: Mutex<HashMap<(String, String), (SymbolId, Timeframe)>>,
    max_markets: usize,
    permit: StoreStorage,
}

impl MarketCatalog {
    /// Open (or create) the catalog at `path`, bringing the schema to
    /// current. Capacity is [`DEFAULT_MAX_MARKETS`].
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_with(path, DEFAULT_MAX_MARKETS, false)
    }

    /// Open an ephemeral catalog backed by a private in-memory database.
    /// For tests and scratch work; nothing persists past the handle.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::open_with(Path::new(":memory:"), DEFAULT_MAX_MARKETS, true)
    }

    /// Open with an explicit market bound, for the full-table refusal pin.
    /// A bound of zero refuses every registration, including the first.
    pub fn open_in_memory_with_max(max_markets: usize) -> Result<Self, StoreError> {
        Self::open_with(Path::new(":memory:"), max_markets, true)
    }

    fn open_with(path: &Path, max_markets: usize, memory: bool) -> Result<Self, StoreError> {
        let conn = if memory {
            fluent_db::sqlite::open_in_memory()?
        } else {
            fluent_db::sqlite::open_wal(path)?
        };
        ensure_schema(&conn)?;
        let stored: Vec<((String, String), (SymbolId, Timeframe))> =
            fluent_db::query::query_rows(&conn, SELECT_MARKET_SQL, [], |row| {
                let market = market_of(row)?;
                Ok((
                    (market.symbol, market.venue),
                    (market.symbol_id, market.timeframe),
                ))
            })?;
        let entries: HashMap<(String, String), (SymbolId, Timeframe)> =
            stored.into_iter().collect();
        let market_count = entries.len();
        let permit = StoreStorage::new(if memory {
            PathBuf::from(":memory:")
        } else {
            path.to_path_buf()
        });
        tracing::info!(
            path = %permit.store_path().display(),
            markets = market_count,
            "market catalog opened"
        );
        Ok(Self {
            conn: Mutex::new(conn),
            entries: Mutex::new(entries),
            max_markets,
            permit,
        })
    }

    /// The permit this catalog gates on. Held by the tape so its own gate
    /// checks the same binding without minting a second token.
    pub(crate) fn permit(&self) -> &StoreStorage {
        &self.permit
    }

    /// The catalog's connection, for the tape's candle statements. The tape
    /// owns the catalog, so this never escapes the crate.
    pub(crate) fn connection(&self) -> &Mutex<Connection> {
        &self.conn
    }

    /// The capacity bound of this catalog.
    #[must_use]
    pub fn max_markets(&self) -> usize {
        self.max_markets
    }

    /// Look up `(symbol, venue)` in the in-memory index. Reads, never
    /// creates: unknown is `Ok(None)`, not an error. No database touch, so
    /// no capability check — there is no effect to gate.
    pub fn resolve(&self, symbol: &str, venue: &str) -> Result<Option<SymbolId>, StoreError> {
        Ok(self.tape_of(symbol, venue).map(|(symbol_id, _)| symbol_id))
    }

    /// Look up `(symbol, venue)` with the tape's stored spacing attached.
    /// The planner reads this, never the rows: support for a requested
    /// spacing is decided from the catalog's durable fact, not inferred
    /// from whatever spacing the stored rows happen to show. Like
    /// [`resolve`](Self::resolve), this touches no database and gates
    /// nothing.
    pub fn tape_of(&self, symbol: &str, venue: &str) -> Option<(SymbolId, Timeframe)> {
        common_core::sync::lock(&self.entries)
            .get(&(symbol.to_owned(), venue.to_owned()))
            .copied()
    }

    /// Register a new market, allocating its durable id. Refuses a blank
    /// symbol, an unrepresentable scale, a duplicate `(symbol, venue)`, and
    /// a full catalog — fail-closed in that order, never by evicting. The
    /// spacing is fixed at registration: one spacing per symbol, so a later
    /// append at any other spacing refuses rather than mixing two grids on
    /// one primary key.
    pub fn register(
        &self,
        symbol: &str,
        venue: &str,
        asset_class: AssetClass,
        price_scale: PriceScale,
        time_zone: &str,
        timeframe: Timeframe,
    ) -> Result<SymbolId, StoreError> {
        check_present(&self.permit)?;
        let mut index = common_core::sync::lock(&self.entries);
        check_register(
            symbol,
            venue,
            price_scale,
            index.contains_key(&(symbol.to_owned(), venue.to_owned())),
            index.len(),
            self.max_markets,
        )?;
        let conn = common_core::sync::lock(&self.conn);
        // The constraint backstops a concurrent registration that slipped
        // past the index check: a unique violation is the same refusal, not
        // a new error.
        let id = match conn.execute(
            "INSERT INTO market(symbol, venue, asset_class, price_scale, time_zone, timeframe)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                symbol,
                venue,
                asset_class.as_str(),
                i64::from(price_scale.digits),
                time_zone,
                timeframe.code()
            ],
        ) {
            Ok(_) => conn.last_insert_rowid(),
            Err(e) if fluent_db::sqlite::is_unique_violation(&e) => {
                return Err(StoreError::Duplicate(format!(
                    "market already registered for ({symbol:?}, {venue:?})"
                )));
            }
            Err(e) => return Err(StoreError::from(e)),
        };
        let id = u32::try_from(id)
            .map_err(|_| StoreError::InvalidBar(format!("allocated symbol_id {id} exceeds u32")))?;
        let symbol_id = SymbolId(id);
        index.insert(
            (symbol.to_owned(), venue.to_owned()),
            (symbol_id, timeframe),
        );
        Ok(symbol_id)
    }

    /// Every registered market, in id order. Gated: it reads the database.
    pub fn markets(&self) -> Result<Vec<Market>, StoreError> {
        check_present(&self.permit)?;
        let conn = common_core::sync::lock(&self.conn);
        let mut markets = fluent_db::query::query_rows(&conn, SELECT_MARKET_SQL, [], market_of)?;
        markets.sort_by_key(|market| market.symbol_id);
        Ok(markets)
    }

    /// Look up one market row by id. `None` when no market carries `id` —
    /// the code-level referential check the append path runs, because pooled
    /// connections do not all carry the `foreign_keys` pragma. The caller
    /// gates; this performs the read.
    pub(crate) fn market_by_id(&self, id: SymbolId) -> Result<Option<Market>, StoreError> {
        let conn = common_core::sync::lock(&self.conn);
        let market = fluent_db::query::query_row(
            &conn,
            &format!("{SELECT_MARKET_SQL} WHERE symbol_id = ?1"),
            params![i64::from(id.0)],
            market_of,
        )?;
        Ok(market)
    }
}

impl std::fmt::Debug for MarketCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarketCatalog")
            .field("permit", &self.permit)
            .field("max_markets", &self.max_markets)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "../tests/catalog.rs"]
mod tests;
