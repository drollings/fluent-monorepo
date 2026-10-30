//! Safe submission protocol: journal intent, dry-run, submit, query-by-ID.
//!
//! The order of operations is the safety case: the intent is durable before
//! any venue call; the risk token is checked before journaling; transient
//! failures retry through [`retry_async`](common_core::retry::retry_async);
//! and a timeout resolves through query-by-client-ID — never a blind
//! resubmit, so a retry can never mint a second order.

use chrono::{DateTime, Utc};
use std::time::Duration;

use protopyne_strategy::RiskApproved;

use crate::error::VenueError;
use crate::journal::{status_name, Journal, VenueStorage, INTENT_KIND, LEG_FILL_KIND};
use crate::order::{OrderIntent, OrderRecord, OrderStatus};
use crate::quote::Quote;
use crate::venue::{LegExecution, Venue};

/// How long one submit attempt may take before the caller queries by ID.
pub const SUBMIT_TIMEOUT: Duration = Duration::from_secs(5);
/// Retry budget for retryable venue failures (transport, 5xx, timeout).
///
/// Submission-protocol budget only (M9.1): the transport retry knobs
/// (`tastytrade::RETRY_MAX_ATTEMPTS/BACKOFF_BASE_MS/JITTER_PCT`, frozen by
/// the transport freeze test and reused as stream config defaults) are a
/// different authority with different values — same `3` is coincidence,
/// not a shared constant. Do not unify.
pub const SUBMIT_RETRIES: u32 = 3;

