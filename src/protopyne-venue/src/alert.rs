//! Out-of-band alerting: breaker trips and gate halts leave the UI.
//!
//! The terminal never carries alerts — a dead UI must not mean a silent
//! book. [`poll_alerts`] collects what tripped or halted into typed alerts
//! (each naming its side of the threshold split) and delivers them through
//! a sink the operator reads elsewhere, usually a file tail or a pager
//! pointed at the same file. Collection drains the gate banks, so each
//! banked line delivers exactly once; the breaker trip and the kill-switch
//! transition alert on the edge through [`AlertCursor`], never on the level.
//!
//! Delivery drains like every other bank in this crate (reconciliation,
//! backfill): a failing sink reports loudly instead of retrying, since
//! retrying an alert send could duplicate pages.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::backfill::BackfillHandle;
use crate::command::{StateHandle, TradingState};
use crate::error::VenueError;
use crate::reconcile::ReconcileHandle;
use crate::stream::StreamBreaker;

/// Which trigger raised the alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertSource {
    /// The producer-confidence breaker tripped.
    BreakerTrip,
    /// Journal and venue books disagree.
    ReconcileMismatch,
    /// A reconnect hole is unspliced.
    BackfillIncomplete,
    /// Entries were halted by operator command.
    KillSwitch,
}

/// Which side of the threshold split the trigger sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertClass {
    /// The decision's inputs may be wrong; entries halt.
    Correctness,
    /// The producer may be untrustworthy; entries disarm.
    ProducerConfidence,
}

impl AlertSource {
    /// The split side this trigger sits on: only the breaker trip doubts
    /// the producer; every halt trusts the doubt and stops entries.
    #[must_use]
    pub fn class(self) -> AlertClass {
        match self {
            Self::BreakerTrip => AlertClass::ProducerConfidence,
            Self::ReconcileMismatch | Self::BackfillIncomplete | Self::KillSwitch => {
                AlertClass::Correctness
            }
        }
    }
}

/// One delivered alert: what tripped, which side, why, when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    /// Which trigger raised it.
    pub source: AlertSource,
    /// The trigger's split side, stored so readers never recompute it.
    pub class: AlertClass,
    /// Human-readable reason.
    pub detail: String,
    /// When it was collected.
    pub at: DateTime<Utc>,
}

impl Alert {
    /// Build an alert whose class follows its source.
    #[must_use]
    pub fn new(source: AlertSource, detail: String, at: DateTime<Utc>) -> Self {
        Self { source, class: source.class(), detail, at }
    }
}

/// Out-of-band delivery: anything except the platform's own UI. Sinks are
/// synchronous snapshots like every other gate read here, so collecting
/// never blocks.
pub trait AlertSink: Send + Sync {
    /// Deliver one alert. Failure reports loudly; the caller decides what
    /// a lost page means.
    fn send(&self, alert: &Alert) -> Result<(), VenueError>;

    /// Deliver a batch in order, stopping at the first failure with the
    /// count delivered so far.
    fn send_all(&self, alerts: &[Alert]) -> Result<usize, VenueError> {
        let mut sent = 0;
        for alert in alerts {
            self.send(alert)?;
            sent += 1;
        }
        Ok(sent)
    }
}

/// In-memory sink: inspection for tests and the soak harness. Never fails.
#[derive(Debug, Clone, Default)]
pub struct MemorySink {
    inner: Arc<Mutex<Vec<Alert>>>,
}

impl MemorySink {
    /// An empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Delivered alerts, oldest first.
    #[must_use]
    pub fn alerts(&self) -> Vec<Alert> {
        common_core::sync::lock(&self.inner).clone()
    }
}

impl AlertSink for MemorySink {
    fn send(&self, alert: &Alert) -> Result<(), VenueError> {
        common_core::sync::lock(&self.inner).push(alert.clone());
        Ok(())
    }
}

/// Append-only file sink: one JSON object per line, parseable without the
/// platform. The alert survives the UI by construction. Unwritable paths
/// refuse fail-closed instead of buffering silently.
#[derive(Debug, Clone)]
pub struct FileSink {
    path: PathBuf,
}

