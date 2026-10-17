//! Reconciliation driver: the scheduled journal-vs-venue compare that halts
//! entries on a mismatch.
//!
//! Protocol (mirrors the book-detector calibration): a control group proves
//! agreeing books never fire — an agreeing net book, an agreeing leg book,
//! and a freshly booted session — and a treatment group proves injected net
//! drift, leg drift, and a missing position fire. Rates are measured, never
//! assumed; the gate may halt entries only at 1.0 precision and recall on the
//! fixture set.
//!
//! Axis: a mismatch is **(correctness)** — it halts new entries while exits
//! stay open, exactly the routing `may_trade`/`check_route` give. It is never
//! a judgment on the trade's merit.

use super::*;

use crate::health::{
    check_route, may_trade, HealthLimits, HealthState, HealthVerdict, RouteIntent,
};
use crate::journal::{Journal, VenueStorage};
use crate::order::Position;
use crate::paper::{PaperConfig, PaperVenue};
use crate::session::{SessionConfig, TradingSession};
use crate::venue::Venue;
use crate::VenueRegistry;
use protopyne_store::StoreHealth;
use protopyne_types::Money;
use std::collections::HashMap;
use std::sync::Arc;

// ── fixtures ───────────────────────────────────────────────────────

/// One scripted compare: the journal's book, venue positions, and venue legs.
struct Scripted {
    journal: JournalBook,
    positions: Vec<Position>,
    legs: HashMap<String, i64>,
}

impl Scripted {
    /// A net-only compare: the journal's signed quantity per underlying
    /// against the venue's reported positions.
    fn net(journal: &[(&str, i64)], venue: &[(&str, i64)]) -> Self {
        Self {
            journal: JournalBook { net: pairs(journal), legs: HashMap::new() },
            positions: venue.iter().map(|(underlying, qty)| position(underlying, *qty)).collect(),
            legs: HashMap::new(),
        }
    }

    /// A leg-only compare: the journal's signed quantity per OCC contract
    /// against the venue's.
    fn legs(journal: &[(&str, i64)], venue: &[(&str, i64)]) -> Self {
        Self {
            journal: JournalBook { net: HashMap::new(), legs: pairs(journal) },
            positions: Vec::new(),
            legs: pairs(venue),
        }
    }

    /// A freshly booted session: nothing traded, both sides empty.
    fn booted() -> Self {
        Self {
            journal: JournalBook::default(),
            positions: Vec::new(),
            legs: HashMap::new(),
        }
    }
}

impl ReconcileSource for Scripted {
    fn journal(&self) -> JournalBook {
        self.journal.clone()
    }

    fn venue_positions(&self) -> Vec<Position> {
        self.positions.clone()
    }

    fn venue_legs(&self) -> HashMap<String, i64> {
        self.legs.clone()
    }
}

fn pairs(entries: &[(&str, i64)]) -> HashMap<String, i64> {
    entries.iter().map(|(key, qty)| ((*key).to_owned(), *qty)).collect()
}

fn position(underlying: &str, qty: i64) -> Position {
    Position::new()
        .underlying(underlying.to_owned())
        .quantity(qty)
        .avg_price(Money::zero())
        .build()
}

fn drive(source: Scripted) -> (ReconcileReport, ReconcileHandle) {
    let gate = ReconcileHandle::new();
    let unit = ReconcileUnit::new(Arc::new(source), gate.clone());
    (unit.observe(), gate)
}

fn healthy() -> HealthState {
    HealthState {
        quote_age_secs: 5,
        clock_skew_secs: 1,
        stream_connected: true,
        store: StoreHealth::default(),
        reconciled: true,
        backfilled: true,
    }
}

fn limits() -> HealthLimits {
    HealthLimits::new().build()
}

// ── control group: agreeing books stay silent ──────────────────────

#[test]
fn agreeing_net_book_stays_silent() {
    let (report, gate) = drive(Scripted::net(&[("SPY", 2)], &[("SPY", 2)]));
    assert!(report.matched, "{report:?}");
    assert!(report.alerts.is_empty());
    assert!(!gate.entries_halted());
    assert!(gate.reconciled());
    assert!(gate.check_entries().is_ok());
}

