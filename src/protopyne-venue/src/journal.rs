//! Journaled single-writer order state machine over one event log.
//!
//! Intent is durable before any venue call: every mutating method takes a
//! [`VenueStorage`] permit that is both bound to this journal file and
//! present in the task-local capability set, so journal writes outside a
//! supervised session fail closed. State rebuilds by replay, which also
//! gives the kill-mid-submit guarantee: after a crash, replay plus
//! query-by-ID leaves no unknown position and no duplicate order.
//!
//! The log is the [`AppendLog`](protopyne_store::AppendLog) the market tape
//! is built on: one plain-text CSV event stream
//! `(seq, client_id, kind, detail, at_ms)`, one write plus flush plus fsync
//! per append before success is reported, and the same deterministic
//! torn-tail rule (a single incomplete trailing record drops with a logged
//! repair on reopen; a parse failure with bytes after it refuses as
//! corruption). There is no second table: intents replay in append order,
//! and each intent's status is the last status-kind event for its client
//! (`pending` when no status event exists) — status-kind rows land the same
//! status no matter which method wrote them.
//!
//! All methods are synchronous and serialize under one mutex that is never
//! held across an await.

use chrono::{DateTime, Utc};
use fluent_wvr::capability::check_capability;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use protopyne_store::{AppendLog, AppendRecord};

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

/// One journaled lifecycle event (one log row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEvent {
    /// Monotonic sequence number.
    pub seq: i64,
    /// Client ID the event belongs to.
    pub client_id: String,
    /// Event kind (`intent`, `submitted`, `acknowledged`, `partial`,
    /// `filled`, `cancelled`, `rejected`, `expired`, `open_failed`, plus one
    /// [`LEG_FILL_KIND`] row per executed spread leg, plus operator
    /// `command.*` audit rows and `verdict` rows).
    pub kind: String,
    /// Machine-readable detail (JSON).
    pub detail: String,
    /// Event time in epoch milliseconds.
    pub at_ms: i64,
}

/// State rebuilt by replaying the journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayedState {
    /// Intents by client ID, in append order.
    pub intents: Vec<OrderIntent>,
    /// Lifecycle events in sequence order.
    pub events: Vec<JournalEvent>,
}

fn now_ms(now: DateTime<Utc>) -> i64 {
    now.timestamp_millis()
}

/// One log row: the event schema the whole journal folds from.
struct JournalRecord {
    seq: i64,
    client_id: String,
    kind: String,
    detail: String,
    at_ms: i64,
}

impl AppendRecord for JournalRecord {
    fn domain() -> &'static str {
        "venue_journal"
    }

    fn version() -> u32 {
        1
    }

    fn header() -> Vec<&'static str> {
        vec!["seq", "client_id", "kind", "detail", "at_ms"]
    }

    fn encode(&self) -> Vec<String> {
        vec![
            self.seq.to_string(),
            self.client_id.clone(),
            self.kind.clone(),
            self.detail.clone(),
            self.at_ms.to_string(),
        ]
    }

    fn decode(fields: &[&str]) -> Result<Self, String> {
        let [seq, client_id, kind, detail, at_ms] = fields else {
            return Err(format!("expected 5 fields, found {}", fields.len()));
        };
        // Slice-pattern bindings over `&[&str]` are `&&str`; copy to `&str`
        // once so the owned conversions below read plainly.
        let (seq, client_id, kind, detail, at_ms): (&str, &str, &str, &str, &str) =
            (*seq, *client_id, *kind, *detail, *at_ms);
        let seq: i64 = seq.parse().map_err(|_| format!("seq {seq:?} is not an integer"))?;
        if seq < 1 {
            return Err(format!("seq {seq} is not positive"));
        }
        let at_ms: i64 = at_ms.parse().map_err(|_| format!("at_ms {at_ms:?} is not an integer"))?;
        Ok(Self {
            seq,
            client_id: client_id.to_owned(),
            kind: kind.to_owned(),
            detail: detail.to_owned(),
            at_ms,
        })
    }
}

impl From<&JournalRecord> for JournalEvent {
    fn from(record: &JournalRecord) -> Self {
        Self {
            seq: record.seq,
            client_id: record.client_id.clone(),
            kind: record.kind.clone(),
            detail: record.detail.clone(),
            at_ms: record.at_ms,
        }
    }
}

/// Whether `kind` carries an order's send state. The `intent` row implies
/// `pending` without naming it; every other row either moves the projection
/// (the nine send states) or leaves it alone (`leg_fill`, `verdict`,
/// `command.*`, and any future audit kind).
fn is_status_kind(kind: &str) -> bool {
    matches!(
        kind,
        "pending"
            | "submitted"
            | "acknowledged"
            | "partial"
            | "filled"
            | "cancelled"
            | "rejected"
            | "expired"
            | "open_failed"
    )
}

/// The durable side of the order state machine. The log is created on open;
/// every append is synced before it reports success.
pub struct Journal {
    path: PathBuf,
    inner: Mutex<JournalInner>,
}

struct JournalInner {
    log: AppendLog<JournalRecord>,
    next_seq: i64,
}

