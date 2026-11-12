//! Calibration registry: thresholds keyed by decision point, model
//! fingerprint, and question version — stored outside build output, loaded
//! from checked-in corpora.
//!
//! Promotion is fail-closed: `authorize` returns `ShadowOnly` unless an
//! entry matches all three keys AND its gate passed. Ground truth is never
//! model output — every case carries a human-family origin by type
//! construction. Gate floors are explicit per call (`GateParams`).

use serde::{Deserialize, Serialize};

use crate::decision::{control_fires, ThresholdSet};

/// `CaseOrigin`: where a label came from. There is deliberately no model-output variant:
/// model predictions are measurements to calibrate, never ground truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// `CaseOrigin`: where a label came from. There is deliberately no model-output variant:
/// model predictions are measurements to calibrate, never ground truth.
pub enum CaseOrigin {
    Human,
    HumanReviewed,
    Synthetic,
}

/// Provenance of one labelled case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Provenance of one labelled case.
pub struct CaseProvenance {
    pub origin: CaseOrigin,
    pub labeller: String,
    pub date: String,
}

impl CaseProvenance {
    /// Human-labelled provenance for fixtures and seeds.
    #[must_use]
    pub fn human(labeller: impl Into<String>, date: impl Into<String>) -> Self {
        Self {
            origin: CaseOrigin::Human,
            labeller: labeller.into(),
            date: date.into(),
        }
    }
}

/// Explicit per-point gate floors. Defaults mirror the shared harness gate
/// (precision ≥ 0.90, FPR ≤ 0.05) plus nonzero recall and zero control fires.
///
/// ```rust
/// use fluent_llm::decision::GateParams;
///
/// let params = GateParams::default();
/// assert!((params.min_precision - 0.90).abs() < 1e-12);
/// assert_eq!(params.max_control_fires, 0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
/// Explicit per-point gate floors. Defaults mirror the shared harness gate
/// (precision ≥ 0.90, FPR ≤ 0.05) plus nonzero recall and zero control fires.
/// 
/// ```rust
/// use fluent_llm::decision::GateParams;
/// 
/// let params = GateParams::default();
/// assert!((params.min_precision - 0.90).abs() < 1e-12);
/// assert_eq!(params.max_control_fires, 0);
/// ```
pub struct GateParams {
    pub min_precision: f64,
    pub max_fpr: f64,
    pub min_recall: f64,
    pub max_control_fires: usize,
}

impl Default for GateParams {
    fn default() -> Self {
        Self {
            min_precision: 0.90,
            max_fpr: 0.05,
            min_recall: 0.0,
            max_control_fires: 0,
        }
    }
}

/// Gate a threshold with explicit floors (recall strictly above the floor —
/// a never-firing threshold proves nothing).
///
/// ```rust
/// use fluent_llm::decision::{gate_for_point_with, GateParams, CaseProvenance, ScoredCase};
///
/// let provenance = CaseProvenance::human("example", "2026-11-08");
/// let cases = vec![
///     ScoredCase { score: 0.9, expected: true, control: false, provenance: provenance.clone() },
///     ScoredCase { score: 0.1, expected: false, control: false, provenance },
/// ];
/// assert!(gate_for_point_with(&cases, 0.5, &GateParams::default()));
/// ```
#[must_use]
/// Gate a threshold with explicit floors (recall strictly above the floor —
/// a never-firing threshold proves nothing).
/// 
/// ```rust
/// use fluent_llm::decision::{gate_for_point_with, GateParams, CaseProvenance, ScoredCase};
/// 
/// let provenance = CaseProvenance::human("example", "2026-11-08");
/// let cases = vec![
/// ScoredCase { score: 0.9, expected: true, control: false, provenance: provenance.clone() },
/// ScoredCase { score: 0.1, expected: false, control: false, provenance },
/// ];
/// assert!(gate_for_point_with(&cases, 0.5, &GateParams::default()));
/// ```
pub fn gate_for_point_with(
    cases: &[crate::decision::ScoredCase],
    threshold: f64,
    params: &GateParams,
) -> bool {
    let report = common_core::calibration::calibrate_threshold(
        cases,
        |c| c.score,
        |c| c.expected,
        threshold,
    );
    report.precision >= params.min_precision
        && report.fpr <= params.max_fpr
        && report.recall > params.min_recall
        && control_fires(cases, threshold) <= params.max_control_fires
}

