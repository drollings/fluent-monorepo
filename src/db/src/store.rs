//! The canonical single-connection SQLite store.
//!
//! `SqliteStore` owns the connection lifecycle (open / WAL / schema init) and
//! exposes typed statement helpers (`query_row`/`query_rows`/`execute`/
//! `execute_batch`/`transaction`). It is the base shape that `GuidanceDb`,
//! `Library`, `ContentNodeStore`, `HolographicStore`, and the charts store all
//! collapse to — each of those hand-rolled `Mutex<Connection>` + `open_wal`
//! before this component existed.
//!
//! Locks are taken via `common_core::sync::lock` (poison-safe). A statement that
//! returns no rows maps to `Ok(None)` for `query_row` and `Ok(vec![])` for
//! `query_rows` — the exact shape consumers already re-implement inline.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use common_core::sync::lock;

use crate::error::DbError;

/// A single-connection SQLite store with WAL mode enabled.
///
/// The connection is wrapped in a `Mutex` because `rusqlite::Connection` is not
/// `Sync`; every operation serializes on it. Consumers that need to hold the lock
/// across multiple statements use `with_conn`/`with_conn_mut` and the typed
/// helpers inside one closure.
#[derive(Debug)]
/// A single-connection SQLite store with WAL mode enabled.
/// 
/// The connection is wrapped in a `Mutex` because `rusqlite::Connection` is not
/// `Sync`; every operation serializes on it. Consumers that need to hold the lock
/// across multiple statements use `with_conn`/`with_conn_mut` and the typed
/// helpers inside one closure.
pub struct SqliteStore {
    pub(crate) conn: Mutex<Connection>,
}

impl SqliteStore {
    /// Open (or create) the database at `path` with WAL mode enabled.
    pub fn open(path: &Path) -> Result<Self, DbError> {
        let conn = crate::sqlite::open_wal(path).map_err(DbError::from)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory database with WAL mode enabled.
    pub fn open_in_memory() -> Result<Self, DbError> {
        let conn = crate::sqlite::open_in_memory().map_err(DbError::from)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Execute a multi-statement DDL batch (schema init) on the connection.
    pub fn init_schema(&self, ddl: &str) -> Result<(), DbError> {
        crate::sqlite::run_batch(&lock(&self.conn), ddl).map_err(DbError::from)
    }

    /// Run a closure against the shared connection.
    ///
    /// The connection lock is held for the duration of the closure, so callers
    /// can run multiple statements atomically (from this thread's perspective).
    pub fn with_conn<R>(
        &self,
        f: impl FnOnce(&Connection) -> Result<R, DbError>,
    ) -> Result<R, DbError> {
        f(&lock(&self.conn))
    }

    /// Run a closure against the shared connection with `&mut` access.
    ///
    /// Needed for `Connection::transaction` and `Connection::prepare`-with-write
    /// shapes.
    pub fn with_conn_mut<R>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<R, DbError>,
    ) -> Result<R, DbError> {
        f(&mut lock(&self.conn))
    }

    /// Execute a DML statement and return the number of rows affected.
    pub fn execute(&self, sql: &str, params: &[&dyn rusqlite::ToSql]) -> Result<usize, DbError> {
        crate::query::execute(&lock(&self.conn), sql, params)
    }

    /// Execute a multi-statement SQL batch.
    pub fn execute_batch(&self, sql: &str) -> Result<(), DbError> {
        crate::query::execute_batch(&lock(&self.conn), sql)
    }

    /// Query at most one row.
    ///
    /// A statement that matches no rows returns `Ok(None)` (not an error).
    pub fn query_row<T>(
        &self,
        sql: &str,
        params: &[&dyn rusqlite::ToSql],
        map: impl FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Option<T>, DbError> {
        crate::query::query_row(&lock(&self.conn), sql, params, map)
    }

    /// Query all rows.
    ///
    /// A statement that matches no rows returns `Ok(vec![])`.
    pub fn query_rows<T>(
        &self,
        sql: &str,
        params: &[&dyn rusqlite::ToSql],
        map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>, DbError> {
        crate::query::query_rows(&lock(&self.conn), sql, params, map)
    }

    /// Run a closure inside a transaction.
    ///
    /// Commits on `Ok`, rolls back on `Err` (the transaction is dropped while
    /// uncommitted, which rolls it back).
    pub fn transaction<T>(
        &self,
        f: impl FnOnce(&mut rusqlite::Transaction<'_>) -> Result<T, DbError>,
    ) -> Result<T, DbError> {
        self.with_conn_mut(|conn| crate::query::transaction(conn, f))
    }

    /// Create the shared outbox table if missing (see `crate::outbox`).
    pub fn outbox_init(&self) -> Result<(), DbError> {
        self.with_conn(crate::outbox::init_schema)
    }

    /// Number of rows still in the shared outbox.
    pub fn outbox_pending(&self) -> Result<usize, DbError> {
        self.with_conn(crate::outbox::pending_count)
    }

    /// Delete delivered outbox rows; returns rows removed.
    pub fn outbox_ack(&self, ids: &[i64]) -> Result<usize, DbError> {
        self.with_conn(|conn| crate::outbox::ack(conn, ids))
    }

    /// Claim up to `limit` outbox rows in FIFO order, leasing them for
    /// `lease_ms` from `now_ms` (both milliseconds since the caller's epoch).
    ///
    /// Contended claims surface as `DbError::Busy` when two writers upgrade
    /// past each other's shared locks; those are retried with a short sleep
    /// (bounded) so concurrent claimants serialize instead of failing.
    pub fn outbox_claim(
        &self,
        limit: usize,
        lease_ms: i64,
        now_ms: i64,
    ) -> Result<Vec<crate::outbox::OutboxEntry>, DbError> {
        let mut attempts = 0u32;
        loop {
            match self.transaction(|tx| crate::outbox::claim_batch(tx, limit, lease_ms, now_ms)) {
                Err(DbError::Busy(_)) if attempts < 20 => {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                done => return done,
            }
        }
    }
}

#[cfg(all(test, feature = "sqlite"))]
#[path = "../tests/store.rs"]
mod tests;