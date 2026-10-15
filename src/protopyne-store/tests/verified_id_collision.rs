//! The collision control group: a digest collision across different bytes
//! must not alias two datasets.
//!
//! Two parameter sets differing in one value fingerprint distinctly through
//! the shared canonical-encoding idiom; their identities carry those
//! fingerprints, so the bytes differ. Forcing digest equality through the
//! field seam then proves the verification order: the digest is a fast
//! reject, the bytes are the authority, and a manifest lookup keyed on
//! identities keeps both entries instead of aliasing one to the other.
//! Without this, a collision silently mislabels a run.

use super::*;
use std::collections::HashMap;

/// Canonical bytes carrying one parameter fingerprint: a domain tag plus
/// the little-endian digest, so bytes from the shared idiom stay
/// distinguishable by origin.
fn probe_bytes(fingerprint: u64) -> Vec<u8> {
    let mut bytes = b"collision-probe-v1".to_vec();
    bytes.extend_from_slice(&fingerprint.to_le_bytes());
    bytes
}

#[test]
fn distinct_params_fingerprint_distinctly() {
    let base = protopyne_kernels::indicator::ParamSet::new()
        .with_int("length", 14)
        .with_bool("flag", true);
    let moved = protopyne_kernels::indicator::ParamSet::new()
        .with_int("length", 15)
        .with_bool("flag", true);
    assert_ne!(
        base.fingerprint(),
        moved.fingerprint(),
        "the probe params must fingerprint distinctly or the control is vacuous"
    );
    let id_base = VerifiedId::of(&probe_bytes(base.fingerprint()));
    let id_moved = VerifiedId::of(&probe_bytes(moved.fingerprint()));
    assert!(
        !id_base.matches(&id_moved),
        "different bytes never match: {id_base:?} vs {id_moved:?}"
    );
    assert_ne!(
        id_base, id_moved,
        "structural equality agrees with matching"
    );
}

#[test]
fn forced_digest_equality_still_reports_inequality() {
    let id = VerifiedId::of(b"tape-a");
    // The field seam: b's digest over a's bytes. Production code never
    // builds this; the control group forces the collision the digest
    // cannot rule out.
    let forced = VerifiedId {
        hash: VerifiedId::of(b"tape-b").hash(),
        canonical: id.canonical.clone(),
    };
    assert_eq!(
        forced.hash(),
        VerifiedId::of(b"tape-b").hash(),
        "the digests really collide"
    );
    assert!(
        !forced.matches(&VerifiedId::of(b"tape-b")),
        "bytes are the authority: a digest collision is one comparison, not a wrong answer"
    );
}

#[test]
fn manifest_lookup_does_not_alias_on_collision() {
    let run_a = VerifiedId::of(b"run-a-canonical");
    let run_b = VerifiedId::of(b"run-b-canonical");
    let forced_a = VerifiedId {
        hash: run_b.hash(),
        canonical: run_a.canonical.clone(),
    };
    let mut ledger: HashMap<VerifiedId, &str> = HashMap::new();
    ledger.insert(run_b.clone(), "run-b");
    ledger.insert(forced_a.clone(), "run-a");
    assert_eq!(ledger.len(), 2, "colliding digests keep distinct entries");
    assert_eq!(ledger.get(&run_b), Some(&"run-b"));
    assert_eq!(ledger.get(&forced_a), Some(&"run-a"));
    assert_eq!(ledger.get(&run_a), None, "uninserted bytes stay absent");
}

#[test]
fn identity_is_deterministic_and_short() {
    let first = VerifiedId::of(b"same-bytes");
    let second = VerifiedId::of(b"same-bytes");
    assert!(first.matches(&second), "same bytes always match");
    assert_eq!(first, second);
    assert_eq!(first.hash(), second.hash());
    assert_eq!(first.short_hex().len(), 8, "short means eight hex chars");
    assert_eq!(first.canonical(), b"same-bytes");
}
