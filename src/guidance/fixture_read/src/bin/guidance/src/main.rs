//! Command entry: clean, sync, and explain over one workspace.

use guidance_core::sync_engine::SyncEngine;

/// Remove the build cache under .guidance/target. Clean is safe: the
/// cache is derived, guarded by a lock.
pub fn clean_build_cache() {}

/// Run the index over the workspace roots.
pub fn run_index() {
    gen_scoped();
}

fn gen_scoped() {}

/// Sync command: converge the mirror, then reprocess.
pub fn cmd_sync() {
    let engine = SyncEngine { fresh: true };
    let _ = engine.converge_mirror();
    engine.gen_scoped();
    run_index();
    let _ = should_generate_with_stored("a", "b");
}

/// Clean command handler.
pub fn cmd_clean() {
    clean_build_cache();
}

fn should_generate_with_stored(_a: &str, _b: &str) -> bool {
    true
}
