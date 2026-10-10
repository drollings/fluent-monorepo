//! Journaled single-writer order state machine with a transactional outbox.
//!
//! Intent is durable before any venue call: every mutating method takes a
//! [`VenueStorage`] permit that is both bound to this journal file and
//! present in the task-local capability set, so journal writes outside a
//! supervised session fail closed. State rebuilds by replay, which also
//! gives the kill-mid-submit guarantee: after a crash, replay plus
//! query-by-ID leaves no unknown position and no duplicate order.
//!
//! The connection lives behind a poison-safe mutex and is never held across
//! an await: all methods are synchronous point writes.

use chrono::{DateTime, Utc};
use fluent_wvr::capability::check_capability;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::VenueError;
use crate::order::{OrderIntent, OrderRecord, OrderStatus};
use crate::venue::LegExecution;

/// Event kind for one executed spread leg. The detail is the leg's
/// [`LegExecution`] as JSON. Additive: rows written before legs existed
/// carry no such events and replay exactly as before.
pub const LEG_FILL_KIND: &str = "leg_fill";

/// Capability token binding journal access to one supervised session.
/// The session mints exactly one per journal file and installs it in the
/// batch/scope capability set; handlers borrow it from their `WorkContext`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueStorage {
    journal_path: PathBuf,
}

impl VenueStorage {
    /// Mint the token for `journal_path`. Only the session boot path calls
    /// this; everyone else borrows the installed token.
    #[must_use]
    pub fn new(journal_path: PathBuf) -> Self {
        Self { journal_path }
    }
}

impl fluent_wvr::Capability for VenueStorage {
    fn name(&self) -> &'static str {
        "venue-storage"
    }
}

/// One journaled lifecycle event (the transactional outbox row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEvent {
    /// Monotonic sequence number.
    pub seq: i64,
    /// Client ID the event belongs to.
    pub client_id: String,
    /// Event kind (`intent`, `submitted`, `acknowledged`, `partial`,
    /// `filled`, `cancelled`, `rejected`, `expired`, `open_failed`, plus one
    /// [`LEG_FILL_KIND`] row per executed spread leg).
    pub kind: String,
    /// Machine-readable detail (JSON).
    pub detail: String,
    /// Event time in epoch milliseconds.
    pub at_ms: i64,
}

/// State rebuilt by replaying the journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayedState {
    /// Intents by client ID, in insertion order.
    pub intents: Vec<OrderIntent>,
    /// Lifecycle events in sequence order.
    pub events: Vec<JournalEvent>,
}

fn now_ms(now: DateTime<Utc>) -> i64 {
    now.timestamp_millis()
}

/// Single INSERT shared by the direct and transactional event writers so the
/// outbox row shape cannot drift between the two paths.
const INSERT_EVENT_SQL: &str =
    "INSERT INTO events(client_id, kind, detail, at_ms) VALUES (?1, ?2, ?3, ?4)";

