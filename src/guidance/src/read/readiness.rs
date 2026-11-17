//! Readiness is coverage, not model confidence: whether the required
//! derived state exists and is fresh enough to answer.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::ProjectConfig;
use crate::index_generation::{schema_generation, stored_schema};
use crate::sync_engine::SyncEngine;

/// Mirror freshness partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
/// Mirror freshness partition.
pub enum MirrorState {
    /// Every indexed source has a current sidecar.
    Fresh { total: usize },
    /// Some sidecars are stale.
    Stale { total: usize, stale: usize },
    /// No source tree or mirror exists yet.
    Missing,
}

/// Derived-index state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
/// Derived-index state.
pub enum IndexState {
    /// Database exists and its schema generation matches this binary.
    Ready,
    /// No database at the expected path.
    Missing,
    /// Stored schema is absent or rotated; reads may misread.
    SchemaMismatch,
}

/// Embedded semantic-unit coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
/// Embedded semantic-unit coverage.
pub struct EmbeddingCoverage {
    pub ready: i64,
    pub total: i64,
}

/// Imported capability-card coverage. `total` is `None` until the external
/// corpus import can count rows; readiness must not invent a denominator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
/// Imported capability-card coverage. `total` is `None` until the external
/// corpus import can count rows; readiness must not invent a denominator.
pub struct CapabilityCoverage {
    pub ready: i64,
    pub total: Option<i64>,
}

/// Decision transport capability, discovered by the endpoint probe. Until then the
/// state is unknown and never treated as a failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
/// Decision transport capability, discovered by the endpoint probe. Until then the
/// state is unknown and never treated as a failure.
pub enum DecisionSupport {
    Unknown,
    Supported,
    Unsupported { reason: String },
}

/// Full coverage snapshot. Fields are additive so status consumers can grow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// Full coverage snapshot. Fields are additive so status consumers can grow.
pub struct Coverage {
    pub mirror: MirrorState,
    pub index: IndexState,
    pub embeddings: EmbeddingCoverage,
    /// True when the embed policy is off (distinct from missing vectors).
    #[serde(default)]
    pub embeddings_off: bool,
    pub capabilities: CapabilityCoverage,
    pub docs_present: bool,
    /// Stored doc-card rows (verbatim doc sections).
    #[serde(default)]
    pub doc_cards: usize,
    pub decision: DecisionSupport,
    /// Claim-support authorization (separate from finding relevance).
    #[serde(default)]
    pub support: DecisionSupport,
}

impl Coverage {
    /// A coverage snapshot with no derived state; useful for callers that
    /// cannot open a database.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            mirror: MirrorState::Missing,
            index: IndexState::Missing,
            embeddings: EmbeddingCoverage { ready: 0, total: 0 },
            embeddings_off: false,
            capabilities: CapabilityCoverage {
                ready: 0,
                total: None,
            },
            docs_present: false,
            doc_cards: 0,
            decision: DecisionSupport::Unknown,
            support: DecisionSupport::Unknown,
        }
    }
}

/// One degraded plane with a task-value reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// One degraded plane with a task-value reason.
pub enum Degradation {
    Stale { stale: usize, total: usize },
    EmbeddingsMissing { total: i64 },
    CapabilitiesMissing,
    DocsMissing,
    DecisionUnsupported { reason: String },
}

/// One repair action for a blocked read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// One repair action for a blocked read.
pub enum RepairAction {
    RunSync,
    RebuildIndex,
    AddSourceTree,
}

impl RepairAction {
    #[must_use]
    pub fn command(&self) -> &'static str {
        match self {
            Self::RunSync => "guidance sync",
            Self::RebuildIndex => "guidance sync (rebuilds the index)",
            Self::AddSourceTree => "configure src_dirs or add a source tree",
        }
    }
}

/// Readiness verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
/// Readiness verdict.
pub enum Readiness {
    Ready {
        coverage: Coverage,
    },
    Degraded {
        coverage: Coverage,
        reasons: Vec<Degradation>,
    },
    Blocked {
        coverage: Coverage,
        repairs: Vec<RepairAction>,
    },
}

impl Readiness {
    #[must_use]
    pub fn coverage(&self) -> &Coverage {
        match self {
            Self::Ready { coverage }
            | Self::Degraded { coverage, .. }
            | Self::Blocked { coverage, .. } => coverage,
        }
    }

    #[must_use]
    pub fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked { .. })
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    #[must_use]
    pub fn repair_hint(&self) -> Option<String> {
        match self {
            Self::Blocked { repairs, .. } => Some(
                repairs
                    .iter()
                    .map(RepairAction::command)
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            _ => None,
        }
    }

    #[must_use]
    pub fn degradation_reasons(&self) -> &[Degradation] {
        match self {
            Self::Degraded { reasons, .. } => reasons,
            _ => &[],
        }
    }
}

