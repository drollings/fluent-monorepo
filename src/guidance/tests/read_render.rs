//! Card and section rendering: order, budgets, code shape.
//! Included via `#[path]` forwarder from `read/render.rs`, per repo
//! convention. Pure: no I/O, no model.

use super::*;
use crate::read::engine::{Finding, FindingKind, FindingProducer};

fn code_finding(id: &str, text: &str) -> Finding {
    Finding {
        id: id.to_string(),
        producer: FindingProducer::Database,
        kind: FindingKind::Code,
        source: crate::source_ref::SourceRef::from_workspace(
            std::path::Path::new("/workspace"),
            std::path::Path::new("src/a.rs"),
        ),
        start_line: Some(1),
        end_line: Some(3),
        text: text.to_string(),
        producer_score: 1.0,
        provenance: "database".to_string(),
    }
}

#[test]
fn sections_render_in_roadmap_order() {
    let answer = crate::read::engine::ReadAnswer {
        query: "q".to_string(),
        intent: crate::query::strategy::QueryIntent::GeneralSearch,
        route: "code".to_string(),
        sections: vec![
            crate::read::engine::ReadSection {
                title: "From the project map".to_string(),
                body: render_card_section(
                    "From the project map",
                    &[("crate:a".to_string(), "facts".to_string())],
                    1000,
                ),
            },
            crate::read::engine::ReadSection {
                title: "Top matches".to_string(),
                body: render_code_findings(&[code_finding("a", "fn alpha() {}")]),
            },
        ],
        coverage: None,
        claims: Vec::new(),
        citations: vec![crate::source_ref::SourceRef::from_workspace(
            std::path::Path::new("/workspace"),
            std::path::Path::new("src/a.rs"),
        )
        .citation(Some(1), Some(3))],
        gaps: Vec::new(),
        degraded: false,
        tokens_used: 0,
        subject: None,
        cache_hit: false,
        trace: Vec::new(),
    };
    let text = render_answer(&answer);
    assert!(text.starts_with("## Explain: q\n"));
    let map = text.find("From the project map").expect("map section");
    let top = text.find("## Code").expect("code section");
    assert!(map < top, "project map renders before top matches");
    assert!(text.contains("fn alpha() {}"), "{text}");
}

#[test]
fn card_sections_truncate_to_budget() {
    let cards = vec![("crate:a".to_string(), "word ".repeat(500))];
    let full = render_card_section("Cards", &cards, 10000);
    assert!(full.contains("### Cards"));
    let cut = render_card_section("Cards", &cards, 5);
    assert!(cut.len() < full.len());
    assert!(render_card_section("Empty", &[], 100).contains("### Empty"));
    // Zero budget keeps the structural title but no entries.
    assert_eq!(render_card_section("Empty", &[], 0), "### Empty\n");
}

#[test]
fn non_code_findings_never_render_as_code() {
    let doc = Finding {
        kind: FindingKind::Doc,
        ..code_finding("d", "prose excerpt")
    };
    assert!(!render_code_findings(std::slice::from_ref(&doc)).contains("prose excerpt"));
}

#[test]
fn sanitize_neutralizes_embedded_fences() {
    let dirty =
        "First claim (src/a.rs:1-2).\n```rust\nfn evil() {}\n```\nSecond claim (src/b.rs:3).";
    let clean = sanitize_for_markdown(dirty);
    assert!(!clean.contains("```"), "no raw fence survives: {clean}");
    assert!(clean.contains("First claim"), "{clean}");
    assert!(clean.contains("Second claim"), "{clean}");
}

#[test]
fn sanitize_balances_json_fences_and_strips_controls() {
    let dirty = "Answer (src/a.rs:1).\n```json\n{\"a\": 1}\n";
    let clean = sanitize_for_markdown(dirty);
    assert_eq!(clean.matches("```").count(), 0, "{clean}");
    let with_control = "hello\x07\x1bworld (src/a.rs:1)".to_string();
    let clean = sanitize_for_markdown(&with_control);
    assert!(
        !clean.contains('\x07') && !clean.contains('\x1b'),
        "{clean}"
    );
    assert!(
        clean.contains("helloworld") || clean.contains("hello"),
        "{clean}"
    );
    let backticks = "uses `inline code` here (src/a.rs:1).";
    let clean = sanitize_for_markdown(backticks);
    assert!(
        clean.contains("`inline code`"),
        "single backticks survive: {clean}"
    );
}

#[test]
fn sanitize_is_idempotent_and_keeps_plain_prose() {
    let plain = "A plain sentence (src/a.rs:1-2).";
    assert_eq!(sanitize_for_markdown(plain), plain);
    let dirty = "x\n```\ny";
    let once = sanitize_for_markdown(dirty);
    assert_eq!(sanitize_for_markdown(&once), once, "stable fixpoint");
}

