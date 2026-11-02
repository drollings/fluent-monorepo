//! Admission policy: bounded concurrency, fast refusal, endpoint
//! ejection with probe-gated re-entry.
//!
//! One policy for every serving path: turns are admitted while under
//! the bound and refused fast with a named reason past it; endpoints
//! whose turns keep failing with transport errors eject from rotation
//! until a cheap probe says they are healthy again. The numbers (the
//! bound, the ejection streak) ship only inside their calibrated bands
//! — see the calibration register and the admission harness.

/// Consecutive `Backend` turn failures that eject the endpoint.
/// Calibrated: a single transient failure must never eject (the
/// harness kills threshold 1), two consecutive must.
pub const EJECTION_THRESHOLD: usize = 2;

/// Named refusal when a turn arrives past the concurrency bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionRefusal {
    /// Exact line shown: cause, inflight count, bound, remedy.
    pub reason: String,
}

/// Bounded concurrency gate: admit while under the bound, refuse fast
/// past it. The counter is a plain mutex held only for
/// increment/decrement — never across an await, never across a turn.
#[derive(Debug)]
pub struct AdmissionGate {
    max_inflight: usize,
    inflight: std::sync::Mutex<usize>,
}

impl AdmissionGate {
    /// Gate admitting at most `max_inflight` concurrent turns.
    #[must_use]
    pub fn new(max_inflight: usize) -> Self {
        Self {
            max_inflight: max_inflight.max(1),
            inflight: std::sync::Mutex::new(0),
        }
    }

    /// Configured bound.
    #[must_use]
    pub fn max_inflight(&self) -> usize {
        self.max_inflight
    }

    /// Turns currently in flight.
    #[must_use]
    pub fn inflight(&self) -> usize {
        *common_core::sync::lock(&self.inflight)
    }

    /// Admit one turn or refuse fast with the named reason. The guard
    /// releases on drop; dropping the registry entry releases its
    /// permit the same way.
    pub fn admit(&self) -> Result<AdmissionGuard<'_>, AdmissionRefusal> {
        let mut inflight = common_core::sync::lock(&self.inflight);
        if *inflight >= self.max_inflight {
            return Err(AdmissionRefusal {
                reason: format!(
                    "admission saturated: {} turns in flight (bound {}) — retry when a turn completes",
                    *inflight, self.max_inflight
                ),
            });
        }
        let depth = *inflight;
        *inflight += 1;
        Ok(AdmissionGuard { gate: self, depth })
    }
}

/// One admitted turn: releases on drop. The depth is the number of
/// other turns already in flight — the calibration harness scores it
/// as the breach predictor.
#[derive(Debug)]
pub struct AdmissionGuard<'a> {
    gate: &'a AdmissionGate,
    depth: usize,
}

impl AdmissionGuard<'_> {
    /// Other turns in flight when this turn was admitted.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.depth
    }
}

impl Drop for AdmissionGuard<'_> {
    fn drop(&mut self) {
        let mut inflight = common_core::sync::lock(&self.gate.inflight);
        *inflight = inflight.saturating_sub(1);
    }
}

/// Endpoint ejection tracker: consecutive transport (`Backend`)
/// turn failures eject; any responsive turn re-enters. Responsive
/// includes voiceless (`NoAudio`) — the server answered, it just had
/// nothing to voice — so only true transport failures count.
#[derive(Debug)]
pub struct EjectionTracker {
    threshold: usize,
    consecutive: usize,
    ejected: bool,
}

impl EjectionTracker {
    /// Tracker ejecting after `threshold` consecutive failures.
    #[must_use]
    pub fn new(threshold: usize) -> Self {
        Self {
            threshold: threshold.max(1),
            consecutive: 0,
            ejected: false,
        }
    }

    /// Whether the endpoint is currently ejected.
    #[must_use]
    pub fn is_ejected(&self) -> bool {
        self.ejected
    }

    /// Consecutive failures counted so far.
    #[must_use]
    pub fn consecutive(&self) -> usize {
        self.consecutive
    }

    /// A responsive turn: breaks the streak and re-enters.
    pub fn record_success(&mut self) {
        self.consecutive = 0;
        self.ejected = false;
    }

    /// A transport-failed turn: counts toward ejection.
    pub fn record_backend(&mut self) {
        self.consecutive += 1;
        if self.consecutive >= self.threshold {
            self.ejected = true;
        }
    }
}

#[cfg(test)]
#[path = "../tests/admission.rs"]
mod tests;
