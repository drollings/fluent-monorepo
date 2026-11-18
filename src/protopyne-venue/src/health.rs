//! Health gate: trade only on fresh data, a sane clock, a live stream, and
//! a reachable, current store.
//!
//! Every bound here is **(correctness)**: quote age, stream state, clock
//! skew, and store liveness decide whether a decision is valid, never whether
//! it is attractive. A closed gate refuses with reasons; it never degrades
//! into a warning that trading continues past.

use bon::Builder;
use serde::{Deserialize, Serialize};

use protopyne_store::StoreHealth;

use crate::error::VenueError;

/// Shared alert bank for entry gates (M9.2): the one `Vec<String>` +
/// take-drain behind `BackfillGate` and `ReconcileGate`. The gates keep
/// their own halt/suspend state, report types, and refusal vocabulary
/// (different ubiquitous language — resync vs reconciliation); only the
/// banking drains through here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlertBuffer {
    alerts: Vec<String>,
}

impl AlertBuffer {
    /// Empty bank.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bank one alert line.
    pub fn push(&mut self, alert: String) {
        self.alerts.push(alert);
    }

    /// Bank a report's alert lines.
    pub fn extend(&mut self, alerts: impl IntoIterator<Item = String>) {
        self.alerts.extend(alerts);
    }

    /// Banked alert lines for out-of-band delivery.
    #[must_use]
    pub fn alerts(&self) -> &[String] {
        &self.alerts
    }

    /// Take the banked alerts for out-of-band delivery, leaving the gate
    /// state untouched. Delivery drains: alerts never accumulate without
    /// bound across reporting cycles.
    pub fn drain_alerts(&mut self) -> Vec<String> {
        std::mem::take(&mut self.alerts)
    }
}

/// Observed health facts. State, not opinion: callers fill these from the
/// feed, the clock, the connection supervisor, and the store's coverage
/// probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthState {
    /// Age of the newest quote in seconds.
    pub quote_age_secs: u64,
    /// Observed clock skew in seconds.
    pub clock_skew_secs: u64,
    /// Whether the market-data stream is connected.
    pub stream_connected: bool,
    /// Latest store liveness reading. `Default` is no reading, which is no
    /// objection: a state that never observed the store evaluates exactly
    /// as it did before the store gate existed. The observation flag tells
    /// a genuine default-valued reading apart; the verdict treats both as
    /// no objection either way.
    pub store: StoreHealth,
    /// Whether the journal's book agrees with the venue's. No objection by
    /// default: a state built before the reconciliation driver ran (or on a
    /// path with no venue to diverge) evaluates exactly as it did before the
    /// reconciliation gate existed.
    #[serde(default = "reconciled_when_absent")]
    pub reconciled: bool,
    /// Whether the evaluated series is contiguous. No objection by default:
    /// a state built where no backfill sensor runs evaluates exactly as it
    /// did before the backfill gate existed; only an observed hole fails it.
    #[serde(default = "backfilled_when_absent")]
    pub backfilled: bool,
}

/// A state that never observed reconciliation is no objection, matching the
/// store reading's "no reading is no objection" rule; only a state that
/// observed a mismatch fails the gate.
fn reconciled_when_absent() -> bool {
    true
}

/// A state that never ran a backfill sensor is no objection, matching the
/// reconciliation rule; only a state that observed a hole fails the gate.
fn backfilled_when_absent() -> bool {
    true
}

/// Health bounds. All five are **(correctness)** gates.
#[derive(Debug, Clone, Copy, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct HealthLimits {
    /// Maximum quote age in seconds **(correctness)**.
    #[builder(default = 60)]
    pub max_quote_age_secs: u64,
    /// Maximum clock skew in seconds **(correctness)**.
    #[builder(default = 5)]
    pub max_clock_skew_secs: u64,
    /// Maximum age of the newest committed bar in seconds
    /// **(correctness)**. Mirrors the quote-age horizon: one staleness
    /// definition for the decision's inputs, whether they arrive over the
    /// feed or out of the store.
    #[builder(default = 60)]
    pub max_store_watermark_age_secs: u64,
    /// Maximum unresolved store holes tolerated **(correctness)**. Zero:
    /// any hole the last classification reported halts entries until the
    /// repair pass closes it.
    #[builder(default = 0)]
    pub max_open_gaps: u64,
}

/// Whether the platform may trade right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthVerdict {
    /// All gates pass.
    Trade,
    /// At least one gate fails, with every reason.
    Halt {
        /// Every failed gate.
        reasons: Vec<String>,
    },
}

impl HealthVerdict {
    /// Whether trading may proceed.
    #[must_use]
    pub fn may_trade(&self) -> bool {
        matches!(self, Self::Trade)
    }
}

