//! One bar's producer-confidence reading over the session's attached gates.
//!
//! A gate the session was not given is declared at construction — allowed,
//! never silently healthy. An attached gate answers from its own state, and
//! an attached gate that never answered is a producer-confidence refusal,
//! never a pass. Exits always route; a trip disarms entries only. Task-value
//! verdicts are never touched here: the reading names the axis, it does not
//! judge the trade.

use crate::auth::AuthHandle;
use crate::backfill::BackfillHandle;
use crate::command::TradingState;
use crate::reconcile::ReconcileHandle;
use crate::stream::StreamBreaker;

/// One gate's entry/exit answer with the reason that refused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateVerdict {
    /// Whether new entries may flow.
    pub allows_entry: bool,
    /// Whether exits and management may flow (always true: a trip disarms
    /// entries only).
    pub allows_exit: bool,
    /// The refusal reason, when entries halt.
    pub reason: Option<String>,
}

impl GateVerdict {
    /// A gate that allows everything: detached (declared at construction) or
    /// observed clean.
    fn allows() -> Self {
        Self {
            allows_entry: true,
            allows_exit: true,
            reason: None,
        }
    }

    /// A producer-confidence refusal: entries halt with the reason, exits
    /// still route.
    fn refuse_entries(reason: String) -> Self {
        Self {
            allows_entry: false,
            allows_exit: true,
            reason: Some(reason),
        }
    }
}

/// One bar's producer-confidence reading: the trading state plus every
/// attached sensor gate, each with its own entry/exit answer and reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateReading {
    /// The operator's trading state (always attached).
    pub trading: GateVerdict,
    /// The reconciliation entry gate, when the session owns one.
    pub reconcile: GateVerdict,
    /// The backfill entry gate, when the session owns one.
    pub backfill: GateVerdict,
    /// The account-stream breaker, when one is attached.
    pub stream: GateVerdict,
    /// The credential authority, when one is attached.
    pub credential: GateVerdict,
}

impl GateReading {
    /// Assemble one bar's reading from the session's gates. Detached gates
    /// are declared at construction (allowed); attached gates answer from
    /// their own state, and an attached gate that never answered refuses
    /// entries.
    #[must_use]
    pub fn assemble(
        trading: TradingState,
        reconcile: Option<&ReconcileHandle>,
        backfill: Option<&BackfillHandle>,
        stream: Option<&StreamBreaker>,
    ) -> Self {
        Self::assemble_with_credential(trading, reconcile, backfill, stream, None)
    }

    /// Assemble with an explicit credential publication. Attached credentials
    /// refuse entries when the lease is invalid; a missing credential source
    /// is detached and gates exactly as the call above.
    #[must_use]
    pub fn assemble_with_credential(
        trading: TradingState,
        reconcile: Option<&ReconcileHandle>,
        backfill: Option<&BackfillHandle>,
        stream: Option<&StreamBreaker>,
        credential: Option<&AuthHandle>,
    ) -> Self {
        Self {
            trading: match trading.check(false) {
                Ok(()) => GateVerdict::allows(),
                Err(error) => GateVerdict::refuse_entries(error.to_string()),
            },
            reconcile: reconcile.map_or_else(GateVerdict::allows, |handle| {
                if handle.is_observed() {
                    match handle.check_entries() {
                        Ok(()) => GateVerdict::allows(),
                        Err(error) => GateVerdict::refuse_entries(error.to_string()),
                    }
                } else {
                    GateVerdict::refuse_entries(
                        "reconciliation gate attached but unread: no report observed yet"
                            .to_owned(),
                    )
                }
            }),
            backfill: backfill.map_or_else(GateVerdict::allows, |handle| {
                if handle.is_observed() {
                    match handle.check_entries() {
                        Ok(()) => GateVerdict::allows(),
                        Err(error) => GateVerdict::refuse_entries(error.to_string()),
                    }
                } else {
                    GateVerdict::refuse_entries(
                        "backfill gate attached but unread: no resync observed yet".to_owned(),
                    )
                }
            }),
            stream: stream.map_or_else(GateVerdict::allows, |breaker| {
                if breaker.tripped() {
                    GateVerdict::refuse_entries(breaker.trip_reason().map_or_else(
                        || "stream breaker tripped".to_owned(),
                        |reason| format!("stream breaker tripped: {reason}"),
                    ))
                } else {
                    GateVerdict::allows()
                }
            }),
            credential: credential.map_or_else(GateVerdict::allows, |handle| {
                if handle.allows_entries() {
                    GateVerdict::allows()
                } else {
                    GateVerdict::refuse_entries(
                        handle.refusal().unwrap_or_else(|| {
                            "credential unavailable: entries disarmed".to_owned()
                        }),
                    )
                }
            }),
        }
    }

    /// Whether new entries may flow this bar: every gate allows.
    #[must_use]
    pub fn allows_entries(&self) -> bool {
        self.trading.allows_entry
            && self.reconcile.allows_entry
            && self.backfill.allows_entry
            && self.stream.allows_entry
            && self.credential.allows_entry
    }

    /// The first entry refusal in pipeline order (trading, reconcile,
    /// backfill, stream, credential), tagged with its axis so the refusal
    /// names what it is about.
    #[must_use]
    pub fn entry_refusal_reason(&self) -> Option<String> {
        [
            &self.trading,
            &self.reconcile,
            &self.backfill,
            &self.stream,
            &self.credential,
        ]
        .into_iter()
        .find_map(|gate| {
            gate.reason
                .as_ref()
                .map(|reason| format!("entry refused (producer-confidence): {reason}"))
        })
    }
}

#[cfg(test)]
#[path = "../tests/reading.rs"]
mod tests;
