//! The stream-health hysteresis: whether a connectivity change is news.
//!
//! A broker disconnect loses no data by itself — the loss shows up as the
//! watermark falling behind, which [`classify`](crate::coverage::classify)
//! already reports. So this machine fires on exactly one transition,
//! recovery: it answers "the stream is back, look at the tape now" once per
//! outage and stays silent about everything else. A repair pass gated on this
//! edge runs after a reconnect and never at boot, because booting healthy is
//! [`HealthEdge::Init`] resolving quietly, not a recovery.
//!
//! The shape is deliberately the same hysteresis vocabulary as the terminal's
//! session-regime labels: one enum, one pure step function, no timers.

/// Where the stream-health machine stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// The stream-health hysteresis: whether a connectivity change is news.
/// 
/// A broker disconnect loses no data by itself — the loss shows up as the
/// watermark falling behind, which [`classify`](crate::coverage::classify)
/// already reports. So this machine fires on exactly one transition,
/// recovery: it answers "the stream is back, look at the tape now" once per
/// outage and stays silent about everything else. A repair pass gated on this
/// edge runs after a reconnect and never at boot, because booting healthy is
/// [`HealthEdge::Init`] resolving quietly, not a recovery.
/// 
/// The shape is deliberately the same hysteresis vocabulary as the terminal's
/// session-regime labels: one enum, one pure step function, no timers.
/// Where the stream-health machine stands.
pub enum HealthEdge {
    /// Never sampled. What keeps a healthy boot from firing a repair pass.
    #[default]
    Init,
    /// Disconnected at least once since the last recovery.
    Down,
    /// Connected after being down: the one transition that fires.
    Up,
    /// Connected, and already reported as such.
    Steady,
}

/// Step the machine on one connectivity sample.
///
/// Returns the next state and whether this transition is a recovery edge —
/// the only signal that should wake a repair pass. `Down → Up` fires exactly
/// once per outage: the step after a firing recovery lands in
/// [`HealthEdge::Up`], whose next connected sample resolves to
/// [`HealthEdge::Steady`] silently.
#[must_use]
/// Step the machine on one connectivity sample.
/// 
/// Returns the next state and whether this transition is a recovery edge —
/// the only signal that should wake a repair pass. `Down → Up` fires exactly
/// once per outage: the step after a firing recovery lands in
/// [`HealthEdge::Up`], whose next connected sample resolves to
/// [`HealthEdge::Steady`] silently.
pub fn health_edge(previous: HealthEdge, connected: bool) -> (HealthEdge, bool) {
    // Silence is the rule: only a recovery wakes a repair pass.
    let fires = connected && previous == HealthEdge::Down;
    (next_health(previous, connected), fires)
}

/// The state half of [`health_edge`], for callers that track edges themselves.
#[must_use]
/// The state half of [`health_edge`], for callers that track edges themselves.
pub fn next_health(previous: HealthEdge, connected: bool) -> HealthEdge {
    if !connected {
        return HealthEdge::Down;
    }
    match previous {
        HealthEdge::Init | HealthEdge::Down => HealthEdge::Up,
        HealthEdge::Up | HealthEdge::Steady => HealthEdge::Steady,
    }
}

#[cfg(test)]
#[path = "../tests/health_edge.rs"]
mod tests;