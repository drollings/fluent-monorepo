//! Deterministic respond-vs-route policy (DD-1).
//!
//! The classifier model never chooses whether to respond. It emits a `domain`
//! route key and a self-assessed `confidence`; the router derives the outcome.
//! This module is the **single** place that computes respond-vs-route from
//! `domain + confidence + always_route` (DRY §5 rule 3) — no other code may
//! branch on a classifier "action".

/// The router-derived outcome of a classifier decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassifierAction {
    /// The classifier answers directly (its `response` text is the answer).
    Respond,
    /// Dispatch to the domain's model group.
    Route,
}

/// Derive `Respond` vs `Route` for a classifier decision.
///
/// - A `dispatch_only` route (config `always_route: true`) always routes: its
///   response function is a dispatch, never a classifier direct answer — even
///   at maximum confidence (DD-3).
/// - Otherwise a confidence at or above `respond_threshold` responds directly;
///   below it routes.
///
/// `domain` is resolved to a route key *before* this is called (an unknown
/// domain falls back to `default_route`), and `dispatch_only` is that resolved
/// route's `always_route` flag — domain resolution and the `always_route`
/// lookup are the classifier stage's job (SRP); this function is only the
/// respond-vs-route computation. `_domain` is retained in the signature so the
/// decision point reads as "derive from the domain's posture", and to keep the
/// caller explicit about which route it resolved.
#[must_use]
pub fn derive_action(
    _domain: &str,
    confidence: f64,
    respond_threshold: f64,
    dispatch_only: bool,
) -> ClassifierAction {
    if dispatch_only {
        return ClassifierAction::Route;
    }
    if confidence >= respond_threshold {
        ClassifierAction::Respond
    } else {
        ClassifierAction::Route
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_only_routes_even_at_maximum_confidence() {
        assert_eq!(
            derive_action("code", 1.0, 0.6, true),
            ClassifierAction::Route,
            "an always_route domain must never respond directly"
        );
    }

    #[test]
    fn dispatch_only_routes_at_any_confidence() {
        assert_eq!(
            derive_action("code", 0.0, 0.6, true),
            ClassifierAction::Route
        );
    }

    #[test]
    fn confident_non_dispatch_only_responds() {
        assert_eq!(
            derive_action("local", 0.9, 0.6, false),
            ClassifierAction::Respond
        );
    }

    #[test]
    fn low_confidence_non_dispatch_only_routes() {
        assert_eq!(
            derive_action("local", 0.5, 0.6, false),
            ClassifierAction::Route
        );
    }

    #[test]
    fn threshold_boundary_is_inclusive() {
        assert_eq!(
            derive_action("local", 0.6, 0.6, false),
            ClassifierAction::Respond,
            "confidence exactly at the threshold responds"
        );
    }
}