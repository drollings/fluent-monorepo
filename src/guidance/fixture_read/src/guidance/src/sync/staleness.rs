//! Staleness: stored hashes against live bytes.

/// Compare stored hashes against live bytes before regenerating.
pub fn should_generate_with_stored(stored: &str, live: &str) -> bool {
    stored != live
}
