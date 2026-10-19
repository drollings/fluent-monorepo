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
//! The log stays the sole authority. In memory it also carries a derived
//! fold — every committed row plus one offset per client id — rebuilt by a
//! single linear pass on open and advanced as appends land, so intent
//! idempotence and status reads answer without re-reading the file. It never
//! outranks the log (a crash only costs a rebuild), and it carries no
//! producer bound: its size tracks the durable rows the process already owns,
//! and refusing to journal a trade to cap an index would be a risk, not a
//! protection.
//!
//! All methods are synchronous and serialize under one mutex that is never
//! held across an await.
//!
//! The ephemeral arm ([`Journal::ephemeral`]) shares this exact fold over
//! memory instead of a file: same rows, same indices, same replay — only the
//! sync differs. It owns no durable authority, so its gate is a documented
//! no-op and its rows land through [`Journal::record_ephemeral`], which
//! refuses a durable journal outright. Production backtests scope one per
//! run; the submission protocol reaches both arms through the single
//! [`Recorder`](crate::submit::Recorder).

use chrono::{DateTime, Utc};
use fluent_wvr::capability::check_capability;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use protopyne_store::{AppendLog, AppendRecord};
use protopyne_strategy::Proposal;

use crate::command::{proposal_bodies, PROPOSAL_BODY_KIND};
use crate::error::VenueError;
use crate::order::{OrderIntent, OrderRecord, OrderStatus};
use crate::venue::LegExecution;

/// Event kind for a recorded intent. Its detail is the [`OrderIntent`] as
/// JSON; it is the row replays fold into the intent set.
pub const INTENT_KIND: &str = "intent";

/// Event kind for one executed spread leg. The detail is the leg's
/// [`LegExecution`] as JSON. Additive: rows written before legs existed
/// carry no such events and replay exactly as before.
pub const LEG_FILL_KIND: &str = "leg_fill";

/// Event kind for one corporate action the replay consumed. The detail is the
/// [`CorporateEventArgs`](crate::order::CorporateEventArgs) plus its
/// [`CorporateOutcome`](crate::order::CorporateOutcome) as JSON: the closed
/// option legs, the generated stock leg, cash credited, and affected
/// contracts. Additive: runs without a corporate-action tape write none.
pub const CORPORATE_KIND: &str = "corporate";

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
        let seq: i64 = seq
            .parse()
            .map_err(|_| format!("seq {seq:?} is not an integer"))?;
        if seq < 1 {
            return Err(format!("seq {seq} is not positive"));
        }
        let at_ms: i64 = at_ms
            .parse()
            .map_err(|_| format!("at_ms {at_ms:?} is not an integer"))?;
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
    /// Journal file this log syncs to, or `None` for the ephemeral arm
    /// (which owns no durable authority and never touches the filesystem).
    path: Option<PathBuf>,
    inner: Mutex<JournalInner>,
}

/// Where committed rows land: the durability axis behind the one recorder.
///
/// The fold above this enum is identical for both arms — every committed row
/// plus one offset per client id — so replay, status, and idempotence agree
/// whether rows sync to a file or not. Only the sync differs.
enum JournalSink {
    /// File log: every append syncs before success is reported.
    Durable(AppendLog<JournalRecord>),
    /// Memory only: appends land in the resident fold, with no I/O, no
    /// fsync, and no capability gate. Production backtests scope one per
    /// run so parallel runs stay isolated; it owns no durable authority.
    Ephemeral,
}

/// The live fold over the log: every committed row in append order, plus one
/// offset per client id. Built by a single linear pass at
/// [`Journal::open`] and advanced as appends land, so intent idempotence and
/// status reads answer without re-reading and re-parsing the file. The log
/// stays the sole authority; this is a derived accelerator a crash only ever
/// costs a rebuild. The ephemeral arm shares this exact fold over memory
/// instead of a file, so both durabilities replay identically.
struct JournalInner {
    sink: JournalSink,
    next_seq: i64,
    /// Every committed row, in append order — the replay medium.
    records: Vec<JournalRecord>,
    /// Client id → offset in `records` of its `intent` row.
    intent_at: HashMap<String, usize>,
    /// Client id → offset in `records` of its last status-kind row.
    status_at: HashMap<String, usize>,
}

impl JournalInner {
    /// Fold one committed row onto the resident state. The first `intent`
    /// row opens a client (later duplicates are ignored, mirroring the
    /// replay fold); a status-kind row advances a client that already has an
    /// intent. Every other shape only extends the record stream.
    fn remember(&mut self, record: JournalRecord) {
        let at = self.records.len();
        if record.kind == INTENT_KIND {
            self.intent_at.entry(record.client_id.clone()).or_insert(at);
        } else if is_status_kind(&record.kind) && self.intent_at.contains_key(&record.client_id) {
            self.status_at.insert(record.client_id.clone(), at);
        }
        self.records.push(record);
    }

