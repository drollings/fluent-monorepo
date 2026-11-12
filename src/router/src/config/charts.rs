//! Chart-store and post-processing configuration — the `charts` and
//! `post_process` sections of `RouterConfig`.

use serde::{Deserialize, Serialize};

// -- Charts (DAG workflow library) configuration --------------------------

/// Chart store configuration - the `charts` section of `RouterConfig`.
///
/// The store is owned by `fluent-router` (see `coral-router`/`charts/`): a
/// directory of human-authored chart JSON files, a router-side
/// `workflow_library` HNSW/SQLite path for retrieval, and the model key
/// used by chart-selection LLM adjudication.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// -- Charts (DAG workflow library) configuration --------------------------
/// Chart store configuration - the `charts` section of `RouterConfig`.
/// 
/// The store is owned by `fluent-router` (see `coral-router`/`charts/`): a
/// directory of human-authored chart JSON files, a router-side
/// `workflow_library` HNSW/SQLite path for retrieval, and the model key
/// used by chart-selection LLM adjudication.
pub struct ChartsConfig {
    /// Directory of `*.json` chart files loaded at boot. `None` - empty
    /// store (a missing directory is tolerated with a `warn!`).
    #[serde(default)]
    pub dir: Option<String>,
    /// `workflow_library` HNSW/SQLite file path. The index is built lazily
    /// at boot only when this is set.
    #[serde(default)]
    pub index_path: Option<String>,
    /// Chart-selection classifier model key (LLM adjudication step).
    /// Legacy spelling: prefer `selector_role` (duties name roles); an
    /// explicit `selector_model` still resolves when no role is set.
    #[serde(default)]
    pub selector_model: Option<String>,
    /// Role serving the chart-selector duty, resolved through
    /// [`crate::config::role_head_key`]. Wins over `selector_model` when set.
    #[serde(default)]
    pub selector_role: Option<String>,
    /// Max candidates surfaced to the selector's LLM adjudication.
    #[serde(default = "default_charts_max_candidates")]
    pub max_candidates: usize,
    /// Embedding-similarity threshold below which a chart is not a candidate.
    #[serde(default = "default_charts_min_score")]
    pub min_score: f64,
    /// Whether bound context entities are exposed to chart templates.
    #[serde(default = "default_charts_entity_context")]
    pub entity_context: bool,
}

impl Default for ChartsConfig {
    fn default() -> Self {
        Self {
            dir: None,
            index_path: None,
            selector_model: None,
            selector_role: None,
            max_candidates: default_charts_max_candidates(),
            min_score: default_charts_min_score(),
            entity_context: default_charts_entity_context(),
        }
    }
}

const fn default_charts_max_candidates() -> usize {
    5
}

const fn default_charts_min_score() -> f64 {
    0.6
}

const fn default_charts_entity_context() -> bool {
    true
}

// -- Post-processing configuration ---------------------

/// Post-processing configuration - the `post_process` section of
/// `RouterConfig`.
///
/// Controls the VISION learning loop: whether a *successful* dispatch is
/// distilled into a reusable draft chart. Per VISION -"Post-processing:
/// audit + workflow extraction", extraction is opt-in and the produced chart
/// is a draft that only becomes selectable after a rubric-validated run.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
/// -- Post-processing configuration ---------------------
/// Post-processing configuration - the `post_process` section of
/// `RouterConfig`.
/// 
/// Controls the VISION learning loop: whether a *successful* dispatch is
/// distilled into a reusable draft chart. Per VISION -"Post-processing:
/// audit + workflow extraction", extraction is opt-in and the produced chart
/// is a draft that only becomes selectable after a rubric-validated run.
pub struct PostProcessConfig {
    /// Whether successful dispatches are decomposed into draft charts
    /// automatically. Default `false` - the operator opts in.
    #[serde(default)]
    pub workflow_extraction: bool,
    /// Which successful dispatches are distilled into draft charts.
    /// Default `"frontier"` - the VISION learning loop learns from
    /// frontier-assisted (escalated/fallback) solutions, not the common
    /// local-primary path. `"all"` restores the blanket behavior by
    /// explicit opt-in.
    #[serde(default)]
    pub workflow_extraction_mode: WorkflowExtractionMode,
}

/// Extraction scope for the learning loop (see `PostProcessConfig`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Extraction scope for the learning loop (see `PostProcessConfig`).
pub enum WorkflowExtractionMode {
    /// Only frontier-assisted dispatches (an index > 0 in the primary +
    /// fallback chain) are distilled into draft charts.
    #[default]
    #[serde(rename = "frontier")]
    Frontier,
    /// Every successful dispatch is distilled.
    #[serde(rename = "all")]
    All,
}