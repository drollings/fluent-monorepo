//! Reconciliation: the venue is the source of truth, the journal is the view.
//!
//! The loop compares journaled fills against venue-truth positions. Any
//! mismatch halts new entries and raises an alert: the platform's own state
//! is never trusted over the venue, and trading never continues on a
//! disagreed book.

use std::collections::HashMap;

use crate::error::VenueError;
use crate::order::Position;

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
    let mut mismatches = Vec::new();
    let mut underlyings: Vec<&str> = journal_signed_qty
        .keys()
        .map(String::as_str)
        .chain(venue_qty.keys().copied())
        .collect();
    underlyings.sort_unstable();
    underlyings.dedup();
    for underlying in underlyings {
        let journal = journal_signed_qty.get(underlying).copied().unwrap_or(0);
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
