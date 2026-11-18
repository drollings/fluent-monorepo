//! Calibration harness for confidence vs task-value thresholds.
//!
//! Zero-domain: no domain types. Sweep math is pure; `emit_markdown_artifact`
//! is the one best-effort I/O edge. Composed everywhere thresholds
//! are trusted for caching/persisting decisions. The shipping gate is
//! precision ≥0.90 and FPR ≤0.05 on control groups (see [`GateBars::SHIPPING`]).
//!
//! Scope: threshold sweeps over scored cases only. Must-fire/must-not-fire
//! control gates (binary accept at a frozen expectation, no scores, no
//! thresholds — e.g. the kernels control/treatment gate in
//! `protopyne-kernels/src/calibration.rs`) do not report through this
//! harness: mapping their rows onto scores and a fictitious threshold would
//! change what a row means while adding a parameter no gate reads.

/// Precision/recall/FPR at a single operating point.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationReport {
    /// TP / (TP+FP) — 1.0 when no positive predictions (no false positives).
    pub precision: f64,
    /// TP / (TP+FN) — 1.0 when no actual positives.
    pub recall: f64,
    /// FP / (FP+TN) — 0.0 when no actual negatives.
    pub fpr: f64,
    /// Number of cases evaluated (TP+FP+TN+FN).
    pub support: usize,
    /// Raw counts for audit.
    pub tp: usize,
    pub fp: usize,
    pub tn: usize,
    pub r#fn: usize,
}

impl CalibrationReport {
    /// Whether this operating point passes the shipping gate
    /// (precision ≥ 0.90, FPR ≤ 0.05 — the historical operating
    /// point, kept as the default, not as shared-code constants
    /// other gates must reuse; see [`GateBars`]).
    #[must_use]
    pub fn passes_gate(&self) -> bool {
        self.passes_gate_at(GateBars::SHIPPING)
    }

    /// Whether this operating point passes caller-supplied bars.
    #[must_use]
    pub fn passes_gate_at(&self, bars: GateBars) -> bool {
        bars.passes(self)
    }

    /// Markdown row for CI artifact tables.
    #[must_use]
    pub fn markdown_row(&self, threshold: f64) -> String {
        format!(
            "| {:.2} | {:.3} | {:.3} | {:.3} | {} | {} |",
            threshold,
            self.precision,
            self.recall,
            self.fpr,
            self.support,
            if self.passes_gate() { "✅" } else { "❌" }
        )
    }
}

/// Bars for [`CalibrationReport::passes_gate`]: minimum precision
/// and maximum FPR. Task-value gated: measured against labeled cases
/// plus a must-not-fire control group before any heuristic ships. The
/// shipping default (0.90 / 0.05) preserves the historical operating
/// point; call sites with their own bars use `passes_gate_at` instead
/// of forking the predicate. (Deliberately not a shared
/// coherence/safety gate: FPR needs `<=` while coherence needs `>=` —
/// routing through one borrowed name would invert a comparison.)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateBars {
    /// Minimum precision to pass.
    pub precision_min: f64,
    /// Maximum false-positive rate to pass.
    pub fpr_max: f64,
}

impl GateBars {
    /// Historical shipping operating point (0.90 / 0.05).
    pub const SHIPPING: Self = Self {
        precision_min: 0.90,
        fpr_max: 0.05,
    };

    /// Whether `report` clears these bars.
    #[must_use]
    pub fn passes(&self, report: &CalibrationReport) -> bool {
        report.precision >= self.precision_min && report.fpr <= self.fpr_max
    }
}

