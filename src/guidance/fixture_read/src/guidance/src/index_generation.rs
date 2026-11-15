//! Generation guard: refuse mixed reads.

/// Refuse the index when the generation mismatches the manifest.
pub fn refuse_on_mismatch(generation: u64, manifest: u64) -> bool {
    generation != manifest
}
