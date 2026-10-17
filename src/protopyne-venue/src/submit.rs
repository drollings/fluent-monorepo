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
use crate::journal::{Journal, VenueStorage};
use crate::order::{OrderIntent, OrderRecord, OrderStatus};
use crate::quote::Quote;
use crate::venue::{LegExecution, Venue};

/// How long one submit attempt may take before the caller queries by ID.
pub const SUBMIT_TIMEOUT: Duration = Duration::from_secs(5);
/// Retry budget for retryable venue failures (transport, 5xx, timeout).
pub const SUBMIT_RETRIES: u32 = 3;

/// The mutation sink the safe-submission protocol writes through.
///
/// The protocol is recorder-agnostic: the durable [`Journal`] in production
/// and the in-memory [`RunIntentLog`](crate::journal::RunIntentLog) in a
/// backtest run both implement it, so the same protocol runs everywhere and
/// only the recorder differs. It carries the four mutations the protocol
/// needs: the intent, each lifecycle status, each executed spread leg, and
/// each transition. Implementations own their own durability/gating; the
/// protocol never reads the log back, so an in-memory recorder is a
/// first-class peer, not a second durable authority.
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

/// The durable [`SubmissionRecorder`]: a [`Journal`] bound to the session's
/// [`VenueStorage`] permit. Every mutation delegates to the journal method of
/// the same name, so rows are byte-identical to the direct calls the journal
/// has always made.
struct JournalRecorder<'a> {
    permit: &'a VenueStorage,
    journal: &'a Journal,
}

impl SubmissionRecorder for JournalRecorder<'_> {
    fn record_intent(&self, intent: &OrderIntent, now: DateTime<Utc>) -> Result<bool, VenueError> {
        self.journal.record_intent(self.permit, intent, now)
    }

    fn mark_status(
        &self,
        client_id: &str,
        status: OrderStatus,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        self.journal.mark_status(self.permit, client_id, status, now)
    }

    fn record_leg_fill(
        &self,
        client_id: &str,
        leg: &LegExecution,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        self.journal.record_leg_fill(self.permit, client_id, leg, now)
    }

    fn record_transition(
        &self,
        client_id: &str,
        kind: &str,
        detail: &str,
        now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        self.journal
            .record_transition(self.permit, client_id, kind, detail, now)
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
/// rows land (the durable [`Journal`] or an in-memory run log); the protocol
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
    let dry = venue.dry_run(&intent, quote)?;
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
                Err(_) => {
                    match venue.query(&intent.client_id).await {
                        Some(record) => Ok(record),
                        None => Err(VenueError::Timeout(format!(
                            "no verdict for {}",
                            intent.client_id
                        ))),
                    }
                }
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
/// [`Journal`]. A thin wrapper over [`submit_guarded`] with the journal
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
    let recorder = JournalRecorder { permit, journal };
    submit_guarded(&recorder, venue, approved, intent, quote, now).await
}

#[cfg(test)]
#[path = "../tests/submit.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/submit_recorder.rs"]
mod submit_recorder_tests;
