//! Typed errors for strategy composition, risk checks, and grant handling.

use thiserror::Error;

/// Errors from strategy evaluation, proposal transitions, risk checks, and
/// grant administration. Refusals of individual proposals are data
/// ([`RiskRefused`](crate::risk::RiskRefused),
/// [`GrantDecision::Deny`](crate::grant::GrantDecision)), never errors: an
/// error means the inputs themselves were unusable, so the caller cannot
/// trust any decision built on them.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum StrategyError {
    /// A lifecycle event does not apply to the proposal's current state.
    #[error("invalid transition: {0}")]
    InvalidTransition(String),
    /// A grant definition or administration step is inconsistent.
    #[error("invalid grant: {0}")]
    InvalidGrant(String),
    /// A strategy element was misconfigured or given unusable input.
    #[error("invalid element: {0}")]
    InvalidElement(String),
}

#[cfg(test)]
#[path = "../tests/error.rs"]
mod tests;
