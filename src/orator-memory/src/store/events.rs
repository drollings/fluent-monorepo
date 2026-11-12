//! Append-only event log with paged replay and live-only ephemerals.
//!
//! Durable events are rows in `events` with an autoincrement cursor
//! assigned at insert time inside one writer transaction. Ephemeral
//! events carry a negative sequence from a decreasing in-memory
//! counter: they fan out live, never touch the database, and never
//! advance a reconnect cursor. Delivery fans out on a broadcast
//! channel sized like the reference room (10, lagged receivers skip).

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use fluent_db::error::DbError;
use fluent_db::store::SqliteStore;
use tokio::sync::broadcast;

/// Rows per replay page. A week-old client pages instead of OOMing the
/// writer.
pub const REPLAY_PAGE: usize = 5_000;

/// One stored (or live-only) event row.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One stored (or live-only) event row.
pub struct StoredEvent {
    /// Cursor. Negative means live-only ephemeral.
    pub seq: i64,
    /// Owning session id.
    pub session: String,
    /// Reply-generation epoch.
    pub epoch: u64,
    /// Event kind label.
    pub kind: String,
    /// JSON payload.
    pub payload: String,
    /// Wall-clock ms since the Unix epoch.
    pub wall_ms: u64,
}

impl StoredEvent {
    /// Whether this event persists and replays.
    #[must_use]
    pub const fn durable(&self) -> bool {
        self.seq >= 0
    }
}

/// The event log: one writer-facing store plus live fan-out.
pub struct EventLog {
    store: SqliteStore,
    live: broadcast::Sender<StoredEvent>,
    next_live: AtomicI64,
}

impl EventLog {
    /// Log over an already-migrated store.
    #[must_use]
    pub fn over(store: SqliteStore) -> Arc<Self> {
        let (live, _) = broadcast::channel(10);
        Arc::new(Self {
            store,
            live,
            next_live: AtomicI64::new(-1),
        })
    }

    /// In-memory log for tests.
    pub fn in_memory() -> Result<Arc<Self>, DbError> {
        let store = SqliteStore::open_in_memory()?;
        store.with_conn(|conn| {
            fluent_db::migrate::migrate(conn, &crate::store::schema::migrations())?;
            Ok(())
        })?;
        Ok(Self::over(store))
    }

    /// Append one durable event. The cursor is assigned by the insert;
    /// the broadcast fires after commit.
    pub fn append(
        &self,
        session: &str,
        epoch: u64,
        kind: &str,
        payload: &str,
        wall_ms: u64,
    ) -> Result<StoredEvent, DbError> {
        let seq = self.store.with_conn_mut(|conn| {
            fluent_db::query::transaction(conn, |tx| {
                tx.execute(
                    "INSERT INTO events (session, epoch, kind, payload, wall_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![session, epoch as i64, kind, payload, wall_ms as i64],
                )
                .map_err(DbError::from)?;
                Ok(tx.last_insert_rowid())
            })
        })?;
        let event = StoredEvent {
            seq,
            session: session.to_string(),
            epoch,
            kind: kind.to_string(),
            payload: payload.to_string(),
            wall_ms,
        };
        let _ = self.live.send(event.clone());
        Ok(event)
    }

    /// Publish one live-only ephemeral event. Never written, never
    /// replayed, never moving a reconnect cursor.
    pub fn publish_ephemeral(
        &self,
        session: &str,
        epoch: u64,
        kind: &str,
        payload: &str,
        wall_ms: u64,
    ) -> StoredEvent {
        let seq = self.next_live.fetch_sub(1, Ordering::Relaxed);
        let event = StoredEvent {
            seq,
            session: session.to_string(),
            epoch,
            kind: kind.to_string(),
            payload: payload.to_string(),
            wall_ms,
        };
        let _ = self.live.send(event.clone());
        event
    }

