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
use crate::venue::Venue;

/// How long one submit attempt may take before the caller queries by ID.
pub const SUBMIT_TIMEOUT: Duration = Duration::from_secs(5);
/// Retry budget for retryable venue failures (transport, 5xx, timeout).
pub const SUBMIT_RETRIES: u32 = 3;

/// Submit `intent` through the full safe protocol. Demands the
/// [`RiskApproved`] token minted for this proposal: without it there is no
/// submission path, so strategy code cannot bypass the risk engine.
pub async fn safe_submit<V: Venue>(
    permit: &VenueStorage,
    journal: &Journal,
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
    let fresh = journal.record_intent(permit, &intent, now)?;
    if !fresh {
        if let Some(known) = venue.query(&intent.client_id).await {
            return Ok(known);
        }
    }
    let dry = venue.dry_run(&intent, quote)?;
    if !dry.allowed {
        journal.mark_status(permit, &intent.client_id, OrderStatus::Rejected, now)?;
        return Err(VenueError::Refused("dry run refused".to_owned()));
    }
    journal.mark_status(permit, &intent.client_id, OrderStatus::Submitted, now)?;

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
            journal.mark_status(permit, &intent.client_id, status, now)?;
            journal.record_transition(
                permit,
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
                    journal.mark_status(permit, &intent.client_id, status, now)?;
                    return Ok(record);
                }
            }
            Err(e)
        }
    }
}

#[cfg(test)]
#[path = "../tests/submit.rs"]
mod tests;
