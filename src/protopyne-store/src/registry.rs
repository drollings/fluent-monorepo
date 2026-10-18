//! File-backed market registry: instrument identity without SQL.
//!
//! [`MarketRegistry`] is the durable counterpart to `types :: symbol ::
//! SymbolIndex`: a bound-checked interner that refuses instead of evicting.
//! It carries `(symbol, venue)` keyed rows with a durable [`SymbolId`](crate::key::SymbolId),
//! asset class, price scale, time zone, and the one stored timeframe — plus
//! the `next_symbol_id` allocator, persisted as `<root>/catalog.json` with
//! the atomic-write pattern from [`common_core::io::write_atomic`].
//!
//! Every mutating or I/O method gates on the registry's own permit, minted
//! at open and bound to the registry's root — the same shape as the
//! journal's `VenueStorage` gate, so a task never granted the store is
//! denied, not defaulted. [`MarketRegistry::resolve`] and
//! [`MarketRegistry::tape_of`] are the deliberate exceptions: they read the
//! in-memory index without touching the filesystem, so there is no effect
//! to gate and they take no capability.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use protopyne_types::Timeframe;

use crate::capability::{check_present, StoreStorage};
use crate::catalog::{check_register, AssetClass, Market, PriceScale};
use crate::error::StoreError;
use crate::key::SymbolId;

/// File name of the persisted catalog inside a store root.
pub const CATALOG_FILE: &str = "catalog.json";

/// The durable catalog document: every market plus the id allocator.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct CatalogDoc {
    /// Every registered market, in id order.
    #[serde(default)]
    markets: Vec<Market>,
    /// The next [`SymbolId`] to allocate. Starts at 1, matching the SQL
    /// catalog's autoincrement, so an export reads identically.
    #[serde(default = "first_symbol_id")]
    next_symbol_id: u32,
}

fn first_symbol_id() -> u32 {
    1
}

/// The file-backed catalog: one JSON document, one in-memory index, one bound.
///
/// The index is loaded once at open and updated on every successful
/// `register`. It is a cache over the document, not a second truth: a
/// single-writer discipline keeps the cache coherent, and every registration
/// persists before returning.
pub struct MarketRegistry {
    root: PathBuf,
    state: Mutex<CatalogDoc>,
    max_markets: usize,
    permit: StoreStorage,
}

impl MarketRegistry {
    /// Open (or create) the registry at `root`, loading `<root>/catalog.json`
    /// when it exists. Capacity is [`DEFAULT_MAX`](crate::catalog::DEFAULT_MAX_MARKETS).
    pub fn open(root: &Path) -> Result<Self, StoreError> {
        Self::open_with(root, crate::catalog::DEFAULT_MAX_MARKETS, false)
    }

