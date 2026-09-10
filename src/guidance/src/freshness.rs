//! P3 freshness contract (port of zvec-grep's `fresh` / `possibly_stale`
//! plus the `background|wait|off` serving matrix).
//!
//! Revision comparison decides freshness; the mode decides whether a
//! stale read triggers a background reconcile, waits for it, or serves
//! stale directly. Server default is `Background`, direct CLI reads are
//! `Off` (P5 wiring).

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

#[cfg(test)]
#[path = "../tests/freshness.rs"]
mod tests;
