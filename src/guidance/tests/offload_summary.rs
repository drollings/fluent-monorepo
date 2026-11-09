use super::{apply_draft, EscalationPacket, ReviewSummary, TriageClass, DEFAULT_MAX_ATTEMPTS};
use fluent_wvr_testutil::tempdir;

// Summaries report what changed (file, lint, rewrite, gate output);
// escalation packets carry everything needed to resume without re-running
// (inputs, attempts, gate excerpt, hashes, next step). Goldens pin both.

fn kept_report() -> super::ApplyReport {
    let dir = tempdir();
    let file = dir.path().join("kept.rs");
    std::fs::write(&file, "pub fn messy( x : u32 ) -> u32 {\n}\n").expect("write");
    let report = apply_draft(
        &file,
        TriageClass::Formatting,
        "pub fn messy(x: u32) -> u32 {}\n",
        &["echo".to_string(), "gate-ok".to_string()],
        DEFAULT_MAX_ATTEMPTS,
    )
    .expect("kept");
    // The temp dir drops here, but the report owns every field; the source
    // file living or dying changes nothing about the packet.
    report
}

#[test]
fn applied_change_golden() {
    let report = kept_report();
    let mut summary = ReviewSummary::new();
    summary.push_report(&report);
    let value = serde_json::to_value(&summary).expect("serializes");
    let entry = &value["entries"][0];
    assert_eq!(entry["tool"], "draft");
    assert_eq!(entry["verdict"], "kept");
    assert_eq!(entry["rewrite"], "draft content");
    assert!(entry["file"].as_str().expect("file").ends_with("kept.rs"));
    assert_eq!(entry["before_hash"], value["entries"][0]["before_hash"]);
    assert!(entry["gate_output"]
        .as_str()
        .expect("gate")
        .contains("gate-ok"));
    let text = summary.render();
    assert!(
        text.contains("offload review: 1 kept, 0 reverted"),
        "{text}"
    );
    assert!(text.contains("[draft] kept (draft content)"), "{text}");
    assert!(text.contains("gate-ok"), "{text}");
}

#[test]
fn reverted_change_golden() {
    let dir = tempdir();
    let file = dir.path().join("red.rs");
    std::fs::write(&file, "pub fn messy( x : u32 ) -> u32 {\n}\n").expect("write");
    let report = apply_draft(
        &file,
        TriageClass::ClippyFix,
        "pub fn messy(x: u32) -> u32 {}\n",
        &[
            "sh".to_string(),
            "-c".to_string(),
            "echo gate-says-no; exit 1".to_string(),
        ],
        DEFAULT_MAX_ATTEMPTS,
    )
    .expect("reports");
    let packet = EscalationPacket::from_report(&report, "gate red on every attempt");
    let value = serde_json::to_value(&packet).expect("serializes");
    assert_eq!(value["reason"], "gate red on every attempt");
    assert_eq!(value["tool"], "draft");
    assert_eq!(value["attempts_used"], DEFAULT_MAX_ATTEMPTS);
    assert!(value["gate_excerpt"]
        .as_str()
        .expect("excerpt")
        .contains("gate-says-no"));
    assert_eq!(value["before_hash"], value["after_hash"]);
    assert!(value["resume"]
        .as_str()
        .expect("resume")
        .contains("narrow gate"));
    let text = packet.render();
    assert!(
        text.contains("offload escalation: gate red on every attempt"),
        "{text}"
    );
    assert!(text.contains("gate-says-no"), "{text}");
}

#[test]
fn refused_policy_golden() {
    let packet = EscalationPacket::from_policy(
        "src/tests/target.rs",
        "draft",
        "manual",
        "test target refused",
    );
    let value = serde_json::to_value(&packet).expect("serializes");
    assert_eq!(value["file"], "src/tests/target.rs");
    assert_eq!(value["class"], "manual");
    assert_eq!(value["attempts_used"], 0);
    assert_eq!(value["gate_excerpt"], "");
    assert!(value["resume"]
        .as_str()
        .expect("resume")
        .contains("manual review"));
    let text = packet.render();
    assert!(text.contains("test target refused"), "{text}");
    assert!(text.contains("attempts: 0"), "{text}");
}
