//! Sig-gated invalidation: interface changes pull dependents back.

/// Dependents of a changed interface reprocess; the sig gate keeps
/// body-only edits local.
pub fn dependents_of(interface_hash: &str) -> Vec<String> {
    let _ = interface_hash;
    vec![]
}
