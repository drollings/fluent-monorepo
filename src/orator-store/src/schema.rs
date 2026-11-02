//! Durable event-log schema on the shared migration framework.
//!
//! One versioned migration owns every table so a fresh database and an
//! upgraded one converge on the same shape. The event table is the only
//! append path with an autoincrement cursor; every other table is a
//! plain record family keyed by its own id.

use fluent_db::error::DbError;
use fluent_db::migrate::Migration;

/// Current schema version. Bump only with a new migration entry.
pub const CURRENT_VERSION: u32 = 1;

/// Initial schema: sessions, the append-only event log, transcript
/// records, meetings, notes, checkpoints, audit, models, and kv.
pub struct V1Init;

impl Migration for V1Init {
    fn version(&self) -> u32 {
        1
    }

    fn name(&self) -> &str {
        "v1_init"
    }

    fn up(&self, tx: &mut rusqlite::Transaction<'_>) -> Result<(), DbError> {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (\
             id TEXT PRIMARY KEY, \
             kind TEXT NOT NULL, \
             started_ms INTEGER NOT NULL, \
             ended_ms INTEGER);\
             CREATE TABLE IF NOT EXISTS events (\
             seq INTEGER PRIMARY KEY AUTOINCREMENT, \
             session TEXT NOT NULL, \
             epoch INTEGER NOT NULL, \
             kind TEXT NOT NULL, \
             payload TEXT NOT NULL, \
             wall_ms INTEGER NOT NULL);\
             CREATE INDEX IF NOT EXISTS idx_events_session_seq ON events (session, seq);\
             CREATE INDEX IF NOT EXISTS idx_events_seq ON events (seq);\
             CREATE TABLE IF NOT EXISTS utterances (\
             id TEXT PRIMARY KEY, \
             session TEXT NOT NULL, \
             text TEXT NOT NULL, \
             started_ms INTEGER NOT NULL, \
             ended_ms INTEGER);\
             CREATE TABLE IF NOT EXISTS words (\
             id INTEGER PRIMARY KEY AUTOINCREMENT, \
             utterance TEXT NOT NULL, \
             text TEXT NOT NULL, \
             start_samples INTEGER NOT NULL, \
             end_samples INTEGER NOT NULL, \
             rate INTEGER NOT NULL, \
             confidence REAL NOT NULL, \
             channel TEXT NOT NULL);\
             CREATE TABLE IF NOT EXISTS speakers (\
             name TEXT PRIMARY KEY, \
             centroid TEXT NOT NULL);\
             CREATE TABLE IF NOT EXISTS meetings (\
             id TEXT PRIMARY KEY, \
             dir TEXT NOT NULL, \
             state TEXT NOT NULL);\
             CREATE TABLE IF NOT EXISTS notes (\
             id TEXT PRIMARY KEY, \
             rev INTEGER NOT NULL, \
             body TEXT NOT NULL);\
             CREATE TABLE IF NOT EXISTS checkpoints (\
             meeting TEXT PRIMARY KEY, \
             step TEXT NOT NULL, \
             state TEXT NOT NULL);\
             CREATE TABLE IF NOT EXISTS tool_audit (\
             id INTEGER PRIMARY KEY AUTOINCREMENT, \
             tool TEXT NOT NULL, \
             verdict TEXT NOT NULL, \
             ms INTEGER, \
             detail TEXT NOT NULL);\
             CREATE TABLE IF NOT EXISTS models (\
             name TEXT PRIMARY KEY, \
             kind TEXT NOT NULL, \
             repo TEXT NOT NULL, \
             file TEXT NOT NULL, \
             size INTEGER, \
             sha256 TEXT, \
             resident INTEGER NOT NULL DEFAULT 0, \
             last_used INTEGER NOT NULL DEFAULT -1);\
             CREATE TABLE IF NOT EXISTS kv (\
             key TEXT PRIMARY KEY, \
             value TEXT NOT NULL);",
        )
        .map_err(DbError::from)?;
        Ok(())
    }
}

/// Ordered migration set for the store.
pub fn migrations() -> Vec<&'static dyn Migration> {
    vec![&V1 as &dyn Migration]
}

static V1: V1Init = V1Init;

#[cfg(test)]
#[path = "../tests/schema.rs"]
mod tests;
