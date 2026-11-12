//! Venue-side typed errors with an explicit retry taxonomy.
//!
//! A rejection (illiquid, non-marketable, over budget) is permanent: retrying
//! the same intent changes nothing. Transport, server, and timeout failures
//! are retryable, but retries always resolve through query-by-client-ID first
//! — never a blind resubmit.

use thiserror::Error;

/// Errors from venue submission, journaling, reconciliation, and health.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
/// Errors from venue submission, journaling, reconciliation, and health.
pub enum VenueError {
    /// The order cannot trade at this quote (illiquid, wide, stale,
    /// non-marketable). Permanent.
    #[error("venue refused order: {0}")]
    Refused(String),
    /// The connection dropped before a verdict. Retryable via query-by-ID.
    #[error("transport failure: {0}")]
    Transport(String),
    /// The venue returned 5xx. Retryable via query-by-ID.
    #[error("venue server error: {0}")]
    Server(String),
    /// No verdict within budget. The order may still exist: the caller must
    /// query by client ID, never blindly resubmit.
    #[error("submit timed out: {0}")]
    Timeout(String),
    /// The journal could not record or replay. Fail closed.
    #[error("journal failure: {0}")]
    Journal(String),
    /// Reconciliation disagrees with venue truth. Entries halt.
    #[error("reconciliation mismatch: {0}")]
    Reconciliation(String),
    /// Health gate closed. Fail closed.
    #[error("health gate closed: {0}")]
    Health(String),
    /// Misconfiguration (bad caps, unknown client ID for cancel, ...).
    #[error("invalid venue request: {0}")]
    Invalid(String),
    /// The out-of-band alert sink failed. Not retryable: retrying an alert
    /// send could duplicate pages, so the failure reports loudly instead.
    #[error("alert sink failure: {0}")]
    Alert(String),
}

impl VenueError {
    /// Whether the operation may be attempted again. Rejections and invalid
    /// requests never retry; everything else resolves through query-by-ID.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Transport(_) | Self::Server(_) | Self::Timeout(_)
        )
    }
}

#[cfg(test)]
#[path = "../tests/error.rs"]
mod tests;