/// The mutation sink the safe-submission protocol writes through.
///
/// The protocol is recorder-agnostic: the single [`Recorder`] serves both
/// durabilities behind this trait, so the same protocol runs everywhere and
/// only the recorder's durability differs. It carries the four mutations the
/// protocol needs: the intent, each lifecycle status, each executed spread
/// leg, and each transition. The protocol never reads the log back, so an
/// ephemeral recorder is a first-class peer, not a second durable
/// authority.
pub trait SubmissionRecorder: Send + Sync {
    /// Durably record `intent`, returning `false` when the client id was
    /// already recorded — intent journaling is idempotent, which is what
    /// makes retries safe.
    fn record_intent(&self, intent: &OrderIntent, now: DateTime<Utc>) -> Result<bool, VenueError>;
    /// Record the intent's send state (`submitted`, `acknowledged`, ...).
    fn mark_status(
        &self,
        client_id: &str,
        status: OrderStatus,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError>;
    /// Record one executed spread leg under `client_id`.
    fn record_leg_fill(
        &self,
        client_id: &str,
        leg: &LegExecution,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError>;
    /// Record a lifecycle transition (`kind` + machine-readable `detail`).
    fn record_transition(
        &self,
        client_id: &str,
        kind: &str,
        detail: &str,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError>;
}

/// The single recorder behind the submission protocol, parameterized by
/// durability: one [`Journal`] plus the permit that gates it, or no permit
/// for the ephemeral arm.
///
/// Durable recording bounds the [`Journal`] to the session's
/// [`VenueStorage`] permit: every row is gated and synced, byte-identical
/// to the direct journal calls. Ephemeral recording lands rows in memory
/// only — no gate, no fsync — for production backtests that scope one
/// recorder per run. The fold is identical, so both arms replay identically;
/// the ephemeral arm owns no durable authority.
pub struct Recorder<'a> {
    journal: &'a Journal,
    permit: Option<&'a VenueStorage>,
}

impl<'a> Recorder<'a> {
    /// Durable recording: every row gated through `permit` and synced.
    #[must_use]
    pub fn durable(journal: &'a Journal, permit: &'a VenueStorage) -> Self {
        Self {
            journal,
            permit: Some(permit),
        }
    }

    /// Ephemeral recording: rows land in memory only.
    #[must_use]
    pub fn ephemeral(journal: &'a Journal) -> Self {
        Self {
            journal,
            permit: None,
        }
    }
}

impl SubmissionRecorder for Recorder<'_> {
    fn record_intent(&self, intent: &OrderIntent, now: DateTime<Utc>) -> Result<bool, VenueError> {
        if let Some(permit) = self.permit {
            self.journal.record_intent(permit, intent, now)
        } else {
            let payload =
                serde_json::to_string(intent).map_err(|e| VenueError::Journal(e.to_string()))?;
            self.journal
                .record_ephemeral(&intent.client_id, INTENT_KIND, &payload, now)
        }
    }

    fn mark_status(
        &self,
        client_id: &str,
        status: OrderStatus,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        if let Some(permit) = self.permit {
            self.journal.mark_status(permit, client_id, status, now)
        } else {
            self.journal
                .record_ephemeral(client_id, status_name(status), "{}", now)
                .map(|_| ())
        }
    }

    fn record_leg_fill(
        &self,
        client_id: &str,
        leg: &LegExecution,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        if let Some(permit) = self.permit {
            self.journal.record_leg_fill(permit, client_id, leg, now)
        } else {
            let detail =
                serde_json::to_string(leg).map_err(|e| VenueError::Journal(e.to_string()))?;
            self.journal
                .record_ephemeral(client_id, LEG_FILL_KIND, &detail, now)
                .map(|_| ())
        }
    }

    fn record_transition(
        &self,
        client_id: &str,
        kind: &str,
        detail: &str,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        if let Some(permit) = self.permit {
            self.journal
                .record_transition(permit, client_id, kind, detail, now)
        } else {
            self.journal
                .record_ephemeral(client_id, kind, detail, now)
                .map(|_| ())
        }
    }
}

/// Journal one row per executed spread leg. Empty for single-leg intents —
/// venues that execute no legs report none — so the legacy journal shape is
/// untouched when no spread filled.
fn record_legs<R: SubmissionRecorder, V: Venue>(
    recorder: &R,
    venue: &V,
    client_id: &str,
    now: DateTime<Utc>,
) -> Result<(), VenueError> {
    for leg in venue.executed_legs(client_id) {
        recorder.record_leg_fill(client_id, &leg, now)?;
    }
    Ok(())
}

/// The safe-submission protocol, recorder-agnostic: journal intent,
/// dry-run, submit, then query-by-ID on any ambiguity — never a blind
/// resubmit. Demands the [`RiskApproved`] token minted for this proposal:
/// without it there is no submission path, so strategy code cannot bypass the
/// risk engine. The recorder decides where the intent/status/leg/transition
/// rows land (the durable [`Journal`] or its ephemeral arm); the protocol
/// itself is identical for both.
pub async fn submit_guarded<R: SubmissionRecorder, V: Venue>(
    recorder: &R,
    venue: &V,
    approved: &RiskApproved,
    intent: OrderIntent,
    quote: &Quote,
    now: DateTime<Utc>,
) -> Result<OrderRecord, VenueError> {
    if approved.proposal_id() != intent.proposal_id {
        return Err(VenueError::Invalid(
            "risk approval belongs to a different proposal".to_owned(),
        ));
    }
    let fresh = recorder.record_intent(&intent, now)?;
    if !fresh {
        if let Some(known) = venue.query(&intent.client_id).await {
            return Ok(known);
        }
    }
    let dry = venue.dry_run_async(&intent, quote).await?;
    if !dry.allowed {
        recorder.mark_status(&intent.client_id, OrderStatus::Rejected, now)?;
        return Err(VenueError::Refused("dry run refused".to_owned()));
    }
    recorder.mark_status(&intent.client_id, OrderStatus::Submitted, now)?;

    let attempt = common_core::retry::retry_async(
        SUBMIT_RETRIES,
        50,
        25,
        VenueError::is_retryable,
        || async {
            let submit = venue.submit(intent.clone(), quote, now);
            match tokio::time::timeout(SUBMIT_TIMEOUT, submit).await {
                Ok(Ok(record)) => Ok(record),
                Ok(Err(e)) => Err(e),
                Err(_) => match venue.query(&intent.client_id).await {
                    Some(record) => Ok(record),
                    None => Err(VenueError::Timeout(format!(
                        "no verdict for {}",
                        intent.client_id
                    ))),
                },
            }
        },
    )
    .await;

    match attempt {
        Ok(record) => {
            let status = record.status;
            recorder.mark_status(&intent.client_id, status, now)?;
            record_legs(recorder, venue, &intent.client_id, now)?;
            recorder.record_transition(
                &intent.client_id,
                "verdict",
                &format!("status={status:?}"),
                now,
            )?;
            Ok(record)
        }
        Err(e) => {
            if e.is_retryable() {
                if let Some(record) = venue.query(&intent.client_id).await {
                    let status = record.status;
                    recorder.mark_status(&intent.client_id, status, now)?;
                    record_legs(recorder, venue, &intent.client_id, now)?;
                    return Ok(record);
                }
            }
            Err(e)
        }
    }
}

/// Submit `intent` through the full safe protocol against the durable
/// [`Journal`]. A thin wrapper over [`submit_guarded`] with the durable
/// recorder, so durable rows stay byte-identical to the direct-call era.
pub async fn safe_submit<V: Venue>(
    permit: &VenueStorage,
    journal: &Journal,
    venue: &V,
    approved: &RiskApproved,
    intent: OrderIntent,
    quote: &Quote,
    now: DateTime<Utc>,
) -> Result<OrderRecord, VenueError> {
    let recorder = Recorder::durable(journal, permit);
    submit_guarded(&recorder, venue, approved, intent, quote, now).await
}

#[cfg(test)]
#[path = "../tests/submit.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/submit_recorder.rs"]
mod submit_recorder_tests;

#[cfg(test)]
#[path = "../tests/submission_guard.rs"]
mod submission_guard_tests;

#[cfg(test)]
#[path = "../tests/async_bridge_guard.rs"]
mod async_bridge_guard_tests;
