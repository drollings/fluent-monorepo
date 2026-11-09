//! Hermetic scorecard: the measuring instrument for the improvement loop.
//!
//! Fast tier runs fully in-process (no subprocess, no model): navigation
//! tiers over the checked-in corpora, fixpoint/mutation behavior, speed via
//! sorted microsecond samples, decision-layer counters, graph accuracy, and
//! lane deferral. One Makefile entry point (`guidance-scorecard`) prints at
//! most 40 lines plus JSON; exit 2 on ratchet violation. Tune/heldout splits
//! are reported side by side; nothing iterates on heldout.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::graph_index::{GraphIndex, HarvestInput};
use crate::grounding::can_synthesize;
use crate::model_lanes::{Lane, LaneConfig, LaneGate};
use crate::query::decision_points::{evaluate_with_metrics, DecisionMetrics};
use crate::query_engine::QueryEngine;
use crate::sync_engine::SyncEngine;

/// How to reproduce a metric's failure with one narrow command.
#[derive(Debug, Clone)]
pub struct Measured {
    /// Flat metric name (`nav.recall_at_3.tune`).
    pub name: String,
    /// Measured value.
    pub value: f64,
    /// One-line reproduction command.
    pub repro: String,
    /// False for lower-is-better metrics (speed).
    pub higher_is_better: bool,
}

/// Checked-in baseline: last approved values plus the ratchet policy.
/// Tolerances absorb measurement noise (higher-better may read down to
/// `base - tol`, lower-better up to `base + tol`); bounds are absolute
/// bars that hold even when tolerance would allow more. History makes
/// hand edits detectable: replaying it must reproduce `metrics`.
#[derive(Debug, Clone, Default)]
pub struct Baseline {
    /// Flat metric name → last approved value.
    pub metrics: HashMap<String, f64>,
    /// Flat metric name → allowed adverse deviation (default 0.0).
    pub tolerances: HashMap<String, f64>,
    /// Flat metric name → absolute bar (higher-better: minimum;
    /// lower-better: maximum).
    pub bounds: HashMap<String, f64>,
    /// Append-only update log (reason + diff per entry).
    pub history: Vec<HistoryEntry>,
}

/// One reasoned update: what changed, why, and the policy in force after.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HistoryEntry {
    /// UTC date (`YYYY-MM-DD`).
    pub date: String,
    /// Human reason for the update.
    pub reason: String,
    /// Metric → (old, new); old is `None` for first appearances.
    pub changes: HashMap<String, (Option<f64>, f64)>,
    /// Tolerances in force after this entry.
    pub tolerances: HashMap<String, f64>,
    /// Bounds in force after this entry.
    pub bounds: HashMap<String, f64>,
}

/// Ratchet status of one metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricStatus {
    /// At or beyond baseline in the good direction.
    Pass,
    /// Below baseline: exits non-zero.
    Violation,
    /// No baseline value: reported, never a violation.
    NoBaseline,
}

/// One scored metric line.
#[derive(Debug, Clone)]
pub struct ScoredLine {
    /// Flat metric name.
    pub name: String,
    /// Measured value.
    pub value: f64,
    /// Baseline value, if any.
    pub baseline: Option<f64>,
    /// `value - baseline`, if any.
    pub delta: Option<f64>,
    /// Ratchet verdict.
    pub status: MetricStatus,
    /// One-line reproduction command.
    pub repro: String,
}

/// One of the three worst regressions.
#[derive(Debug, Clone)]
pub struct Regression {
    /// Flat metric name.
    pub name: String,
    /// Adverse delta.
    pub delta: f64,
    /// One-line reproduction command.
    pub repro: String,
}

/// Scored tier: lines, worst regressions, process exit code.
#[derive(Debug, Clone)]
pub struct Scorecard {
    /// One line per metric (violations first).
    pub lines: Vec<ScoredLine>,
    /// Up to three worst regressions by adverse delta.
    pub regressions: Vec<Regression>,
    /// 0 clean, 2 on any ratchet violation.
    pub exit_code: i32,
}