/// Compute precision/recall/FPR at `threshold`.
///
/// `score_of` is the detector confidence (e.g. cosine similarity, overall
/// confidence), `label_of` is the ground-truth correctness (true = should fire).
/// A case fires when `score >= threshold`.
pub fn calibrate_threshold<S>(
    cases: &[S],
    score_of: impl Fn(&S) -> f64,
    label_of: impl Fn(&S) -> bool,
    threshold: f64,
) -> CalibrationReport {
    let mut tp = 0usize;
    let mut fp = 0usize;
    let mut tn = 0usize;
    let mut r#fn = 0usize;
    for c in cases {
        let score = score_of(c);
        let label = label_of(c);
        let predicted = score >= threshold;
        match (predicted, label) {
            (true, true) => tp += 1,
            (true, false) => fp += 1,
            (false, false) => tn += 1,
            (false, true) => r#fn += 1,
        }
    }
    let precision = if tp + fp == 0 {
        1.0
    } else {
        tp as f64 / (tp + fp) as f64
    };
    let recall = if tp + r#fn == 0 {
        1.0
    } else {
        tp as f64 / (tp + r#fn) as f64
    };
    let fpr = if fp + tn == 0 {
        0.0
    } else {
        fp as f64 / (fp + tn) as f64
    };
    CalibrationReport {
        precision,
        recall,
        fpr,
        support: cases.len(),
        tp,
        fp,
        tn,
        r#fn,
    }
}

/// Sweep `thresholds` over the same case set, returning one report per threshold
/// in the order supplied.
pub fn sweep_thresholds<S>(
    cases: &[S],
    score_of: impl Fn(&S) -> f64,
    label_of: impl Fn(&S) -> bool,
    thresholds: &[f64],
) -> Vec<(f64, CalibrationReport)> {
    thresholds
        .iter()
        .map(|&t| {
            let r = calibrate_threshold(cases, &score_of, &label_of, t);
            (t, r)
        })
        .collect()
}

/// Default sweep grid 0.0..1.0 step 0.05 (21 points) plus caller-supplied extras.
pub fn default_thresholds() -> Vec<f64> {
    (0..=20).map(|i| f64::from(i) * 0.05).collect()
}

/// Per-label hit counts for multi-class precision/recall (shared
/// core). The empty-denominator convention is a caller-supplied
/// fallback, never a hidden constant: triage passes 0.0 (an
/// unpredicted class scores 0), intent measurement passes 1.0 (an
/// unused class cannot fail its bar).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClassCounts {
    /// Class label.
    pub label: String,
    /// Predicted correctly.
    pub hits: usize,
    /// Times predicted.
    pub predicted: usize,
    /// Times actually correct (golden).
    pub actual: usize,
}

impl ClassCounts {
    /// Precision: hits / predicted, `empty` when never predicted.
    #[must_use]
    pub fn precision_or(&self, empty: f64) -> f64 {
        precision_or(self.hits, self.predicted, empty)
    }

    /// Recall: hits / actual, `empty` when never golden.
    #[must_use]
    pub fn recall_or(&self, empty: f64) -> f64 {
        recall_or(self.hits, self.actual, empty)
    }
}

/// Precision with an explicit empty-denominator fallback: hits /
/// predicted, `empty` when predicted == 0.
#[must_use]
pub fn precision_or(hits: usize, predicted: usize, empty: f64) -> f64 {
    if predicted == 0 {
        empty
    } else {
        hits as f64 / predicted as f64
    }
}

/// Recall with an explicit empty-denominator fallback: hits / actual,
/// `empty` when actual == 0.
#[must_use]
pub fn recall_or(hits: usize, actual: usize, empty: f64) -> f64 {
    if actual == 0 {
        empty
    } else {
        hits as f64 / actual as f64
    }
}

/// Macro-averaged precision over classes (0.0 when there are none,
/// matching the triage rollup).
#[must_use]
pub fn macro_precision_or(classes: &[ClassCounts], empty: f64) -> f64 {
    if classes.is_empty() {
        0.0
    } else {
        classes.iter().map(|c| c.precision_or(empty)).sum::<f64>() / classes.len() as f64
    }
}

/// Macro-averaged recall over classes (0.0 when there are none,
/// matching the triage rollup).
#[must_use]
pub fn macro_recall_or(classes: &[ClassCounts], empty: f64) -> f64 {
    if classes.is_empty() {
        0.0
    } else {
        classes.iter().map(|c| c.recall_or(empty)).sum::<f64>() / classes.len() as f64
    }
}

