//! Hash-parameterized label matching: "does this stored `u64` hash name this
//! label?" without coupling the caller to any one hash function.
//!
//! The shape recurs wherever a closed string vocabulary is stored
//! content-addressed: spacy-rs dependency labels (`token.dep` vs
//! `hash_utf8(label)`), router chart filters, and ledger overlays all ask the
//! same question. The hash function is a parameter (`fn(&str) -> u64`), so
//! this crate never learns spaCy's Murmur seed-1 contract or any other
//! domain's digest — callers bind their own function at the call site.
//!
//! Hot-loop note: both are free functions over a stored hash, never trait
//! objects — safe to call per token in tight loops.

/// Whether `stored` is the hash of `label` under `hash_fn`.
#[must_use]
#[inline]
pub fn label_eq(stored: u64, label: &str, hash_fn: fn(&str) -> u64) -> bool {
    hash_fn(label) == stored
}

/// Whether `stored` is the hash of any entry of `labels` under `hash_fn`.
///
/// Empty `labels` matches nothing (vacuous false, never true) — the
/// load-bearing edge for role tables that may legitimately be empty.
#[must_use]
#[inline]
pub fn label_in(stored: u64, labels: &[&str], hash_fn: fn(&str) -> u64) -> bool {
    labels.iter().any(|l| hash_fn(l) == stored)
}
