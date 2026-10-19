//! Reconciliation: the venue is the source of truth, the journal is the view.
//!
//! The loop compares journaled fills against venue-truth positions. Any
//! mismatch halts new entries and raises an alert: the platform's own state
//! is never trusted over the venue, and trading never continues on a
//! disagreed book.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fluent_wvr::prelude::*;

use crate::error::VenueError;
use crate::journal::{JournalEvent, LEG_FILL_KIND};
use crate::order::{OrderSide, Position};
use crate::venue::LegExecution;

/// One position-level disagreement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileMismatch {
    /// Underlying that disagrees.
    pub underlying: String,
    /// Quantity the journal implies.
    pub journal_qty: i64,
    /// Quantity the venue reports.
    pub venue_qty: i64,
    /// Why they are believed to differ.
    pub reason: String,
}

/// Outcome of one reconciliation pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Whether every position agrees.
    pub matched: bool,
    /// One entry per disagreeing underlying.
    pub mismatches: Vec<ReconcileMismatch>,
    /// Human-readable alert lines; empty when matched.
    pub alerts: Vec<String>,
}

impl ReconcileReport {
    /// Fold `other`'s disagreements into this report: matched only when both
    /// match, mismatches and alerts concatenated. The one place the net and
    /// leg compares become a single verdict — observing them piecemeal would
    /// let a clean leg pass clear a net halt.
    pub fn absorb(&mut self, other: Self) {
        self.matched = self.matched && other.matched;
        self.mismatches.extend(other.mismatches);
        self.alerts.extend(other.alerts);
    }
}

/// Compare journaled fills (as signed quantities by underlying) against
/// venue-truth positions. Pure function: no I/O, deterministic.
#[must_use]
pub fn reconcile<S: std::hash::BuildHasher>(
    journal_signed_qty: &HashMap<String, i64, S>,
    venue: &[Position],
) -> ReconcileReport {
    let mut venue_qty: HashMap<&str, i64> = HashMap::new();
    for position in venue {
        *venue_qty.entry(position.underlying.as_str()).or_default() += position.quantity;
    }
    let journal_owned: HashMap<String, i64> = journal_signed_qty
        .iter()
        .map(|(key, qty)| (key.clone(), *qty))
        .collect();
    let venue_owned: HashMap<String, i64> = venue_qty
        .iter()
        .map(|(key, qty)| ((*key).to_owned(), *qty))
        .collect();
    compare_nets(&journal_owned, &venue_owned)
}

/// Compare leg-aware journal fills (signed quantities by contract OCC
/// symbol) against leg-aware venue truth on the same keys. Same pure compare
/// shape as [`reconcile`]: any disagreement halts entries through the same
/// [`ReconcileGate`].
#[must_use]
pub fn reconcile_legs<S: std::hash::BuildHasher, T: std::hash::BuildHasher>(
    journal_signed_qty: &HashMap<String, i64, S>,
    venue_signed_qty: &HashMap<String, i64, T>,
) -> ReconcileReport {
    compare_nets(journal_signed_qty, venue_signed_qty)
}

/// Net signed quantity per spread-leg contract from journaled leg rows:
/// buys add, sells subtract, keyed by OCC symbol. Non-leg rows are skipped;
/// a leg row whose detail does not parse is a journal fault, never a silent
/// skip.
pub fn journal_leg_net(events: &[JournalEvent]) -> Result<HashMap<String, i64>, VenueError> {
    let mut net: HashMap<String, i64> = HashMap::new();
    for event in events {
        if event.kind != LEG_FILL_KIND {
            continue;
        }
        let leg: LegExecution = serde_json::from_str(&event.detail)
            .map_err(|e| VenueError::Journal(format!("unreadable leg row: {e}")))?;
        let signed = match leg.side {
            OrderSide::Buy => i64::from(leg.quantity),
            OrderSide::Sell => -i64::from(leg.quantity),
        };
        *net.entry(leg.contract.as_str().to_owned()).or_default() += signed;
    }
    Ok(net)
}