/// Score measured values against the baseline. A metric passes when it is
/// at least as good as its approved value within tolerance, and within its
/// absolute bound: higher-better needs `value + tol >= base` and
/// `value >= bound`; lower-better needs `value <= base + tol` and
/// `value <= bound`. Missing values or policy never pass silently — they
/// report `no-baseline`.
#[must_use]
pub fn score_metrics(measured: &[Measured], baseline: &Option<Baseline>) -> Scorecard {
    let mut lines: Vec<ScoredLine> = measured
        .iter()
        .map(|metric| {
            let base = baseline
                .as_ref()
                .and_then(|found| found.metrics.get(&metric.name).copied());
            let (delta, status) = match (baseline, base) {
                (Some(policy), Some(base)) => {
                    let tol = policy.tolerances.get(&metric.name).copied().unwrap_or(0.0);
                    let bound = policy.bounds.get(&metric.name).copied();
                    let delta = metric.value - base;
                    let within_tolerance = if metric.higher_is_better {
                        metric.value + tol >= base
                    } else {
                        metric.value <= base + tol
                    };
                    let within_bound = bound.is_none_or(|bound| {
                        if metric.higher_is_better {
                            metric.value >= bound
                        } else {
                            metric.value <= bound
                        }
                    });
                    let pass = within_tolerance && within_bound;
                    (
                        Some(delta),
                        if pass {
                            MetricStatus::Pass
                        } else {
                            MetricStatus::Violation
                        },
                    )
                }
                _ => (None, MetricStatus::NoBaseline),
            };
            ScoredLine {
                name: metric.name.clone(),
                value: metric.value,
                baseline: base,
                delta,
                status,
                repro: metric.repro.clone(),
            }
        })
        .collect();
    // Violations surface first so truncation can never hide one.
    lines.sort_by(|left, right| {
        (left.status != MetricStatus::Violation).cmp(&(right.status != MetricStatus::Violation))
    });
    let mut regressions: Vec<Regression> = lines
        .iter()
        .filter(|line| line.status == MetricStatus::Violation)
        .map(|line| Regression {
            name: line.name.clone(),
            delta: line.delta.unwrap_or(0.0),
            repro: line.repro.clone(),
        })
        .collect();
    regressions.sort_by(|left, right| {
        left.delta
            .partial_cmp(&right.delta)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    regressions.truncate(3);
    let exit_code = if lines
        .iter()
        .any(|line| line.status == MetricStatus::Violation)
    {
        2
    } else {
        0
    };
    Scorecard {
        lines,
        regressions,
        exit_code,
    }
}

/// At most 40 lines: header, metric lines (violations first), worst
/// regressions with repro commands. Full detail lives in JSON/the log.
#[must_use]
pub fn render_human(card: &Scorecard) -> String {
    const MAX_METRIC_LINES: usize = 34;
    let pass = card
        .lines
        .iter()
        .filter(|line| line.status == MetricStatus::Pass)
        .count();
    let violated = card
        .lines
        .iter()
        .filter(|line| line.status == MetricStatus::Violation)
        .count();
    let unbased = card.lines.len() - pass - violated;
    use std::fmt::Write as _;
    let mut text = format!(
        "scorecard: {} metrics, {pass} pass, {violated} violation, {unbased} no-baseline\n",
        card.lines.len()
    );
    for line in card.lines.iter().take(MAX_METRIC_LINES) {
        let (base, delta) = match (line.baseline, line.delta) {
            (Some(base), Some(delta)) => (format!("{base:.3}"), format!("{delta:+.3}")),
            _ => ("—".to_string(), "—".to_string()),
        };
        let status = match line.status {
            MetricStatus::Pass => "pass",
            MetricStatus::Violation => "VIOLATION",
            MetricStatus::NoBaseline => "no-baseline",
        };
        let _ = writeln!(
            text,
            "{} value={:.3} baseline={base} delta={delta} {status}",
            line.name, line.value
        );
    }
    if card.lines.len() > MAX_METRIC_LINES {
        let _ = writeln!(
            text,
            "... ({} more in JSON)",
            card.lines.len() - MAX_METRIC_LINES
        );
    }
    if !card.regressions.is_empty() {
        text.push_str("worst regressions:\n");
        for regression in &card.regressions {
            let _ = writeln!(
                text,
                "- {} delta={:+.3} repro: {}",
                regression.name, regression.delta, regression.repro
            );
        }
    }
    text
}

/// Machine-readable scorecard for agents.
#[must_use]
pub fn render_json(card: &Scorecard) -> String {
    let metrics: Vec<serde_json::Value> = card
        .lines
        .iter()
        .map(|line| {
            serde_json::json!({
                "name": line.name,
                "value": line.value,
                "baseline": line.baseline,
                "delta": line.delta,
                "status": match line.status {
                    MetricStatus::Pass => "pass",
                    MetricStatus::Violation => "violation",
                    MetricStatus::NoBaseline => "no-baseline",
                },
                "repro": line.repro,
            })
        })
        .collect();
    let regressions: Vec<serde_json::Value> = card
        .regressions
        .iter()
        .map(|regression| {
            serde_json::json!({
                "name": regression.name,
                "delta": regression.delta,
                "repro": regression.repro,
            })
        })
        .collect();
    serde_json::json!({
        "tier": "fast",
        "metrics": metrics,
        "regressions": regressions,
        "exit_code": card.exit_code,
    })
    .to_string()
}

/// Load a checked-in baseline. New policy fields default empty so older
/// files still load; missing or unparseable files are not violations.
#[must_use]
pub fn load_baseline(path: &Path) -> Option<Baseline> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let table = |key: &str| {
        value
            .get(key)
            .and_then(|section| section.as_object())
            .map(|object| {
                object
                    .iter()
                    .filter_map(|(name, metric)| metric.as_f64().map(|value| (name.clone(), value)))
                    .collect()
            })
            .unwrap_or_default()
    };
    let history = value
        .get("history")
        .and_then(|entries| entries.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let changes = entry
                        .get("changes")?
                        .as_object()?
                        .iter()
                        .filter_map(|(name, pair)| {
                            let pair = pair.as_array()?;
                            let old = match pair.first()? {
                                serde_json::Value::Null => None,
                                old => old.as_f64(),
                            };
                            Some((name.clone(), (old, pair.get(1)?.as_f64()?)))
                        })
                        .collect();
                    Some(HistoryEntry {
                        date: entry.get("date")?.as_str()?.to_string(),
                        reason: entry.get("reason")?.as_str()?.to_string(),
                        changes,
                        tolerances: entry
                            .get("tolerances")
                            .and_then(|section| section.as_object())
                            .map(|object| {
                                object
                                    .iter()
                                    .filter_map(|(name, metric)| {
                                        metric.as_f64().map(|value| (name.clone(), value))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        bounds: entry
                            .get("bounds")
                            .and_then(|section| section.as_object())
                            .map(|object| {
                                object
                                    .iter()
                                    .filter_map(|(name, metric)| {
                                        metric.as_f64().map(|value| (name.clone(), value))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Baseline {
        metrics: table("metrics"),
        tolerances: table("tolerances"),
        bounds: table("bounds"),
        history,
    })
}

/// Replay the history log onto an empty map. A hand edit to `metrics`
/// without a matching history entry fails this check — the file proves
/// every value went through the reasoned-update command. The latest entry
/// must also carry the live policy, so hand-loosened tolerances fail too.
#[must_use]
pub fn baseline_replays(baseline: &Baseline) -> bool {
    let mut replayed: HashMap<String, f64> = HashMap::new();
    for entry in &baseline.history {
        if entry.reason.trim().is_empty() || entry.date.trim().is_empty() {
            return false;
        }
        for (name, (old, new)) in &entry.changes {
            if replayed.get(name) != old.as_ref() {
                return false;
            }
            replayed.insert(name.clone(), *new);
        }
    }
    replayed == baseline.metrics
        && baseline.history.last().is_none_or(|last| {
            last.tolerances == baseline.tolerances && last.bounds == baseline.bounds
        })
}

/// Why a baseline update was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineUpdateError {
    /// Updates require a recorded human reason.
    EmptyReason,
    /// A stable metric moved adversely beyond tolerance.
    Regression { metric: String },
    /// A tolerance override failed to parse (`name=value` expected).
    BadTolerance(String),
}

impl std::fmt::Display for BaselineUpdateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyReason => write!(formatter, "baseline update needs --reason"),
            Self::Regression { metric } => write!(
                formatter,
                "baseline would regress {metric}: pass --allow-regression to record it explicitly"
            ),
            Self::BadTolerance(spec) => write!(formatter, "bad tolerance override: {spec:?}"),
        }
    }
}

impl std::error::Error for BaselineUpdateError {}

/// Parse one `name=value` tolerance override.
fn parse_tolerance_override(spec: &str) -> Result<(String, f64), BaselineUpdateError> {
    let (name, value) = spec
        .split_once('=')
        .ok_or_else(|| BaselineUpdateError::BadTolerance(spec.to_string()))?;
    let value: f64 = value
        .parse()
        .map_err(|_| BaselineUpdateError::BadTolerance(spec.to_string()))?;
    if !value.is_finite() || value < 0.0 {
        return Err(BaselineUpdateError::BadTolerance(spec.to_string()));
    }
    Ok((name.to_string(), value))
}

/// Build the next baseline from a fresh measurement. Fails without a
/// reason, and fails on an adverse move beyond tolerance unless the
/// regression is explicitly allowed — the improving agent may never edit
/// the file to make a number pass. Allowed regressions, new metrics, and
/// tolerance overrides are all recorded in the history entry.
pub fn apply_baseline_update(
    current: Option<&Baseline>,
    measured: &[Measured],
    tolerances: &[String],
    bounds: &[String],
    reason: &str,
    allow_regression: bool,
    date: &str,
) -> Result<Baseline, BaselineUpdateError> {
    if reason.trim().is_empty() {
        return Err(BaselineUpdateError::EmptyReason);
    }
    let mut overrides = HashMap::new();
    for spec in tolerances {
        let (name, value) = parse_tolerance_override(spec)?;
        overrides.insert(name, value);
    }
    let mut bound_overrides = HashMap::new();
    for spec in bounds {
        let (name, value) = parse_tolerance_override(spec)?;
        bound_overrides.insert(name, value);
    }
    let previous = current.cloned().unwrap_or_default();
    let mut metrics = previous.metrics.clone();
    let mut policy_tolerances = previous.tolerances.clone();
    let mut policy_bounds = previous.bounds.clone();
    let mut history = previous.history.clone();
    // Genesis: values that predate the history discipline enter the log
    // first, so every current value replays from an explicit entry.
    if history.is_empty() && !metrics.is_empty() {
        history.push(HistoryEntry {
            date: date.to_string(),
            reason: "initial capture (pre-history baseline)".to_string(),
            changes: metrics
                .iter()
                .map(|(name, value)| (name.clone(), (None, *value)))
                .collect(),
            tolerances: policy_tolerances.clone(),
            bounds: policy_bounds.clone(),
        });
    }
    let mut changes: HashMap<String, (Option<f64>, f64)> = HashMap::new();
    for metric in measured {
        if let Some((name, value)) = overrides.get_key_value(&metric.name) {
            policy_tolerances.insert(name.clone(), *value);
        }
        if let Some((name, value)) = bound_overrides.get_key_value(&metric.name) {
            policy_bounds.insert(name.clone(), *value);
        }
        let old = metrics.get(&metric.name).copied();
        let tol = policy_tolerances.get(&metric.name).copied().unwrap_or(0.0);
        if let Some(old) = old {
            let regressed = if metric.higher_is_better {
                metric.value + tol < old
            } else {
                metric.value > old + tol
            };
            if regressed && !allow_regression {
                return Err(BaselineUpdateError::Regression {
                    metric: metric.name.clone(),
                });
            }
            if (metric.value - old).abs() > f64::EPSILON {
                changes.insert(metric.name.clone(), (Some(old), metric.value));
            }
        } else {
            changes.insert(metric.name.clone(), (None, metric.value));
        }
        metrics.insert(metric.name.clone(), metric.value);
    }
    let mut next = Baseline {
        metrics,
        tolerances: policy_tolerances,
        bounds: policy_bounds,
        history,
    };
    next.history.push(HistoryEntry {
        date: date.to_string(),
        reason: reason.to_string(),
        changes,
        tolerances: next.tolerances.clone(),
        bounds: next.bounds.clone(),
    });
    Ok(next)
}

/// Serialize a baseline for the checked-in file (reason, date, metrics,
/// policy, and the tamper-evident history).
#[must_use]
pub fn baseline_to_value(baseline: &Baseline, reason: &str, date: &str) -> serde_json::Value {
    serde_json::json!({
        "reason": reason,
        "date": date,
        "tolerance_source": "repeat-run variance (3 fast-tier runs, max spread <1%); deterministic metrics tol 0, timing 10% of value, ceilings 2x value; bootstrap pending long-run calibration",
        "metrics": baseline.metrics,
        "tolerances": baseline.tolerances,
        "bounds": baseline.bounds,
        "history": baseline.history,
    })
}

/// Today's UTC date (`YYYY-MM-DD`) for history entries.
#[must_use]
pub fn today_utc() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() / 86_400);
    let (year, month, day) = days_to_civil(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since the Unix epoch to a civil date (Hinnant's algorithm).
fn days_to_civil(days: u64) -> (i64, u32, u32) {
    let shifted = days as i64 + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month + 2) / 5 + 1;
    let month = if month < 10 { month + 3 } else { month - 9 };
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

// ── Fast-tier measurement (all hermetic, all in-process) ──────────────

static TIER_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Scratch dir under the process temp dir (unique per call, removed by the
/// caller best-effort). Staging never touches the fixture sources.
fn scratch_dir(prefix: &str) -> PathBuf {
    let id = TIER_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "guidance-tier-{prefix}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixture_graph")
}

/// Parse every fixture source into member docs (sidecars to scratch).
fn stage_fixture_docs() -> (PathBuf, Vec<fluent_types::GuidanceDoc>) {
    let root = fixture_root();
    let guidance = scratch_dir("docs");
    let mut engine = SyncEngine::new(guidance.clone(), root.clone());
    let mut names: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("fixture dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"))
        .collect();
    names.sort();
    let mut docs = Vec::new();
    for file in &names {
        docs.push((file.clone(), engine.gen(file).expect("fixture parses")));
    }
    let _ = std::fs::remove_dir_all(&guidance);
    (root, docs.into_iter().map(|(_, doc)| doc).collect())
}

struct NavQuery {
    id: String,
    query: String,
    files: Vec<String>,
    symbols: Vec<String>,
    gap: bool,
    split: String,
}

fn load_nav_queries() -> Vec<NavQuery> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpora/nav_seed.json");
    let text = std::fs::read_to_string(&path).expect("nav corpus reads");
    let value: serde_json::Value = serde_json::from_str(&text).expect("nav corpus parses");
    value["queries"]
        .as_array()
        .expect("queries")
        .iter()
        .map(|entry| NavQuery {
            id: entry["id"].as_str().expect("id").to_string(),
            query: entry["query"].as_str().expect("query").to_string(),
            files: entry["expected_files"]
                .as_array()
                .map(|files| {
                    files
                        .iter()
                        .filter_map(|file| file.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            symbols: entry["expected_symbols"]
                .as_array()
                .map(|symbols| {
                    symbols
                        .iter()
                        .filter_map(|symbol| symbol.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            gap: entry["expect_gap"].as_bool().unwrap_or(false),
            split: entry["split"].as_str().expect("split").to_string(),
        })
        .collect()
}

fn basename_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
}

/// Explain one query over every fixture doc, stages concatenated in doc order.
fn explain_all(
    engine: &QueryEngine,
    docs: &[fluent_types::GuidanceDoc],
    query: &str,
) -> Vec<crate::query::synthesize::Stage> {
    let mut stages = Vec::new();
    for doc in docs {
        if let Ok(mut found) = engine.explain(query, doc) {
            stages.append(&mut found);
        }
    }
    stages
}

fn hit_rank(
    stages: &[crate::query::synthesize::Stage],
    files: &[String],
    symbols: &[String],
) -> Option<usize> {
    stages
        .iter()
        .position(|stage| {
            symbols
                .iter()
                .any(|symbol| stage.member_name.as_deref() == Some(symbol.as_str()))
                || files.iter().any(|file| basename_of(&stage.source) == *file)
        })
        .map(|index| index + 1)
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        1.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

/// Navigation tiers over the checked-in query corpus (tune + heldout).
fn measure_navigation(docs: &[fluent_types::GuidanceDoc], queries: &[NavQuery]) -> Vec<Measured> {
    const NAV_REPRO: &str = "cargo test -p guidance-core --lib synthesis_gate_wiring";
    let engine = QueryEngine::new();
    let mut out = Vec::new();
    for split in ["tune", "heldout"] {
        let mut recalls = Vec::new();
        let mut inverses = Vec::new();
        let mut gaps = Vec::new();
        for query in queries.iter().filter(|query| query.split == split) {
            let stages = explain_all(&engine, docs, &query.query);
            if query.gap {
                gaps.push(f64::from(stages.is_empty()));
                continue;
            }
            if let Some(rank) = hit_rank(&stages, &query.files, &query.symbols) {
                recalls.push(f64::from(rank <= 3));
                inverses.push(1.0 / rank as f64);
            } else {
                recalls.push(0.0);
                inverses.push(0.0);
            }
        }
        out.push(Measured {
            name: format!("nav.recall_at_3.{split}"),
            value: mean(&recalls),
            repro: NAV_REPRO.to_string(),
            higher_is_better: true,
        });
        out.push(Measured {
            name: format!("nav.mrr.{split}"),
            value: mean(&inverses),
            repro: NAV_REPRO.to_string(),
            higher_is_better: true,
        });
        out.push(Measured {
            name: format!("nav.gap_correct.{split}"),
            value: mean(&gaps),
            repro: NAV_REPRO.to_string(),
            higher_is_better: true,
        });
    }
    // Citation validity over every non-gap answer: code excerpts carry
    // source + line, reusing the synthesis gate itself as the checker.
    let mut code = 0usize;
    let mut grounded = 0usize;
    for query in queries.iter().filter(|query| !query.gap) {
        for stage in explain_all(&engine, docs, &query.query) {
            if stage.kind == fluent_types::StageKind::Code {
                code += 1;
                if can_synthesize(std::slice::from_ref(&stage)) {
                    grounded += 1;
                }
            }
        }
    }
    out.push(Measured {
        name: "nav.citation_valid".to_string(),
        value: if code == 0 {
            1.0
        } else {
            grounded as f64 / code as f64
        },
        repro: NAV_REPRO.to_string(),
        higher_is_better: true,
    });
    out
}

/// Graph tiers over the same fixture + ground truth as the calibration
/// gate (reporting twin: same files, same verdicts, no shared code).
fn measure_graph() -> Vec<Measured> {
    const GRAPH_REPRO: &str = "cargo test -p guidance-core --lib graph_accuracy";
    let root = fixture_root();
    let root_str = root.to_string_lossy().into_owned();
    let mut names: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("fixture dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"))
        .collect();
    names.sort();
    let inputs: Vec<HarvestInput> = names
        .iter()
        .map(|file| HarvestInput {
            path: file.to_string_lossy().into_owned(),
            format: "rust".to_string(),
            text: std::fs::read_to_string(file).expect("fixture reads"),
        })
        .collect();
    let graph = GraphIndex::build(&inputs, std::slice::from_ref(&root_str)).expect("build");
    let truth: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("ground_truth.json")).expect("truth reads"),
    )
    .expect("truth parses");
    let relative = |absolute: &str| {
        absolute
            .strip_prefix(&format!("{root_str}/"))
            .unwrap_or(absolute)
            .to_string()
    };
    let mut actual_calls: Vec<(String, String, String, Vec<String>)> = graph
        .call_edges()
        .iter()
        .map(|edge| {
            let mut files: Vec<String> = edge
                .callee_files
                .iter()
                .map(|file| relative(file))
                .collect();
            files.sort();
            (
                relative(&edge.caller_file),
                edge.caller_symbol.clone().unwrap_or_default(),
                edge.callee.clone(),
                files,
            )
        })
        .collect();
    actual_calls.sort();
    let mut actual_imports: Vec<(String, String, Option<String>)> = graph
        .import_edges()
        .iter()
        .map(|edge| {
            (
                relative(&edge.importer),
                edge.specifier.clone(),
                edge.resolved_file.as_deref().map(&relative),
            )
        })
        .collect();
    actual_imports.sort();
    let expected_calls: Vec<(String, String, String, Vec<String>)> = truth["call_edges"]
        .as_array()
        .expect("call_edges")
        .iter()
        .map(|entry| {
            let mut files: Vec<String> = entry["callee_files"]
                .as_array()
                .expect("files")
                .iter()
                .filter_map(|file| file.as_str().map(str::to_string))
                .collect();
            files.sort();
            (
                entry["caller"].as_str().expect("str").to_string(),
                entry["caller_symbol"].as_str().expect("str").to_string(),
                entry["callee"].as_str().expect("str").to_string(),
                files,
            )
        })
        .collect();
    let expected_imports: Vec<(String, String, Option<String>)> = truth["import_edges"]
        .as_array()
        .expect("import_edges")
        .iter()
        .map(|entry| {
            (
                entry["importer"].as_str().expect("str").to_string(),
                entry["specifier"].as_str().expect("str").to_string(),
                entry["resolved"].as_str().map(str::to_string),
            )
        })
        .collect();
    let rate = |hit: usize, total: usize| {
        if total == 0 {
            1.0
        } else {
            hit as f64 / total as f64
        }
    };
    let call_hit = actual_calls
        .iter()
        .filter(|actual| expected_calls.contains(actual))
        .count();
    let import_hit = actual_imports
        .iter()
        .filter(|actual| expected_imports.contains(actual))
        .count();
    let no_caller_errors = truth["symbols_with_callers"]
        .as_object()
        .expect("callers")
        .iter()
        .filter(|(symbol, callers)| {
            !callers.as_array().expect("array").is_empty()
                && !graph
                    .call_edges()
                    .iter()
                    .any(|edge| edge.callee == **symbol && !edge.callee_files.is_empty())
        })
        .count();
    vec![
        Measured {
            name: "graph.call_precision".to_string(),
            value: rate(call_hit, actual_calls.len()),
            repro: GRAPH_REPRO.to_string(),
            higher_is_better: true,
        },
        Measured {
            name: "graph.call_recall".to_string(),
            value: rate(call_hit, expected_calls.len()),
            repro: GRAPH_REPRO.to_string(),
            higher_is_better: true,
        },
        Measured {
            name: "graph.import_precision".to_string(),
            value: rate(import_hit, actual_imports.len()),
            repro: GRAPH_REPRO.to_string(),
            higher_is_better: true,
        },
        Measured {
            name: "graph.import_recall".to_string(),
            value: rate(import_hit, expected_imports.len()),
            repro: GRAPH_REPRO.to_string(),
            higher_is_better: true,
        },
        Measured {
            name: "graph.no_caller_ok".to_string(),
            value: f64::from(no_caller_errors == 0),
            repro: GRAPH_REPRO.to_string(),
            higher_is_better: true,
        },
    ]
}

/// Freshness tiers: clean second pass skips, comment-write second pass
/// skips, whitespace touch skips while a content edit fires.
fn measure_freshness() -> Vec<Measured> {
    // Clean fixpoint (mtime only, no stored hashes).
    let dir = scratch_dir("fixpoint");
    let src = dir.join("src");
    std::fs::create_dir(&src).expect("mkdir");
    let file = src.join("clean.rs");
    std::fs::write(&file, "pub fn clean() {}\n").expect("write");
    let guidance = dir.join(".guidance");
    let mut engine = SyncEngine::new(guidance, src.clone());
    let first = engine.gen_if_stale(&file).expect("gen");
    let second = engine.gen_if_stale(&file).expect("gate");
    let clean_skip = f64::from(first && !second);
    // Comment-write seam (stored-hash path, Python `#` trigger).
    let py = src.join("noted.py");
    let v1 = "# Human note.\ndef noted():\n    return 1\n";
    std::fs::write(&py, v1).expect("write");
    let absolute = crate::sync_engine::absolute_or(py.clone())
        .to_string_lossy()
        .into_owned();
    let hashes =
        std::collections::HashMap::from([(absolute, common_core::hash::sha256_hex(v1.as_bytes()))]);
    let mut gated = SyncEngine::new(dir.join(".guidance"), src.clone()).with_source_hashes(hashes);
    let ran = gated.gen_if_stale(&py).expect("gate");
    let again = gated.gen_if_stale(&py).expect("gate");
    let comment_skips = f64::from(ran && !again);
    // Clock behavior inside the resolution window.
    let touched = src.join("touched.rs");
    std::fs::write(&touched, "pub fn touched() -> u64 { 1 }\n").expect("write");
    let absolute = crate::sync_engine::absolute_or(touched.clone())
        .to_string_lossy()
        .into_owned();
    let hashes = std::collections::HashMap::from([(
        absolute,
        common_core::hash::sha256_hex("pub fn touched() -> u64 { 1 }\n".as_bytes()),
    )]);
    let mut clock = SyncEngine::new(dir.join(".guidance"), src.clone()).with_source_hashes(hashes);
    clock.gen_if_stale(&touched).expect("prime");
    let bytes = std::fs::read(&touched).expect("read");
    std::fs::write(&touched, &bytes).expect("touch");
    let whitespace_skips = !clock.gen_if_stale(&touched).expect("gate");
    std::fs::write(&touched, "pub fn touched() -> u64 { 2 }\n").expect("edit");
    let edit_fires = clock.gen_if_stale(&touched).expect("gate");
    let _ = std::fs::remove_dir_all(&dir);
    vec![
        Measured {
            name: "fixpoint.clean_skip".to_string(),
            value: clean_skip,
            repro: "cargo test -p guidance-core --lib offload_stale_only".to_string(),
            higher_is_better: true,
        },
        Measured {
            name: "fixpoint.comment_write_skips".to_string(),
            value: comment_skips,
            repro: "cargo test -p guidance-core --lib sync_fixpoint".to_string(),
            higher_is_better: true,
        },
        Measured {
            name: "mutation.clock_correct".to_string(),
            value: f64::from(whitespace_skips && edit_fires),
            repro: "cargo test -p guidance-core --lib staleness".to_string(),
            higher_is_better: true,
        },
    ]
}

fn percentile(sorted: &[f64], pct: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (pct / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

/// Speed tier: microsecond p50/p99 of one explain sweep over the fixture.
fn measure_speed(docs: &[fluent_types::GuidanceDoc]) -> Vec<Measured> {
    const SPEED_REPRO: &str = "make guidance-scorecard-deep";
    let engine = QueryEngine::new();
    let mut samples = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        for doc in docs {
            let _ = engine.explain("dup", doc);
        }
        samples.push(start.elapsed().as_secs_f64() * 1e6);
    }
    samples.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    vec![
        Measured {
            name: "speed.explain_p50_us".to_string(),
            value: percentile(&samples, 50.0),
            repro: SPEED_REPRO.to_string(),
            higher_is_better: false,
        },
        Measured {
            name: "speed.explain_p99_us".to_string(),
            value: percentile(&samples, 99.0),
            repro: SPEED_REPRO.to_string(),
            higher_is_better: false,
        },
    ]
}

fn scripted_decision(label: &str, top: f64, degraded: bool) -> fluent_llm::decision::Decision {
    let mut decision = fluent_llm::decision::Decision::from_distribution(
        label,
        vec![(label.to_string(), top), ("other".to_string(), 1.0 - top)],
        "test".to_string(),
        1,
        0,
        false,
    )
    .expect("decision");
    decision.degraded = degraded;
    decision
}

async fn rung_once(
    metrics: &DecisionMetrics,
    cache: &fluent_llm::decision::DecisionCache,
    rule: Option<fluent_llm::decision::Decision>,
    key: &str,
    model: Option<fluent_llm::decision::Decision>,
) {
    let breaker = fluent_llm::decision::DecisionBreaker::new(3, 60_000).expect("breaker");
    let fallback = scripted_decision("FALLBACK", 1.0, false);
    let _ = evaluate_with_metrics(
        metrics,
        rule,
        cache,
        key.to_string(),
        true,
        &breaker,
        || async move {
            model
                .clone()
                .ok_or(fluent_llm::decision::DecisionError::Unavailable(
                    "down".to_string(),
                ))
        },
        fallback,
        None,
    )
    .await;
}

/// Decision tier: every ladder rung records exactly its counter.
async fn measure_decision() -> Vec<Measured> {
    const DECISION_REPRO: &str = "cargo test -p guidance-core --lib decision_metrics";
    let metrics = DecisionMetrics::new();
    let cache = fluent_llm::decision::DecisionCache::new(4).expect("cache");
    cache.insert("k".to_string(), scripted_decision("CACHED", 1.0, false));
    rung_once(
        &metrics,
        &cache,
        Some(scripted_decision("RULE", 1.0, false)),
        "k1",
        None,
    )
    .await;
    rung_once(&metrics, &cache, None, "k", None).await;
    rung_once(
        &metrics,
        &cache,
        None,
        "k2",
        Some(scripted_decision("MODEL", 0.9, false)),
    )
    .await;
    rung_once(&metrics, &cache, None, "k3", None).await;
    let degraded = scripted_decision("MODEL", 0.6, true);
    rung_once(&metrics, &cache, None, "k4", Some(degraded)).await;
    let snapshot = metrics.snapshot();
    let rung_ok = f64::from(
        snapshot.rule_wins == 1
            && snapshot.cache_hits == 1
            && snapshot.model_answers == 1
            && snapshot.fallbacks == 1
            && snapshot.drops == 1
            && snapshot.latency_samples == 5,
    );
    metrics.record_shadow(true);
    metrics.record_shadow(false);
    let shadow_ok = f64::from(metrics.snapshot().shadow_disagreements == 1);
    vec![
        Measured {
            name: "decision.rung_ok".to_string(),
            value: rung_ok,
            repro: DECISION_REPRO.to_string(),
            higher_is_better: true,
        },
        Measured {
            name: "decision.shadow_ok".to_string(),
            value: shadow_ok,
            repro: DECISION_REPRO.to_string(),
            higher_is_better: true,
        },
    ]
}

/// Lane tier: a saturated interactive lane defers (never fails) background.
async fn measure_lanes() -> Vec<Measured> {
    let gate = LaneGate::new(LaneConfig {
        interactive_in_flight: 1,
        background_in_flight: 1,
        interactive_timeout_ms: 5_000,
        background_timeout_ms: 5_000,
    });
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let holder = async {
        gate.admit(Lane::Interactive, || async move {
            let _ = started_tx.send(());
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            "interactive"
        })
        .await
    };
    let background = async {
        started_rx.await.expect("holder owns the lane");
        let start = Instant::now();
        let outcome = gate
            .admit(Lane::Background, || async { "background" })
            .await;
        (outcome, start.elapsed())
    };
    let (held, (background, deferred)) = tokio::join!(holder, background);
    let ok = held.is_ok() && background.is_ok() && deferred >= std::time::Duration::from_millis(30);
    vec![Measured {
        name: "lane.defer_ok".to_string(),
        value: f64::from(ok),
        repro: "cargo test -p guidance-core --lib lane_priority".to_string(),
        higher_is_better: true,
    }]
}

/// Fast tier: every hermetic measurement, in-process, no model.
pub async fn run_fast_tier() -> Vec<Measured> {
    let (_root, docs) = stage_fixture_docs();
    let queries = load_nav_queries();
    let mut measured = measure_navigation(&docs, &queries);
    measured.extend(measure_graph());
    measured.extend(measure_freshness());
    measured.extend(measure_speed(&docs));
    measured.extend(measure_decision().await);
    measured.extend(measure_lanes().await);
    measured
}

/// Deep tier extra: per-query hit lines for drill-down.
#[must_use]
pub fn run_deep_extra() -> Vec<Measured> {
    let (_root, docs) = stage_fixture_docs();
    let queries = load_nav_queries();
    let engine = QueryEngine::new();
    queries
        .iter()
        .map(|query| {
            let stages = explain_all(&engine, &docs, &query.query);
            let hit = if query.gap {
                stages.is_empty()
            } else {
                hit_rank(&stages, &query.files, &query.symbols).is_some_and(|rank| rank <= 3)
            };
            Measured {
                name: format!("deep.nav.hit.{}", query.id),
                value: f64::from(hit),
                repro: "cargo test -p guidance-core --lib synthesis_gate_wiring".to_string(),
                higher_is_better: true,
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/scorecard.rs"]
mod scorecard_tests;

#[cfg(test)]
#[path = "../tests/scorecard_ratchet.rs"]
mod scorecard_ratchet_tests;