    /// Replay durable events after `since`, oldest first, at most
    /// `limit` rows (paged by the caller at [`REPLAY_PAGE`]).
    pub fn replay(&self, since: i64, limit: usize) -> Result<Vec<StoredEvent>, DbError> {
        let limit = limit.min(REPLAY_PAGE) as i64;
        self.store.with_conn(|conn| {
            fluent_db::query::query_rows(
                conn,
                "SELECT seq, session, epoch, kind, payload, wall_ms FROM events WHERE seq > ?1 ORDER BY seq ASC LIMIT ?2",
                rusqlite::params![since, limit],
                |row| {
                    Ok(StoredEvent {
                        seq: row.get(0)?,
                        session: row.get(1)?,
                        epoch: row.get::<_, i64>(2)? as u64,
                        kind: row.get(3)?,
                        payload: row.get(4)?,
                        wall_ms: row.get::<_, i64>(5)? as u64,
                    })
                },
            )
        })
    }

    /// Highest durable cursor, if any event exists.
    pub fn latest(&self) -> Result<Option<i64>, DbError> {
        self.store
            .with_conn(|conn| {
                fluent_db::query::query_row(
                    conn,
                    "SELECT MAX(seq) FROM events",
                    rusqlite::params![],
                    |row| row.get::<_, Option<i64>>(0),
                )
            })
            .map(Option::flatten)
    }

    /// Replay one kind after `since`, oldest first, at most `limit`
    /// rows. Thin history/usage view over the same table: dictation
    /// turns append kind `"dictation"` with the clean text payload,
    /// usage counters append kind `"usage"` with a JSON payload —
    /// no second store, just filtered replay.
    pub fn replay_kind(
        &self,
        kind: &str,
        since: i64,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, DbError> {
        let limit = limit.min(REPLAY_PAGE) as i64;
        self.store.with_conn(|conn| {
            fluent_db::query::query_rows(
                conn,
                "SELECT seq, session, epoch, kind, payload, wall_ms FROM events WHERE seq > ?1 AND kind = ?2 ORDER BY seq ASC LIMIT ?3",
                rusqlite::params![since, kind, limit],
                |row| {
                    Ok(StoredEvent {
                        seq: row.get(0)?,
                        session: row.get(1)?,
                        epoch: row.get::<_, i64>(2)? as u64,
                        kind: row.get(3)?,
                        payload: row.get(4)?,
                        wall_ms: row.get::<_, i64>(5)? as u64,
                    })
                },
            )
        })
    }

    /// Replay one session's durable events after `since`, oldest
    /// first, at most `limit` rows. Thin per-meeting view over the
    /// same table (the export gather path) — no second store, just
    /// filtered replay like `replay_kind`.
    pub fn replay_session(
        &self,
        session: &str,
        since: i64,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, DbError> {
        let limit = limit.min(REPLAY_PAGE) as i64;
        self.store.with_conn(|conn| {
            fluent_db::query::query_rows(
                conn,
                "SELECT seq, session, epoch, kind, payload, wall_ms FROM events WHERE seq > ?1 AND session = ?2 ORDER BY seq ASC LIMIT ?3",
                rusqlite::params![since, session, limit],
                |row| {
                    Ok(StoredEvent {
                        seq: row.get(0)?,
                        session: row.get(1)?,
                        epoch: row.get::<_, i64>(2)? as u64,
                        kind: row.get(3)?,
                        payload: row.get(4)?,
                        wall_ms: row.get::<_, i64>(5)? as u64,
                    })
                },
            )
        })
    }

    /// Count durable rows of one kind after `since`. Usage-counter
    /// view: totals without replaying payloads.
    pub fn count_kind(&self, kind: &str, since: i64) -> Result<u64, DbError> {
        self.store
            .with_conn(|conn| {
                fluent_db::query::query_row(
                    conn,
                    "SELECT COUNT(*) FROM events WHERE seq > ?1 AND kind = ?2",
                    rusqlite::params![since, kind],
                    |row| row.get::<_, i64>(0),
                )
            })
            .map(|count| count.unwrap_or(0).max(0) as u64)
    }

    /// Subscribe to live fan-out (durable appends plus ephemerals).
    pub fn subscribe(&self) -> broadcast::Receiver<StoredEvent> {
        self.live.subscribe()
    }
}

#[cfg(test)]
#[path = "../../tests/store_events.rs"]
mod tests;