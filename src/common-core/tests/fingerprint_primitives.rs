//! Call-site-independent pins for the shared fingerprint framers.
//!
//! These tests know nothing about parameters, panes, or configs: they pin
//! the byte layouts of [`common_core::hash::fingerprint_json`] and
//! [`common_core::hash::fingerprint_framed`] against hand-built bytes, so
//! any layout drift fails here before it can fork a cache key downstream.

use common_core::hash::{fingerprint_framed, fingerprint_json, fnv1a64};

#[test]
fn framed_empty_holds_the_offset_basis() {
    // No pairs, no bytes: the digest is the FNV-1a offset basis itself
    // (the same value the parameter fingerprint pins as its empty golden).
    assert_eq!(fingerprint_framed(&[]), fnv1a64(&[]));
    assert_eq!(fingerprint_framed(&[]), 14_695_981_039_346_656_037);
}

#[test]
fn framed_matches_hand_built_bytes() {
    // One pair: u64-LE name length, name bytes, raw payload — no payload
    // length, exactly the parameter-fingerprint outer loop.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&6u64.to_le_bytes());
    bytes.extend_from_slice(b"source");
    bytes.push(1);
    assert_eq!(fingerprint_framed(&[("source", &[1])]), fnv1a64(&bytes));
}

#[test]
fn framed_sorts_names_before_hashing() {
    let a = fingerprint_framed(&[("length", &[14]), ("source", &[1])]);
    let b = fingerprint_framed(&[("source", &[1]), ("length", &[14])]);
    assert_eq!(a, b, "insertion order never matters");
}

#[test]
fn framed_separates_adjacent_name_splits() {
    // The name length prefix separates ("ab","c") from ("a","bc").
    assert_ne!(
        fingerprint_framed(&[("ab", b"c")]),
        fingerprint_framed(&[("a", b"bc")]),
        "length framing must separate adjacent splits"
    );
}

#[test]
fn framed_distinguishes_empty_payload_from_missing() {
    assert_ne!(
        fingerprint_framed(&[]),
        fingerprint_framed(&[("", b"")]),
        "one empty pair is not zero pairs"
    );
}

#[test]
fn framed_distinguishes_float_bit_legs() {
    let pos = fingerprint_framed(&[("v", &0.0f64.to_bits().to_le_bytes())]);
    let neg = fingerprint_framed(&[("v", &(-0.0f64).to_bits().to_le_bytes())]);
    assert_ne!(pos, neg, "+0.0 and -0.0 key distinctly");
    assert_ne!(
        pos,
        fingerprint_framed(&[("v", &f64::NAN.to_bits().to_le_bytes())])
    );
}

#[test]
fn framed_unicode_names_hash_distinctly() {
    assert_ne!(
        fingerprint_framed(&[("length", &[14])]),
        fingerprint_framed(&[("l\u{00e9}ngth", &[14])])
    );
}

#[test]
fn framed_duplicate_names_hash_in_order() {
    // Duplicate names sort adjacently and frame in slice order (stable
    // sort): the same multiset always digests the same.
    let a = fingerprint_framed(&[("k", &[1]), ("k", &[2])]);
    let b = fingerprint_framed(&[("k", &[1]), ("k", &[2])]);
    assert_eq!(a, b);
    assert_ne!(a, fingerprint_framed(&[("k", &[2]), ("k", &[1])]));
}

#[test]
fn json_matches_serialized_bytes() {
    #[derive(serde::Serialize)]
    struct Cfg {
        length: i64,
        source: String,
    }
    let cfg = Cfg {
        length: 14,
        source: "close".to_owned(),
    };
    let bytes = serde_json::to_vec(&cfg).expect("serializes");
    assert_eq!(fingerprint_json(&cfg), fnv1a64(&bytes));
    assert_ne!(
        fingerprint_json(&cfg),
        fingerprint_json(&Cfg {
            length: 15,
            source: "close".to_owned()
        })
    );
}

#[test]
fn json_failure_path_hashes_the_empty_encoding() {
    struct AlwaysFails;
    impl serde::Serialize for AlwaysFails {
        fn serialize<S: serde::Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("no encoding"))
        }
    }
    assert_eq!(fingerprint_json(&AlwaysFails), fnv1a64(&[]));
}