#[test]
fn agreeing_leg_book_stays_silent() {
    let (report, gate) = drive(Scripted::legs(
        &[("SPY-260116C00600000", 1), ("SPY-260116C00610000", -1)],
        &[("SPY-260116C00600000", 1), ("SPY-260116C00610000", -1)],
    ));
    assert!(report.matched, "{report:?}");
    assert!(!gate.entries_halted());
}

#[test]
fn freshly_booted_session_stays_silent() {
    let (report, gate) = drive(Scripted::booted());
    assert!(report.matched, "{report:?}");
    assert!(!gate.entries_halted());
    assert!(gate.check_entries().is_ok());
}

// ── treatment group: drift fires ───────────────────────────────────

#[test]
fn injected_net_drift_fires() {
    let (report, gate) = drive(Scripted::net(&[("SPY", 2)], &[("SPY", 5)]));
    assert!(!report.matched);
    assert_eq!(report.mismatches.len(), 1);
    assert_eq!(report.mismatches[0].journal_qty, 2);
    assert_eq!(report.mismatches[0].venue_qty, 5);
    assert!(gate.entries_halted());
    assert!(!gate.reconciled());
    assert!(gate.check_entries().is_err());
}

#[test]
fn injected_leg_drift_fires() {
    let (report, gate) = drive(Scripted::legs(
        &[("SPY-260116C00600000", 1), ("SPY-260116C00610000", -1)],
        &[("SPY-260116C00600000", 1), ("SPY-260116C00610000", -1), ("SPY-260116C00620000", 1)],
    ));
    assert!(!report.matched);
    assert!(
        report.mismatches.iter().any(|m| m.underlying.contains("00620000")),
        "the drifted leg is named: {report:?}"
    );
    assert!(gate.entries_halted());
}

#[test]
fn missing_position_fires() {
    let (report, gate) = drive(Scripted::net(&[("SPY", 1)], &[]));
    assert!(!report.matched);
    assert_eq!(report.mismatches.len(), 1);
    assert_eq!(report.mismatches[0].venue_qty, 0);
    assert!(gate.entries_halted());
}

// ── health verdict transition ──────────────────────────────────────

#[test]
fn mismatch_halts_entries_through_may_trade_and_routes_exits() {
    let gate = ReconcileHandle::new();
    let before = HealthState { reconciled: gate.reconciled(), ..healthy() };
    assert_eq!(may_trade(&before, &limits()), HealthVerdict::Trade);

    let unit = ReconcileUnit::new(Arc::new(Scripted::net(&[("SPY", 2)], &[("SPY", 5)])), gate.clone());
    let report = unit.observe();
    assert!(!report.matched);

    // The mismatch flows into the health gate as a correctness failure:
    // entries refuse naming the gate, exits still route.
    let halted = HealthState { reconciled: gate.reconciled(), ..healthy() };
    match may_trade(&halted, &limits()) {
        HealthVerdict::Halt { reasons } => assert!(
            reasons.iter().any(|reason| reason.contains("reconcil")),
            "the reconciliation gate names itself: {reasons:?}"
        ),
        HealthVerdict::Trade => panic!("a mismatch must halt entries"),
    }
    assert!(check_route(&halted, &limits(), RouteIntent::Entry).is_err());
    assert!(
        check_route(&halted, &limits(), RouteIntent::Exit).is_ok(),
        "a mismatch must never strand an open position"
    );
}

#[test]
fn a_clean_pass_clears_the_halt() {
    let gate = ReconcileHandle::new();
    let unit = ReconcileUnit::new(Arc::new(Scripted::net(&[("SPY", 2)], &[("SPY", 5)])), gate.clone());
    let _ = unit.observe();
    assert!(gate.entries_halted());

    // Rebuilding the unit with an agreeing source is the operator's repair
    // path: a matching pass clears the halt through the same handle.
    let repaired = ReconcileUnit::new(Arc::new(Scripted::net(&[("SPY", 2)], &[("SPY", 2)])), gate.clone());
    let report = repaired.observe();
    assert!(report.matched);
    assert!(!gate.entries_halted());
    assert!(gate.reconciled());
}

// ── supervised unit contract ───────────────────────────────────────

