//! Reconciliation: the venue is the source of truth, the journal is the view.
//!
//! The loop compares journaled fills against venue-truth positions. Any
//! mismatch halts new entries and raises an alert: the platform's own state
//! is never trusted over the venue, and trading never continues on a
//! disagreed book.

use std::collections::HashMap;

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
}

impl ReconcileGate {
    /// Observe one reconciliation report. A clean report clears the halt;
    /// any mismatch sets it and banks the alerts.
    pub fn observe(&mut self, report: &ReconcileReport) {
        if report.matched {
            self.halted = false;
        } else {
            self.halted = true;
            self.alerts.extend(report.alerts.iter().cloned());
        }
    }

    /// Whether new entries must be refused.
    #[must_use]
    pub fn entries_halted(&self) -> bool {
        self.halted
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

#[cfg(test)]
#[path = "../tests/reconcile.rs"]
mod tests;