/// Corpus-level provenance (`CorpusProvenance`) for a labelled set.
///
/// ```rust
/// use fluent_llm::decision::CorpusProvenance;
///
/// let provenance = CorpusProvenance {
///     source: "hand labels".to_string(),
///     labeller: "team".to_string(),
///     date: "2026-11-08".to_string(),
/// };
/// assert_eq!(provenance.labeller, "team");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Corpus-level provenance (`CorpusProvenance`) for a labelled set.
/// 
/// ```rust
/// use fluent_llm::decision::CorpusProvenance;
/// 
/// let provenance = CorpusProvenance {
/// source: "hand labels".to_string(),
/// labeller: "team".to_string(),
/// date: "2026-11-08".to_string(),
/// };
/// assert_eq!(provenance.labeller, "team");
/// ```
pub struct CorpusProvenance {
    pub source: String,
    pub labeller: String,
    pub date: String,
    /// Hash of the labelled cases the entry was calibrated from.
    /// Empty means unpinned (legacy): rotation cannot be detected.
    #[serde(default)]
    pub dataset: String,
}

/// A labelled corpus on disk: the unit the registry calibrates from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// A labelled corpus on disk: the unit the registry calibrates from.
pub struct Corpus {
    pub point: String,
    pub model_fingerprint: String,
    pub question_version: u64,
    pub provenance: CorpusProvenance,
    pub cases: Vec<crate::decision::ScoredCase>,
}

/// One gate measurement (`GateReport`), persisted with the thresholds it earned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// One gate measurement (`GateReport`), persisted with the thresholds it earned.
pub struct GateReport {
    pub threshold: f64,
    pub precision: f64,
    pub recall: f64,
    pub fpr: f64,
    pub support: usize,
    pub control_fires: usize,
    pub passed: bool,
    pub date: String,
}

/// Thresholds plus the gate that earned them, keyed for lookup (`RegistryEntry`).
///
/// Calibrate over a [`Corpus`] (see `RegistryEntry::calibrate`); the
/// round trip is exercised in `tests/decision_registry.rs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Thresholds plus the gate that earned them, keyed for lookup (`RegistryEntry`).
/// 
/// Calibrate over a [`Corpus`] (see `RegistryEntry::calibrate`); the
/// round trip is exercised in `tests/decision_registry.rs`.
pub struct RegistryEntry {
    pub point: String,
    pub model_fingerprint: String,
    pub question_version: u64,
    pub thresholds: ThresholdSet,
    pub gate: GateReport,
    pub provenance: CorpusProvenance,
    /// Dataset hash this entry was calibrated from. Rotating the
    /// dataset reverts the point to measuring until recalibrated.
    #[serde(default)]
    pub dataset: String,
}

impl RegistryEntry {
    /// Calibrate one threshold over `corpus` with default floors. The entry
    /// records the measurement truthfully — `gate.passed` may be false, in
    /// which case `authorize` still fails closed.
    pub fn calibrate(
        point: &str,
        model_fingerprint: &str,
        question_version: u64,
        corpus: &Corpus,
        threshold: f64,
    ) -> Result<Self, String> {
        Self::calibrate_with(
            point,
            model_fingerprint,
            question_version,
            corpus,
            threshold,
            None,
        )
    }

