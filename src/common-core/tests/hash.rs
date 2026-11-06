use common_core::hash::*;
use std::path::Path;

#[test]
fn sha256_hex_properties() {
    let h = sha256_hex(b"hello");
    assert_eq!(h.len(), 64);
    assert_eq!(h, sha256_hex(b"hello"));
    assert_ne!(h, sha256_hex(b"world"));
}

#[test]
fn blake3_lengths() {
    assert_eq!(blake3_hash(b"test").len(), 32);
    assert_eq!(blake3_hex(b"test").len(), 64);
}

#[test]
fn fnv1a64_basic() {
    let h = fnv1a64(b"hello");
    assert_ne!(h, 0);
    assert_eq!(fnv1a64(b"hello"), fnv1a64(b"hello"));
}

#[test]
fn digest_length_values() {
    assert_eq!(HashAlgorithm::Sha256.digest_length(), 32);
    assert_eq!(HashAlgorithm::Sha512.digest_length(), 64);
    assert_eq!(HashAlgorithm::Blake3.digest_length(), 32);
}

#[test]
fn hash_state_incremental_various() {
    for (algo, expected_len) in [
        (HashAlgorithm::Sha256, 64),
        (HashAlgorithm::Sha512, 128),
        (HashAlgorithm::Blake3, 64),
    ] {
        let mut state = HashState::new(algo);
        state.update(b"hello ");
        state.update(b"world");
        assert_eq!(state.digest_hex().len(), expected_len);
    }
}

#[test]
fn content_hash_properties() {
    assert_eq!(
        content_hash_with_model("hello", "model"),
        content_hash_with_model("hello", "model")
    );
    assert_ne!(
        content_hash_with_model("hello", "model-a"),
        content_hash_with_model("hello", "model-b")
    );
}

#[test]
fn hash_file_small() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.txt");
    std::fs::write(&path, b"hello").unwrap();
    let hash = hash_file(&path, HashAlgorithm::Sha256).unwrap();
    assert_eq!(hash.len(), 64);
}

#[test]
fn hash_file_nonexistent() {
    let result = hash_file(Path::new("/nonexistent/file.txt"), HashAlgorithm::Sha256);
    assert!(result.is_err());
}

#[test]
fn hash_batch_mixed_results() {
    let dir = tempfile::TempDir::new().unwrap();
    let p1 = dir.path().join("a.txt");
    let p2 = dir.path().join("b.txt");
    std::fs::write(&p1, b"data1").unwrap();
    std::fs::write(&p2, b"data2").unwrap();
    let results = hash_batch(&[p1.clone(), p2.clone()], HashAlgorithm::Sha256);
    assert!(results.iter().all(|r| r.hash.is_some()));
    let p_missing = dir.path().join("missing.txt");
    let results = hash_batch(&[p1, p_missing], HashAlgorithm::Sha256);
    assert!(results[0].hash.is_some());
    assert!(results[1].hash.is_none());
}

#[test]
fn sha256_str_known_vectors() {
    assert_eq!(
        sha256_str(""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_str("hello"),
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert_eq!(sha256_str("hello"), sha256_hex(b"hello"));
}

#[test]
fn sha256_file_matches_hex_and_fails_open() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("hello.txt");
    std::fs::write(&path, b"hello").unwrap();
    assert_eq!(
        sha256_file(&path).as_deref(),
        Some(sha256_hex(b"hello").as_str())
    );
    let empty = dir.path().join("empty.txt");
    std::fs::write(&empty, b"").unwrap();
    assert_eq!(
        sha256_file(&empty).as_deref(),
        Some(sha256_hex(b"").as_str())
    );
    assert_eq!(sha256_file(Path::new("/nonexistent/file.txt")), None);
    assert_eq!(sha256_file(&dir.path().join("missing.txt")), None);
}

#[test]
fn hex_encode_matches_format_reference_all_bytes() {
    // The shared encoder must render exactly what `format!("{b:02x}")`
    // produces, for every byte value (including a multi-byte sequence).
    let all: Vec<u8> = (0u8..=255u8).collect();
    let reference: String = all.iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write;
        write!(acc, "{b:02x}").expect("write to String");
        acc
    });
    assert_eq!(hex_encode(&all), reference);
    assert_eq!(hex_encode(&[]), "");
    assert_eq!(hex_encode(&[0x00, 0x0f, 0xf0, 0xff]), "000ff0ff");
}

#[test]
fn hmac_sha256_rfc4231_vectors() {
    // RFC 4231 §4.2 HMAC-SHA-256 test cases 1–4 (key/message/digest).
    let cases: &[(&[u8], &[u8], &str)] = &[
        (
            &[0x0bu8; 20],
            b"Hi There",
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        ),
        (
            b"Jefe",
            b"what do ya want for nothing?",
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
        ),
        (
            &[0xaau8; 20],
            &[0xddu8; 50],
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe",
        ),
        (
            &[0x0cu8; 20],
            b"Test With Truncation",
            "a3b6167473100ee06e0c796c2955552bfa6f7c0a6a8aef8b93f860aab0cd20c5",
        ),
    ];
    for (key, msg, want_prefix) in cases {
        let got = hex_encode(&hmac_sha256(key, msg));
        assert!(
            got.starts_with(want_prefix),
            "hmac_sha256({key:?}, {msg:?}) = {got}, want prefix {want_prefix}"
        );
    }
    // Long-key path (> block size): hashed key, still deterministic and
    // 32 bytes. Case 6 key (131 bytes of 0xaa) with its message.
    let long_key = vec![0xaau8; 131];
    let d1 = hmac_sha256(
        &long_key,
        b"Test Using Larger Than Block-Sized Key - Hash Key First",
    );
    let d2 = hmac_sha256(
        &long_key,
        b"Test Using Larger Than Block-Sized Key - Hash Key First",
    );
    assert_eq!(d1, d2);
    assert_eq!(
        hex_encode(&d1),
        "ad47763c29284e9399feab51e0e79c9c5869cd06617d14fe69f0af19ffbd51cd"
    );
}