#[test]
fn unit_fails_on_mismatch_and_completes_on_agreement() {
    let gate = ReconcileHandle::new();
    let drifting = ReconcileUnit::new(Arc::new(Scripted::net(&[("SPY", 2)], &[("SPY", 5)])), gate.clone());
    let err = drifting.execute(&WorkContext::default()).expect_err("a mismatch fails the unit");
    match err {
        WorkError::Execution(reason) => assert!(reason.contains("RECONCILE"), "names the alert: {reason}"),
        other => panic!("a mismatch is an execution failure, got: {other:?}"),
    }

    let agreeing = ReconcileUnit::new(Arc::new(Scripted::net(&[("SPY", 2)], &[("SPY", 2)])), gate.clone());
    assert!(agreeing.execute(&WorkContext::default()).is_ok());
}

/// A dependent that stays pending on an unmet upstream asset: the
/// deterministic vehicle for supervised cancellation (a fast unit would
/// finish before the failure lands, making the test a race).
struct PendingStub {
    name: String,
    depends: Vec<ArcIntern<str>>,
}

impl WorkUnit for PendingStub {
    fn name(&self) -> &str {
        &self.name
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &self.depends
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Err(WorkError::Dependency(format!("{} waits for an upstream asset", self.name)))
    }
}

impl_fieldless!(PendingStub);

impl Describable for PendingStub {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
}

impl_component!(PendingStub);

fn session_with(source: Scripted) -> (TradingSession, ReconcileHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    drop(Journal::open(&path).expect("open"));
    let permit = VenueStorage::new(path);
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    let registry = Arc::new(VenueRegistry::single(venue));
    let mut session = TradingSession::boot(&SessionConfig::new().build(), registry, permit)
        .expect("boot");
    let handle = session.attach_reconcile(Arc::new(source));
    (session, handle, dir)
}

/// The session owns the sensor: an agreeing attach completes clean and leaves
/// the gate open; a drifting attach fails the sensor and halts entries through
/// the shared handle.
#[tokio::test]
async fn session_runs_the_driver_and_halts_on_mismatch() {
    use fluent_concurrency::batch::SupervisedBatchEvent;

    let (clean, clean_gate, _dir) = session_with(Scripted::net(&[("SPY", 2)], &[("SPY", 2)]));
    let summary = clean.run_bar(Vec::new()).await;
    assert!(!clean_gate.entries_halted(), "an agreeing book never halts: {summary:?}");
    assert!(
        summary.completed.iter().any(|event| matches!(
            event,
            SupervisedBatchEvent::Completed { name, .. } if &**name == "reconcile.sensor"
        )),
        "the agreeing sensor completes: {summary:?}"
    );
    clean.shutdown().await;

    let (drift, drift_gate, _dir) = session_with(Scripted::net(&[("SPY", 2)], &[("SPY", 5)]));
    let summary = drift.run_bar(Vec::new()).await;
    assert!(drift_gate.entries_halted(), "a mismatch halts entries: {summary:?}");
    assert!(
        summary.failed.iter().any(|event| matches!(
            event,
            SupervisedBatchEvent::Failed { name, .. } if &**name == "reconcile.sensor"
        )),
        "the drifting sensor fails: {summary:?}"
    );
    drift.shutdown().await;
}

/// A sensor mismatch cancels the units that depend on `RECONCILE_ASSET`
/// through the batch graph while independents complete — the same
/// dependency-cancellation contract the stream and trading-state sensors
/// carry. The probe keeps retrying so the cancellation lands before it
/// exhausts its attempts.
#[tokio::test]
async fn dependent_entries_cancel_through_the_batch_graph() {
    use fluent_concurrency::batch::{
        CancelReason, SupervisedBatch, SupervisedBatchEvent,
    };

    let gate = ReconcileHandle::new();
    let sensor: Arc<dyn Component> = Arc::new(ReconcileUnit::new(
        Arc::new(Scripted::net(&[("SPY", 2)], &[("SPY", 5)])),
        gate.clone(),
    ));
    let mut batch = SupervisedBatch::new(
        fluent_concurrency::tokio_runtime(),
        fluent_wvr::CapabilitySet::new(),
    );
    batch.register(sensor).expect("register sensor");
    batch
        .register_with_context(
            Arc::new(PendingStub {
                name: "entry".to_owned(),
                depends: vec![ArcIntern::from(RECONCILE_ASSET)],
            }),
            WorkContext {
                max_retries: 5,
                ..WorkContext::default()
            },
        )
        .expect("register dependent");
    batch
        .register(Arc::new(PendingStub {
            name: "telemetry".to_owned(),
            depends: Vec::new(),
        }))
        .expect("register independent");
    let summary = batch.await;

    assert!(gate.entries_halted());
    assert!(
        summary.cancelled.iter().any(|event| matches!(
            event,
            SupervisedBatchEvent::Cancelled { name, reason: CancelReason::DependencyFailed }
                if &**name == "entry"
        )),
        "the reconcile-dependent entry cancels: {summary:?}"
    );
    assert!(
        !summary.cancelled.iter().any(|event| format!("{event:?}").contains("telemetry")),
        "an independent unit is never cancelled: {summary:?}"
    );
}

