//! Shared bounded-capacity guard: zero refuses with a reason.
//!
//! M14-S2 audit (keep-decision): this module stays a guard, not a
//! drop-oldest counter home. The ceilings' counters span `AtomicUsize`
//! (shared playout queue, batch adds), plain `u64` (single-owner
//! spill/jitter, single adds), and absent (scrollback drain, broadcast
//! lag) — and the container ops stay local regardless — so a shared
//! counter would abstract one `fetch_add`/`+= 1` line while forcing a
//! `Sync` choice on single-threaded owners. Values stay calibrated at
//! the call sites, never unified.

use std::num::NonZeroUsize;

/// Shared bounded-capacity guard: zero refuses with a reason.
pub fn nonzero_capacity(capacity: usize, what: &str) -> Result<NonZeroUsize, String> {
    NonZeroUsize::new(capacity)
        .ok_or_else(|| format!("{what} capacity must be non-zero, got {capacity}"))
}