/// Render a markdown table for `reports` (from `sweep_thresholds`) to `path`.
///
/// Creates parent directories as needed. Returns the markdown string.
///
/// Named `render_calibration_table` canonically to distinguish it from the
/// generic [`render_markdown_table`](crate::format::render_markdown_table)
/// (headers + string rows). This one renders `(threshold, CalibrationReport)`
/// rows with the fixed `threshold | precision | recall | FPR | support | gate`
/// schema; the generic one renders arbitrary string grids.
pub fn render_calibration_table(reports: &[(f64, CalibrationReport)]) -> String {
    let mut out = String::new();
    out.push_str("| threshold | precision | recall | FPR | support | gate |\n");
    out.push_str("|-----------|-----------|--------|-----|---------|------|\n");
    for (t, r) in reports {
        out.push_str(&r.markdown_row(*t));
        out.push('\n');
    }
    out
}

/// Render a markdown table for `reports` (from `sweep_thresholds`).
///
/// Legacy name for [`render_calibration_table`]: same schema, same output.
/// New code prefers `render_calibration_table`, which does not collide with
/// the generic `format::render_markdown_table`.
#[deprecated(note = "use `render_calibration_table` instead (same output)")]
pub fn render_markdown_table(reports: &[(f64, CalibrationReport)]) -> String {
    render_calibration_table(reports)
}

/// Write `reports` as markdown to the first writable `target/calibration/<name>.md`.
/// Creates the directory if missing. Never panics — logs on error.
/// Tries workspace root and current dir to be robust under `cargo test -p`.
pub fn emit_markdown_artifact(name: &str, reports: &[(f64, CalibrationReport)]) {
    let md = render_calibration_table(reports);
    let candidates = [
        std::path::PathBuf::from("target/calibration"),
        std::path::PathBuf::from("../target/calibration"),
        std::path::PathBuf::from("../../target/calibration"),
        std::path::PathBuf::from("../../../target/calibration"),
    ];
    // Also try to locate workspace root by walking up from current_dir for Cargo.toml with [workspace]
    let mut extra = Vec::new();
    if let Ok(cur) = std::env::current_dir() {
        let mut p = cur.as_path();
        for _ in 0..6 {
            let candidate = p.join("target/calibration");
            if p.join("Cargo.toml").exists() {
                extra.push(candidate);
            }
            if let Some(parent) = p.parent() {
                p = parent;
            } else {
                break;
            }
        }
        // Also try CARGO_MANIFEST_DIR parent chain
        if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
            let mut pm = std::path::Path::new(&manifest);
            for _ in 0..6 {
                extra.push(pm.join("../../target/calibration"));
                extra.push(pm.join("../target/calibration"));
                if let Some(parent) = pm.parent() {
                    pm = parent;
                } else {
                    break;
                }
            }
        }
    }
    let mut dirs = candidates.to_vec();
    dirs.extend(extra);
    // Deduplicate, then write to the first writable directory only.
    dirs.sort();
    dirs.dedup();
    if let Some(dir) = first_writable_dir(&dirs) {
        let path = dir.join(format!("{name}.md"));
        if std::fs::write(&path, &md).is_ok() {
            eprintln!("calibration artifact: {}", path.display());
            return;
        }
    }
    // Fallback: original path
    let dir = std::path::Path::new("target/calibration");
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(format!("{name}.md")), &md);
}

/// First directory in `dirs` that can be created (in order). Extracted so
/// the first-writable selection is unit-testable without mutating the
/// process working directory.
fn first_writable_dir(dirs: &[std::path::PathBuf]) -> Option<&std::path::PathBuf> {
    dirs.iter().find(|dir| std::fs::create_dir_all(dir).is_ok())
}

#[cfg(test)]
#[path = "tests/calibration_emit.rs"]
mod calibration_emit_tests;
