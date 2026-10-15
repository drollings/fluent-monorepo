//! Serving-time store liveness: what the trading gate needs to know.
//!
//! [`StoreHealth`] is the one fact the venue health gate reads about the
//! store: is the data behind this decision reachable and current. It is a
//! **task-value** fact — halt or not — never a producer-confidence reading,
//! and it names no confidence, quality, or staleness score anywhere. By the
//! crate's standing rule no stored bar may ever carry such a field either:
//! a bar is committed or it is not, and uncertainty at write time is
//! resolved by not writing.
//!
//! The reading derives from a coverage probe plus the gap count the last
//! classification reported ([`StoreHealth::of`]): a probe that failed reads
//! unreachable, an empty tape reads reachable with no age, and a covered
//! tape reads reachable with the watermark's UTC age. Ages are absolute UTC
//! differences of epoch milliseconds — no session calendar, no wall clock
//! inside this function; the caller supplies `now_ms`.

use serde::{Deserialize, Serialize};

use crate::coverage::Coverage;
use crate::error::StoreError;

/// Observed store liveness. A **task-value** fact, not a producer
/// confidence reading: it answers "is the data behind this decision
/// reachable and current", never "how sure are we".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreHealth {
    /// Whether the store answered its last coverage probe.
    pub reachable: bool,
    /// Age of the newest committed bar, seconds. `None` when unknown.
    pub watermark_age_secs: Option<u64>,
    /// Number of unresolved holes the last classification reported.
    pub open_gaps: u64,
}

impl Default for StoreHealth {
    /// No reading is no objection: a state carrying no store observation
    /// must evaluate exactly as if the store gate did not exist. (A derived
    /// `Default` would read `reachable: false` and halt every caller that
    /// never supplied a reading — the opposite of contained.)
    fn default() -> Self {
        Self {
            reachable: true,
            watermark_age_secs: None,
            open_gaps: 0,
        }
    }
}

impl StoreHealth {
    /// Derive the serving-time reading from a coverage probe result and the
    /// gap count the last classification reported (conventionally
    /// `classify(…).repair_spans().len()`).
    ///
    /// - `Err` — the probe itself failed: unreachable. The age and the gap
    ///   count are unknown, so they read their inert values; only
    ///   `reachable: false` carries signal, and the other fields must not
    ///   be read while it is false.
    /// - `Ok(None)` — the store answered "no rows": reachable with no age.
    ///   An empty table is absence, never an error.
    /// - `Ok(Some(coverage))` — reachable with the watermark's age in whole
    ///   seconds, saturating at zero for a watermark ahead of `now_ms`
    ///   (clock skew must not wrap into an enormous age).
    #[must_use]
    pub fn of(probe: &Result<Option<Coverage>, StoreError>, open_gaps: u64, now_ms: i64) -> Self {
        match probe {
            Err(_) => Self {
                reachable: false,
                watermark_age_secs: None,
                open_gaps: 0,
            },
            Ok(None) => Self {
                reachable: true,
                watermark_age_secs: None,
                open_gaps,
            },
            Ok(Some(coverage)) => Self {
                reachable: true,
                watermark_age_secs: Some(
                    now_ms.saturating_sub(coverage.last.ts_ms).max(0) as u64 / 1_000,
                ),
                open_gaps,
            },
        }
    }
}

#[cfg(test)]
#[path = "../tests/store_health_derivation.rs"]
mod tests;
