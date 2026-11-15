//! Code plane: FTS + vector + lemma recall within code only, and
//! ripgrep normalization.
//!
//! The fused RRF itself stays in `run_recall` — the plane builds the
//! code-only plan (no capability routes, no cross-source fusion) and
//! normalizes every producer into the one `Finding` shape, including
//! the exhaustive L0 arm: `rg` hits become `FindingProducer::Ripgrep`
//! findings instead of a second output path.

use crate::search_types::{SearchHit, SearchPlan, SearchPlanRoute, SearchPlanRouteMode};

/// Code-only recall plan: FTS plus vector routes over the query, lemma
/// always on. Symbol preference rides along only for Symbol/Graph
/// intents — the caller resolves the intent, the plane never guesses.
#[must_use]
/// Code-only recall plan: FTS plus vector routes over the query, lemma
/// always on. Symbol preference rides along only for Symbol/Graph
/// intents — the caller resolves the intent, the plane never guesses.
pub fn code_plan(query: &str, limit: usize, prefer_symbol: bool) -> SearchPlan {
    SearchPlan {
        routes: vec![
            SearchPlanRoute {
                mode: SearchPlanRouteMode::Fts,
                query: query.to_string(),
            },
            SearchPlanRoute {
                mode: SearchPlanRouteMode::Vector,
                query: query.to_string(),
            },
        ],
        limit: Some(limit),
        trace: true,
        prefer_symbol: Some(prefer_symbol),
        ..Default::default()
    }
}

/// Normalize fused code hits into findings: rank order preserved,
/// producer score kept as rank-only signal, provenance names the
/// recall path. Excerpts resolve against live bytes by symbol
/// identity (tree-sitter on the fly); stored evidence is the
/// fallback, never a live claim. Unresolvable symbols keep their
/// stored slice — the assembly layer labels the drift.
#[must_use]
/// Normalize fused code hits into findings: rank order preserved,
/// producer score kept as rank-only signal, provenance names the
/// recall path. Excerpts resolve against live bytes by symbol
/// identity (tree-sitter on the fly); stored evidence is the
/// fallback, never a live claim. Unresolvable symbols keep their
/// stored slice — the assembly layer labels the drift.
pub fn search_hits_to_findings(hits: &[SearchHit]) -> Vec<crate::read::engine::Finding> {
    use crate::read::engine::{Finding, FindingKind, FindingProducer};
    use crate::search_types::FragmentContent;
    hits.iter()
        .filter(|hit| {
            crate::source_scope::of_file(&hit.file) == crate::source_scope::SourceScope::Product
        })
        .enumerate()
        .map(|(index, hit)| {
            let (text, start_line, end_line) = live_symbol_excerpt(hit)
                .or_else(|| {
                    hit.evidence.first().map(|evidence| {
                        let text = match &evidence.content {
                            FragmentContent::Text { text } => text.clone(),
                            FragmentContent::Image { .. } => String::new(),
                        };
                        let (start, end) = match &evidence.range {
                            crate::search_types::FragmentSpan::Text {
                                start_line,
                                end_line,
                                ..
                            } => (Some(*start_line), Some(*end_line)),
                            _ => (None, None),
                        };
                        (text, start, end)
                    })
                })
                .unwrap_or_default();
            Finding {
                id: format!("code-{}:{index}", hit.file.relative_path),
                producer: FindingProducer::Database,
                kind: FindingKind::Code,
                source: crate::source_ref::SourceRef::from_file(&hit.file),
                start_line,
                end_line,
                text,
                producer_score: hit.score.0,
                provenance: "code".to_string(),
            }
        })
        .collect()
}

/// Live excerpt for one hit over live bytes: the symbol's parsed
/// span, sliced from the current file. `None` when the hit carries
/// no resolvable symbol — the caller keeps stored evidence.
fn live_symbol_excerpt(hit: &SearchHit) -> Option<(String, Option<u32>, Option<u32>)> {
    use std::path::PathBuf;
    let (name, symbol) = hit_symbol_name(hit)?;
    let member = member_type_for_symbol(symbol)?;
    let span =
        crate::ast_parser::resolve_span(&PathBuf::from(&hit.file.absolute_path), &name, member)?;
    let text = std::fs::read_to_string(&hit.file.absolute_path).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let from = span.start_line.max(1) as usize - 1;
    let to = (span.end_line as usize).min(lines.len());
    if from >= to {
        return None;
    }
    Some((
        lines[from..to].join("\n"),
        Some(span.start_line),
        Some(span.end_line),
    ))
}

/// Symbol identity behind one hit: entity metadata first, then the
/// first code evidence carrying a declared name.
fn hit_symbol_name(hit: &SearchHit) -> Option<(String, crate::search_types::CodeSymbolType)> {
    use crate::search_types::EntityMetadata;
    let from = |meta: &crate::search_types::EntityMetadata| match meta {
        EntityMetadata::Code {
            symbol_name: Some(name),
            symbol_type,
            ..
        } => Some((name.clone(), *symbol_type)),
        _ => None,
    };
    hit.evidence
        .iter()
        .find_map(|evidence| evidence.metadata.as_ref().and_then(from))
        .or_else(|| hit.entity.metadata.as_ref().and_then(from))
}

/// Tree-sitter member kind for one closed symbol type. Unmatched
/// kinds decline: callers fall back to the stored evidence range
/// instead of guessing a span.
fn member_type_for_symbol(
    symbol: crate::search_types::CodeSymbolType,
) -> Option<fluent_types::MemberType> {
    use fluent_types::MemberType;
    match symbol {
        crate::search_types::CodeSymbolType::Function => Some(MemberType::FnDecl),
        crate::search_types::CodeSymbolType::Class
        | crate::search_types::CodeSymbolType::Interface => Some(MemberType::Struct),
        _ => None,
    }
}

