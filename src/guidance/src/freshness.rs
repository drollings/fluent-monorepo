//! P3 freshness contract (port of zvec-grep's `fresh` / `possibly_stale`
//! plus the `background|wait|off` serving matrix).
//!
//! Revision comparison decides freshness; the mode decides whether a
//! stale read triggers a background reconcile, waits for it, or serves
//! stale directly. Server default is `Background`, direct CLI reads are
//! `Off` (P5 wiring).
//!
//! Single decision home (M12): this module owns every freshness decision —
//! the revision matrix ([`FreshnessGate`]), the mtime skew table
//! ([`mtime_skew`]), and the content tiebreak ([`hash_decides_fresh`]).
//! The `sync::staleness` sidecar helpers are I/O shells over this core
//! (mtime fetching); they add no decision logic of their own.

use std::time::SystemTime;

/// Serving mode for stale reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FreshnessMode {
    /// Enqueue a reconcile, serve the stale index now.
    #[default]
    Background,
    /// Enqueue a reconcile, block until fresh, then serve.
    Wait,
    /// Serve whatever is indexed; never trigger work.
    Off,
}

/// Freshness of the index behind a read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// Indexed revision covers all watched changes.
    Fresh {
        /// Serving revision.
        revision: u64,
    },
    /// Changes arrived after the last indexed revision.
    PossiblyStale {
        /// Last indexed revision.
        indexed_revision: u64,
        /// Newest dirty revision.
        dirty_revision: u64,
    },
}

/// The serving matrix: pure decision + caller-supplied effects (trigger,
/// wait, fetch), so the contract unit-tests without I/O.
#[derive(Debug, Clone, Copy)]
pub struct FreshnessGate {
    mode: FreshnessMode,
}

impl FreshnessGate {
    /// Build a gate for one serving mode.
    #[must_use]
    pub fn new(mode: FreshnessMode) -> Self {
        Self { mode }
    }

    /// Classify without side effects.
    #[must_use]
    pub fn check(&self, indexed_revision: u64, dirty_revision: u64) -> Freshness {
        if dirty_revision <= indexed_revision {
            Freshness::Fresh {
                revision: indexed_revision,
            }
        } else {
            Freshness::PossiblyStale {
                indexed_revision,
                dirty_revision,
            }
        }
    }

    /// Serve one read through the matrix: `trigger` enqueues a reconcile,
    /// `wait_for_fresh` blocks until the reconcile lands and reports the
    /// fresh revision, `fetch` serves under the resulting freshness.
    pub fn serve<T>(
        &self,
        indexed_revision: u64,
        dirty_revision: u64,
        trigger: impl FnOnce(),
        wait_for_fresh: impl FnOnce() -> u64,
        fetch: impl FnOnce(Freshness) -> T,
    ) -> T {
        match self.check(indexed_revision, dirty_revision) {
            Freshness::Fresh { .. } => fetch(Freshness::Fresh {
                revision: indexed_revision,
            }),
            stale @ Freshness::PossiblyStale { .. } => match self.mode {
                FreshnessMode::Off => fetch(stale),
                FreshnessMode::Background => {
                    trigger();
                    fetch(stale)
                }
                FreshnessMode::Wait => {
                    trigger();
                    let revision = wait_for_fresh();
                    fetch(Freshness::Fresh { revision })
                }
            },
        }
    }
}

/// Mtime skew classification: the single decision table for the 1-second
/// timestamp-resolution rule (M12). Pure over `SystemTime` pairs — the
/// `sync::staleness` shells fetch the mtimes, this table classifies them,
/// so the stale/ambiguous partition cannot drift between two mirrored
/// predicates.
///
/// The window measures *producer self-doubt* (filesystem timestamp
/// granularity), never task value. Inside it the clock is evidence-free,
/// so the content hash ([`hash_decides_fresh`]) decides instead. The
/// threshold gates which evidence to consult, never the conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MtimeSkew {
    /// Source strictly older than the sidecar: no work (both legacy
    /// predicates read false).
    SourceOlder,
    /// Source at-or-newer but within the resolution window: the clock is
    /// evidence-free, consult the content hash.
    Ambiguous,
    /// Source newer beyond the window: stale, regenerate.
    SourceNewer,
}

/// Classify one `(sidecar, source)` mtime pair. Equal mtimes land
/// [`MtimeSkew::Ambiguous`] (zero skew is evidence-free, matching the
/// legacy window predicate exactly).
#[must_use]
pub fn mtime_skew(json_mtime: SystemTime, source_mtime: SystemTime) -> MtimeSkew {
    match source_mtime.duration_since(json_mtime) {
        Err(_) => MtimeSkew::SourceOlder,
        Ok(skew)
            if skew.as_secs() > 1 || (skew.as_secs() == 1 && skew.subsec_nanos() > 0) =>
        {
            MtimeSkew::SourceNewer
        }
        Ok(_) => MtimeSkew::Ambiguous,
    }
}

/// Content tiebreak for the ambiguity window: the fresh source sha256
/// against the stored one (the `compute_diff` precedent — same hasher,
/// `common_core::hash::sha256_hex` — never a second hasher). `None`
/// stored means no evidence: fail open (not fresh — regenerate), never
/// serve ambiguous content as fresh.
///
/// Axis statement: the hash measures *task correctness* (bytes changed),
/// the complement of the window's self-doubt. It gates a regen decision
/// only — never cached, never persisted, never served as data.
///
/// Moved verbatim from `sync::staleness` (M12 single home); that module
/// re-resolves it for its inline tests.
#[must_use]
pub fn hash_decides_fresh(current_hash: &str, stored_hash: Option<&str>) -> bool {
    stored_hash.is_some_and(|want| want == current_hash)
}

#[cfg(test)]
#[path = "../tests/freshness.rs"]
mod tests;