    /// Calibrate with explicit per-point thresholds mapped from one
    /// operating point: `act`/`review` bound the shipped band.
    pub fn calibrate_with(
        point: &str,
        model_fingerprint: &str,
        question_version: u64,
        corpus: &Corpus,
        threshold: f64,
        thresholds: Option<ThresholdSet>,
    ) -> Result<Self, String> {
        let report = common_core::calibration::calibrate_threshold(
            &corpus.cases,
            |c| c.score,
            |c| c.expected,
            threshold,
        );
        let fires = control_fires(&corpus.cases, threshold);
        let thresholds = thresholds.unwrap_or(ThresholdSet {
            act: threshold,
            review: threshold,
        });
        thresholds
            .validate()
            .map_err(|detail| format!("shipped thresholds invalid: {detail}"))?;
        Ok(Self {
            point: point.to_string(),
            model_fingerprint: model_fingerprint.to_string(),
            question_version,
            thresholds,
            gate: GateReport {
                threshold,
                precision: report.precision,
                recall: report.recall,
                fpr: report.fpr,
                support: report.support,
                control_fires: fires,
                passed: crate::decision::gate_for_point(&corpus.cases, threshold),
                date: corpus.provenance.date.clone(),
            },
            provenance: corpus.provenance.clone(),
            dataset: corpus.provenance.dataset.clone(),
        })
    }
}

/// Fail-closed authorization (`CalibrationStatus`): calibrated thresholds or shadow-only.
#[derive(Debug, Clone, PartialEq)]
/// Fail-closed authorization (`CalibrationStatus`): calibrated thresholds or shadow-only.
pub enum CalibrationStatus {
    /// Authorized: may steer behavior (side effects gated elsewhere).
    Calibrated(ThresholdSet),
    /// Advisory: may affect ordering and priority only, never side
    /// effects. Same evidence bar as authorized; narrower authority.
    Advisory(ThresholdSet),
    /// Measuring only: never obeyed, never cached as a verdict.
    ShadowOnly { reason: String },
}

impl CalibrationStatus {
    /// Authorized to steer behavior (side effects still gated elsewhere).
    #[must_use]
    pub fn is_calibrated(&self) -> bool {
        matches!(self, Self::Calibrated(_))
    }

    /// Allowed to affect ordering and priority only, never side effects.
    #[must_use]
    pub fn is_advisory(&self) -> bool {
        matches!(self, Self::Advisory(_))
    }

    #[must_use]
    pub fn is_shadow_only(&self) -> bool {
        matches!(self, Self::ShadowOnly { .. })
    }
}

/// Owned threshold registry with JSON persistence.
///
/// ```rust
/// use fluent_llm::decision::CalibrationRegistry;
/// use std::path::PathBuf;
///
/// let registry = CalibrationRegistry::empty();
/// assert!(registry.entries.is_empty());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
/// Owned threshold registry with JSON persistence.
/// 
/// ```rust
/// use fluent_llm::decision::CalibrationRegistry;
/// use std::path::PathBuf;
/// 
/// let registry = CalibrationRegistry::empty();
/// assert!(registry.entries.is_empty());
/// ```
pub struct CalibrationRegistry {
    pub entries: Vec<RegistryEntry>,
}

impl CalibrationRegistry {
    /// Empty registry: everything fails closed to shadow-only.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Insert or replace the entry for its (point, fingerprint, version).
    ///
    /// ```rust
    /// use fluent_llm::decision::CalibrationRegistry;
    ///
    /// let mut registry = CalibrationRegistry::empty();
    /// assert!(registry.lookup("intent", "fp", 1).is_none());
    /// ```
    pub fn upsert(&mut self, entry: RegistryEntry) {
        if let Some(slot) = self.entries.iter_mut().find(|e| {
            e.point == entry.point
                && e.model_fingerprint == entry.model_fingerprint
                && e.question_version == entry.question_version
        }) {
            *slot = entry;
        } else {
            self.entries.push(entry);
        }
    }

    /// Look up a passing entry by all three keys.
    #[must_use]
    pub fn lookup(
        &self,
        point: &str,
        model_fingerprint: &str,
        question_version: u64,
    ) -> Option<&RegistryEntry> {
        self.entries.iter().find(|e| {
            e.point == point
                && e.model_fingerprint == model_fingerprint
                && e.question_version == question_version
                && e.gate.passed
        })
    }