/// Shared comparison: every key either side mentions, sorted for stable
/// reports; one mismatch entry per disagreement.
fn compare_nets<S: std::hash::BuildHasher, T: std::hash::BuildHasher>(
    journal_qty: &HashMap<String, i64, S>,
    venue_qty: &HashMap<String, i64, T>,
) -> ReconcileReport {
    let mut mismatches = Vec::new();
    let mut underlyings: Vec<&str> = journal_qty
        .keys()
        .map(String::as_str)
        .chain(venue_qty.keys().map(String::as_str))
        .collect();
    underlyings.sort_unstable();
    underlyings.dedup();
    for underlying in underlyings {
        let journal = journal_qty.get(underlying).copied().unwrap_or(0);
        let venue = venue_qty.get(underlying).copied().unwrap_or(0);
        if journal != venue {
            mismatches.push(ReconcileMismatch {
                underlying: underlying.to_owned(),
                journal_qty: journal,
                venue_qty: venue,
                reason: format!("journal implies {journal}, venue reports {venue}"),
            });
        }
    }
    let alerts = mismatches
        .iter()
        .map(|m| {
            format!(
                "RECONCILE HALT: {} journal={} venue={}",
                m.underlying, m.journal_qty, m.venue_qty
            )
        })
        .collect();
    ReconcileReport {
        matched: mismatches.is_empty(),
        mismatches,
        alerts,
    }
}

/// Entry gate driven by reconciliation: a mismatch halts entries while exits
/// stay permitted (managing existing positions never stops).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileGate {
    halted: bool,
    alerts: Vec<String>,
    /// Whether a report was ever observed. The gate itself stays permissive
    /// until it reads (every existing caller is preserved); the per-bar
    /// reading fails closed on no reading instead.
    observed: bool,
}

impl ReconcileGate {
    /// Observe one reconciliation report. A clean report clears the halt;
    /// any mismatch sets it and banks the alerts. Either way the gate has
    /// now answered.
    pub fn observe(&mut self, report: &ReconcileReport) {
        self.observed = true;
        if report.matched {
            self.halted = false;
        } else {
            self.halted = true;
            self.alerts.extend(report.alerts.iter().cloned());
        }
    }

    /// Whether the gate has answered at least once.
    #[must_use]
    pub fn is_observed(&self) -> bool {
        self.observed
    }

    /// Whether new entries must be refused.
    #[must_use]
    pub fn entries_halted(&self) -> bool {
        self.halted
    }

    /// Whether the journal and venue books currently agree, as the health
    /// taxonomy reads it: `false` fails the reconciliation gate.
    #[must_use]
    pub fn reconciled(&self) -> bool {
        !self.halted
    }

    /// Banked alert lines for out-of-band delivery.
    #[must_use]
    pub fn alerts(&self) -> &[String] {
        &self.alerts
    }

    /// Take the banked alerts for out-of-band delivery, leaving the halt
    /// state untouched. Delivery drains: alerts never accumulate without
    /// bound across reporting cycles.
    pub fn drain_alerts(&mut self) -> Vec<String> {
        std::mem::take(&mut self.alerts)
    }

    /// Refuse entries while halted. Call from the submission path.
    pub fn check_entries(&self) -> Result<(), VenueError> {
        if self.halted {
            Err(VenueError::Reconciliation(
                "entries halted: book disagrees with venue".to_owned(),
            ))
        } else {
            Ok(())
        }
    }
}

/// Net signed quantity the journal implies, for one underlying, from filled
/// records. Kept beside reconciliation (not in the journal) so replay and
/// live share the same accumulation.
#[must_use]
pub fn journal_net(fills: &[(String, i64)]) -> HashMap<String, i64> {
    let mut net: HashMap<String, i64> = HashMap::new();
    for (underlying, qty) in fills {
        *net.entry(underlying.clone()).or_default() += qty;
    }
    net
}

/// Asset the reconciliation sensor provides: units whose validity depends on
/// the book agreeing declare it in `depends`, so a mismatch cancels them
/// through the batch graph while exits and monitors stay independent.
pub const RECONCILE_ASSET: &str = "reconcile.agreed";

/// The journal's view of the book: signed quantity by underlying (the net
/// compare) and by OCC contract (the leg compare). Built once per pass from
/// the live fold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalBook {
    /// Signed quantity per underlying.
    pub net: HashMap<String, i64>,
    /// Signed quantity per OCC contract symbol.
    pub legs: HashMap<String, i64>,
}

