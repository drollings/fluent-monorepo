//! Generic append-only transactional outbox.
//!
//! The journal/outbox substrate every durable producer shares: the protopyne
//! journal, the package cache, and the trace store all need "append, claim a
//! batch for delivery, acknowledge what landed" with crash redelivery. This
//! module owns exactly that shape and nothing domain-specific — payloads are
//! opaque bytes keyed by a caller-chosen idempotency key, so it never learns
//! about orders, bars, or Pine.
//!
//! Delivery contract:
//! - `append` is idempotent per `key`: a repeated append returns the existing
//!   row id without touching the payload.
//! - `claim_batch` leases rows in FIFO (`id`) order inside one transaction,
//!   so two concurrent claimants never observe the same row.
//! - `ack` deletes delivered rows; anything claimed but never acked becomes
//!   claimable again once its lease (`claimed_at_ms + lease_ms`) expires, so
//!   a crash between claim and ack redelivers with a bumped `attempts` count.

use rusqlite::{Connection, Transaction};

use crate::error::DbError;

/// DDL for the outbox table and its claim index. Applied with
/// `init_schema`; idempotent (`IF NOT EXISTS`).
pub const OUTBOX_DDL: &str = "CREATE TABLE IF NOT EXISTS outbox (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  key TEXT NOT NULL UNIQUE,
  payload BLOB NOT NULL,
  claimed_at_ms INTEGER,
  attempts INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_outbox_claim ON outbox(claimed_at_ms, id);";

/// One claimed outbox row.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One claimed outbox row.
pub struct OutboxEntry {
    /// Row id (FIFO order; pass back to `ack`).
    pub id: i64,
    /// Caller-chosen idempotency key.
    pub key: String,
    /// Opaque payload bytes.
    pub payload: Vec<u8>,
    /// Times this row has been claimed before (0 on first claim).
    pub attempts: i64,
}

/// Create the outbox table and claim index if missing.
pub fn init_schema(conn: &Connection) -> Result<(), DbError> {
    crate::query::execute_batch(conn, OUTBOX_DDL)
}

/// Append `payload` under `key`, returning its row id.
///
/// Idempotent: when `key` is already present the existing row id is returned
/// and the stored payload is left untouched.
pub fn append(conn: &Connection, key: &str, payload: &[u8]) -> Result<i64, DbError> {
    conn.execute(
        "INSERT OR IGNORE INTO outbox(key, payload) VALUES (?1, ?2)",
        rusqlite::params![key, payload],
    )
    .map_err(DbError::from)?;
    crate::query::query_row(
        conn,
        "SELECT id FROM outbox WHERE key = ?1",
        rusqlite::params![key],
        |row| row.get::<_, i64>(0),
    )?
    .ok_or_else(|| DbError::Other(format!("outbox append for {key:?} left no row")))
}

/// Claim up to `limit` rows in FIFO order, leasing them to the caller.
///
/// A row is claimable when never claimed or when its lease expired
/// (`claimed_at_ms <= now_ms - lease_ms`). Claiming runs inside the caller's
/// transaction — select plus lease-update commit atomically, which is what
/// keeps two concurrent claimants from observing the same row. `limit == 0`
/// claims nothing.
pub fn claim_batch(
    tx: &mut Transaction<'_>,
    limit: usize,
    lease_ms: i64,
    now_ms: i64,
) -> Result<Vec<OutboxEntry>, DbError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let expiry = now_ms.saturating_sub(lease_ms);
    let rows: Vec<OutboxEntry> = crate::query::query_rows(
        tx,
        "SELECT id, key, payload, attempts FROM outbox
         WHERE claimed_at_ms IS NULL OR claimed_at_ms <= ?1
         ORDER BY id LIMIT ?2",
        rusqlite::params![expiry, limit as i64],
        |row| {
            Ok(OutboxEntry {
                id: row.get(0)?,
                key: row.get(1)?,
                payload: row.get(2)?,
                attempts: row.get(3)?,
            })
        },
    )?;
    if rows.is_empty() {
        return Ok(rows);
    }
    let ids: Vec<i64> = rows.iter().map(|e| e.id).collect();
    let placeholders = crate::in_clause(ids.len());
    let sql = format!("UPDATE outbox SET claimed_at_ms = ?1, attempts = attempts + 1 WHERE id IN ({placeholders})");
    let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(ids.len() + 1);
    params.push(rusqlite::types::Value::Integer(now_ms));
    for id in &ids {
        params.push(rusqlite::types::Value::Integer(*id));
    }
    tx.execute(&sql, rusqlite::params_from_iter(params))
        .map_err(DbError::from)?;
    Ok(rows)
}

/// Delete delivered rows. Returns the number of rows removed; an empty `ids`
/// slice removes nothing.
pub fn ack(conn: &Connection, ids: &[i64]) -> Result<usize, DbError> {
    if ids.is_empty() {
        return Ok(0);
    }
    let placeholders = crate::in_clause(ids.len());
    let sql = format!("DELETE FROM outbox WHERE id IN ({placeholders})");
    crate::query::execute(conn, &sql, rusqlite::params_from_iter(ids.iter().copied()))
}

/// Number of rows still in the outbox (claimed-but-unacked rows count until
/// acked — the lease only gates re-claiming, never visibility here).
pub fn pending_count(conn: &Connection) -> Result<usize, DbError> {
    let count: Option<i64> =
        crate::query::query_row(conn, "SELECT COUNT(*) FROM outbox", [], |row| row.get(0))?;
    Ok(count.unwrap_or(0) as usize)
}

#[cfg(all(test, feature = "sqlite"))]
#[path = "../tests/outbox.rs"]
mod tests;