    /// Open an ephemeral registry with no backing file. For tests and
    /// scratch work; nothing persists past the handle.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::open_with(
            Path::new(":memory:"),
            crate::catalog::DEFAULT_MAX_MARKETS,
            true,
        )
    }

    /// Open an ephemeral registry with an explicit market bound, for the
    /// full-table refusal pin. A bound of zero refuses every registration,
    /// including the first.
    pub fn open_in_memory_with_max(max_markets: usize) -> Result<Self, StoreError> {
        Self::open_with(Path::new(":memory:"), max_markets, true)
    }

    fn open_with(root: &Path, max_markets: usize, memory: bool) -> Result<Self, StoreError> {
        let doc = if memory {
            CatalogDoc::default()
        } else {
            std::fs::create_dir_all(root).map_err(common_core::error::IoError::from)?;
            load_doc(&root.join(CATALOG_FILE))?
        };
        let market_count = doc.markets.len();
        let permit = StoreStorage::new(if memory {
            PathBuf::from(":memory:")
        } else {
            root.to_path_buf()
        });
        tracing::info!(
            path = %permit.store_path().display(),
            markets = market_count,
            "market registry opened"
        );
        Ok(Self {
            root: permit.store_path().to_path_buf(),
            state: Mutex::new(doc),
            max_markets,
            permit,
        })
    }

    /// The permit this registry gates on. Held by the tape so its own gate
    /// checks the same binding without minting a second token.
    pub(crate) fn permit(&self) -> &StoreStorage {
        &self.permit
    }

    /// The capacity bound of this registry.
    #[must_use]
    pub fn max_markets(&self) -> usize {
        self.max_markets
    }

    /// Look up `(symbol, venue)` in the in-memory index. Reads, never
    /// creates: unknown is `Ok(None)`, not an error. No filesystem touch, so
    /// no capability check — there is no effect to gate.
    pub fn resolve(&self, symbol: &str, venue: &str) -> Result<Option<SymbolId>, StoreError> {
        Ok(self.tape_of(symbol, venue).map(|(symbol_id, _)| symbol_id))
    }

    /// Look up `(symbol, venue)` with the tape's stored spacing attached.
    /// The planner reads this, never the rows: support for a requested
    /// spacing is decided from the registry's durable fact, not inferred
    /// from whatever spacing the stored rows happen to show. Like
    /// [`resolve`](Self::resolve), this touches no filesystem and gates
    /// nothing.
    pub fn tape_of(&self, symbol: &str, venue: &str) -> Option<(SymbolId, Timeframe)> {
        common_core::sync::lock(&self.state)
            .markets
            .iter()
            .find(|market| market.symbol == symbol && market.venue == venue)
            .map(|market| (market.symbol_id, market.timeframe))
    }

    /// Register a new market, allocating its durable id and persisting the
    /// catalog before returning. Refuses a blank symbol, an unrepresentable
    /// scale, a duplicate `(symbol, venue)`, and a full catalog — fail-closed
    /// in that order, never by evicting. The spacing is fixed at
    /// registration: one spacing per symbol, so a later append at any other
    /// spacing refuses rather than mixing two grids on one key.
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
        let mut state = common_core::sync::lock(&self.state);
        check_register(
            symbol,
            venue,
            price_scale,
            state
                .markets
                .iter()
                .any(|market| market.symbol == symbol && market.venue == venue),
            state.markets.len(),
            self.max_markets,
        )?;
        let mut next = state.next_symbol_id.max(1);
        let symbol_id = SymbolId(next);
        // Saturating: the allocator never wraps past u32::MAX back to a live
        // id. Reaching the ceiling refuses downstream as a duplicate rather
        // than recycling an identity.
        next = next.saturating_add(1).max(1);
        state.next_symbol_id = next;
        state.markets.push(Market {
            symbol_id,
            symbol: symbol.to_owned(),
            venue: venue.to_owned(),
            asset_class,
            price_scale,
            timeframe,
            time_zone: time_zone.to_owned(),
        });
        state.markets.sort_by_key(|market| market.symbol_id);
        let doc = state.clone();
        drop(state);
        self.persist(&doc)?;
        Ok(symbol_id)
    }

    /// Every registered market, in id order. Gated: it reads the persisted
    /// document's cache, which is only trustworthy for a granted task.
    pub fn markets(&self) -> Result<Vec<Market>, StoreError> {
        check_present(&self.permit)?;
        let mut markets = common_core::sync::lock(&self.state).markets.clone();
        markets.sort_by_key(|market| market.symbol_id);
        Ok(markets)
    }

    /// Every registered market, in id order, without gating. Setup-time
    /// only (store open replays every tape): every serving or mutating
    /// operation gates through [`markets`](Self::markets) instead.
    pub(crate) fn markets_unchecked(&self) -> Vec<Market> {
        let mut markets = common_core::sync::lock(&self.state).markets.clone();
        markets.sort_by_key(|market| market.symbol_id);
        markets
    }

    /// Look up one market by id. `None` when no market carries `id`. The
    /// read is infallible — it scans the in-memory index — so there is no
    /// `Result`; the caller gates before calling.
    pub(crate) fn market_by_id(&self, id: SymbolId) -> Option<Market> {
        common_core::sync::lock(&self.state)
            .markets
            .iter()
            .find(|market| market.symbol_id == id)
            .cloned()
    }

    /// Persist the catalog document atomically: encode, write to a temp
    /// sibling, fsync, rename. Readers always observe the old or the new
    /// document, never a mix; a crash before the rename leaves the old file
    /// intact, a crash after it leaves the new file intact.
    fn persist(&self, doc: &CatalogDoc) -> Result<(), StoreError> {
        if self.root.as_os_str() == ":memory:" {
            return Ok(());
        }
        let text = serde_json::to_string_pretty(doc).map_err(|e| {
            StoreError::InvalidBar(format!("registry document does not serialize: {e}"))
        })?;
        common_core::io::write_atomic(&self.root.join(CATALOG_FILE), text.as_bytes())
            .map_err(|e| StoreError::Io(e.into()))?;
        Ok(())
    }
}

/// Load the catalog document, or an empty one when no file exists yet. A
/// file that exists but does not parse refuses loudly — falling back to an
/// empty default would silently orphan every previously registered market.
fn load_doc(path: &Path) -> Result<CatalogDoc, StoreError> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CatalogDoc::default()),
        Err(e) => Err(StoreError::Io(common_core::error::IoError::from(e))),
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            StoreError::InvalidBar(format!(
                "registry document {} is corrupt: {e}",
                path.display()
            ))
        }),
    }
}

impl std::fmt::Debug for MarketRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarketRegistry")
            .field("permit", &self.permit)
            .field("max_markets", &self.max_markets)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "../tests/common/registry_setup.rs"]
pub(crate) mod test_setup;

#[cfg(test)]
#[path = "../tests/registry.rs"]
mod tests;