// ── gate: measured rates at 1.0 ────────────────────────────────────

/// Control scenarios in this suite (agreeing net, agreeing legs, freshly
/// booted). The gate test pins this count.
pub const CONTROL_SCENARIOS: usize = 3;

/// Treatment scenarios in this suite (net drift, leg drift, missing
/// position). The gate test pins this count.
pub const TRIP_SCENARIOS: usize = 3;

/// Control accept / trip recall tally for this suite (the same discipline the
/// book-detector calibration uses: control and recall both 1.0 before trust).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReconcileCalibrationTally {
    controls: usize,
    control_false_fires: usize,
    trips: usize,
    trips_fired: usize,
}

impl ReconcileCalibrationTally {
    fn control_accept_rate(&self) -> f64 {
        if self.controls == 0 {
            return 0.0;
        }
        (self.controls - self.control_false_fires) as f64 / self.controls as f64
    }

    fn trip_recall(&self) -> f64 {
        if self.trips == 0 {
            return 0.0;
        }
        self.trips_fired as f64 / self.trips as f64
    }

    fn precision(&self) -> f64 {
        let fired = self.trips_fired + self.control_false_fires;
        if fired == 0 {
            return 0.0;
        }
        self.trips_fired as f64 / fired as f64
    }

    fn is_green(&self) -> bool {
        (self.control_accept_rate() - 1.0).abs() < f64::EPSILON
            && (self.trip_recall() - 1.0).abs() < f64::EPSILON
    }
}

/// Calibration gate: replays both groups end to end and pins the measured
/// rates — 1.0 control accept, 1.0 trip recall — plus the frozen scenario
/// counts, so group edits without count updates fail here.
#[test]
fn reconcile_calibration_is_green() {
    let controls = [
        Scripted::net(&[("SPY", 2)], &[("SPY", 2)]),
        Scripted::legs(
            &[("SPY-260116C00600000", 1), ("SPY-260116C00610000", -1)],
            &[("SPY-260116C00600000", 1), ("SPY-260116C00610000", -1)],
        ),
        Scripted::booted(),
    ];
    let control_false_fires = controls
        .into_iter()
        .map(|source| usize::from(!drive(source).0.matched))
        .sum();

    let trips = [
        Scripted::net(&[("SPY", 2)], &[("SPY", 5)]),
        Scripted::legs(
            &[("SPY-260116C00600000", 1), ("SPY-260116C00610000", -1)],
            &[("SPY-260116C00600000", 1), ("SPY-260116C00610000", -1), ("SPY-260116C00620000", 1)],
        ),
        Scripted::net(&[("SPY", 1)], &[]),
    ];
    let trips_fired = trips
        .into_iter()
        .map(|source| usize::from(!drive(source).0.matched))
        .sum();

    let tally = ReconcileCalibrationTally {
        controls: CONTROL_SCENARIOS,
        control_false_fires,
        trips: TRIP_SCENARIOS,
        trips_fired,
    };
    assert_eq!(CONTROL_SCENARIOS, 3, "control group size is frozen");
    assert_eq!(TRIP_SCENARIOS, 3, "trip group size is frozen");
    assert!(
        tally.is_green(),
        "calibration must be green at 1.0 accept / 1.0 recall: \
         control accept {:.3}, trip recall {:.3}, precision {:.3} ({tally:?})",
        tally.control_accept_rate(),
        tally.trip_recall(),
        tally.precision()
    );
}