/// One named health gate: a single check the verdict builder evaluates.
/// There is exactly one variant per `may_trade` gate, so a gate added to
/// the evaluation is classified here by construction — no gate is ever
/// enforced for entries and silently bypassed for exits without its class
/// stating the decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthGate {
    /// Quote age above the limit.
    QuoteAge,
    /// Clock skew above the limit.
    ClockSkew,
    /// Market-data stream disconnected.
    StreamState,
    /// Price store unreachable.
    StoreReachable,
    /// Store watermark age above the limit.
    StoreWatermarkAge,
    /// Store open gaps above the limit.
    StoreGaps,
    /// Journal and venue books disagree.
    Reconcile,
    /// A reconnect hole is unspliced: the series the signals would read may
    /// be wrong.
    Backfill,
}

impl HealthGate {
    /// The gate's class. Authority gates refuse because the platform lacks
    /// standing to act (the store never answered); integrity gates refuse
    /// because the decision's inputs may be wrong (a stale quote, a skewed
    /// clock, a dark stream, a stale watermark, open gaps).
    #[must_use]
    pub fn class(self) -> GateClass {
        match self {
            Self::StoreReachable => GateClass::Authority,
            Self::QuoteAge
            | Self::ClockSkew
            | Self::StreamState
            | Self::StoreWatermarkAge
            | Self::StoreGaps
            | Self::Reconcile
            | Self::Backfill => GateClass::Integrity,
        }
    }
}

/// What a gate's refusal is about: standing to act versus input integrity.
/// Entries refuse on both classes; exits route past both until the
/// exit-annotation question is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateClass {
    /// The platform lacks standing to act.
    Authority,
    /// The decision's inputs may be wrong.
    Integrity,
}

/// One failed gate with its reason. The reason strings are the verdict's
/// vocabulary, shared verbatim so reports and refusals name gates alike.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateFailure {
    /// The gate that failed.
    pub gate: HealthGate,
    /// Why it failed.
    pub reason: String,
}

/// The classified verdict: the outcome plus every failure behind it.
/// Reports never gate behavior on their own; the route functions read the
/// verdict and the submit path reads the refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateReport {
    /// The evaluated outcome.
    pub verdict: HealthVerdict,
    /// Every failed gate, in evaluation order.
    pub failures: Vec<GateFailure>,
}

impl GateReport {
    /// Evaluate every gate and report the verdict with each failure.
    /// Pure function: same facts, same report.
    #[must_use]
    pub fn evaluate(state: &HealthState, limits: &HealthLimits) -> Self {
        let failures = evaluate_gates(state, limits);
        let verdict = if failures.is_empty() {
            HealthVerdict::Trade
        } else {
            HealthVerdict::Halt {
                reasons: failures
                    .iter()
                    .map(|failure| failure.reason.clone())
                    .collect(),
            }
        };
        Self { verdict, failures }
    }
}

/// What the order wants to do. Entries need every gate; exits always route
/// so a halted book stays manageable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteIntent {
    /// A new entry: every gate applies.
    Entry,
    /// Closing or cancelling out: routes past every gate, including the
    /// integrity gates, without evaluating. Whether integrity failures
    /// should annotate (never block) exits is undecided and left
    /// unapplied: this variant records the routing outcome, never a
    /// partial evaluation.
    Exit,
}

/// Evaluate every gate independently and collect each failure with its
/// gate. The single verdict builder: [`may_trade`] and [`check_route`]
/// both read this, so a new gate is one push here plus its taxonomy
/// variant — never a second enforcement site to forget.
fn evaluate_gates(state: &HealthState, limits: &HealthLimits) -> Vec<GateFailure> {
    let mut failures = Vec::new();
    if state.quote_age_secs > limits.max_quote_age_secs {
        failures.push(GateFailure {
            gate: HealthGate::QuoteAge,
            reason: format!(
                "quote age {}s above maximum {}s",
                state.quote_age_secs, limits.max_quote_age_secs
            ),
        });
    }
    if state.clock_skew_secs > limits.max_clock_skew_secs {
        failures.push(GateFailure {
            gate: HealthGate::ClockSkew,
            reason: format!(
                "clock skew {}s above maximum {}s",
                state.clock_skew_secs, limits.max_clock_skew_secs
            ),
        });
    }
    if !state.stream_connected {
        failures.push(GateFailure {
            gate: HealthGate::StreamState,
            reason: "market-data stream disconnected".to_owned(),
        });
    }
    if !state.store.reachable {
        failures.push(GateFailure {
            gate: HealthGate::StoreReachable,
            reason: "price store unreachable".to_owned(),
        });
    }
    if let Some(age) = state.store.watermark_age_secs {
        if age > limits.max_store_watermark_age_secs {
            failures.push(GateFailure {
                gate: HealthGate::StoreWatermarkAge,
                reason: format!(
                    "store watermark age {}s above maximum {}s",
                    age, limits.max_store_watermark_age_secs
                ),
            });
        }
    }
    if state.store.open_gaps > limits.max_open_gaps {
        failures.push(GateFailure {
            gate: HealthGate::StoreGaps,
            reason: format!(
                "store reports {} open gaps above maximum {}",
                state.store.open_gaps, limits.max_open_gaps
            ),
        });
    }
    if !state.reconciled {
        failures.push(GateFailure {
            gate: HealthGate::Reconcile,
            reason: "reconciliation mismatch: journal and venue books disagree".to_owned(),
        });
    }
    if !state.backfilled {
        failures.push(GateFailure {
            gate: HealthGate::Backfill,
            reason: "backfill incomplete: signals suspended until the missing bars land".to_owned(),
        });
    }
    failures
}

