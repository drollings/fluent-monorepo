//! `models verify` reporting: named unverifiable fails, all-models
//! fails only on present-and-pinned mismatch — all hermetic.

use orator_models::ModelStatus;

use crate::models::verify_report;

fn row(name: &str, present: bool, pinned: bool) -> ModelStatus {
    ModelStatus {
        name: name.to_string(),
        kind: "whisper".to_string(),
        pinned,
        present,
        verified: None,
    }
}

fn check(name: &str) -> Result<bool, String> {
    match name {
        "good" => Ok(true),
        "bad" => Ok(false),
        _ => Err("boom".to_string()),
    }
}

fn rows() -> Vec<ModelStatus> {
    vec![
        row("good", true, true),
        row("bad", true, true),
        row("unpinned", true, false),
        row("absent", false, false),
    ]
}

#[test]
fn named_match_reports_ok() {
    let report = verify_report(&rows(), "good", check);
    assert_eq!(report.lines, vec!["good: ok"]);
    assert!(!report.failed);
    assert!(!report.unknown);
}

#[test]
fn named_mismatch_fails() {
    let report = verify_report(&rows(), "bad", check);
    assert_eq!(report.lines, vec!["bad: MISMATCH"]);
    assert!(report.failed);
}

#[test]
fn named_check_error_fails_with_detail() {
    let report = verify_report(&rows(), "good", |_| Err::<bool, _>("disk gone".to_string()));
    assert_eq!(report.lines, vec!["good: disk gone"]);
    assert!(report.failed);
}

#[test]
fn named_unpinned_is_unverified_and_fails() {
    let report = verify_report(&rows(), "unpinned", check);
    assert_eq!(report.lines, vec!["unpinned: unverified (unpinned)"]);
    assert!(report.failed);
}

#[test]
fn named_absent_is_missing_and_fails() {
    let report = verify_report(&rows(), "absent", check);
    assert_eq!(report.lines, vec!["absent: missing"]);
    assert!(report.failed);
}

#[test]
fn unknown_name_is_usage() {
    let report = verify_report(&rows(), "nope", check);
    assert!(report.unknown);
    assert!(report.failed);
}

#[test]
fn all_models_reports_skips_and_fails_on_mismatch() {
    let report = verify_report(&rows(), "", check);
    assert_eq!(
        report.lines,
        vec![
            "good: ok",
            "bad: MISMATCH",
            "unpinned: unverified (unpinned)",
            "absent: missing",
        ]
    );
    assert!(report.failed);
}

#[test]
fn all_models_succeeds_when_only_optionals_are_missing() {
    let rows = vec![
        row("unpinned", true, false),
        row("absent", false, true),
    ];
    let report = verify_report(&rows, "", check);
    assert_eq!(
        report.lines,
        vec!["unpinned: unverified (unpinned)", "absent: missing",]
    );
    assert!(!report.failed);
}

// --- `models pull` plane tests: pinned fetches and verifies, unpinned
// refuses before network, tampered bytes fail integrity. All hermetic
// (stub mirror, tempdir cache), never the real network.

use std::sync::Mutex;

fn pinned_entry(name: &str, file: &str, bytes: &[u8]) -> orator_models::ModelEntry {
    orator_models::ModelEntry {
        name: name.to_string(),
        kind: orator_models::ModelKind::Whisper,
        repo: "test-org/test-repo".to_string(),
        file: file.to_string(),
        size: Some(bytes.len() as u64),
        sha256: Some(common_core::hash::sha256_hex(bytes)),
    }
}

struct StubBody {
    chunks: std::vec::Vec<std::vec::Vec<u8>>,
}

#[async_trait::async_trait]
impl fluent_llm::ArtifactBody for StubBody {
    async fn next_chunk(
        &mut self,
    ) -> Option<Result<std::vec::Vec<u8>, fluent_llm::ArtifactFetchError>> {
        if self.chunks.is_empty() {
            return None;
        }
        Some(Ok(self.chunks.remove(0)))
    }
}

struct StubMirror {
    bytes: Vec<u8>,
    calls: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl fluent_llm::ArtifactFetcher for StubMirror {
    async fn fetch(
        &self,
        url: &str,
    ) -> Result<fluent_llm::ArtifactFetchResponse, fluent_llm::ArtifactFetchError> {
        self.calls.lock().expect("calls").push(url.to_string());
        Ok(fluent_llm::ArtifactFetchResponse {
            status: 200,
            status_text: "OK".to_string(),
            retry_after: None,
            body: Box::new(StubBody {
                chunks: vec![self.bytes.clone()],
            }),
        })
    }
}

struct PanicFetcher;

#[async_trait::async_trait]
impl fluent_llm::ArtifactFetcher for PanicFetcher {
    async fn fetch(
        &self,
        url: &str,
    ) -> Result<fluent_llm::ArtifactFetchResponse, fluent_llm::ArtifactFetchError> {
        panic!("network must not be touched (unpinned refuses first): {url}")
    }
}

#[test]
fn pull_pinned_fetches_through_the_plane_and_verifies() {
    let bytes = b"tiny model bytes";
    let entry = pinned_entry("pull-me", "pull-me.bin", bytes);
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("pull-me.bin");
    let fetcher = StubMirror {
        bytes: bytes.to_vec(),
        calls: Mutex::new(Vec::new()),
    };
    let landed = common_core::runtime::block_on_shared(crate::models::pull_single(
        &entry,
        &dest,
        &fetcher,
    ))
    .expect("pinned pull fetches");
    assert_eq!(landed, dest);
    assert_eq!(std::fs::read(&dest).expect("reads back"), bytes);
    assert!(!fetcher.calls.lock().expect("calls").is_empty(), "plane dialed the mirror");
}

#[test]
fn pull_unpinned_refuses_before_network() {
    let entry = orator_models::ModelEntry {
        name: "draft".to_string(),
        kind: orator_models::ModelKind::Whisper,
        repo: "test-org/test-repo".to_string(),
        file: "draft.bin".to_string(),
        size: None,
        sha256: None,
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("draft.bin");
    let error = common_core::runtime::block_on_shared(crate::models::pull_single(
        &entry,
        &dest,
        &PanicFetcher,
    ))
    .expect_err("unpinned refuses");
    assert!(error.contains("unpinned"), "{error}");
    assert!(!dest.exists(), "nothing lands for unpinned");
}

#[test]
fn pull_tampered_bytes_fail_integrity() {
    let bytes = b"real model bytes";
    let entry = pinned_entry("tampered", "tampered.bin", bytes);
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("tampered.bin");
    let fetcher = StubMirror {
        bytes: b"wrong bytes".to_vec(),
        calls: Mutex::new(Vec::new()),
    };
    let error = common_core::runtime::block_on_shared(crate::models::pull_single(
        &entry,
        &dest,
        &fetcher,
    ))
    .expect_err("tampered must fail integrity");
    assert!(
        error.to_lowercase().contains("integrity")
            || error.to_lowercase().contains("hash")
            || error.to_lowercase().contains("mismatch"),
        "integrity names itself: {error}"
    );
}

#[test]
fn pull_present_and_verified_skips_network() {
    let bytes = b"cached bytes";
    let entry = pinned_entry("cached", "cached.bin", bytes);
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("cached.bin");
    std::fs::write(&dest, bytes).expect("pre-seed cache");
    // PanicFetcher proves the plane short-circuits on a verified
    // cache hit instead of re-downloading.
    let landed = common_core::runtime::block_on_shared(crate::models::pull_single(
        &entry,
        &dest,
        &PanicFetcher,
    ))
    .expect("verified cache hit skips network");
    assert_eq!(landed, dest);
}
