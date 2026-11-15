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
    pub fn dedupe(&mut self) {
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut seen_locs: HashSet<(String, Option<u32>, Option<u32>)> = HashSet::new();
        let mut out = Vec::with_capacity(self.findings.len());
        for finding in std::mem::take(&mut self.findings) {
            if !seen_ids.insert(finding.id.clone()) {
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
