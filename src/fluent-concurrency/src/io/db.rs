//! SQLite-backed database capability.

use std::collections::HashMap;
use std::sync::Mutex;

use fluent_wvr::{Capability, ConcurrencyError};
use rusqlite::Connection;
use rusqlite::types::Value;

/// Capability token for SQLite database operations.
pub struct DbCapability {
    conn: Mutex<Connection>,
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
            conn: Mutex::new(conn),
        })
    }

    /// Executes a SQL query and returns rows as `Vec<HashMap<String, String>>`.
    pub fn query(&self, sql: &str) -> Result<Vec<HashMap<String, String>>, ConcurrencyError> {
        let conn = self.conn.lock().map_err(|e| {
            ConcurrencyError::Io(std::io::Error::other(format!(
                "db lock poisoned: {e}"
            )))
        })?;

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| ConcurrencyError::Io(std::io::Error::other(e)))?;

        let columns: Vec<String> = stmt.column_names().iter().map(ToString::to_string).collect();

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
    }

    /// Executes a SQL statement (INSERT, UPDATE, DELETE) and returns rows affected.
    pub fn execute(&self, sql: &str) -> Result<usize, ConcurrencyError> {
        let conn = self.conn.lock().map_err(|e| {
            ConcurrencyError::Io(std::io::Error::other(format!(
                "db lock poisoned: {e}"
            )))
        })?;

        let rows_affected = conn
            .execute(sql, [])
            .map_err(|e| ConcurrencyError::Io(std::io::Error::other(e)))?;

        Ok(rows_affected)
    }
}
