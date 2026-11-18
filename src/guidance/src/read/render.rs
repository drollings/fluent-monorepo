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
            content: crate::read::planes::code::clip_finding_text(
                &finding.text,
                crate::read::engine::RG_EXCERPT_CHAR_CAP,
            ),
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

/// Render one capped list: budget-fitted entries plus one aggregate
/// line naming the rest. Entry identity stays byte-true; only the list
/// length is policy, bounded by the intent budget.
#[must_use]
pub fn render_capped_evidence_section(
    title: &str,
    entries: &[(String, String)],
    budget_tokens: usize,
) -> String {
    let mut items = String::new();
    let mut budget = fluent_llm::tokens::TokenBudget(budget_tokens);
    let mut shown = 0usize;
    for (source, excerpt) in entries {
        let line = if excerpt.is_empty() {
            format!("- {source}\n")
        } else {
            format!("- {source}: {excerpt}\n")
        };
        let cost = fluent_llm::tokens::estimate_tokens(&line) as usize;
        if !budget.consume(cost) {
            break;
        }
        items.push_str(&line);
        shown += 1;
    }
    let hidden = entries.len().saturating_sub(shown);
    if hidden > 0 {
        let _ = writeln!(items, "- + {hidden} more");
    }
    format!("### {title}\n{items}")
}

/// Markdown envelope view: citation digests shorten to their first 8
/// hex chars. Machine JSON keeps the full identity untouched.
#[must_use]
pub fn markdown_envelope(mut envelope: serde_json::Value) -> serde_json::Value {
    if let Some(citations) = envelope
        .get_mut("citations")
        .and_then(serde_json::Value::as_array_mut)
    {
        for citation in citations {
            if let Some(serde_json::Value::String(digest)) = citation.get_mut("digest") {
                digest.truncate(8);
            }
        }
    }
    envelope
}

/// Sanitize model-derived text for markdown rendering: embedded
/// fences are neutralized so they can never open or close a block,
/// and control characters are stripped. Deterministic sections are
/// already fence-safe; only the `Answer` body passes through here.
#[must_use]
pub fn sanitize_for_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_control() && ch != '\n' && ch != '\t' {
            continue;
        }
        out.push(ch);
    }
    out.replace("```", "``\u{200b}`")
}

/// Render the `--trace` block: one line per candidate with its
/// anchor decision, judge label, and admission/budget outcome. The
/// rows are collected deterministically during `explain`; this only
/// formats them, so rendering never triggers a model call.
#[must_use]
pub fn render_trace(rows: &[crate::read::engine::TraceRow]) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("### Trace\n");
    if rows.is_empty() {
        out.push_str("- (no candidates)\n");
        return out;
    }
    for row in rows {
        let _ = writeln!(
            out,
            "- [{}] {} | anchor: {} | judge: {} | {}",
            row.plane, row.candidate, row.anchor, row.judge, row.outcome
        );
    }
    out
}

/// Assemble the markdown answer in section order: project map, docs,
/// capabilities, top matches, citations, gaps.
#[must_use]
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
