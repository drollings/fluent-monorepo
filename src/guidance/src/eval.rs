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

#[cfg(test)]
#[path = "../tests/read_eval.rs"]
mod read_eval_tests;

#[cfg(test)]
#[path = "../tests/explain_characterization.rs"]
mod explain_characterization_tests;
