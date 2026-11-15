//! One deduplicated evidence set across producers.
use std::collections::HashSet;

use crate::read::engine::Finding;

/// Deduplicated findings before budget capping. Rank order is
/// first-seen: the first producer to surface a location wins, later
/// duplicates drop. Location is `(workspace path, start, end)`; ids
/// dedupe exactly to keep distinct findings at one location.
#[derive(Debug, Default, Clone)]
/// Deduplicated findings before budget capping. Rank order is
/// first-seen: the first producer to surface a location wins, later
/// duplicates drop. Location is `(workspace path, start, end)`; ids
/// dedupe exactly to keep distinct findings at one location.
pub struct EvidenceSet {
    findings: Vec<Finding>,
}

impl EvidenceSet {
    /// Empty set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            findings: Vec::new(),
        }
    }

    /// Append findings preserving order.
    pub fn extend(&mut self, findings: Vec<Finding>) {
        self.findings.extend(findings);
    }

    /// Dedupe by id then by location, preserving first-seen order.
    /// File-level collapse keeps one row per file: distinct excerpts
    /// union into that row (longest first, bounded by one excerpt's
    /// budget) so facts living in shorter excerpts still render, while
    /// the cited span follows the longest excerpt. Contained excerpts
    /// drop as redundant.
    pub fn dedupe(&mut self) {
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut seen_locs: HashSet<(String, Option<u32>, Option<u32>)> = HashSet::new();
        // Per-file slot plus the longest served constituent's length;
        // the row's span follows that constituent (span stability).
        let mut best: std::collections::HashMap<String, (usize, usize)> =
            std::collections::HashMap::new();
        let mut out: Vec<Finding> = Vec::new();
        for finding in std::mem::take(&mut self.findings) {
            if !seen_ids.insert(finding.id.clone()) {
                continue;
            }
            // Edge rows are a list, not excerpts: exact-id dupes drop,
            // but same-file edges never union into each other or into
            // code rows, or the caller list fuses into one excerpt and
            // the Call graph section starves.
            if finding.kind == crate::read::engine::FindingKind::Edge {
                out.push(finding);
                continue;
            }
            let loc = (
                finding.source.display().to_string(),
                finding.start_line,
                finding.end_line,
            );
            if !seen_locs.insert(loc) {
                continue;
            }
            let file = finding.source.display().to_string();
            if let Some(&(slot, _)) = best.get(&file) {
                let row = &mut out[slot];
                if row.text.contains(&finding.text) {
                    continue;
                }
                if finding.text.contains(&row.text) {
                    *row = finding;
                    best.insert(file, (slot, row.text.len()));
                    continue;
                }
                let cap = crate::read::engine::RG_EXCERPT_CHAR_CAP;
                if row.text.len() + 1 + finding.text.len() > cap {
                    continue;
                }
                if finding.text.len() > best[&file].1 {
                    row.text = format!("{}\n{}", finding.text, row.text);
                    row.id.clone_from(&finding.id);
                    row.producer = finding.producer;
                    row.kind = finding.kind;
                    row.start_line = finding.start_line;
                    row.end_line = finding.end_line;
                    row.producer_score = finding.producer_score;
                    row.provenance.clone_from(&finding.provenance);
                    best.insert(file, (slot, finding.text.len()));
                } else {
                    row.text = format!("{}\n{}", row.text, finding.text);
                }
                continue;
            }
            let len = finding.text.len();
            best.insert(file.clone(), (out.len(), len));
            out.push(finding);
        }
        self.findings = out;
    }

    /// Cap by token budget, whole findings only (first always kept).
    #[must_use]
    pub fn capped(self, budget_tokens: usize) -> Vec<Finding> {
        crate::read::engine::cap_findings_by_budget(&self.findings, budget_tokens)
    }

    /// Current length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.findings.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }

    /// Consume into the ordered findings.
    #[must_use]
    pub fn into_findings(self) -> Vec<Finding> {
        self.findings
    }
}

#[cfg(test)]
#[path = "../../tests/evidence_set.rs"]
mod evidence_set_tests;
