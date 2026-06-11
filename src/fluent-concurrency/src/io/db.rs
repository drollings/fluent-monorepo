//! SQLite-backed database capability with async I/O via `spawn_blocking`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fluent_wvr::{Capability, ConcurrencyError};
use rusqlite::Connection;
use rusqlite::types::Value;

use crate::io::check_capability;

/// Capability token for SQLite database operations.
pub struct DbCapability {
    conn: Arc<Mutex<Connection>>,
}

impl Capability for DbCapability {
    fn name(&self) -> &'static str {
        "db"
    }
}

impl DbCapability {
    /// Opens a database at the given path (or ":memory:" for in-memory).
    pub fn open(path: &str) -> Result<Self, ConcurrencyError> {
        let conn = Connection::open(path)
            .map_err(|e| ConcurrencyError::Io(std::io::Error::other(e)))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Executes a SQL query and returns rows as `Vec<HashMap<String, String>>`.
    /// Offloads the synchronous `rusqlite` work to Tokio's blocking thread pool.
    pub async fn query(&self, sql: &str) -> Result<Vec<HashMap<String, String>>, ConcurrencyError> {
        check_capability(self)?;
        let conn = Arc::clone(&self.conn);
        let sql = sql.to_string();
        match tokio::task::spawn_blocking(move || {
            let guard = conn.lock().map_err(|e| {
                ConcurrencyError::Io(std::io::Error::other(format!(
                    "db lock poisoned: {e}"
                )))
            })?;

            let mut stmt = guard
                .prepare(&sql)
                .map_err(|e| ConcurrencyError::Io(std::io::Error::other(e)))?;

            let columns: Vec<String> =
                stmt.column_names().iter().map(ToString::to_string).collect();

            let mut rows = Vec::new();
            let mut rows_iter = stmt
                .query([])
                .map_err(|e| ConcurrencyError::Io(std::io::Error::other(e)))?;

            while let Some(row) = rows_iter
                .next()
                .map_err(|e| ConcurrencyError::Io(std::io::Error::other(e)))?
            {
                let mut map = HashMap::new();
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
        .await
        {
            Ok(result) => result,
            Err(e) => Err(ConcurrencyError::Io(std::io::Error::other(e.to_string()))),
        }
    }

    /// Executes a SQL statement (INSERT, UPDATE, DELETE) and returns rows affected.
    /// Offloads the synchronous `rusqlite` work to Tokio's blocking thread pool.
    pub async fn execute(&self, sql: &str) -> Result<usize, ConcurrencyError> {
        check_capability(self)?;
        let conn = Arc::clone(&self.conn);
        let sql = sql.to_string();
        match tokio::task::spawn_blocking(move || {
            let guard = conn.lock().map_err(|e| {
                ConcurrencyError::Io(std::io::Error::other(format!(
                    "db lock poisoned: {e}"
                )))
            })?;

            let rows_affected = guard
                .execute(&sql, [])
                .map_err(|e| ConcurrencyError::Io(std::io::Error::other(e)))?;

            Ok(rows_affected)
        })
        .await
        {
            Ok(result) => result,
            Err(e) => Err(ConcurrencyError::Io(std::io::Error::other(e.to_string()))),
        }
    }
}