/// The two sides one reconciliation pass compares. The session implements
/// this over its live journal fold and venue truth; tests implement it over
/// fixtures. Reads are synchronous snapshots, so [`ReconcileUnit::observe`]
/// never blocks.
pub trait ReconcileSource: Send + Sync {
    /// The book the journal implies.
    fn journal(&self) -> JournalBook;
    /// Venue-truth positions, for the net compare.
    fn venue_positions(&self) -> Vec<Position>;
    /// Venue-truth signed quantities by OCC contract, for the leg compare.
    /// Empty for venues that report no leg positions.
    fn venue_legs(&self) -> HashMap<String, i64>;
}

/// Shared handle to the reconciliation entry gate: the driver observes into
/// it, the submission path reads it. Locking never crosses an await (every
/// transition is synchronous), so a recovering mutex is the honest
/// primitive, never a wedged gate.
#[derive(Debug, Clone, Default)]
pub struct ReconcileHandle {
    inner: Arc<Mutex<ReconcileGate>>,
}

impl ReconcileHandle {
    /// A handle over an empty gate: nothing observed yet. The gate itself
    /// stays permissive until it reads; the per-bar reading fails closed on
    /// no reading.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the gate has answered at least once.
    #[must_use]
    pub fn is_observed(&self) -> bool {
        common_core::sync::lock(&self.inner).is_observed()
    }

    /// Observe one report: a clean report clears the halt, any mismatch sets
    /// it and banks the alerts.
    pub fn observe(&self, report: &ReconcileReport) {
        common_core::sync::lock(&self.inner).observe(report);
    }

    /// Whether new entries must be refused.
    #[must_use]
    pub fn entries_halted(&self) -> bool {
        common_core::sync::lock(&self.inner).entries_halted()
    }

    /// Whether the books currently agree (the health gate's reconciliation
    /// fact).
    #[must_use]
    pub fn reconciled(&self) -> bool {
        common_core::sync::lock(&self.inner).reconciled()
    }

    /// Refuse entries while halted.
    pub fn check_entries(&self) -> Result<(), VenueError> {
        common_core::sync::lock(&self.inner).check_entries()
    }

    /// Banked alert lines, cloned out (the lock never escapes).
    #[must_use]
    pub fn alerts(&self) -> Vec<String> {
        common_core::sync::lock(&self.inner).alerts().to_vec()
    }

    /// Take the banked alerts for out-of-band delivery.
    pub fn drain_alerts(&self) -> Vec<String> {
        common_core::sync::lock(&self.inner).drain_alerts()
    }
}

/// One reconciliation pass as a supervised unit, scheduled by the session
/// where the venue can diverge from the journal (the paper venue that
/// produced the journal is the one case it is a self-check). Reads the
/// journal's book and venue truth from `source` (both synchronous
/// snapshots), observes the combined net-plus-leg report into the shared
/// gate, and fails on mismatch so entry-carrying units are cancelled through
/// the batch graph while exits stay independent.
pub struct ReconcileUnit {
    source: Arc<dyn ReconcileSource>,
    gate: ReconcileHandle,
    provides: Vec<ArcIntern<str>>,
}

impl ReconcileUnit {
    /// Build the sensor over `source`, observing into `gate`.
    #[must_use]
    pub fn new(source: Arc<dyn ReconcileSource>, gate: ReconcileHandle) -> Self {
        Self {
            source,
            gate,
            provides: vec![ArcIntern::from(RECONCILE_ASSET)],
        }
    }

    /// Run one pass: compare the journal's book against venue truth, observe
    /// the combined report into the shared gate, and return it.
    #[must_use]
    pub fn observe(&self) -> ReconcileReport {
        let journal = self.source.journal();
        let mut report = reconcile(&journal.net, &self.source.venue_positions());
        report.absorb(reconcile_legs(&journal.legs, &self.source.venue_legs()));
        self.gate.observe(&report);
        report
    }
}

impl WorkUnit for ReconcileUnit {
    fn name(&self) -> &str {
        "reconcile.sensor"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let report = self.observe();
        if report.matched {
            Ok(WorkOutput::ok("book agrees with venue"))
        } else {
            Err(WorkError::Execution(report.alerts.join("; ")))
        }
    }
}

impl_fieldless!(ReconcileUnit);

impl Describable for ReconcileUnit {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "description": "Reconciliation sensor: fails while the journal book disagrees with the venue",
        })
    }
}

impl_component!(ReconcileUnit);

#[cfg(test)]
#[path = "../tests/reconcile.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/reconcile_driver.rs"]
mod driver_tests;
