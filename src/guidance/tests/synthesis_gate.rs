use super::{can_synthesize, stage_content_hash};
use crate::query::synthesize::Stage;
use fluent_types::StageKind;

fn code_stage(source: &str, line: u32, content: &str) -> Stage {
    Stage {
        kind: StageKind::Code,
        content: content.to_string(),
        source: source.to_string(),
        line: Some(line),
        end_line: None,
        member_name: None,
        member_type: None,
        trace: None,
    }
}

#[test]
fn content_hash_is_deterministic() {
    let stages = vec![code_stage("src/main.rs", 42, "fn test() {}")];
    assert_eq!(stage_content_hash(&stages), stage_content_hash(&stages));
    assert_eq!(stage_content_hash(&stages).len(), 64);
}

#[test]
fn changed_source_changes_the_key() {
    // Changed source always wins over a stored summary: the key moves.
    let before = vec![code_stage("src/main.rs", 42, "fn test() {}")];
    let edited = vec![code_stage("src/main.rs", 42, "fn test() { changed }")];
    let moved_line = vec![code_stage("src/main.rs", 43, "fn test() {}")];
    assert_ne!(stage_content_hash(&before), stage_content_hash(&edited));
    assert_ne!(stage_content_hash(&before), stage_content_hash(&moved_line));
}

#[test]
fn empty_stages_hash_stably() {
    assert_eq!(stage_content_hash(&[]), stage_content_hash(&[]));
}

#[test]
fn synthesis_without_excerpts_is_refused() {
    // No excerpts, no synthesis: the gate holds with no model present.
    assert!(!can_synthesize(&[]));
    assert!(!can_synthesize(&[Stage {
        kind: StageKind::Prose,
        content: "description".to_string(),
        source: String::new(),
        line: None,
        end_line: None,
        member_name: None,
        member_type: None,
        trace: None,
    }]));
    assert!(can_synthesize(&[code_stage(
        "src/main.rs",
        42,
        "fn test() {}"
    )]));
}