/// Re-resolve code findings against live bytes: the index stores
/// ranges, but edits shift lines without re-syncing — and stored
/// text is normalized (comment markers stripped), so verification
/// compares normalized cores, never raw bytes. A finding whose
/// normalized text still appears live is kept; a moved block is
/// re-sliced to its live lines; text gone from the live file keeps
/// its stored slice with a gap (labelled, never silently wrong).
/// Findings without line ranges (graph edges, ownership notes)
/// pass through — they are relation facts, not excerpts.
#[must_use]
/// Re-resolve code findings against live bytes: the index stores
/// ranges, but edits shift lines without re-syncing — and stored
/// text is normalized (comment markers stripped), so verification
/// compares normalized cores, never raw bytes. A finding whose
/// normalized text still appears live is kept; a moved block is
/// re-sliced to its live lines; text gone from the live file keeps
/// its stored slice with a gap (labelled, never silently wrong).
/// Findings without line ranges (graph edges, ownership notes)
/// pass through — they are relation facts, not excerpts.
pub fn resolve_live_findings(
    _workspace: &std::path::Path,
    findings: Vec<crate::read::engine::Finding>,
) -> (Vec<crate::read::engine::Finding>, Vec<String>) {
    use crate::read::engine::FindingKind;
    let mut gaps = Vec::new();
    let mut out = Vec::with_capacity(findings.len());
    for mut finding in findings {
        let (Some(_), Some(_)) = (finding.start_line, finding.end_line) else {
            out.push(finding);
            continue;
        };
        if !matches!(
            finding.kind,
            FindingKind::Code | FindingKind::Symbol | FindingKind::Edge
        ) {
            out.push(finding);
            continue;
        }
        let path = finding.source.live_path();
        let Ok(live) = std::fs::read_to_string(&path) else {
            gaps.push(format!("source changed since index: {}", finding.source));
            out.push(finding);
            continue;
        };
        let live_lines: Vec<String> = live.lines().map(normalize_line).collect();
        let finding_lines: Vec<String> = finding
            .text
            .lines()
            .map(normalize_line)
            .filter(|line| !line.is_empty())
            .collect();
        if finding_lines.is_empty() {
            out.push(finding);
            continue;
        }
        // Shifted block: the first substantial line anchors a live
        // search. A stored range that still names the anchor stands;
        // otherwise the range moves with the anchor. No anchor at all
        // means the text is gone from the live file.
        let anchor = finding_lines.iter().find(|line| line.len() >= 4);
        let Some(anchor) = anchor else {
            gaps.push(format!("source changed since index: {}", finding.source));
            out.push(finding);
            continue;
        };
        let positions: Vec<usize> = live_lines
            .iter()
            .enumerate()
            .filter(|(_, line)| *line == anchor)
            .map(|(index, _)| index + 1)
            .collect();
        if positions.is_empty() {
            gaps.push(format!("source changed since index: {}", finding.source));
            out.push(finding);
            continue;
        }
        let stored = finding.start_line.unwrap_or(0) as usize;
        if positions.contains(&stored) {
            out.push(finding);
            continue;
        }
        let position = positions[0] - 1;
        let len = finding_lines.len();
        let raw: Vec<&str> = live.lines().collect();
        let from = position + 1;
        let end = (position + len).min(raw.len()).max(from);
        finding.start_line = Some(from as u32);
        finding.end_line = Some(end as u32);
        finding.text = raw[position..end].join("\n");
        out.push(finding);
    }
    (out, gaps)
}

/// Normalized line core: leading comment fences, quote markers, and
/// other non-identifier punctuation do not survive indexing, so they
/// cannot distinguish a live line from a stale one. Trailing
/// whitespace never does either.
fn normalize_line(line: &str) -> String {
    line.trim()
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim_end()
        .to_string()
}

/// Normalize ripgrep hits into findings: the exhaustive arm speaks
/// the same `Finding` as every other producer, so one relevance judge
/// covers it. Scores are rank-free (`0.0`) — exhaustion has no signal.
#[must_use]
/// Normalize ripgrep hits into findings: the exhaustive arm speaks
/// the same `Finding` as every other producer, so one relevance judge
/// covers it. Scores are rank-free (`0.0`) — exhaustion has no signal.
pub fn rg_hits_to_findings(
    workspace: &std::path::Path,
    hits: &[crate::query::rg_backend::RgHit],
) -> Vec<crate::read::engine::Finding> {
    use crate::read::engine::{Finding, FindingKind, FindingProducer};
    hits.iter()
        .filter(|hit| {
            crate::source_scope::classify(&hit.path, None)
                == crate::source_scope::SourceScope::Product
        })
        .enumerate()
        .map(|(index, hit)| {
            let mut text = hit.text.clone();
            for line in hit.context_before.iter().chain(hit.context_after.iter()) {
                text.push('\n');
                text.push_str(line);
            }
            Finding {
                id: format!("rg-{}:{}:{}", hit.path, hit.line, index),
                producer: FindingProducer::Ripgrep,
                kind: FindingKind::Code,
                source: crate::source_ref::SourceRef::from_workspace(
                    workspace,
                    std::path::Path::new(&hit.path),
                ),
                start_line: Some(hit.line),
                end_line: Some(hit.line),
                text,
                producer_score: 0.0,
                provenance: "ripgrep".to_string(),
            }
        })
        .collect()
}
