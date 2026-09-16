//! Idempotent schema migrations.
//!
//! Generalizes the ad-hoc `ensure_column` sequences that consumers hand-roll
//! (e.g. `router/src/ledger.rs:636-641` and the holographic store's inline
//! `PRAGMA table_info` check) into a versioned framework driven by
//! `PRAGMA user_version`.

use rusqlite::Connection;

use crate::error::DbError;
use crate::sqlite::quote_ident;

/// Add a column to `table` if it does not already exist.
///
/// `column` is the bare column name and `col_def` is the *type-and-default*
/// text only (e.g. `"TEXT NOT NULL DEFAULT ''"`) — it never re-states the
/// column name. Both `table` and `column` are validated and quoted via
/// [`quote_ident`](crate::sqlite::quote_ident) before being spliced into the
/// `ALTER TABLE` statement, so a hostile identifier fails with
/// [`DbError::InvalidIdentifier`](crate::error::DbError::InvalidIdentifier)
/// instead of executing. The existence check needs no quoting at all: it
/// binds the table name as a parameter to `pragma_table_info(?)`.
pub fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    col_def: &str,
) -> Result<(), DbError> {
    let has = conn
        .prepare("SELECT 1 FROM pragma_table_info(?) WHERE name = ?")
        .and_then(|mut stmt| stmt.query_row(rusqlite::params![table, column], |_| Ok(())))
        .is_ok();
    if !has {
        let q_table = quote_ident(table)?;
        let q_column = quote_ident(column)?;
        let alter_sql = format!("ALTER TABLE {q_table} ADD COLUMN {q_column} {col_def}");
        conn.execute_batch(&alter_sql).map_err(DbError::from)?;
    }
    Ok(())
}

/// A single idempotent schema migration.
///
/// `version` must be strictly monotonic within a migration list. Each `up`
/// runs inside a transaction; on success the framework bumps
/// `PRAGMA user_version` to the highest applied version.
pub trait Migration {
    /// Monotonic version number (matches the `PRAGMA user_version` it moves
    /// the database to).
    fn version(&self) -> u32;
    /// Human-readable name for logging / diagnostics.
    fn name(&self) -> &str;
    /// Apply the migration's DDL/data changes inside the transaction.
    fn up(&self, tx: &mut rusqlite::Transaction<'_>) -> Result<(), DbError>;
}

/// Read the database's schema version (`PRAGMA user_version`).
pub fn schema_version(conn: &Connection) -> Result<u32, DbError> {
    let v: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(DbError::from)?;
    Ok(v as u32)
}

/// Apply all migrations with `version > current` in version order inside a
/// single transaction, then bump `PRAGMA user_version` to the highest applied.
///
/// Already-applied migrations (`version <= current`) are skipped, so repeated
/// calls are no-ops and the sequence is idempotent across processes.
pub fn migrate(conn: &Connection, migrations: &[&dyn Migration]) -> Result<(), DbError> {
    let current = schema_version(conn)?;
    let mut pending: Vec<&dyn Migration> = migrations
        .iter()
        .copied()
        .filter(|m| m.version() > current)
        .collect();
    if pending.is_empty() {
        return Ok(());
    }
    pending.sort_by_key(|m| m.version());

    let mut tx = conn.unchecked_transaction().map_err(DbError::from)?;
    for migration in &pending {
        migration.up(&mut tx)?;
    }
    let new_version = pending.last().expect("non-empty pending list").version();
    tx.pragma_update(None, "user_version", new_version)
        .map_err(DbError::from)?;
    tx.commit().map_err(DbError::from)?;
    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
#[path = "../tests/migrate.rs"]
mod tests;
