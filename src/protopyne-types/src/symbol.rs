//! Scoped symbol registry: per-run string interning with `Copy` keys.
//!
//! Books and registries look symbols up on hot paths; comparing short
//! strings on every tick is wasteful and a process-global interner would
//! leak identity across parallel runs (breaking per-run isolation and
//! replay determinism). [`SymbolIndex`] is therefore owned by the
//! session/run that uses it: the paper book, the portfolio registry, and
//! the preview attribution fold each hold their own index, and keys never
//! leave it — serialization (journal rows, JSON, manifests) keeps strings.
//!
//! [`SymKey`] is opaque and [`Copy`]; it supports equality and hashing
//! only, never arithmetic.

use std::collections::HashMap;

use crate::error::TypesError;

/// Default capacity of a fresh [`SymbolIndex`]: far above the
/// single-operator universe (a handful of underlyings, strategies, and
/// portfolios) while keeping a misbehaving producer from growing the
/// table without bound.
pub const DEFAULT_MAX_SYMBOLS: usize = 4_096;

/// Opaque handle for one interned symbol. `Copy` for hot-path map keys;
/// no arithmetic by construction (there is no `Add`/`Sub`/`From<usize>`
/// impl, and the inner value is private).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Opaque handle for one interned symbol. `Copy` for hot-path map keys;
/// no arithmetic by construction (there is no `Add`/`Sub`/`From<usize>`
/// impl, and the inner value is private).
pub struct SymKey(u32);

impl SymKey {
    /// The table position behind this key, for diagnostics only.
    #[must_use]
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

/// Scoped string interner: `underlying`/`strategy`/portfolio names in,
/// [`SymKey`] out, resolved back only through the owning index.
#[derive(Debug)]
/// Scoped string interner: `underlying`/`strategy`/portfolio names in,
/// [`SymKey`] out, resolved back only through the owning index.
pub struct SymbolIndex {
    names: Vec<String>,
    lookup: HashMap<String, SymKey>,
    max_symbols: usize,
}

impl SymbolIndex {
    /// Open an empty index with [`DEFAULT_MAX_SYMBOLS`] capacity.
    #[must_use]
    pub fn new() -> Self {
        Self {
            names: Vec::new(),
            lookup: HashMap::new(),
            max_symbols: DEFAULT_MAX_SYMBOLS,
        }
    }

    /// Open with an explicit bound, stored verbatim. Zero refuses at
    /// `validate`/register time, never at construction.
    #[must_use]
    pub fn with_max_symbols(max_symbols: usize) -> Self {
        Self {
            names: Vec::new(),
            lookup: HashMap::new(),
            max_symbols,
        }
    }

    /// Capacity bound of this index.
    #[must_use]
    pub fn max_symbols(&self) -> usize {
        self.max_symbols
    }

    /// Intern `symbol`, returning its key. Re-interning the same spelling
    /// returns the same key. Refuses empty symbols and a full table
    /// fail-closed, never by evicting.
    pub fn intern(&mut self, symbol: &str) -> Result<SymKey, TypesError> {
        if symbol.is_empty() {
            return Err(TypesError::InvalidSymbol(
                "symbol must be nonempty".to_owned(),
            ));
        }
        if let Some(key) = self.lookup.get(symbol) {
            return Ok(*key);
        }
        if self.names.len() >= self.max_symbols {
            return Err(TypesError::TooManySymbols(format!(
                "symbol table full at {} entries; refusing '{symbol}'",
                self.max_symbols
            )));
        }
        let key = SymKey(self.names.len() as u32);
        self.names.push(symbol.to_owned());
        self.lookup.insert(symbol.to_owned(), key);
        Ok(key)
    }

    /// Look up a previously interned symbol without inserting. Unknown
    /// spellings are absence (`None`), never an implicit insert — reads
    /// must not grow the table.
    #[must_use]
    pub fn lookup(&self, symbol: &str) -> Option<SymKey> {
        self.lookup.get(symbol).copied()
    }

    /// Resolve a key back to its spelling. Keys from another index resolve
    /// positionally only when the spelling happens to coincide; indexes
    /// never share state, so cross-index keys are meaningless by
    /// construction and callers must not mix them.
    #[must_use]
    pub fn resolve(&self, key: SymKey) -> Option<&str> {
        self.names.get(key.0 as usize).map(String::as_str)
    }

    /// Count of interned symbols.
    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether any symbol is interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

impl Default for SymbolIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl fluent_wvr::validate::Validate for SymbolIndex {
    type Err = String;
    fn validate(&self) -> Result<(), Self::Err> {
        if self.max_symbols < 1 {
            return Err("max_symbols must be at least 1".to_owned());
        }
        if self.names.len() > self.max_symbols {
            return Err(format!(
                "symbol table holds {} entries over its bound of {}",
                self.names.len(),
                self.max_symbols
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/symbol.rs"]
mod tests;