impl Journal {
    /// Open (or create) the journal at `path`, repairing a torn tail once
    /// when the previous writer died mid-record and refusing mid-file
    /// corruption loudly.
    pub fn open(path: &Path) -> Result<Self, VenueError> {
        let log = AppendLog::<JournalRecord>::open(path)
            .map_err(|e| VenueError::Journal(e.to_string()))?;
        let records = log.read_all().map_err(|e| VenueError::Journal(e.to_string()))?;
        let next_seq = records.iter().map(|record| record.seq).max().unwrap_or(0) + 1;
        Ok(Self {
            path: path.to_owned(),
            inner: Mutex::new(JournalInner { log, next_seq }),
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

    /// Append one event row, synced before returning.
    fn append(
        &self,
        client_id: &str,
        kind: &str,
        detail: &str,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        let mut inner = common_core::sync::lock(&self.inner);
        let seq = inner.next_seq;
        let record = JournalRecord {
            seq,
            client_id: client_id.to_owned(),
            kind: kind.to_owned(),
            detail: detail.to_owned(),
            at_ms: now_ms(now),
        };
        inner
            .log
            .append_batch(std::slice::from_ref(&record))
            .map_err(|e| VenueError::Journal(e.to_string()))?;
        inner.next_seq = seq + 1;
        Ok(())
    }

    /// Durably record an intent before any venue call. Recording the same
    /// client ID twice returns the stored copy: intent journaling is
    /// idempotent, which is what makes retries safe.
    ///
    /// One row lands for the first recording; the re-record writes nothing,
    /// so the intent and its event can never disagree.
    pub fn record_intent(
        &self,
        permit: &VenueStorage,
        intent: &OrderIntent,
        now: DateTime<Utc>,
    ) -> Result<bool, VenueError> {
        self.gate(permit)?;
        let payload =
            serde_json::to_string(intent).map_err(|e| VenueError::Journal(e.to_string()))?;
        {
            let inner = common_core::sync::lock(&self.inner);
            let seen = inner
                .log
                .read_all()
                .map_err(|e| VenueError::Journal(e.to_string()))?
                .iter()
                .any(|record| record.kind == "intent" && record.client_id == intent.client_id);
            if seen {
                return Ok(false);
            }
        }
        self.append(&intent.client_id, "intent", &payload, now)?;
        Ok(true)
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
        self.append(client_id, kind, detail, now)
    }

    /// Mark an intent's send state (`submitted`, `acknowledged`, `filled`,
    /// `cancelled`, `rejected`). One row lands; the stored status and the
    /// replayed events agree because they fold the same row.
    pub fn mark_status(
        &self,
        permit: &VenueStorage,
        client_id: &str,
        status: OrderStatus,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        self.gate(permit)?;
        let name = status_name(status);
        self.append(client_id, name, "{}", now)
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
        self.append(client_id, LEG_FILL_KIND, &detail, now)
    }

    /// Fold the log into per-client send states: the intent plus the last
    /// status-kind event, in append order. Unknown clients stay absent;
    /// clients with no status event read `pending`.
    fn fold_statuses(
        &self,
        permit: &VenueStorage,
    ) -> Result<Vec<(OrderIntent, String)>, VenueError> {
        self.gate(permit)?;
        let inner = common_core::sync::lock(&self.inner);
        let records = inner
            .log
            .read_all()
            .map_err(|e| VenueError::Journal(e.to_string()))?;
        let mut folded: Vec<(OrderIntent, String)> = Vec::new();
        for record in &records {
            if record.kind == "intent" {
                if folded.iter().any(|(intent, _)| intent.client_id == record.client_id) {
                    continue;
                }
                let intent: OrderIntent = serde_json::from_str(&record.detail)
                    .map_err(|e| VenueError::Journal(e.to_string()))?;
                folded.push((intent, "pending".to_owned()));
            } else if is_status_kind(&record.kind) {
                if let Some(entry) = folded
                    .iter_mut()
                    .find(|(intent, _)| intent.client_id == record.client_id)
                {
                    entry.1.clone_from(&record.kind);
                }
            }
        }
        Ok(folded)
    }

    /// Intents not yet terminal: the event-sourced outbox. After a crash,
    /// each row resolves through query-by-ID against venue truth.
    ///
    /// The predicate is every non-terminal status (`pending`, `submitted`,
    /// `acknowledged`, `partial`): an acknowledgement — or a partial fill
    /// still working — is still mid-flight, so excluding it would strand a
    /// crashed-after-ack order as an unknown position. Any future mid-flight
    /// status must join this predicate, or crash recovery will skip it.
    pub fn outbox_pending(&self, permit: &VenueStorage) -> Result<Vec<OrderIntent>, VenueError> {
        Ok(self
            .fold_statuses(permit)?
            .into_iter()
            .filter(|(_, status)| {
                matches!(status.as_str(), "pending" | "submitted" | "acknowledged" | "partial")
            })
            .map(|(intent, _)| intent)
            .collect())
    }

    /// Stored status of one intent, if journaled.
    pub fn intent_status(
        &self,
        permit: &VenueStorage,
        client_id: &str,
    ) -> Result<Option<String>, VenueError> {
        Ok(self
            .fold_statuses(permit)?
            .into_iter()
            .find(|(intent, _)| intent.client_id == client_id)
            .map(|(_, status)| status))
    }

    /// Rebuild state by replay: every intent in append order plus every
    /// event, in sequence order.
    pub fn replay(&self, permit: &VenueStorage) -> Result<ReplayedState, VenueError> {
        self.gate(permit)?;
        let inner = common_core::sync::lock(&self.inner);
        let records = inner
            .log
            .read_all()
            .map_err(|e| VenueError::Journal(e.to_string()))?;
        let mut intents = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for record in &records {
            if record.kind == "intent" && seen.insert(record.client_id.clone()) {
                intents.push(
                    serde_json::from_str(&record.detail)
                        .map_err(|e| VenueError::Journal(e.to_string()))?,
                );
            }
        }
        let events = records.iter().map(JournalEvent::from).collect();
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

#[cfg(test)]
#[path = "../tests/journal_characterization.rs"]
mod journal_characterization_tests;

#[cfg(test)]
#[path = "../tests/journal_log.rs"]
mod journal_log_tests;
