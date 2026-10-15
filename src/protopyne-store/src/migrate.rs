//! Versioned store schema: the market catalog and the candle tape.
//!
//! The schema is versioned from its first commit through
//! `fluent_db :: migrate`, so a schema change adds a migration rather
//! than editing this DDL. Two structural properties are load-bearing:
//!
//! - The candle table is `WITHOUT ROWID` on `PRIMARY KEY (symbol_id,
//!   ts_ms)`, so the key *is* the clustering order: a range scan for one
//!   symbol is a contiguous b-tree walk, and the watermark query is a
//!   reverse walk over the same structure. No second index.
//! - Prices are canonical `Decimal` text, never `REAL`: `Decimal ::
//!   normalize().to_string()` round-trips exactly and stays human-auditable
//!   in the file. `volume` is `INTEGER` so a negative print is representable
//!   and refused by validation rather than unrepresentable by accident.
//!
//! One additive column beyond the failure-mode sketch: `candle.adj` carries
//! the bar's corporate-action adjustment as JSON (`NULL` when absent).
//! Without it a stored bar would silently lose its `adj` field, which the
//! scope boundary forbids — the store keeps adjustments without interpreting
//! them, and no read path is adjustment-aware.

use fluent_db::migrate::{Migration, ensure_column, migrate, schema_version};
use fluent_db::sqlite::run_batch;
use rusqlite::Connection;

use crate::error::StoreError;

/// Current schema version. Bumped only by adding a [`Migration`], never by
/// editing the DDL below.
pub const SCHEMA_VERSION: u32 = 3;

/// Connection pragmas: WAL with a bounded busy wait, normal sync for a
/// working store, foreign keys on. These run **outside** the migration
/// transaction — SQLite refuses to change the safety level inside one —
/// so they live here rather than in the migration body.
const PRAGMAS: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 5000;
PRAGMA foreign_keys = ON;
";

/// The V1 tables: catalog plus tape. Idempotent (`IF NOT EXISTS`
/// throughout) so re-running over a migrated database is a no-op — the
/// version gate in [`ensure_schema`] is what makes application
/// once-only, not the DDL.
const V1_TABLES: &str = "
CREATE TABLE IF NOT EXISTS market (
    symbol_id   INTEGER PRIMARY KEY,
    symbol      TEXT    NOT NULL,
    venue       TEXT    NOT NULL DEFAULT '',
    asset_class TEXT    NOT NULL,
    price_scale INTEGER NOT NULL,
    time_zone   TEXT    NOT NULL DEFAULT 'UTC',
    UNIQUE (symbol, venue)
);
CREATE INDEX IF NOT EXISTS market_by_symbol ON market (symbol);

CREATE TABLE IF NOT EXISTS candle (
    symbol_id INTEGER NOT NULL REFERENCES market (symbol_id),
    ts_ms     INTEGER NOT NULL,
    open      TEXT    NOT NULL,
    high      TEXT    NOT NULL,
    low       TEXT    NOT NULL,
    close     TEXT    NOT NULL,
    volume    INTEGER NOT NULL,
    adj       TEXT,
    PRIMARY KEY (symbol_id, ts_ms)
) WITHOUT ROWID;
";

/// The first and so far only migration: catalog plus tape.
struct V1MarketCandle;

impl Migration for V1MarketCandle {
    fn version(&self) -> u32 {
        1
    }

    fn name(&self) -> &str {
        "market-candle-v1"
    }

    fn up(&self, tx: &mut rusqlite::Transaction<'_>) -> Result<(), fluent_db::error::DbError> {
        tx.execute_batch(V1_TABLES)
            .map_err(fluent_db::error::DbError::from)
    }
}

/// The second migration: the tape's spacing becomes durable.
///
/// The read planner must know which spacing a tape holds to decide whether
/// a requested spacing folds from it — and that fact must survive a
/// restart, so it lives in the table rather than in a caller's memory. Rows
/// written before this column existed default to minute tapes, which is
/// what every tape written so far holds.
struct V2MarketTimeframe;

impl Migration for V2MarketTimeframe {
    fn version(&self) -> u32 {
        2
    }

    fn name(&self) -> &str {
        "market-timeframe-v2"
    }

    fn up(&self, tx: &mut rusqlite::Transaction<'_>) -> Result<(), fluent_db::error::DbError> {
        ensure_column(tx, "market", "timeframe", "TEXT NOT NULL DEFAULT 'M1'")
    }
}

/// The third migration: the sealed-snapshot manifest.
///
/// One durable row per sealed Parquet artifact. The coverage columns
/// (`first_ts_ms`, `last_ts_ms`, `row_count`) let a read planner reason
/// about a snapshot without opening it — so `ReadPlan :: resolve` stays a
/// pure function over facts already in the catalog. This is **not** a tier
/// system: a snapshot is a byte-stable, content-addressed run input, and the
/// working SQLite tape remains the one store.
///
/// The primary key is `(symbol_id, timeframe, path)`, so several snapshots
/// may coexist for one tape; `WITHOUT ROWID` clusters them under the key.
/// The row is inserted in the same transaction that records the flush, so a
/// committed row always points at a fully-written, fsynced file.
struct V3TapeSnapshot;

impl Migration for V3TapeSnapshot {
    fn version(&self) -> u32 {
        3
    }

    fn name(&self) -> &str {
        "tape-snapshot-v3"
    }

    fn up(&self, tx: &mut rusqlite::Transaction<'_>) -> Result<(), fluent_db::error::DbError> {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS tape_snapshot (
                symbol_id    INTEGER NOT NULL REFERENCES market (symbol_id),
                timeframe    TEXT    NOT NULL,
                path         TEXT    NOT NULL,
                first_ts_ms  INTEGER NOT NULL,
                last_ts_ms   INTEGER NOT NULL,
                row_count    INTEGER NOT NULL,
                byte_len     INTEGER NOT NULL,
                digest       TEXT    NOT NULL,
                written_at_ms INTEGER NOT NULL,
                PRIMARY KEY (symbol_id, timeframe, path)
            ) WITHOUT ROWID;",
        )
        .map_err(fluent_db::error::DbError::from)
    }
}

/// Bring `conn` to [`SCHEMA_VERSION`], running every pending migration in
/// one transaction, and return the resulting version.
///
/// Pragmas apply first, outside any transaction; the migration body holds
/// tables only. The connection must already be open (file-backed via
/// `open_wal`, or an in-memory handle for tests).
pub fn ensure_schema(conn: &Connection) -> Result<u32, StoreError> {
    run_batch(conn, PRAGMAS)?;
    migrate(conn, &[&V1MarketCandle, &V2MarketTimeframe, &V3TapeSnapshot])?;
    Ok(schema_version(conn)?)
}

#[cfg(test)]
#[path = "../tests/migrate_timeframe.rs"]
mod tests_migrate;
