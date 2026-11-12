//! Model-call priority lanes: interactive queries first, background defers.
//!
//! One gate, two semaphores, no new scheduler. Interactive calls are always
//! admitted up to their budget; background calls additionally wait for spare
//! interactive capacity (acquire-and-release: a FIFO wait that holds no
//! capacity) and never fail from saturation. Timeouts bound the call body
//! only — queuing is waiting, not failure. Axis statement: admission measures
//! task value (interactive latency, deadline risk), never producer
//! confidence — a background call defers because of what it costs the user
//! now, not because the model is unsure.

use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

/// Which path a model call serves. Query dispatch admits `Interactive`;
/// sync and maintenance loops admit `Background`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Which path a model call serves. Query dispatch admits `Interactive`;
/// sync and maintenance loops admit `Background`.
pub enum Lane {
    /// Serves a waiting query: admitted first, own timeout.
    Interactive,
    /// Serves maintenance: defers to interactive, own timeout.
    Background,
}

/// Per-lane in-flight budgets and timeouts. Defaults bind nothing under
/// normal load and match the model-role timeout, so an unconfigured gate
/// behaves like today's direct calls; tighten budgets only from measured
/// contention (the saturation test pins the mechanism, not the numbers).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Per-lane in-flight budgets and timeouts. Defaults bind nothing under
/// normal load and match the model-role timeout, so an unconfigured gate
/// behaves like today's direct calls; tighten budgets only from measured
/// contention (the saturation test pins the mechanism, not the numbers).
pub struct LaneConfig {
    /// Max concurrent interactive calls.
    pub interactive_in_flight: usize,
    /// Max concurrent background calls.
    pub background_in_flight: usize,
    /// Interactive call body budget.
    pub interactive_timeout_ms: u64,
    /// Background call body budget.
    pub background_timeout_ms: u64,
}

/// Default lane budgets (see `LaneConfig` for the policy).
#[must_use]
/// Default lane budgets (see `LaneConfig` for the policy).
pub fn default_lane_config() -> LaneConfig {
    LaneConfig {
        interactive_in_flight: 16,
        background_in_flight: 4,
        interactive_timeout_ms: crate::config::default_role_timeout_ms(),
        background_timeout_ms: crate::config::default_role_timeout_ms(),
    }
}

impl Default for LaneConfig {
    fn default() -> Self {
        default_lane_config()
    }
}

/// How lane admission failed. Saturation is absent by design: waiting is
/// deferral, and only a closed gate (unreachable in practice) errors.
/// Transport failure (`Unavailable`) is neither: the ladder signal passes
/// through intact so callers can wait without consuming attempts.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
/// How lane admission failed. Saturation is absent by design: waiting is
/// deferral, and only a closed gate (unreachable in practice) errors.
/// Transport failure (`Unavailable`) is neither: the ladder signal passes
/// through intact so callers can wait without consuming attempts.
pub enum LaneError {
    /// The call body outran its lane timeout.
    #[error("lane {lane:?} timed out after {timeout_ms}ms")]
    Timeout {
        /// Lane whose budget bound the call.
        lane: Lane,
        /// Budget that fired.
        timeout_ms: u64,
    },
    /// The gate shut down mid-wait.
    #[error("lane gate closed")]
    Closed,
    /// Backend busy or unreachable: wait and retry later, never a timeout.
    #[error("lane {lane:?} unavailable: {detail}")]
    Unavailable {
        /// Lane the call waited on.
        lane: Lane,
        /// Backend reason, verbatim.
        detail: String,
    },
}

impl LaneError {
    /// Whether this is a transport failure (wait), not a budget failure.
    #[must_use]
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }
}

/// Admission gate over two semaphores (the same primitive as the shared
/// `Limiter`; the defer-on-saturation policy is new and lives only here).
pub struct LaneGate {
    interactive: Arc<tokio::sync::Semaphore>,
    background: Arc<tokio::sync::Semaphore>,
    config: LaneConfig,
}

impl LaneGate {
    /// Gate with explicit budgets.
    #[must_use]
    pub fn new(config: LaneConfig) -> Self {
        Self {
            interactive: Arc::new(tokio::sync::Semaphore::new(
                config.interactive_in_flight.max(1),
            )),
            background: Arc::new(tokio::sync::Semaphore::new(
                config.background_in_flight.max(1),
            )),
            config,
        }
    }

    /// Gate with default budgets.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(LaneConfig::default())
    }

    /// Admit one call on `lane`: budget, then (background only) wait for
    /// spare interactive capacity, then run under the lane timeout.
    pub async fn admit<F, Fut, T>(&self, lane: Lane, call: F) -> Result<T, LaneError>
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = T> + Send,
    {
        self.admit_checked(lane, || async { Ok(call().await) })
            .await
            .map(|inner: Result<T, std::convert::Infallible>| {
                inner.expect("infallible call cannot fail")
            })
    }

    /// Fallible admission with the same budgets, deferral, and timeout:
    /// lane mechanics stay outer (`LaneError`), the call's own error —
    /// including the ladder's `Unavailable` — passes through intact as
    /// the inner `Err`, never converted into a timeout. Background still
    /// waits for spare interactive capacity first, so an unavailable
    /// backend waits (no attempt consumed upstream) instead of failing.
    pub async fn admit_checked<F, Fut, T, E>(
        &self,
        lane: Lane,
        call: F,
    ) -> Result<Result<T, E>, LaneError>
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = Result<T, E>> + Send,
    {
        // Budget permits live to the end of the function, across the call;
        // dropping them in the match arms below would release the budget
        // before the call body runs.
        let _budget = match lane {
            Lane::Interactive => self
                .interactive
                .acquire()
                .await
                .map_err(|_| LaneError::Closed)?,
            Lane::Background => self
                .background
                .acquire()
                .await
                .map_err(|_| LaneError::Closed)?,
        };
        let timeout_ms = match lane {
            Lane::Interactive => self.config.interactive_timeout_ms,
            Lane::Background => {
                // Defer: hold no interactive capacity, just wait for some.
                drop(
                    self.interactive
                        .acquire()
                        .await
                        .map_err(|_| LaneError::Closed)?,
                );
                self.config.background_timeout_ms
            }
        };
        match tokio::time::timeout(Duration::from_millis(timeout_ms), call()).await {
            Ok(value) => Ok(value),
            Err(_) => Err(LaneError::Timeout { lane, timeout_ms }),
        }
    }
}

#[cfg(test)]
#[path = "../tests/lane_priority.rs"]
mod lane_priority_tests;
