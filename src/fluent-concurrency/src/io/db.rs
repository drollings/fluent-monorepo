//! SQLite-backed database capability with async I/O via connection pool.
//!
//! Replaces the single `Arc<Mutex<Connection>>` with a lightweight pool of
//! `rusqlite::Connection` objects (default size 5) backed by `tokio::sync::Semaphore`.
//! This allows concurrent reads across the pool, and WAL mode is enabled so
//! SQLite can serve multiple readers concurrently without blocking.

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

use common_core::error::IoError;
use fluent_wvr::Capability;
use rusqlite::types::Value;
use rusqlite::Connection;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::io::check_capability;
use crate::io::CapabilityError;

/// Default pool size. SQLite with WAL mode can serve many concurrent readers,
/// but we keep the pool modest to avoid file-descriptor pressure.
const DEFAULT_POOL_SIZE: usize = 5;

/// A lightweight pool of `rusqlite::Connection` objects.
///
/// `Semaphore` gates access so at most `size` operations are in flight.
/// `Mutex<Vec<Connection>>` holds idle connections. Both are `std::sync`
/// primitives because the critical sections are tiny (push/pop) and the
/// heavy work is offloaded to `spawn_blocking` via `PooledConnection`.
struct Pool {
    connections: Mutex<Vec<Connection>>,
    semaphore: Arc<Semaphore>,
}

impl Pool {
    /// Opens `size` connections to the given path and enables WAL mode.
    fn open(path: &str, size: usize) -> Result<Self, rusqlite::Error> {
        let mut connections = Vec::with_capacity(size);
        for _ in 0..size {
            let conn = common_core::sqlite::open_wal(std::path::Path::new(path))?;
            connections.push(conn);
        }
        Ok(Self {
            connections: Mutex::new(connections),
            semaphore: Arc::new(Semaphore::new(size)),
        })
    }

    /// Acquires a connection from the pool.
    ///
    /// Returns `Err` if the pool is exhausted (should never happen with a
    /// properly sized semaphore) or the semaphore is closed.
    async fn get(self: Arc<Self>) -> Result<PooledConnection, IoError> {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(std::io::Error::other)?;
        let conn = {
            let mut connections = self.connections.lock().unwrap();
            connections.pop().ok_or_else(|| -> IoError {
                IoError(std::io::Error::from(CapabilityError::Exhausted {
                    name: "db",
                    detail: "all connections in use".into(),
                }))
            })?
        };
        Ok(PooledConnection {
            conn: Some(conn),
            pool: self,
            _permit: permit,
        })
    }

    /// Returns a connection to the idle set.
    fn put(&self, conn: Connection) {
        let mut connections = self.connections.lock().unwrap();
        connections.push(conn);
    }

    #[cfg(test)]
    fn empty() -> Self {
        Self {
            connections: Mutex::new(Vec::new()),
            semaphore: Arc::new(Semaphore::new(1)),
        }
    }
}

/// A connection checked out from the pool.
///
/// `Deref`/`DerefMut` expose the underlying `rusqlite::Connection`.
/// When dropped, the connection is automatically returned to the pool and the
/// semaphore permit is released.
struct PooledConnection {
    conn: Option<Connection>,
    pool: Arc<Pool>,
    _permit: OwnedSemaphorePermit,
}

impl Deref for PooledConnection {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        // unwrap is safe: `conn` is `Some` until `Drop` runs.
        self.conn.as_ref().unwrap()
    }
}

impl DerefMut for PooledConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.conn.as_mut().unwrap()
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.put(conn);
        }
    }
}

/// Capability token for SQLite database operations.
pub struct DbCapability {
    pool: Arc<Pool>,
}

impl Capability for DbCapability {
    fn name(&self) -> &'static str {
        "db"
    }
}

impl DbCapability {
    /// Opens a database at the given path (or `:memory:` for in-memory).
    ///
    /// Creates a pool of 5 connections, all with WAL mode enabled.
    pub fn open(path: &str) -> Result<Self, IoError> {
        let pool = Pool::open(path, DEFAULT_POOL_SIZE).map_err(std::io::Error::other)?;
        Ok(Self {
            pool: Arc::new(pool),
        })
    }

    /// Executes a SQL query and returns rows as `Vec<HashMap<String, String>>`.
    ///
    /// Grabs a connection from the pool, offloads the synchronous `rusqlite`
    /// work to Tokio's blocking thread pool, and returns the connection
    /// automatically when the closure completes.
    pub async fn query(
        &self,
        sql: &str,
    ) -> Result<Vec<std::collections::HashMap<String, String>>, IoError> {
        check_capability(self)?;
        let sql = sql.to_string();
        let conn = Arc::clone(&self.pool).get().await?;
        let result = tokio::task::spawn_blocking(move || {
            let mut stmt = conn.prepare(&sql).map_err(std::io::Error::other)?;

            let columns: Vec<String> = stmt
                .column_names()
                .iter()
                .map(ToString::to_string)
                .collect();

            let mut rows = Vec::new();
            let mut rows_iter = stmt.query([]).map_err(std::io::Error::other)?;

            while let Some(row) = rows_iter.next().map_err(std::io::Error::other)? {
                let mut map = std::collections::HashMap::new();
                for (i, col) in columns.iter().enumerate() {
                    let value: String = match row.get::<_, Value>(i) {
                        Ok(Value::Integer(n)) => n.to_string(),
                        Ok(Value::Real(f)) => f.to_string(),
                        Ok(Value::Text(s)) => s,
                        Ok(Value::Blob(b)) => format!("<blob {} bytes>", b.len()),
                        _ => String::new(),
                    };
                    map.insert(col.clone(), value);
                }
                rows.push(map);
            }

            Ok(rows)
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(e) => Err(IoError(std::io::Error::other(e.to_string()))),
        }
    }

    /// Executes a SQL statement (INSERT, UPDATE, DELETE) and returns rows affected.
    ///
    /// Grabs a connection from the pool, offloads the synchronous work to Tokio's
    /// blocking thread pool, and returns the connection automatically.
    pub async fn execute(&self, sql: &str) -> Result<usize, IoError> {
        check_capability(self)?;
        let sql = sql.to_string();
        let conn = Arc::clone(&self.pool).get().await?;
        let result = tokio::task::spawn_blocking(move || {
            let rows_affected = conn.execute(&sql, []).map_err(std::io::Error::other)?;
            Ok(rows_affected)
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(e) => Err(IoError(std::io::Error::other(e.to_string()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn test_pool_exhausted_returns_typed_error() {
        let pool = Arc::new(Pool::empty());
        match pool.get().await {
            Err(io_err) => {
                assert_eq!(io_err.kind(), std::io::ErrorKind::PermissionDenied);
                assert!(
                    io_err.to_string().contains("exhausted"),
                    "expected 'exhausted', got: {io_err}"
                );
            }
            Ok(_) => panic!("expected error from exhausted pool"),
        }
    }
}