/// The durable side of the order state machine. Schemas are created on open;
/// the journal runs in WAL mode so readers never block a writer.
pub struct Journal {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl Journal {
    /// Open (or create) the journal at `path`, in WAL mode.
    pub fn open(path: &Path) -> Result<Self, VenueError> {
        let conn = Connection::open(path).map_err(|e| VenueError::Journal(e.to_string()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS intents(
               client_id TEXT PRIMARY KEY,
               proposal_id TEXT NOT NULL,
               payload TEXT NOT NULL,
               status TEXT NOT NULL,
               created_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS events(
               seq INTEGER PRIMARY KEY AUTOINCREMENT,
               client_id TEXT NOT NULL,
               kind TEXT NOT NULL,
               detail TEXT NOT NULL,
               at_ms INTEGER NOT NULL
             );",
        )
        .map_err(|e| VenueError::Journal(e.to_string()))?;
        Ok(Self {
            path: path.to_owned(),
            conn: Mutex::new(conn),
        })
    }

    fn gate(&self, permit: &VenueStorage) -> Result<(), VenueError> {
        if permit.journal_path != self.path {
            return Err(VenueError::Journal(
                "storage permit is bound to a different journal".to_owned(),
            ));
        }
        check_capability(permit)
            .map_err(|_| VenueError::Journal("venue storage capability missing".to_owned()))
    }

    /// Durably record an intent before any venue call. Recording the same
    /// client ID twice returns the stored copy: intent journaling is
    /// idempotent, which is what makes retries safe.
    ///
    /// The INSERT plus its event commit atomically through the shared
    /// transaction primitive: a crash between the statements leaves neither
    /// half behind, so status and events always agree.
    pub fn record_intent(
        &self,
        permit: &VenueStorage,
        intent: &OrderIntent,
        now: DateTime<Utc>,
    ) -> Result<bool, VenueError> {
        self.gate(permit)?;
        let payload = serde_json::to_string(intent)
            .map_err(|e| VenueError::Journal(e.to_string()))?;
        let mut conn = common_core::sync::lock(&self.conn);
        fluent_db::query::transaction(&mut conn, |tx| {
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO intents(client_id, proposal_id, payload, status, created_ms)
                 VALUES (?1, ?2, ?3, 'pending', ?4)",
                params![
                    intent.client_id,
                    intent.proposal_id,
                    payload,
                    now_ms(now)
                ],
            )?;
            if inserted == 1 {
                Self::append_tx(tx, &intent.client_id, "intent", &payload, now)?;
            }
            Ok(inserted == 1)
        })
        .map_err(|e| VenueError::Journal(e.to_string()))
    }

    /// Record a lifecycle transition for `client_id`.
    pub fn record_transition(
        &self,
        permit: &VenueStorage,
        client_id: &str,
        kind: &str,
        detail: &str,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        self.gate(permit)?;
        let conn = common_core::sync::lock(&self.conn);
        Self::append_locked(&conn, client_id, kind, detail, now)
    }

    /// Mark an intent's send state (`submitted`, `acknowledged`, `filled`,
    /// `cancelled`, `rejected`). The UPDATE plus its event commit atomically
    /// through the shared transaction primitive, so status and events always
    /// agree.
    pub fn mark_status(
        &self,
        permit: &VenueStorage,
        client_id: &str,
        status: OrderStatus,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        self.gate(permit)?;
        let name = status_name(status);
        let mut conn = common_core::sync::lock(&self.conn);
        fluent_db::query::transaction(&mut conn, |tx| {
            tx.execute(
                "UPDATE intents SET status = ?1 WHERE client_id = ?2",
                params![name, client_id],
            )?;
            Self::append_tx(tx, client_id, name, "{}", now)?;
            Ok(())
        })
        .map_err(|e| VenueError::Journal(e.to_string()))
    }

    /// Record one executed spread leg under `client_id`. One row per leg, in
    /// leg order; single-leg fills write no such rows. Append-only like
    /// every other event, so replay sees the same atomic package the venue
    /// executed.
    pub fn record_leg_fill(
        &self,
        permit: &VenueStorage,
        client_id: &str,
        leg: &LegExecution,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        self.gate(permit)?;
        let detail =
            serde_json::to_string(leg).map_err(|e| VenueError::Journal(e.to_string()))?;
        let conn = common_core::sync::lock(&self.conn);
        Self::append_locked(&conn, client_id, LEG_FILL_KIND, &detail, now)
    }

    /// Intents not yet terminal: the transactional outbox. After a crash,
    /// each row resolves through query-by-ID against venue truth.
    ///
    /// The predicate is every non-terminal status (`pending`, `submitted`,
    /// `acknowledged`, `partial`): an acknowledgement — or a partial fill
    /// still working — is still mid-flight, so excluding it would strand a
    /// crashed-after-ack order as an unknown position. Any future mid-flight
    /// status must join this predicate, or crash recovery will skip it.
    pub fn outbox_pending(&self, permit: &VenueStorage) -> Result<Vec<OrderIntent>, VenueError> {
        self.gate(permit)?;
        let conn = common_core::sync::lock(&self.conn);
        let mut stmt = conn
            .prepare("SELECT payload FROM intents WHERE status IN ('pending','submitted','acknowledged','partial') ORDER BY created_ms")
            .map_err(|e| VenueError::Journal(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| VenueError::Journal(e.to_string()))?;
        let mut intents = Vec::new();
        for payload in rows {
            let payload = payload.map_err(|e| VenueError::Journal(e.to_string()))?;
            let intent: OrderIntent = serde_json::from_str(&payload)
                .map_err(|e| VenueError::Journal(e.to_string()))?;
            intents.push(intent);
        }
        Ok(intents)
    }

    /// Stored status of one intent, if journaled.
    pub fn intent_status(
        &self,
        permit: &VenueStorage,
        client_id: &str,
    ) -> Result<Option<String>, VenueError> {
        self.gate(permit)?;
        let conn = common_core::sync::lock(&self.conn);
        conn.query_row(
            "SELECT status FROM intents WHERE client_id = ?1",
            params![client_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| VenueError::Journal(e.to_string()))
    }

    /// Rebuild state by replay: every intent plus every event, in order.
    pub fn replay(&self, permit: &VenueStorage) -> Result<ReplayedState, VenueError> {
        self.gate(permit)?;
        let conn = common_core::sync::lock(&self.conn);
        let mut intents = Vec::new();
        {
            let mut stmt = conn
                .prepare("SELECT payload FROM intents ORDER BY created_ms")
                .map_err(|e| VenueError::Journal(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| VenueError::Journal(e.to_string()))?;
            for payload in rows {
                let payload = payload.map_err(|e| VenueError::Journal(e.to_string()))?;
                intents.push(
                    serde_json::from_str(&payload)
                        .map_err(|e| VenueError::Journal(e.to_string()))?,
                );
            }
        }
        let mut events = Vec::new();
        {
            let mut stmt = conn
                .prepare("SELECT seq, client_id, kind, detail, at_ms FROM events ORDER BY seq")
                .map_err(|e| VenueError::Journal(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(JournalEvent {
                        seq: row.get(0)?,
                        client_id: row.get(1)?,
                        kind: row.get(2)?,
                        detail: row.get(3)?,
                        at_ms: row.get(4)?,
                    })
                })
                .map_err(|e| VenueError::Journal(e.to_string()))?;
            for event in rows {
                events.push(event.map_err(|e| VenueError::Journal(e.to_string()))?);
            }
        }
        Ok(ReplayedState { intents, events })
    }

    /// Latest known record for `client_id` from journal rows alone.
    pub fn record_from_replay(
        &self,
        permit: &VenueStorage,
        client_id: &str,
    ) -> Result<Option<OrderRecord>, VenueError> {
        let state = self.replay(permit)?;
        let intent = state.intents.into_iter().find(|i| i.client_id == client_id);
        Ok(intent.map(|intent| {
            let mut record = OrderRecord::pending(intent, Utc::now());
            for event in &state.events {
                if event.client_id == client_id {
                    record.status = match event.kind.as_str() {
                        "submitted" => OrderStatus::Submitted,
                        "acknowledged" => OrderStatus::Acknowledged,
                        "partial" => OrderStatus::Partial,
                        "filled" => OrderStatus::Filled,
                        "cancelled" => OrderStatus::Cancelled,
                        "rejected" => OrderStatus::Rejected,
                        "expired" => OrderStatus::Expired,
                        "open_failed" => OrderStatus::OpenFailed,
                        _ => record.status,
                    };
                }
            }
            record
        }))
    }

    fn append_locked(
        conn: &Connection,
        client_id: &str,
        kind: &str,
        detail: &str,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        conn.execute(
            INSERT_EVENT_SQL,
            params![client_id, kind, detail, now_ms(now)],
        )
        .map_err(|e| VenueError::Journal(e.to_string()))?;
        Ok(())
    }

    /// The transactional half of [`Self::append_locked`] for closures running
    /// inside the shared transaction primitive (whose `?` speaks `DbError`).
    /// Single-write callers keep using `append_locked`.
    fn append_tx(
        tx: &rusqlite::Transaction<'_>,
        client_id: &str,
        kind: &str,
        detail: &str,
        now: DateTime<Utc>,
    ) -> Result<(), fluent_db::error::DbError> {
        tx.execute(
            INSERT_EVENT_SQL,
            params![client_id, kind, detail, now_ms(now)],
        )?;
        Ok(())
    }
}

fn status_name(status: OrderStatus) -> &'static str {
    match status {
        OrderStatus::Pending => "pending",
        OrderStatus::Submitted => "submitted",
        OrderStatus::Acknowledged => "acknowledged",
        OrderStatus::Partial => "partial",
        OrderStatus::Filled => "filled",
        OrderStatus::Cancelled => "cancelled",
        OrderStatus::Rejected => "rejected",
        OrderStatus::Expired => "expired",
        OrderStatus::OpenFailed => "open_failed",
    }
}

#[cfg(test)]
#[path = "../tests/journal.rs"]
mod tests;
