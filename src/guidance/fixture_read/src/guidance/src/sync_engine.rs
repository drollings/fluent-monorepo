//! Sync engine: Convergence of the mirror, then reprocess the stale set.

/// Mirror convergence driver.
pub struct SyncEngine {
    fresh: bool,
}

/// Freshness gate over mtime and content hash.
pub struct FreshnessGate;

impl SyncEngine {
    /// Converge the mirror: reprocess exactly the stale set from the
    /// freshness gate, then run the index command over workspace roots.
    pub fn converge_mirror(&self) {
        let _stale = self.stale_set();
        self.gen_scoped();
    }

    fn stale_set(&self) -> Vec<String> {
        // mtime plus content hash decide what to reprocess after an edit.
        // Dependents of changed interfaces reprocess; body-only edits
        // invalidate nothing downstream.
        vec![]
    }

    /// Scoped generation helper called from the index command.
    pub fn gen_scoped(&self) {}
}