#[test]
fn hash_file_engine_matches_oneshot_digest() {
    // The streaming file engine must agree with the one-shot digest over
    // the same bytes (empty, small, and multi-chunk inputs).
    let dir = std::env::temp_dir().join("common-core-hash-file-check");
    std::fs::create_dir_all(&dir).unwrap();
    let payloads: &[&[u8]] = &[b"", b"hello", &[0xabu8; 200_000]];
    for (i, payload) in payloads.iter().enumerate() {
        let path = dir.join(format!("payload-{i}.bin"));
        std::fs::write(&path, payload).unwrap();
        assert_eq!(
            hash_file(&path, HashAlgorithm::Sha256).unwrap(),
            sha256_hex(payload),
            "payload {i}"
        );
        assert_eq!(sha256_file(&path).unwrap(), sha256_hex(payload));
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_json_is_compact_and_field_ordered() {
    #[derive(serde::Serialize)]
    struct Fixed {
        b: u32,
        a: u32,
    }
    assert_eq!(canonical_json(&Fixed { b: 2, a: 1 }), r#"{"b":2,"a":1}"#);
    let mut sorted = std::collections::BTreeMap::new();
    sorted.insert("b", 2);
    sorted.insert("a", 1);
    assert_eq!(canonical_json(&sorted), r#"{"a":1,"b":2}"#);
}

#[test]
fn canonical_json_renders_floats_plainly() {
    assert_eq!(canonical_json(&1.0_f64), "1.0");
    assert_eq!(canonical_json(&0.5_f64), "0.5");
    assert_eq!(canonical_json(&100_u32), "100");
}

#[test]
fn canonical_json_reports_serialization_failure_loudly() {
    struct Fail;
    impl serde::Serialize for Fail {
        fn serialize<S: serde::Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("boom"))
        }
    }
    assert_eq!(canonical_json(&Fail), r#"{"serialization_error":"boom"}"#);
}

// ── verify_sha256_file: size first, then digest ─────────────────────

fn size(n: u64) -> Option<std::num::NonZeroU64> {
    std::num::NonZeroU64::new(n)
}

#[test]
fn verify_empty_file_with_no_size_check() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("empty.bin");
    std::fs::write(&path, b"").expect("empty fixture");
    assert_eq!(
        verify_sha256_file(&path, None, &sha256_hex(b"")).expect("verifies"),
        FileVerification::Match
    );
    assert_eq!(
        verify_sha256_file(&path, size(1), &sha256_hex(b"")).expect("size reported"),
        FileVerification::SizeMismatch {
            actual: 0,
            expected: 1
        }
    );
}

#[test]
fn verify_matching_file_reports_match() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("w.bin");
    std::fs::write(&path, b"hello").expect("fixture");
    assert_eq!(
        verify_sha256_file(&path, size(5), &sha256_hex(b"hello")).expect("verifies"),
        FileVerification::Match
    );
}

#[test]
fn verify_wrong_size_does_not_hash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("short.bin");
    std::fs::write(&path, b"hi").expect("fixture");
    assert_eq!(
        verify_sha256_file(&path, size(5), &sha256_hex(b"hello")).expect("size reported"),
        FileVerification::SizeMismatch {
            actual: 2,
            expected: 5
        }
    );
}

#[test]
fn verify_same_length_wrong_hash_reports_both_digests() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tampered.bin");
    std::fs::write(&path, b"world").expect("fixture");
    let expected = sha256_hex(b"hello");
    assert_eq!(
        verify_sha256_file(&path, size(5), &expected).expect("hash reported"),
        FileVerification::HashMismatch {
            actual: sha256_hex(b"world"),
            expected,
        }
    );
}

#[test]
fn verify_size_check_precedes_any_read() {
    // A directory has readable metadata but fails a read; with a mismatched
    // expected size the verifier must return SizeMismatch without reading.
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        verify_sha256_file(dir.path(), size(1), &sha256_hex(b"")).expect("size reported"),
        FileVerification::SizeMismatch {
            actual: std::fs::metadata(dir.path()).expect("metadata").len(),
            expected: 1,
        }
    );
}

#[test]
fn verify_missing_file_is_an_io_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing.bin");
    let error = verify_sha256_file(&missing, size(5), &sha256_hex(b"hello"))
        .expect_err("missing file must be an I/O error");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn verify_read_error_is_an_io_error() {
    // A directory opens but cannot be read as a file; no size pin means the
    // verifier reaches the read and surfaces the error unchanged.
    let dir = tempfile::tempdir().expect("tempdir");
    let error = verify_sha256_file(dir.path(), None, &sha256_hex(b""))
        .expect_err("directory read must be an I/O error");
    assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
}
