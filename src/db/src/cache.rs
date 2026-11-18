//! Generic TTL + write-order FIFO key-value cache store (D7).
//!
//! `SqliteTtlCache` is the canonical home for the TTL/FIFO SQLite cache
//! shape that `knowledge::QueryCache` hand-rolled (open, get-with-expiry,
//! put, evict_expired, evict_fifo, stats). Eviction is insertion-order
//! FIFO, not LRU: reads never refresh recency. The `query_cache` table
//! DDL and all eviction SQL move here verbatim so existing `.db` files
//! keep working unchanged and behavior stays byte-identical.
//!
//! Key derivation is parameterized via `hash_key: fn(&str) -> String` so
//! consumer-specific key schemes (e.g. knowledge's lowercase-`fnv1a64`)
//! remain configurable; the default is the identity function.

use std::path::Path;

use common_core::time::now_secs;

use crate::error::DbError;
use crate::store::SqliteStore;

/// Table DDL — kept under the historical `query_cache` name so databases
/// written by the old in-crate implementation keep working unchanged.
const QUERY_CACHE_DDL: &str = "CREATE TABLE IF NOT EXISTS query_cache (
    key TEXT PRIMARY KEY,
    result_json TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    ttl_seconds INTEGER NOT NULL
)";

/// A single cached entry.
#[derive(Debug, Clone)]
/// A single cached entry.
pub struct Entry {
    /// Stored key (post-`hash_key` derivation).
    pub key: String,
    /// The cached result payload.
    pub result_summary: String,
    /// Unix timestamp of insertion.
    pub timestamp: u64,
    /// Time-to-live in seconds (`0` means immediately expired).
    pub ttl_seconds: u64,
}

/// Default key derivation: store the raw key.
fn identity_key(key: &str) -> String {
    key.to_string()
}

/// A TTL + write-order FIFO key-value store over a single SQLite connection.
///
/// Reads never refresh recency: eviction is insertion-order FIFO, not
/// LRU. The TTL half is task-value gated (a cache hit is outcome
/// freshness, never a confidence endorsement).
///
/// Reads never refresh recency: eviction is insertion-order FIFO, not LRU.
pub struct SqliteTtlCache {
    store: SqliteStore,
    default_ttl_seconds: u64,
    max_entries: usize,
    hash_key: fn(&str) -> String,
}

/// Deprecated alias: use [`SqliteTtlCache`] instead.
#[deprecated(note = "use SqliteTtlCache instead")]
pub type TtlCache = SqliteTtlCache;

impl std::fmt::Debug for SqliteTtlCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteTtlCache")
            .field("default_ttl_seconds", &self.default_ttl_seconds)
            .field("max_entries", &self.max_entries)
            .finish_non_exhaustive()
    }
}

/// Default entry cap when the caller does not set one explicitly.
pub const DEFAULT_TTL_CACHE_CAP: usize = 4096;

impl SqliteTtlCache {
    /// Open (or create) a cache at `path` with the default (identity) key
    /// derivation and the default entry cap. Expired entries are evicted
    /// on open. Refine with [`SqliteTtlCache::with_key`]/[`SqliteTtlCache::with_cap`].
    pub fn open(path: &Path, default_ttl_seconds: u64) -> Result<Self, DbError> {
        Self::open_inner(SqliteStore::open(path)?, default_ttl_seconds)
    }

    /// Open an in-memory cache with the default key derivation and cap.
    /// Refine with [`TtlCache::with_key`]/[`TtlCache::with_cap`].
    pub fn open_in_memory(default_ttl_seconds: u64) -> Result<Self, DbError> {
        Self::open_inner(SqliteStore::open_in_memory()?, default_ttl_seconds)
    }

    fn open_inner(store: SqliteStore, default_ttl_seconds: u64) -> Result<Self, DbError> {
        store.init_schema(QUERY_CACHE_DDL)?;
        let cache = Self {
            store,
            default_ttl_seconds,
            max_entries: DEFAULT_TTL_CACHE_CAP,
            hash_key: identity_key,
        };
        cache.evict_expired()?;
        Ok(cache)
    }

    /// Use a consumer-provided key derivation instead of the identity
    /// default (e.g. knowledge's lowercase-`fnv1a64`).
    #[must_use]
    pub fn with_key(mut self, hash_key: fn(&str) -> String) -> Self {
        self.hash_key = hash_key;
        self
    }