impl FileSink {
    /// Sink appending to `path`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Where lines land.
    #[must_use]
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl AlertSink for FileSink {
    fn send(&self, alert: &Alert) -> Result<(), VenueError> {
        use std::fs::OpenOptions;
        use std::io::Write as _;
        let mut line =
            serde_json::to_string(alert).map_err(|e| VenueError::Alert(e.to_string()))?;
        line.push('\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut file| file.write_all(line.as_bytes()))
            .map_err(|e| VenueError::Alert(format!("{}: {e}", self.path.display())))
    }
}

/// Fan-out over member sinks: every member is attempted even when an
/// earlier one fails, and the failure reports each dark member by name.
/// Delivery is per-member all-or-reporting — a dark member never blocks a
/// live one.
#[derive(Clone, Default)]
pub struct FanoutSink {
    members: Vec<Arc<dyn AlertSink>>,
}

impl FanoutSink {
    /// Fan out over `members` in order.
    #[must_use]
    pub fn new(members: Vec<Arc<dyn AlertSink>>) -> Self {
        Self { members }
    }
}

impl AlertSink for FanoutSink {
    fn send(&self, alert: &Alert) -> Result<(), VenueError> {
        let mut failures = Vec::new();
        for member in &self.members {
            if let Err(error) = member.send(alert) {
                failures.push(error.to_string());
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(VenueError::Alert(format!("dark alert members: {}", failures.join("; "))))
        }
    }
}

/// Edge state for [`poll_alerts`]: which level triggers already reported,
/// so sticky trips and held halts alert once, never per poll.
#[derive(Debug, Default)]
pub struct AlertCursor {
    breaker_reported: bool,
    last_state: Option<TradingState>,
}

impl AlertCursor {
    /// A cursor that reported nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// The gates one poll reads. `None` skips the gate: watch only what the
/// deployment runs.
#[derive(Debug)]
pub struct GateWatch<'a> {
    /// Producer-confidence breaker; alerts once per trip.
    pub breaker: Option<&'a StreamBreaker>,
    /// Reconciliation gate; every banked line alerts.
    pub reconcile: Option<&'a ReconcileHandle>,
    /// Backfill gate; every banked line alerts.
    pub backfill: Option<&'a BackfillHandle>,
    /// Trading state; the transition into halt alerts.
    pub state: Option<&'a StateHandle>,
}

/// Collect newly tripped or halted alerts and deliver them through `sink`.
/// Returns the count delivered. Banks drain as alerts are built, so each
/// line delivers exactly once; the cursor keeps level triggers to their
/// edge.
pub fn poll_alerts(
    cursor: &mut AlertCursor,
    watch: &GateWatch<'_>,
    sink: &dyn AlertSink,
    now: DateTime<Utc>,
) -> Result<usize, VenueError> {
    let mut alerts = Vec::new();
    if let Some(breaker) = watch.breaker {
        if breaker.tripped() && !cursor.breaker_reported {
            alerts.push(Alert::new(
                AlertSource::BreakerTrip,
                breaker.trip_reason().unwrap_or_else(|| "stream breaker tripped".to_owned()),
                now,
            ));
            cursor.breaker_reported = true;
        }
    }
    if let Some(gate) = watch.reconcile {
        alerts.extend(
            gate.drain_alerts()
                .into_iter()
                .map(|detail| Alert::new(AlertSource::ReconcileMismatch, detail, now)),
        );
    }
    if let Some(gate) = watch.backfill {
        alerts.extend(
            gate.drain_alerts()
                .into_iter()
                .map(|detail| Alert::new(AlertSource::BackfillIncomplete, detail, now)),
        );
    }
    if let Some(state) = watch.state {
        let current = state.get();
        if current == TradingState::Disabled && cursor.last_state != Some(TradingState::Disabled) {
            alerts.push(Alert::new(
                AlertSource::KillSwitch,
                "entries halted by operator command: exits still route".to_owned(),
                now,
            ));
        }
        cursor.last_state = Some(current);
    }
    sink.send_all(&alerts)
}

#[cfg(test)]
#[path = "../tests/alert.rs"]
mod tests;
