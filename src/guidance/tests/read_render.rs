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
