use super::*;
use fluent_types::{CommentSummary, GuidanceDoc, OverlayStatus};

fn test_summary(text: &str, hash: &str) -> CommentSummary {
    CommentSummary {
        text: text.into(),
        comment_hash: hash.to_string(),
        code_hash: String::new(),
        model: "test-model".into(),
        prompt_version: 1,
        temperature: 0.0,
        seed: 42,
        generated_at: 1700000000000,
        status: OverlayStatus::Ready,
        failure_kind: None,
        failure_reason: None,
    }
}

fn doc_with_summaries() -> GuidanceDoc {
    let mut doc = crate::tests::common::make_test_doc_hello();
    doc.summary = Some(test_summary("File overview.", "hash-file"));
    doc.summaries
        .insert("alpha".to_string(), test_summary("Adds one.", "hash-a"));
    doc.summaries
        .insert("beta".to_string(), test_summary("Removes two.", "hash-b"));
    doc
}

#[test]
fn summary_fields_round_trip() {
    let dir = fluent_wvr_testutil::tempdir();
    let path = dir.path().join("roundtrip.json");
    let doc = doc_with_summaries();
    save_guidance(&path, &doc, &crate::components::SidecarReceipt::default()).expect("save");
    let loaded = load_guidance(&path).expect("load").expect("doc");
    let summary = loaded.summary.expect("file summary");
    assert_eq!(summary.text.as_str(), "File overview.");
    assert_eq!(summary.comment_hash, "hash-file");
    assert_eq!(summary.model.as_str(), "test-model");
    assert_eq!(summary.prompt_version, 1);
    assert_eq!(summary.status, OverlayStatus::Ready);
    assert_eq!(loaded.summaries.len(), 2);
    assert_eq!(loaded.summaries["alpha"].text.as_str(), "Adds one.");
    assert_eq!(loaded.summaries["beta"].comment_hash, "hash-b");
}

#[test]
fn merge_preserves_existing_summaries() {
    let existing = doc_with_summaries();
    let mut fresh = crate::tests::common::make_test_doc_hello();
    assert!(fresh.summary.is_none());
    assert!(fresh.summaries.is_empty());
    merge_doc(&existing, &mut fresh);
    assert!(fresh.summary.is_some(), "file summary must survive");
    assert_eq!(fresh.summaries.len(), 2, "node summaries must survive");
    assert_eq!(fresh.summaries["alpha"].text.as_str(), "Adds one.");
}

#[test]
fn merge_never_overwrites_human_prose() {
    let mut existing = crate::tests::common::make_test_doc_hello();
    existing.comment = Some("Human words.".into());
    let mut fresh = crate::tests::common::make_test_doc_hello();
    fresh.comment = None;
    fresh
        .summaries
        .insert("alpha".to_string(), test_summary("Adds one.", "hash-a"));
    merge_doc(&existing, &mut fresh);
    assert_eq!(fresh.comment.as_deref(), Some("Human words."));
    assert!(fresh.summaries.contains_key("alpha"));
}

#[test]
fn merge_prefers_fresh_summary_for_same_key() {
    let mut existing = crate::tests::common::make_test_doc_hello();
    existing.summaries.insert(
        "alpha".to_string(),
        test_summary("Stale words.", "hash-old"),
    );
    let mut fresh = crate::tests::common::make_test_doc_hello();
    fresh.summaries.insert(
        "alpha".to_string(),
        test_summary("Fresh words.", "hash-new"),
    );
    merge_doc(&existing, &mut fresh);
    assert_eq!(fresh.summaries["alpha"].text.as_str(), "Fresh words.");
}

#[test]
fn old_sidecars_parse_without_summaries() {
    let dir = fluent_wvr_testutil::tempdir();
    let path = dir.path().join("legacy.json");
    let doc = crate::tests::common::make_test_doc_hello();
    assert!(doc.summary.is_none());
    save_guidance(&path, &doc, &crate::components::SidecarReceipt::default()).expect("save");
    let raw = std::fs::read_to_string(&path).expect("read");
    assert!(
        !raw.contains("\"summary\""),
        "empty summaries stay out of the bytes"
    );
    assert!(!raw.contains("\"summaries\""), "{raw}");
    let loaded = load_guidance(&path).expect("load").expect("doc");
    assert!(loaded.summary.is_none());
    assert!(loaded.summaries.is_empty());
}

#[test]
fn summary_save_preserves_receipt_and_prose() {
    use std::collections::BTreeMap;
    let dir = fluent_wvr_testutil::tempdir();
    let path = dir.path().join("summed.json");
    let mut doc = crate::tests::common::make_test_doc_hello();
    doc.comment = Some("Human words.".into());
    doc.summaries
        .insert("kept".to_string(), test_summary("Kept.", "hash-kept"));
    let receipt = crate::components::SidecarReceipt::new(
        crate::components::ComponentVector {
            components: BTreeMap::from([("parser".to_string(), "abc123".to_string())]),
        },
        Some("srchash-1".to_string()),
    );
    save_guidance(&path, &doc, &receipt).expect("save");
    let before = std::fs::read_to_string(&path).expect("read");
    let mut fresh = BTreeMap::new();
    fresh.insert("alpha".to_string(), test_summary("Adds one.", "hash-a"));
    save_guidance_summaries(&path, Some(test_summary("File.", "hash-file")), &fresh)
        .expect("summary save");
    let loaded = load_guidance(&path).expect("load").expect("doc");
    assert_eq!(loaded.comment.as_deref(), Some("Human words."));
    assert!(loaded.summaries.contains_key("kept"));
    assert!(loaded.summaries.contains_key("alpha"));
    assert_eq!(
        loaded.summary.as_ref().map(|s| s.text.as_str()),
        Some("File.")
    );
    let after = std::fs::read_to_string(&path).expect("read");
    let hash_of = |text: &str| {
        text.split_once('\n')
            .and_then(|(first, _)| fluent_types::sidecar::SidecarHeader::parse_line(first))
            .and_then(|header| header.src_hash)
    };
    assert_eq!(hash_of(&before), Some("srchash-1".to_string()));
    assert_eq!(hash_of(&after), Some("srchash-1".to_string()));
}