/// Evaluate the health gate. Pure function: same facts, same verdict.
#[must_use]
pub fn may_trade(state: &HealthState, limits: &HealthLimits) -> HealthVerdict {
    GateReport::evaluate(state, limits).verdict
}

/// Evaluate the gate for one intent, returning the report behind the
/// routing decision. Entries evaluate every gate and carry the passing
/// report, or refuse with every halting reason; exits bypass evaluation
/// and carry the routing outcome. The fail-closed enforcement twin of
/// [`TradingState::check`](crate::command::TradingState::check): both
/// route exits past a halt.
pub fn check_route(
    state: &HealthState,
    limits: &HealthLimits,
    intent: RouteIntent,
) -> Result<GateReport, VenueError> {
    match intent {
        RouteIntent::Exit => Ok(GateReport {
            verdict: HealthVerdict::Trade,
            failures: Vec::new(),
        }),
        RouteIntent::Entry => {
            let report = GateReport::evaluate(state, limits);
            match &report.verdict {
                HealthVerdict::Trade => Ok(report),
                HealthVerdict::Halt { reasons } => Err(VenueError::Health(format!(
                    "halted: {}",
                    reasons.join("; ")
                ))),
            }
        }
    }
}

/// Evaluate the gate for one intent, or refuse with a [`VenueError`] for the
/// submit path. Exits always route: a halted book must stay manageable, so a
/// halt — store outage or otherwise — never strands an open position.
/// Entries refuse with every halting reason. Delegates to [`check_route`]
/// with the exact prior semantics; existing callers are byte-identical.
pub fn check_trade(
    state: &HealthState,
    limits: &HealthLimits,
    is_exit: bool,
) -> Result<(), VenueError> {
    let intent = if is_exit {
        RouteIntent::Exit
    } else {
        RouteIntent::Entry
    };
    check_route(state, limits, intent).map(|_| ())
}

/// The health gate evaluated over the one fact a serving path observes —
/// the store reading — with quote age, clock skew, and stream state supplied
/// as the no-objection values the pruning means. The serving path never sees
/// quotes, clocks, or stream state; those are the live feed's facts. The
/// shutdown and preview paths share this one verdict, so a store outage
/// refuses both identically.
#[must_use]
pub fn serving_verdict(reading: StoreHealth) -> HealthVerdict {
    let state = HealthState {
        quote_age_secs: 0,
        clock_skew_secs: 0,
        stream_connected: true,
        store: reading,
        reconciled: true,
        backfilled: true,
    };
    may_trade(&state, &HealthLimits::new().build())
}

/// Whether entries are allowed on a serving path right now: the command
/// plane's verdict AND the serving verdict. A default store reading is
/// inert, so a server that never observed the store evaluates exactly as it
/// did before the gate existed; a store outage halts entries while exits
/// stay the command plane's answer, which routes exits through and never
/// strands a position.
#[must_use]
pub fn serving_allows_entries(trading: crate::command::TradingState, reading: StoreHealth) -> bool {
    trading.allows(false) && serving_verdict(reading).may_trade()
}

#[cfg(test)]
#[path = "../tests/health.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/health_store_gate.rs"]
mod tests_store_gate;

#[cfg(test)]
#[path = "../tests/health_exits_survive_store_halt.rs"]
mod tests_exits_survive_store_halt;

#[cfg(test)]
#[path = "../tests/health_gate_matrix.rs"]
mod tests_gate_matrix;

#[cfg(test)]
#[path = "../tests/health_serving.rs"]
mod tests_serving;
