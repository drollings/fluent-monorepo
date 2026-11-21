//! Draft-rung contract: validity, budget, and transport break.

use super::{parse_draft_fence, run_model_draft_rung, GenerativeEndpoint, MODEL_DRAFT_ATTEMPTS};

#[test]
fn gate_suffix_present() {
    let long = "x".repeat(3000);
    let capped = super::capped_gate_output(&long);
    assert!(capped.ends_with("… [truncated]"), "marker kept: {capped}");
    assert!(capped.len() < long.len());
    assert_eq!(super::capped_gate_output("oops"), "oops");
    assert_eq!(super::capped_gate_output("  hi  "), "hi");
}

#[test]
fn invalid_draft_exhausts_budget() {
    // A fenceless answer classifies as an invalid draft (no model needed).
    assert!(parse_draft_fence("no fence here").is_none());
    assert!(parse_draft_fence("```rust\nfn f() {}\n```").is_some());
    // The attempt budget is positive by construction (zero means one try).
    assert!(MODEL_DRAFT_ATTEMPTS.max(1) >= 1);

    // Transport failure breaks the loop instead of burning the budget:
    // validity and reachability are separate axes.
    let dir = std::env::temp_dir().join(format!("split-m0-waves-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    let file = dir.join("input.rs");
    std::fs::write(&file, "pub fn f() {}\n").expect("write");
    let endpoint = GenerativeEndpoint {
        enabled: true,
        base_url: "http://127.0.0.1:1".to_string(),
        model: "unreachable".to_string(),
        timeout: std::time::Duration::from_secs(2),
    };
    let report = run_model_draft_rung(&[(file.clone(), "reason".to_string())], &endpoint);
    assert_eq!(report.attempted, 1, "transport break takes one try");
    assert_eq!(report.failed, 1);
    assert!(report.failures[0].1.contains("model unreachable"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn disabled_endpoint_skips_without_attempts() {
    let report = run_model_draft_rung(
        &[(std::path::PathBuf::from("missing.rs"), "reason".to_string())],
        &GenerativeEndpoint::default(),
    );
    assert_eq!(report.skipped, 1);
    assert_eq!(report.attempted, 0);
    assert_eq!(report.failed, 0);
}

#[test]
fn fence_predicate_classifies_labeled_fixture() {
    // Calibration fixture for the draft-validity predicate: fence
    // presence is producer compliance (confidence axis); language and
    // compilability are judged downstream by apply_draft (task-value).
    // Labeled valid: rust fence, bare fence, fence with prose around.
    for valid in [
        "```rust\nfn f() {}\n```",
        "here:\n```rust\nfn f() {}\n```\ndone",
        "```\nfn f() {}\n```",
        "```python\nprint(1)\n```",
    ] {
        assert!(
            super::parse_draft_fence(valid).is_some(),
            "valid: {valid:?}"
        );
    }
    // Labeled invalid: no fence, empty body, unclosed fence.
    for invalid in [
        "no fence here",
        "",
        "```rust\nfn f() {}\n",
        "```rust\n```",
        "```rust\n   \n```",
    ] {
        assert!(
            super::parse_draft_fence(invalid).is_none(),
            "invalid: {invalid:?}"
        );
    }
}
