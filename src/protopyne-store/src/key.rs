//! Durable symbol identity.
//!
//! `types::symbol::SymKey` is a **run-scoped** interner handle: it means "row
//! 7 of the table this run happened to build". A store's [`SymbolId`] is
//! **durable**: it means "this instrument, in this catalog, since it was
//! registered", and it survives a restart, an export, and a rerun.
//!
//! Both are `u32`-shaped, which is exactly why they are separate types. There
//! is deliberately no `From<u32>`, no `From<SymbolId> for SymKey`, and no
//! `From<SymKey> for SymbolId`: a function that accepted either type would
//! launder a run-scoped handle into a durable record with no complaint from
//! the compiler, and the resulting run would pin provenance to whatever row
//! happened to sit at that index in a different table.

use protopyne_types::Timeframe;
use serde::{Deserialize, Serialize};

/// One instrument's durable identity within a store's catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// One instrument's durable identity within a store's catalog.
pub struct SymbolId(pub u32);

/// One tape: one instrument at one bar spacing.
///
/// A store file holds one timeframe per symbol — rows are keyed
/// `(symbol_id, ts_ms)` with no timeframe column, so two spacings for one
/// symbol would collide on the primary key. The key names the spacing the
/// caller wrote and reads at; coarser spacings are derived on read by
/// folding, never stored twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// One tape: one instrument at one bar spacing.
/// 
/// A store file holds one timeframe per symbol — rows are keyed
/// `(symbol_id, ts_ms)` with no timeframe column, so two spacings for one
/// symbol would collide on the primary key. The key names the spacing the
/// caller wrote and reads at; coarser spacings are derived on read by
/// folding, never stored twice.
pub struct TapeKey {
    /// The instrument, resolved through the catalog.
    pub symbol_id: SymbolId,
    /// The bar spacing of this tape.
    pub timeframe: Timeframe,
}

#[cfg(test)]
#[path = "../tests/key_newtype.rs"]
mod tests;