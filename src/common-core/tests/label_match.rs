//! Focused unit tests for `common_core::label_match`, independent of any call
//! site: a toy FNV-style hash stands in for every domain digest so the tests
//! pin the *parameterized* semantics, not one caller's constants.

use common_core::label_match::{label_eq, label_in};

fn toy_hash(s: &str) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01B3);
    }
    h
}

#[test]
fn eq_matches_only_the_same_label() {
    let stored = toy_hash("nsubj");
    assert!(label_eq(stored, "nsubj", toy_hash));
    assert!(!label_eq(stored, "dobj", toy_hash));
    assert!(!label_eq(stored, "NSUBJ", toy_hash), "case-sensitive, like the callers");
    assert!(!label_eq(stored, "", toy_hash));
}

#[test]
fn eq_rejects_the_zero_sentinel() {
    // No label hashes to zero under the toy digest (and `MISSING_DEP == 0`
    // must never match) — the sentinel edge every caller relies on.
    assert!(!label_eq(0, "root", toy_hash));
    assert!(!label_eq(0, "", toy_hash));
}

#[test]
fn in_matches_membership_and_empty_is_false() {
    let stored = toy_hash("pobj");
    assert!(label_in(stored, &["prep", "pobj"], toy_hash));
    assert!(!label_in(stored, &["prep", "det"], toy_hash));
    assert!(!label_in(stored, &[], toy_hash), "empty table matches nothing");
    assert!(!label_in(0, &["root"], toy_hash), "sentinel matches nothing");
}

#[test]
fn in_handles_duplicates_and_singletons() {
    let stored = toy_hash("aux");
    assert!(label_in(stored, &["aux"], toy_hash));
    assert!(label_in(stored, &["aux", "aux"], toy_hash));
    assert!(!label_in(stored, &["AUX"], toy_hash));
}

#[test]
fn different_hash_fn_same_semantics() {
    // The primitive is hash-agnostic: identity-length as the digest still
    // yields coherent eq/in behavior.
    fn len_hash(s: &str) -> u64 {
        s.len() as u64
    }
    assert!(label_eq(4, "root", len_hash));
    assert!(label_in(4, &["det", "root"], len_hash));
    assert!(!label_in(4, &["det", "aux"], len_hash));
}