    /// Stage one row: assign its sequence number, sync when durable, and
    /// fold it in. One lock covers the whole step, so an idempotence check
    /// and its append can never interleave at either durability.
    fn stage(
        &mut self,
        client_id: &str,
        kind: &str,
        detail: &str,
        at_ms: i64,
    ) -> Result<(), VenueError> {
        let record = JournalRecord {
            seq: self.next_seq,
            client_id: client_id.to_owned(),
            kind: kind.to_owned(),
            detail: detail.to_owned(),
            at_ms,
        };
        if let JournalSink::Durable(log) = &mut self.sink {
            log.append_batch(std::slice::from_ref(&record))
                .map_err(|e| VenueError::Journal(e.to_string()))?;
        }
        self.next_seq = record.seq + 1;
        self.remember(record);
        Ok(())
    }

    /// A client's stored status: the kind of its last status-kind row, or
    /// `pending` when none has landed. Caller checks intent presence.
    fn status_of(&self, client_id: &str) -> String {
        self.status_at
            .get(client_id)
            .map_or_else(|| "pending".to_owned(), |&at| self.records[at].kind.clone())
    }
}

impl Journal {
    /// Open (or create) the journal at `path`, repairing a torn tail once
    /// when the previous writer died mid-record and refusing mid-file
    /// corruption loudly, then folding the committed rows into the index.
    pub fn open(path: &Path) -> Result<Self, VenueError> {
        let log = AppendLog::<JournalRecord>::open(path)
            .map_err(|e| VenueError::Journal(e.to_string()))?;
        let rows = log
            .read_all()
            .map_err(|e| VenueError::Journal(e.to_string()))?;
        let next_seq = rows.iter().map(|record| record.seq).max().unwrap_or(0) + 1;
        let mut inner = JournalInner {
            sink: JournalSink::Durable(log),
            next_seq,
            records: Vec::with_capacity(rows.len()),
            intent_at: HashMap::with_capacity(rows.len()),
            status_at: HashMap::new(),
        };
        for row in rows {
            inner.remember(row);
        }
        Ok(Self {
            path: Some(path.to_owned()),
            inner: Mutex::new(inner),
        })
    }

    /// Open an empty ephemeral log: the same fold over memory, with no file,
    /// no sync, and no gate. Production backtests scope one per run; it owns
    /// no durable authority and never touches the filesystem.
    #[must_use]
    pub fn ephemeral() -> Self {
        Self {
            path: None,
            inner: Mutex::new(JournalInner {
                sink: JournalSink::Ephemeral,
                next_seq: 1,
                records: Vec::new(),
                intent_at: HashMap::new(),
                status_at: HashMap::new(),
            }),
        }
    }

    fn gate(&self, permit: &VenueStorage) -> Result<(), VenueError> {
        let Some(path) = self.path.as_ref() else {
            // The ephemeral arm owns no durable authority, so there is no
            // session to bind and nothing to steal: the no-op is
            // load-bearing, scoped to this sink by construction. Durable
            // journals always carry a path and always check below.
            return Ok(());
        };
        if permit.journal_path != *path {
            return Err(VenueError::Journal(
                "storage permit is bound to a different journal".to_owned(),
            ));
        }
        check_capability(permit)
            .map_err(|_| VenueError::Journal("venue storage capability missing".to_owned()))
    }

    /// Permit-free row append for the ephemeral recorder: intent rows check
    /// idempotence and append under one lock (atomic, unlike the old
    /// check-then-append across two), every other kind appends. Refuses on a
    /// durable journal — durable writes demand a permit through the gated
    /// methods — so no ungated path into the durable log exists.
    pub fn record_ephemeral(
        &self,
        client_id: &str,
        kind: &str,
        detail: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, VenueError> {
        if self.path.is_some() {
            return Err(VenueError::Journal(
                "durable writes demand a storage permit".to_owned(),
            ));
        }
        let mut inner = common_core::sync::lock(&self.inner);
        if kind == INTENT_KIND && inner.intent_at.contains_key(client_id) {
            return Ok(false);
        }
        inner.stage(client_id, kind, detail, now_ms(now))?;
        Ok(true)
    }

    /// Append one event row, synced before returning.
    fn append(
        &self,
        client_id: &str,
        kind: &str,
        detail: &str,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        common_core::sync::lock(&self.inner).stage(client_id, kind, detail, now_ms(now))
    }

    /// Durably record an intent before any venue call. Recording the same
    /// client ID twice returns the stored copy: intent journaling is
    /// idempotent, which is what makes retries safe.
    ///
    /// One row lands for the first recording; the re-record writes nothing,
    /// so the intent and its event can never disagree. The presence check
    /// and the append share one lock over the index, never a file re-read.
    pub fn record_intent(
        &self,
        permit: &VenueStorage,
        intent: &OrderIntent,
        now: DateTime<Utc>,
    ) -> Result<bool, VenueError> {
        self.gate(permit)?;
        let payload =
            serde_json::to_string(intent).map_err(|e| VenueError::Journal(e.to_string()))?;
        let mut inner = common_core::sync::lock(&self.inner);
        if inner.intent_at.contains_key(&intent.client_id) {
            return Ok(false);
        }
        inner.stage(&intent.client_id, INTENT_KIND, &payload, now_ms(now))?;
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

    /// Durably record one proposal body so a restart rebuilds the approval
    /// inbox and a later human decision acts on the exact proposal proposed.
    /// Keyed by the proposal's own id; the body is the [`Proposal`] JSON.
    pub fn record_proposal_body(
        &self,
        permit: &VenueStorage,
        proposal: &Proposal,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        self.gate(permit)?;
        let detail =
            serde_json::to_string(proposal).map_err(|e| VenueError::Journal(e.to_string()))?;
        self.append(&proposal.id, PROPOSAL_BODY_KIND, &detail, now)
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
        let detail = serde_json::to_string(leg).map_err(|e| VenueError::Journal(e.to_string()))?;
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
        let mut folded: Vec<(OrderIntent, String)> = Vec::with_capacity(inner.intent_at.len());
        for (at, record) in inner.records.iter().enumerate() {
            // The index holds each client's first intent offset, so the fold
            // reads every intent once without a rescan per status row.
            if record.kind != INTENT_KIND || inner.intent_at.get(&record.client_id) != Some(&at) {
                continue;
            }
            let intent: OrderIntent = serde_json::from_str(&record.detail)
                .map_err(|e| VenueError::Journal(e.to_string()))?;
            folded.push((intent, inner.status_of(&record.client_id)));
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
                matches!(
                    status.as_str(),
                    "pending" | "submitted" | "acknowledged" | "partial"
                )
            })
            .map(|(intent, _)| intent)
            .collect())
    }