    /// Bound the entry count (evicted oldest-first on `put` past the cap).
    #[must_use]
    pub fn with_cap(mut self, max_entries: usize) -> Self {
        self.max_entries = max_entries;
        self
    }

    /// Fetch a live entry for `key`. An expired entry (TTL `0`, or
    /// `now > timestamp + ttl`) is deleted and treated as a miss.
    pub fn get(&self, key: &str) -> Result<Option<Entry>, DbError> {
        let hashed = (self.hash_key)(key);
        let entry = self.store.query_row(
            "SELECT key, result_json, timestamp, ttl_seconds FROM query_cache WHERE key = ?1",
            rusqlite::params![hashed],
            |row| {
                Ok(Entry {
                    key: row.get(0)?,
                    result_summary: row.get(1)?,
                    timestamp: row.get(2)?,
                    ttl_seconds: row.get(3)?,
                })
            },
        )?;
        match entry {
            Some(entry) => {
                let now = now_secs();
                if entry.ttl_seconds == 0 || now > entry.timestamp + entry.ttl_seconds {
                    self.store.execute(
                        "DELETE FROM query_cache WHERE key = ?1",
                        rusqlite::params![entry.key],
                    )?;
                    return Ok(None);
                }
                Ok(Some(entry))
            }
            None => Ok(None),
        }
    }

    /// Insert (or replace) `result` under `key`, then evict oldest entries
    /// when over capacity.
    pub fn put(&self, key: &str, result: &str) -> Result<(), DbError> {
        let hashed = (self.hash_key)(key);
        let now = now_secs();
        self.store.execute(
            "INSERT OR REPLACE INTO query_cache (key, result_json, timestamp, ttl_seconds) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![hashed, result, now, self.default_ttl_seconds],
        )?;
        self.evict_fifo()?;
        Ok(())
    }

    /// Remove all expired entries (TTL `0`, or `now > timestamp + ttl`).
    /// Returns the number of rows deleted.
    pub fn evict_expired(&self) -> Result<usize, DbError> {
        let now = now_secs();
        self.store.execute(
            "DELETE FROM query_cache WHERE ttl_seconds = 0 OR ?1 > timestamp + ttl_seconds",
            rusqlite::params![now],
        )
    }

    /// Evict the oldest entries (by write timestamp — insertion-order FIFO,
    /// never recency: reads do not refresh) when over capacity.
    /// Returns the number of rows deleted.
    pub fn evict_fifo(&self) -> Result<usize, DbError> {
        let count: usize = self
            .store
            .query_row("SELECT COUNT(*) FROM query_cache", &[], |row| row.get(0))?
            .unwrap_or(0);
        if count > self.max_entries {
            let excess = count - self.max_entries;
            self.store.execute(
                "DELETE FROM query_cache WHERE key IN (
                    SELECT key FROM query_cache ORDER BY timestamp ASC LIMIT ?1
                )",
                rusqlite::params![excess],
            )
        } else {
            Ok(0)
        }
    }

    /// Deprecated alias: use [`SqliteTtlCache::evict_fifo`] instead.
    #[deprecated(note = "use evict_fifo instead")]
    pub fn evict_oldest(&self) -> Result<usize, DbError> {
        self.evict_fifo()
    }

    /// Remove every entry. Returns the number of rows deleted.
    pub fn clear(&self) -> Result<usize, DbError> {
        self.store.execute("DELETE FROM query_cache", &[])
    }

    /// `(total entries, default ttl seconds, currently-expired entries)`.
    pub fn stats(&self) -> Result<(usize, u64, usize), DbError> {
        let count: usize = self
            .store
            .query_row("SELECT COUNT(*) FROM query_cache", &[], |row| row.get(0))?
            .unwrap_or(0);
        let expired: usize = {
            let now = now_secs();
            self.store
                .query_row(
                    "SELECT COUNT(*) FROM query_cache WHERE ?1 > timestamp + ttl_seconds",
                    rusqlite::params![now],
                    |row| row.get(0),
                )?
                .unwrap_or(0)
        };
        Ok((count, self.default_ttl_seconds, expired))
    }
}

#[cfg(all(test, feature = "sqlite"))]
#[path = "../tests/cache.rs"]
mod tests;
