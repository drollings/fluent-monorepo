//! Independent natural-language evaluation cases.
//!
//! The corpus lives outside every indexed root (`doc/guidance/eval/`) and is
//! consumed by the benchmark and scorecard. It is never a retrieval input.

use std::path::Path;

use serde::Deserialize;

/// One evaluation question plus its task-value goldens.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
/// One evaluation question plus its task-value goldens.
pub struct EvalTask {
    /// Stable task id. Duplicate ids reject the corpus.
    pub id: String,
    /// Intended route: orientation, ownership, graph, mechanism, provenance,
    /// ranking, or general.
    #[serde(default = "default_intent")]
    pub intent: String,
    /// Question text.
    pub query: String,
    /// Files an orchestrator must read. `golden_files` is accepted as an
    /// alias so the corpus and older benchmark task files share one parser.
    #[serde(default, alias = "golden_files")]
    pub relevant: Vec<String>,
    /// Facts a correct answer must contain.
    #[serde(default)]
    pub golden_facts: Vec<String>,
    /// Sections the answer must render.
    #[serde(default)]
    pub required_sections: Vec<String>,
    /// `tune` or `heldout`.
    #[serde(default = "default_split")]
    pub split: String,
    /// A control case must stay silent.
    #[serde(default)]
    pub control: bool,
}

impl Default for EvalTask {
    fn default() -> Self {
        Self {
            id: String::new(),
            intent: default_intent(),
            query: String::new(),
            relevant: Vec::new(),
            golden_facts: Vec::new(),
            required_sections: Vec::new(),
            split: default_split(),
            control: false,
        }
    }
}

fn default_intent() -> String {
    "general".to_string()
}

fn default_split() -> String {
    "tune".to_string()
}

/// Parse JSONL eval cases. Blank lines are skipped; duplicate ids and
/// missing queries reject the whole file.
pub fn parse_eval_tasks(text: &str) -> Result<Vec<EvalTask>, String> {
    let mut tasks = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let task: EvalTask =
            serde_json::from_str(line).map_err(|error| format!("line {}: {error}", index + 1))?;
        if task.query.trim().is_empty() {
            return Err(format!("line {}: missing `query`", index + 1));
        }
        if task.id.trim().is_empty() {
            return Err(format!("line {}: missing `id`", index + 1));
        }
        if !seen.insert(task.id.clone()) {
            return Err(format!("line {}: duplicate id {:?}", index + 1, task.id));
        }
        tasks.push(task);
    }
    Ok(tasks)
}

/// Load one eval corpus file.
pub fn load_eval_tasks(path: &Path) -> Result<Vec<EvalTask>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("eval corpus read {}: {error}", path.display()))?;
    parse_eval_tasks(&raw)
}

/// Whether a workspace-relative path belongs to the eval corpus. Eval files
/// are excluded from indexing, doc scans, and capability imports.
#[must_use]
/// Whether a workspace-relative path belongs to the eval corpus. Eval files
/// are excluded from indexing, doc scans, and capability imports.
pub fn is_eval_path(relative: &str) -> bool {
    let relative = relative
        .strip_prefix("./")
        .unwrap_or(relative)
        .replace('\\', "/");
    relative == "doc/guidance/eval" || relative.starts_with("doc/guidance/eval/")
}

/// Spam ceiling: at most this many citations per answer.
pub const SPAM_MAX_CITATIONS: usize = 8;

/// Spam ceiling: the longest true citation range the battery tolerates.
/// Catches the measured 536/1000-line spans while allowing the 131-line
/// fixture shape; rendered excerpts stay windowed separately.
pub const SPAM_MAX_SPAN_LINES: u64 = 200;

/// Lines spanned by one rendered citation (`path:a-b` → `b - a`;
/// single lines and ranges without numbers span zero).
#[must_use]
pub fn citation_span_lines(citation: &str) -> u64 {
    let tail = citation
        .split_whitespace()
        .next()
        .unwrap_or("")
        .rsplit(':')
        .next()
        .unwrap_or("");
    let Some((start, end)) = tail.split_once('-') else {
        return 0;
    };
    let (Ok(start), Ok(end)) = (start.parse::<u64>(), end.parse::<u64>()) else {
        return 0;
    };
    end.saturating_sub(start)
}

/// Longest citation range in one answer (0 when there are none).
#[must_use]
pub fn max_citation_span(citations: &[String]) -> u64 {
    citations
        .iter()
        .map(|citation| citation_span_lines(citation))
        .max()
        .unwrap_or(0)
}

/// Measurement paths in one citation set: fixture or eval rows.
/// Zero is the contract; anything else is spam.
#[must_use]
pub fn fixture_path_count(paths: &[String]) -> usize {
    paths
        .iter()
        .filter(|path| {
            crate::source_scope::is_fixture_path(path) || is_eval_path(path)
        })
        .count()
}

/// One spam verdict for one answer: citation count, fixture
/// absence, and span ceiling all hold.
#[must_use]
pub fn spam_ok(citation_count: usize, fixture_count: usize, max_span: u64) -> bool {
    citation_count <= SPAM_MAX_CITATIONS
        && fixture_count == 0
        && max_span <= SPAM_MAX_SPAN_LINES
}

/// Rendered citation entries in one markdown answer: `- ` lines
/// under `### Citations`, excluding the `+ N more` aggregate. The
/// payload trim caps this list; the structured list may be longer.
#[must_use]
pub fn rendered_citation_count(rendered: &str) -> usize {
    let mut in_citations = false;
    let mut count = 0usize;
    for line in rendered.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("### ") {
            in_citations = trimmed == "### Citations";
            continue;
        }
        if !in_citations {
            continue;
        }
        if trimmed.starts_with("- + ") {
            continue;
        }
        if trimmed.starts_with("- ") {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
#[path = "../tests/read_eval.rs"]
mod read_eval_tests;

#[cfg(test)]
#[path = "../tests/explain_characterization.rs"]
mod explain_characterization_tests;
