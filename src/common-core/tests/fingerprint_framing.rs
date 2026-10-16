//! Baseline pins for the fingerprint framers consolidated into the shared
//! hash home.
//!
//! No new primitive exists yet: these tests pin the CURRENT byte layouts so
//! any shared helper must reproduce them bit-for-bit. Two layouts are
//! preserved, never unified here:
//!
//! - sorted length-framed pairs (`ParamSet`/`mtf` idiom): order-insensitive,
//!   collision-safe across `("ab","c")` vs `("a","bc")`.
//! - `serde_json::to_vec` bytes (`config_fingerprint` idiom): order-sensitive
//!   struct bytes, `unwrap_or_default` failure path (unserializable value
//!   hashes the empty encoding).

use common_core::hash::fnv1a64;

fn framed_bytes(pairs: &[(&str, &[u8])]) -> Vec<u8> {
    let mut ordered: Vec<(&str, &[u8])> = pairs.to_vec();
    ordered.sort_by(|a, b| a.0.cmp(b.0));
    let mut bytes = Vec::new();
    for (name, payload) in ordered {
        bytes.extend_from_slice(&(name.len() as u64).to_le_bytes());
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(payload);
    }
    bytes
}

#[test]
fn framed_digest_is_order_insensitive() {
    let a = fnv1a64(&framed_bytes(&[("length", &[14]), ("source", &[1])]));
    let b = fnv1a64(&framed_bytes(&[("source", &[1]), ("length", &[14])]));
    assert_eq!(a, b);
}

#[test]
fn framed_digest_splits_adjacent_names() {
    let ab_c = fnv1a64(&framed_bytes(&[("ab", b"c")]));
    let a_bc = fnv1a64(&framed_bytes(&[("a", b"bc")]));
    assert_ne!(ab_c, a_bc, "length framing must separate (\"ab\",\"c\") from (\"a\",\"bc\")");
}

#[test]
fn framed_digest_empty_map_is_stable() {
    let empty = fnv1a64(&framed_bytes(&[]));
    assert_eq!(empty, fnv1a64(&[]));
    assert_ne!(empty, fnv1a64(&framed_bytes(&[("", b"")])));
}

#[test]
fn framed_digest_distinguishes_float_legs() {
    let pos_zero = fnv1a64(&framed_bytes(&[("v", &0.0f64.to_bits().to_le_bytes())]));
    let neg_zero = fnv1a64(&framed_bytes(&[("v", &(-0.0f64).to_bits().to_le_bytes())]));
    assert_ne!(pos_zero, neg_zero, "+0.0 and -0.0 key distinctly");
    let nan_a = fnv1a64(&framed_bytes(&[("v", &f64::NAN.to_bits().to_le_bytes())]));
    let nan_b = fnv1a64(&framed_bytes(&[("v", &f64::NAN.to_bits().to_le_bytes())]));
    assert_eq!(nan_a, nan_b);
}

#[test]
fn json_digest_is_order_sensitive_struct_bytes() {
    #[derive(serde::Serialize)]
    struct Cfg {
        length: i64,
        source: String,
    }
    let bytes = serde_json::to_vec(&Cfg { length: 14, source: "close".to_owned() }).expect("serializes");
    let digest = fnv1a64(&bytes);
    assert_eq!(digest, fnv1a64(&serde_json::to_vec(&serde_json::json!({
        "length": 14,
        "source": "close",
    })).expect("serializes")));
    let other = fnv1a64(
        &serde_json::to_vec(&Cfg { length: 15, source: "close".to_owned() }).expect("serializes"),
    );
    assert_ne!(digest, other);
}

#[test]
fn json_digest_failure_path_hashes_empty_encoding() {
    struct AlwaysFails;
    impl serde::Serialize for AlwaysFails {
        fn serialize<S: serde::Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("no encoding"))
        }
    }
    let bytes: Vec<u8> = serde_json::to_vec(&AlwaysFails).unwrap_or_default();
    assert!(bytes.is_empty(), "encode failure yields the empty default");
    assert_eq!(fnv1a64(&bytes), fnv1a64(&[]));
}

#[test]
fn unicode_names_hash_distinctly() {
    let ascii = fnv1a64(&framed_bytes(&[("length", &[14])]));
    let unicode = fnv1a64(&framed_bytes(&[("l\u{00e9}ngth", &[14])]));
    assert_ne!(ascii, unicode);
}