/// Classify one coverage snapshot. Blocking is reserved for states where a
/// read cannot be trusted at all; partial planes degrade and are labelled.
#[must_use]
/// Classify one coverage snapshot. Blocking is reserved for states where a
/// read cannot be trusted at all; partial planes degrade and are labelled.
pub fn classify(coverage: Coverage) -> Readiness {
    let mut repairs = Vec::new();
    let mut reasons = Vec::new();
    match &coverage.mirror {
        MirrorState::Missing => repairs.push(RepairAction::RunSync),
        MirrorState::Stale { total, stale } if *stale > 0 && *stale == *total => {
            repairs.push(RepairAction::RunSync);
        }
        MirrorState::Stale { total, stale } => {
            reasons.push(Degradation::Stale {
                stale: *stale,
                total: *total,
            });
        }
        MirrorState::Fresh { .. } => {}
    }
    match coverage.index {
        IndexState::Missing | IndexState::SchemaMismatch => {
            repairs.push(RepairAction::RebuildIndex);
        }
        IndexState::Ready => {}
    }
    if coverage.embeddings.total > 0 && coverage.embeddings.ready == 0 {
        reasons.push(Degradation::EmbeddingsMissing {
            total: coverage.embeddings.total,
        });
    }
    if coverage.capabilities.ready == 0 {
        reasons.push(Degradation::CapabilitiesMissing);
    }
    if !coverage.docs_present {
        reasons.push(Degradation::DocsMissing);
    }
    if let DecisionSupport::Unsupported { reason } = &coverage.decision {
        reasons.push(Degradation::DecisionUnsupported {
            reason: reason.clone(),
        });
    }
    if !repairs.is_empty() {
        Readiness::Blocked { coverage, repairs }
    } else if !reasons.is_empty() {
        Readiness::Degraded { coverage, reasons }
    } else {
        Readiness::Ready { coverage }
    }
}

/// Inspect the workspace. I/O failures are reported as missing state rather
/// than propagated, because readiness is a state, not an error.
#[must_use]
/// Inspect the workspace. I/O failures are reported as missing state rather
/// than propagated, because readiness is a state, not an error.
pub fn inspect(
    workspace: &Path,
    guidance_dir: &Path,
    db_path: &Path,
    cfg: &ProjectConfig,
) -> Readiness {
    let mut coverage = Coverage::empty();
    let src_dirs: Vec<PathBuf> = if cfg.src_dirs.is_empty() {
        // Same roots the sync sweep walks: the coverage counter and
        // `status` read one source, never two tallies.
        crate::read::project_map::default_source_roots(workspace)
    } else {
        cfg.src_dirs.iter().map(|dir| workspace.join(dir)).collect()
    };
    let mut total = 0usize;
    let mut stale = 0usize;
    let mut found_source = false;
    for src_dir in &src_dirs {
        if !src_dir.is_dir() {
            continue;
        }
        found_source = true;
        let engine = SyncEngine::new(guidance_dir.to_path_buf(), src_dir.clone());
        if let Ok(status) = engine.status() {
            total += status.total_files;
            stale += status.stale_files;
        }
    }
    coverage.mirror = if !found_source {
        MirrorState::Missing
    } else if stale == 0 {
        MirrorState::Fresh { total }
    } else {
        MirrorState::Stale { total, stale }
    };
    coverage.index = if db_path.is_file() {
        let index_dir = db_path.parent().unwrap_or_else(|| Path::new("."));
        match stored_schema(index_dir) {
            Some(schema) if schema == schema_generation() => IndexState::Ready,
            _ => IndexState::SchemaMismatch,
        }
    } else {
        IndexState::Missing
    };
    if db_path.is_file() {
        if let Ok(db) = crate::query::search_db::GuidanceSearchDb::open(db_path) {
            coverage.embeddings = EmbeddingCoverage {
                ready: db.embedded_fragment_count().unwrap_or(0),
                total: db.fragment_count().unwrap_or(0),
            };
            coverage.capabilities = CapabilityCoverage {
                ready: db.capability_ready_count().unwrap_or(0),
                total: None,
            };
            coverage.doc_cards = db
                .project_cards_by_kind("doc")
                .map_or(0, |cards| cards.len());
        }
    }
    coverage.embeddings_off = matches!(cfg.embed_policy, crate::config::EmbedPolicy::Off);
    coverage.docs_present = workspace.join(&cfg.docs_dir).is_dir()
        || workspace.join("README.md").is_file()
        || workspace.join("AGENTS.md").is_file();
    classify(coverage)
}

#[cfg(test)]
#[path = "../../tests/readiness.rs"]
mod readiness_tests;