    /// Load a registry from JSON.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("registry read {}: {e}", path.display()))?;
        serde_json::from_str(&raw).map_err(|e| format!("registry parse: {e}"))
    }

    /// Persist atomically (parent created as needed).
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("registry mkdir {}: {e}", parent.display()))?;
            }
        }
        let raw =
            serde_json::to_string_pretty(self).map_err(|e| format!("registry encode: {e}"))?;
        common_core::io::write_atomic(path, raw.as_bytes())
            .map_err(|e| format!("registry write {}: {e}", path.display()))
    }

    /// Load a labelled corpus from JSON.
    pub fn load_corpus(path: &std::path::Path) -> Result<Corpus, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("corpus read {}: {e}", path.display()))?;
        serde_json::from_str(&raw).map_err(|e| format!("corpus parse: {e}"))
    }
}

/// Authorize a decision point: passing entry on all three keys yields its
/// thresholds, anything else is shadow-only with the reason named.
///
/// ```rust
/// use fluent_llm::decision::{authorize, CalibrationRegistry};
///
/// let status = authorize(&CalibrationRegistry::empty(), "intent", "fp", 1);
/// assert!(status.is_shadow_only());
/// ```
#[must_use]
/// Authorize a decision point: passing entry on all three keys yields its
/// thresholds, anything else is shadow-only with the reason named.
/// 
/// ```rust
/// use fluent_llm::decision::{authorize, CalibrationRegistry};
/// 
/// let status = authorize(&CalibrationRegistry::empty(), "intent", "fp", 1);
/// assert!(status.is_shadow_only());
/// ```
pub fn authorize(
    registry: &CalibrationRegistry,
    point: &str,
    model_fingerprint: &str,
    question_version: u64,
) -> CalibrationStatus {
    match registry.lookup(point, model_fingerprint, question_version) {
        Some(entry) => CalibrationStatus::Calibrated(entry.thresholds),
        None => CalibrationStatus::ShadowOnly {
            reason: format!(
                "no passing calibration for ({point}, {model_fingerprint}, v{question_version})"
            ),
        },
    }
}

/// Advisory authorization: a passing entry on all three keys yields
/// ordering-and-priority-only authority, never side-effect authority.
/// Same evidence bar as `authorize`; narrower grant.
#[must_use]
/// Advisory authorization: a passing entry on all three keys yields
/// ordering-and-priority-only authority, never side-effect authority.
/// Same evidence bar as `authorize`; narrower grant.
pub fn authorize_advisory(
    registry: &CalibrationRegistry,
    point: &str,
    model_fingerprint: &str,
    question_version: u64,
) -> CalibrationStatus {
    match registry.lookup(point, model_fingerprint, question_version) {
        Some(entry) => CalibrationStatus::Advisory(entry.thresholds),
        None => CalibrationStatus::ShadowOnly {
            reason: format!(
                "no passing calibration for ({point}, {model_fingerprint}, v{question_version})"
            ),
        },
    }
}

/// Dataset-pinned authorization: the passing entry must additionally
/// record the expected dataset hash. Rotating the dataset (or asking
/// with an unpinned expectation against a pinned entry) reverts the
/// point to measuring until recalibrated.
#[must_use]
/// Dataset-pinned authorization: the passing entry must additionally
/// record the expected dataset hash. Rotating the dataset (or asking
/// with an unpinned expectation against a pinned entry) reverts the
/// point to measuring until recalibrated.
pub fn authorize_pinned(
    registry: &CalibrationRegistry,
    point: &str,
    model_fingerprint: &str,
    question_version: u64,
    dataset: &str,
) -> CalibrationStatus {
    match registry.lookup(point, model_fingerprint, question_version) {
        Some(entry) if entry.dataset == dataset => {
            CalibrationStatus::Calibrated(entry.thresholds)
        }
        Some(_) => CalibrationStatus::ShadowOnly {
            reason: format!("dataset rotated for ({point}, {model_fingerprint}, v{question_version})"),
        },
        None => CalibrationStatus::ShadowOnly {
            reason: format!(
                "no passing calibration for ({point}, {model_fingerprint}, v{question_version})"
            ),
        },
    }
}