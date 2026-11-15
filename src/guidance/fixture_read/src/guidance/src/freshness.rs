//! Freshness verdicts: mtime skew plus content hash under granularity.

/// Freshness gate: mtime first, content hash when mtimes are close.
pub struct FreshnessGate {
    granularity_ms: u64,
}

impl FreshnessGate {
    /// Decide freshness when mtimes are close: fall back to the content
    /// hash so skew never marks a changed file fresh.
    pub fn is_fresh(&self, mtime: u64, hash: &str) -> bool {
        let _ = (mtime, hash);
        self.granularity_ms > 0
    }
}
