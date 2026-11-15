//! Card and section rendering for the read engine.
//!
//! Code stages render through the shared `query::formatter`
//! (`MarkdownFormatter` over `Stage`s) — this module only adds
//! card/section assembly plus budget enforcement. No retrieval, no
//! judgment, no model.

use std::fmt::Write as _;

use crate::query::formatter::{Formatter, MarkdownFormatter};
use crate::query::synthesize::Stage;
use fluent_types::StageKind;

/// Render code findings through the shared stage formatter: findings
/// become `Code` stages (source + line + fenced text) and the one
/// code renderer owns the shape.
#[must_use]
/// Render code findings through the shared stage formatter: findings
/// become `Code` stages (source + line + fenced text) and the one
/// code renderer owns the shape.
pub fn render_code_findings(findings: &[crate::read::engine::Finding]) -> String {
    let stages: Vec<Stage> = findings
        .iter()
        .filter(|finding| {
            matches!(
                finding.kind,
                crate::read::engine::FindingKind::Code
                    | crate::read::engine::FindingKind::Symbol
                    | crate::read::engine::FindingKind::Edge
            )
        })
        .map(|finding| Stage {
            kind: StageKind::Code,
            content: finding.text.clone(),
            source: finding.source.display().to_string(),
            line: finding.start_line,
            end_line: finding.end_line,
            member_name: None,
            member_type: None,
            trace: None,
        })
        .collect();
    MarkdownFormatter.format(&stages)
}

/// Render one card section: title plus budget-truncated cards. The
/// title is structural and always renders; the budget caps entries.
#[must_use]
/// Render one card section: title plus budget-truncated cards. The
/// title is structural and always renders; the budget caps entries.
pub fn render_card_section(
    title: &str,
    cards: &[(String, String)],
    budget_tokens: usize,
) -> String {
    let mut entries = String::new();
    for (card_title, body) in cards {
        let _ = writeln!(entries, "\n#### {card_title}\n\n{body}");
    }
    let entries = crate::read::budget::enforce_budget(&entries, budget_tokens);
    format!("### {title}\n{entries}")
}

/// Render one evidence section: source-anchored excerpts. The title
/// is structural and always renders; the budget caps entries.
#[must_use]
/// Render one evidence section: source-anchored excerpts. The title
/// is structural and always renders; the budget caps entries.
pub fn render_evidence_section(
    title: &str,
    entries: &[(String, String)],
    budget_tokens: usize,
) -> String {
    let mut items = String::new();
    for (source, excerpt) in entries {
        let _ = writeln!(items, "- {source}: {excerpt}");
    }
    let items = crate::read::budget::enforce_budget(&items, budget_tokens);
    format!("### {title}\n{items}")
}

/// Assemble the markdown answer in section order: project map, docs,
/// capabilities, top matches, citations, gaps.
#[must_use]
/// Assemble the markdown answer in section order: project map, docs,
/// capabilities, top matches, citations, gaps.
pub fn render_answer(answer: &crate::read::engine::ReadAnswer) -> String {
    let mut out = format!("## Explain: {}\n", answer.query);
    for section in &answer.sections {
        out.push('\n');
        out.push_str(&section.body);
        if !section.body.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
#[path = "../../tests/read_render.rs"]
mod read_render_tests;
