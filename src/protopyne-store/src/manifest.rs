//! Sealed-snapshot manifest: one durable row per sealed Parquet artifact.
//!
//! A sealed snapshot is a byte-stable, content-addressed run input — the
//! property CSV cannot provide (no schema, no guaranteed ordering, no stable
//! digest). The manifest records what was sealed, where it lives, and the
//! digest that proves it: `(symbol_id, timeframe, path)` is the key, and the
//! coverage columns let a read planner reason about a snapshot without
//! opening it.
//!
//! This is deliberately **not** a tier system. There is one snapshot format,
//! one digest, one coverage row per artifact; the working SQLite tape stays
//! the store. The row exists so [`ReadPlan`](crate::plan::ReadPlan) can stay
//! pure, not so storage can be layered.
//!
//! The manifest is inserted in the same transaction that records the flush
//! (see [`seal`](crate::snapshot::seal)), so a committed row always points
//! at a fully-written, fsynced file. The reverse — a file with no row — is
//! an orphan, removed loudly by
//! [`reconcile_orphans`](crate::snapshot::reconcile_orphans).

use protopyne_types::Timeframe;
use rusqlite::{Connection, Transaction, params};

use crate::key::SymbolId;

/// The full `tape_snapshot` row select. Column order is the contract
/// [`record_of`] reads; every reader shares this string so it cannot drift.
const SELECT_SNAPSHOT_SQL: &str = "SELECT symbol_id, timeframe, path, first_ts_ms, last_ts_ms, \
                                   row_count, byte_len, digest, written_at_ms FROM tape_snapshot";

/// One sealed artifact's manifest row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRecord {
    /// The instrument the sealed tape belongs to.
    pub symbol_id: SymbolId,
    /// The bar spacing of the sealed tape.
    pub timeframe: Timeframe,
    /// The artifact path, as sealed.
    pub path: String,
    /// The first bar stamp in the artifact.
    pub first_ts_ms: i64,
    /// The last bar stamp in the artifact.
    pub last_ts_ms: i64,
    /// Rows in the artifact.
    pub row_count: u64,
    /// Artifact size in bytes.
    pub byte_len: u64,
    /// The content digest (BLAKE3 hex) of the artifact.
    pub digest: String,
    /// The wall-clock instant the seal committed, epoch milliseconds.
    pub written_at_ms: i64,
}

/// Materialize one `tape_snapshot` row. Corrupt spellings and out-of-range
/// integers refuse here, at the boundary, so no caller ever sees a defaulted
/// record.
pub(crate) fn record_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<SnapshotRecord> {
    fn bad(col: usize, what: String) -> rusqlite::Error {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Text,
            std::io::Error::new(std::io::ErrorKind::InvalidData, what).into(),
        )
    }
    let symbol_id = u32::try_from(row.get::<_, i64>(0)?)
        .map_err(|_| bad(0, "symbol_id out of u32 range".to_owned()))?;
    let timeframe_text: String = row.get(1)?;
    let timeframe: Timeframe = timeframe_text
        .parse()
        .map_err(|_| bad(1, format!("unknown timeframe {timeframe_text:?}")))?;
    let row_count = u64::try_from(row.get::<_, i64>(5)?)
        .map_err(|_| bad(5, "row_count is negative".to_owned()))?;
    let byte_len = u64::try_from(row.get::<_, i64>(6)?)
        .map_err(|_| bad(6, "byte_len is negative".to_owned()))?;
    Ok(SnapshotRecord {
        symbol_id: SymbolId(symbol_id),
        timeframe,
        path: row.get(2)?,
        first_ts_ms: row.get(3)?,
        last_ts_ms: row.get(4)?,
        row_count,
        byte_len,
        digest: row.get(7)?,
        written_at_ms: row.get(8)?,
    })
}

/// Write the manifest row for a freshly sealed artifact, replacing any row
/// for the same `(symbol_id, timeframe, path)`. Called inside the seal's
/// transaction, so the row and the flush commit together.
pub(crate) fn upsert_record(
    tx: &Transaction<'_>,
    record: &SnapshotRecord,
) -> Result<(), fluent_db::error::DbError> {
    tx.execute(
        "INSERT INTO tape_snapshot(
            symbol_id, timeframe, path, first_ts_ms, last_ts_ms,
            row_count, byte_len, digest, written_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(symbol_id, timeframe, path) DO UPDATE SET
            first_ts_ms = excluded.first_ts_ms,
            last_ts_ms = excluded.last_ts_ms,
            row_count = excluded.row_count,
            byte_len = excluded.byte_len,
            digest = excluded.digest,
            written_at_ms = excluded.written_at_ms",
        params![
            i64::from(record.symbol_id.0),
            record.timeframe.code(),
            record.path,
            record.first_ts_ms,
            record.last_ts_ms,
            i64::try_from(record.row_count).unwrap_or(i64::MAX),
            i64::try_from(record.byte_len).unwrap_or(i64::MAX),
            record.digest,
            record.written_at_ms,
        ],
    )?;
    Ok(())
}

/// One manifest row for `(symbol_id, timeframe, path)`, or `None`.
pub(crate) fn record_for(
    conn: &Connection,
    symbol_id: SymbolId,
    timeframe: Timeframe,
    path: &str,
) -> Result<Option<SnapshotRecord>, fluent_db::error::DbError> {
    fluent_db::query::query_row(
        conn,
        &format!("{SELECT_SNAPSHOT_SQL} WHERE symbol_id = ?1 AND timeframe = ?2 AND path = ?3"),
        params![i64::from(symbol_id.0), timeframe.code(), path],
        record_of,
    )
}

/// Every manifest row, in key order.
pub(crate) fn all_records(
    conn: &Connection,
) -> Result<Vec<SnapshotRecord>, fluent_db::error::DbError> {
    fluent_db::query::query_rows(conn, SELECT_SNAPSHOT_SQL, [], record_of)
}