    /// Stored status of one intent, if journaled. Reads the index alone: no
    /// file access and no fold over the log.
    pub fn intent_status(
        &self,
        permit: &VenueStorage,
        client_id: &str,
    ) -> Result<Option<String>, VenueError> {
        self.gate(permit)?;
        let inner = common_core::sync::lock(&self.inner);
        Ok(inner
            .intent_at
            .contains_key(client_id)
            .then(|| inner.status_of(client_id)))
    }

    /// Rebuild state by replay: every intent in append order plus every
    /// event, in sequence order. Served from the resident fold, so replaying
    /// never re-reads the file.
    pub fn replay(&self, permit: &VenueStorage) -> Result<ReplayedState, VenueError> {
        self.gate(permit)?;
        let inner = common_core::sync::lock(&self.inner);
        let mut intents = Vec::with_capacity(inner.intent_at.len());
        for (at, record) in inner.records.iter().enumerate() {
            if record.kind == INTENT_KIND && inner.intent_at.get(&record.client_id) == Some(&at) {
                intents.push(
                    serde_json::from_str(&record.detail)
                        .map_err(|e| VenueError::Journal(e.to_string()))?,
                );
            }
        }
        let events = inner.records.iter().map(JournalEvent::from).collect();
        Ok(ReplayedState { intents, events })
    }

    /// Rebuild journaled proposal bodies by replay: last body per id wins.
    /// The approval inbox reads this beside [`replay`](Self::replay), so a
    /// restart acts on the proposals that were proposed, never a
    /// reconstruction.
    pub fn replay_proposals(
        &self,
        permit: &VenueStorage,
    ) -> Result<BTreeMap<String, Proposal>, VenueError> {
        let state = self.replay(permit)?;
        Ok(proposal_bodies(&state.events))
    }

    /// Latest known record for `client_id` from journal rows alone.
    pub fn record_from_replay(
        &self,
        permit: &VenueStorage,
        client_id: &str,
    ) -> Result<Option<OrderRecord>, VenueError> {
        self.gate(permit)?;
        let inner = common_core::sync::lock(&self.inner);
        let Some(&at) = inner.intent_at.get(client_id) else {
            return Ok(None);
        };
        let intent: OrderIntent = serde_json::from_str(&inner.records[at].detail)
            .map_err(|e| VenueError::Journal(e.to_string()))?;
        let mut record = OrderRecord::pending(intent, Utc::now());
        if inner.status_at.contains_key(client_id) {
            record.status = status_from_name(&inner.status_of(client_id));
        }
        Ok(Some(record))
    }
}

/// Inverse of [`status_name`] for rows read back from the log: a status-kind
/// row's kind back to its enum. Unknown kinds read `pending`, matching the
/// fold's default.
fn status_from_name(name: &str) -> OrderStatus {
    match name {
        "submitted" => OrderStatus::Submitted,
        "acknowledged" => OrderStatus::Acknowledged,
        "partial" => OrderStatus::Partial,
        "filled" => OrderStatus::Filled,
        "cancelled" => OrderStatus::Cancelled,
        "rejected" => OrderStatus::Rejected,
        "expired" => OrderStatus::Expired,
        "open_failed" => OrderStatus::OpenFailed,
        _ => OrderStatus::Pending,
    }
}

pub(crate) fn status_name(status: OrderStatus) -> &'static str {
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