fn giant_text() -> String {
    use std::fmt::Write;
    let mut text = String::from("pub fn giant_entry() {\n");
    for i in 0..200 {
        let _ = writeln!(text, "    let step_{i} = {i};");
    }
    text.push_str("}\n");
    text
}

fn wide_finding(id: &str, text: &str, end_line: u32) -> Finding {
    Finding {
        id: id.to_string(),
        producer: FindingProducer::Database,
        kind: FindingKind::Code,
        source: crate::source_ref::SourceRef::from_workspace(
            std::path::Path::new("/workspace"),
            std::path::Path::new("src/big.rs"),
        ),
        start_line: Some(1),
        end_line: Some(end_line),
        text: text.to_string(),
        producer_score: 1.0,
        provenance: "database".to_string(),
    }
}

#[test]
fn oversized_code_excerpts_window_to_the_anchor_head() {
    let text = giant_text();
    let rendered = render_code_findings(&[wide_finding("big", &text, 202)]);
    assert!(
        rendered.contains("pub fn giant_entry() {"),
        "signature survives: {rendered}"
    );
    assert!(
        rendered.contains("step_0 = 0"),
        "anchored head survives: {rendered}"
    );
    assert!(
        !rendered.contains("step_199 = 199"),
        "tail is windowed out: {rendered}"
    );
    assert!(
        rendered.contains("… [truncated]"),
        "truncation is labelled, never silent: {rendered}"
    );
}

#[test]
fn small_code_excerpts_render_verbatim_without_a_marker() {
    let rendered = render_code_findings(&[code_finding("tiny", "fn tiny() {}")]);
    assert!(rendered.contains("fn tiny() {}"), "{rendered}");
    assert!(
        !rendered.contains("… [truncated]"),
        "untouched text never claims truncation: {rendered}"
    );
}

#[test]
fn clip_keeps_the_head_for_every_item_shape() {
    for head in [
        "pub fn alpha() -> u64 {",
        "pub struct Beta {",
        "pub enum Gamma {",
        "impl Delta {",
        "//! Module header line",
    ] {
        use std::fmt::Write;
        let mut text = format!("{head}\n");
        for i in 0..200 {
            let _ = writeln!(text, "    let pad_{i} = {i};");
        }
        let windowed = crate::read::planes::code::clip_finding_text(&text, 600);
        assert!(
            windowed.starts_with(head),
            "head window keeps the anchored signature: {windowed}"
        );
    }
}

#[test]
fn clip_is_a_fixpoint_under_rerun() {
    let text = giant_text();
    let once = crate::read::planes::code::clip_finding_text(&text, 600);
    let twice = crate::read::planes::code::clip_finding_text(&once, 600);
    assert_eq!(once, twice, "windowing never compounds");
}

#[test]
fn capped_citation_list_aggregates_the_remainder() {
    let entries: Vec<(String, String)> = (0..40)
        .map(|i| (format!("src/file_{i}.rs:{}", i + 1), String::new()))
        .collect();
    let rendered = render_capped_evidence_section("Citations", &entries, 60);
    let shown = rendered
        .lines()
        .filter(|line| line.starts_with("- src/file_"))
        .count();
    assert!(shown < entries.len(), "budget caps the list: {shown}");
    assert!(
        rendered.contains(&format!("- + {} more", entries.len() - shown)),
        "aggregate names the remainder: {rendered}"
    );
    assert!(
        !rendered.contains('…'),
        "entries are whole lines, never mid-line cuts: {rendered}"
    );
}

#[test]
fn capped_citation_list_keeps_every_entry_within_budget() {
    let entries: Vec<(String, String)> = (0..5)
        .map(|i| (format!("src/file_{i}.rs:{}", i + 1), String::new()))
        .collect();
    let rendered = render_capped_evidence_section("Citations", &entries, 10_000);
    assert_eq!(
        rendered
            .lines()
            .filter(|line| line.starts_with("- src/file_"))
            .count(),
        5
    );
    assert!(!rendered.contains(" more"), "nothing hidden: {rendered}");
}

#[test]
fn markdown_envelope_shortens_only_digests() {
    let full = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let envelope = serde_json::json!({
        "schema_version": 2,
        "citations": [
            {"citation": "src/a.rs:1-2", "digest": full, "stale": false}
        ],
        "claims": [{"text": "alpha"}],
    });
    let markdown = markdown_envelope(envelope.clone());
    assert_eq!(markdown["citations"][0]["digest"], "01234567");
    assert_eq!(
        envelope["citations"][0]["digest"], full,
        "the machine view keeps the full identity"
    );
    assert_eq!(markdown["schema_version"], envelope["schema_version"]);
    assert_eq!(markdown["claims"], envelope["claims"]);
    assert_eq!(markdown["citations"][0]["citation"], "src/a.rs:1-2");
}
