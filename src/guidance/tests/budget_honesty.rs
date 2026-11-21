//! Budget honesty: sections stay within their budgets and oversized
//! excerpts clip with their signature intact.
use crate::query::strategy::QueryIntent;
use crate::read::budget::{budgets_for, enforce_budget, tokens_for};

fn finding(id: &str, text: &str) -> crate::read::engine::Finding {
    crate::read::engine::Finding {
        id: id.to_string(),
        producer: crate::read::engine::FindingProducer::Database,
        kind: crate::read::engine::FindingKind::Code,
        source: crate::source_ref::SourceRef::from_workspace(
            std::path::Path::new("/workspace"),
            std::path::Path::new("src/oversized.rs"),
        ),
        start_line: Some(1),
        end_line: Some(266),
        text: text.to_string(),
        producer_score: 1.0,
        provenance: "code".to_string(),
    }
}

fn oversized_text() -> String {
    use std::fmt::Write as _;
    let mut out =
        String::from("pub fn cmd_calibrate() -> u64 {\n    // Calibrates the registry.\n");
    for i in 0..260 {
        let _ = writeln!(out, "    let _v{i} = {i};");
    }
    out.push_str("    1\n}\n");
    out
}

#[test]
fn oversized_finding_clips_with_signature_and_marker() {
    let text = oversized_text();
    assert!(text.contains("pub fn cmd_calibrate()"));
    let clipped = crate::read::planes::code::clip_finding_text(&text, 600);
    assert!(
        clipped.contains("pub fn cmd_calibrate()"),
        "signature survives: {clipped:.120}"
    );
    assert!(
        clipped.contains("[truncated"),
        "marker names the cut: {clipped:.120}"
    );
    assert!(
        clipped.len() < text.len(),
        "oversized shrinks: {} -> {}",
        text.len(),
        clipped.len()
    );
    let budget = budgets_for(QueryIntent::Graph);
    let kept = crate::read::engine::cap_findings_by_budget(&[finding("a", &clipped)], budget.code);
    assert_eq!(kept.len(), 1, "never-empty guarantee holds via windowing");
    assert!(
        kept[0].start_line == Some(1) && kept[0].end_line == Some(266),
        "citation range keeps the full span: {:?}",
        (kept[0].start_line, kept[0].end_line)
    );
}

#[test]
fn rendered_top_matches_stays_within_code_budget() {
    let text = oversized_text();
    let clipped = crate::read::planes::code::clip_finding_text(&text, 600);
    let findings = vec![finding("a", &clipped), finding("b", &clipped)];
    let budget = budgets_for(QueryIntent::Graph);
    let kept = crate::read::engine::cap_findings_by_budget(&findings, budget.code);
    let rendered = crate::read::render::render_code_findings(&kept);
    let capped = enforce_budget(&rendered, budget.code);
    assert!(
        tokens_for(&capped) <= (budget.code * 11 / 10).max(1),
        "section within budget x1.1: {} tokens vs {}",
        tokens_for(&capped),
        budget.code
    );
    assert!(
        capped.contains("pub fn cmd_calibrate()"),
        "signature intact"
    );
}

#[test]
fn section_budgets_hold_across_intents() {
    for intent in [
        QueryIntent::SingleIdentifier,
        QueryIntent::Graph,
        QueryIntent::Ownership,
        QueryIntent::Orientation,
    ] {
        let budgets = budgets_for(intent);
        let body = enforce_budget(&oversized_text(), budgets.total);
        assert!(
            tokens_for(&body) <= (budgets.total * 11 / 10).max(1),
            "{intent:?} over budget"
        );
    }
}
