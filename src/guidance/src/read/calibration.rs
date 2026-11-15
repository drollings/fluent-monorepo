//! Task-value calibration for the read plane.
//!
//! Measurement, bars, and registry entries for the intent, finding,
//! support, and capability points. Nothing here filters, reorders, or
//! promotes: gates report pass/fail, entries store only on pass, and
//! failures stay `HELD`. Producer confidence (`Decision.confidence`,
//! judge scores, similarity) is logged and ordered but never gates.

use crate::eval::EvalTask;
use crate::query::strategy::QueryIntent;

/// Finding-relevance point (native `choice`: relevant / irrelevant /
/// insufficient). Matches the `finding_judge_shadow` record point.
pub const FINDING_POINT: &str = "finding";

/// Claim-support point (supported / contradicted / insufficient).
pub const SUPPORT_POINT: &str = "support";

/// Question versions for the two new points.
pub const FINDING_QUESTION_VERSION: u64 = 1;
pub const SUPPORT_QUESTION_VERSION: u64 = 1;

/// Task-value bars for one scored gate: precision floor, FPR ceiling,
/// recall floor, and zero control fires (enforced separately — a
/// control firing vetoes promotion even when the sweep is clean).
#[derive(Debug, Clone, Copy, PartialEq)]
/// Task-value bars for one scored gate: precision floor, FPR ceiling,
/// recall floor, and zero control fires (enforced separately — a
/// control firing vetoes promotion even when the sweep is clean).
pub struct TaskBars {
    pub min_precision: f64,
    pub max_fpr: f64,
    pub min_recall: f64,
}

/// Initial finding-relevance bars: precision ≥ 0.95, recall ≥ 0.85,
/// FPR ≤ 0.05, zero control fires. Changes need a recorded reason.
pub const FINDING_BARS: TaskBars = TaskBars {
    min_precision: 0.95,
    max_fpr: 0.05,
    min_recall: 0.85,
};

/// Initial claim-support bars: precision ≥ 0.95, recall ≥ 0.90,
/// FPR ≤ 0.05, zero control fires. Changes need a recorded reason.
pub const SUPPORT_BARS: TaskBars = TaskBars {
    min_precision: 0.95,
    max_fpr: 0.05,
    min_recall: 0.90,
};

/// Intent class bar: a predicted rule class below this precision
/// routes to `General` with a coverage gap instead of its plane.
pub const INTENT_MIN_CLASS_PRECISION: f64 = 0.90;

/// Minimum support for demotion: a class with fewer predictions stays
/// on its plane (a single sample cannot demote a route).
pub const INTENT_MIN_CLASS_SUPPORT: usize = 2;

/// Whether one rule-table prediction counts for a golden eval intent.
/// Ranking rows are answered by mechanism recall or the general
/// fallback, so `HowTo`, `Conceptual`, and `GeneralSearch` all count.
#[must_use]
/// Whether one rule-table prediction counts for a golden eval intent.
/// Ranking rows are answered by mechanism recall or the general
/// fallback, so `HowTo`, `Conceptual`, and `GeneralSearch` all count.
pub fn golden_intent_matches(predicted: QueryIntent, golden: &str) -> bool {
    match golden {
        "orientation" => predicted == QueryIntent::Orientation,
        "ownership" => predicted == QueryIntent::Ownership,
        "graph" => predicted == QueryIntent::Graph,
        "mechanism" => matches!(predicted, QueryIntent::HowTo | QueryIntent::Conceptual),
        "provenance" => matches!(
            predicted,
            QueryIntent::Provenance | QueryIntent::CapabilityQuery
        ),
        "ranking" => matches!(
            predicted,
            QueryIntent::HowTo | QueryIntent::Conceptual | QueryIntent::GeneralSearch
        ),
        "general" => predicted == QueryIntent::GeneralSearch,
        _ => false,
    }
}

/// Per-class score for one predicted rule class.
#[derive(Debug, Clone, Copy, PartialEq)]
/// Per-class score for one predicted rule class.
pub struct IntentClassScore {
    /// Predicted rule class.
    pub intent: QueryIntent,
    /// Times the table predicted this class (non-control tasks).
    pub predicted: usize,
    /// Predicted correctly (golden matches).
    pub correct: usize,
    /// Golden tasks mapping to this class.
    pub golden: usize,
}

impl IntentClassScore {
    /// Precision: correct / predicted (1.0 when never predicted —
    /// an unused class cannot fail its bar).
    #[must_use]
    pub fn precision(&self) -> f64 {
        if self.predicted == 0 {
            1.0
        } else {
            self.correct as f64 / self.predicted as f64
        }
    }

    /// Recall: correct / golden (1.0 when no golden maps here).
    #[must_use]
    pub fn recall(&self) -> f64 {
        if self.golden == 0 {
            1.0
        } else {
            self.correct as f64 / self.golden as f64
        }
    }
}

/// Deterministic intent measurement over the eval corpus: per-class
/// precision/recall plus overall accuracy. Pure: no I/O, no model —
/// the table never calls one, so `model_calls` is always zero and the
/// exact symbol/path controls are structurally unable to reach a
/// backend. Controls are excluded from accuracy (they must stay
/// silent, which is a retrieval property, not a routing label).
#[derive(Debug, Clone, PartialEq)]
/// Deterministic intent measurement over the eval corpus: per-class
/// precision/recall plus overall accuracy. Pure: no I/O, no model —
/// the table never calls one, so `model_calls` is always zero and the
/// exact symbol/path controls are structurally unable to reach a
/// backend. Controls are excluded from accuracy (they must stay
/// silent, which is a retrieval property, not a routing label).
pub struct IntentMeasurement {
    /// Non-control tasks evaluated.
    pub evaluated: usize,
    /// Predictions matching golden.
    pub correct: usize,
    /// Routed to the `General` fallback (the ambiguous share — the
    /// only queries a decision model may ever see).
    pub ambiguous: usize,
    /// Model calls made by the table (always zero).
    pub model_calls: u64,
    /// One score per predicted class, intent order.
    pub classes: Vec<IntentClassScore>,
}

impl IntentMeasurement {
    /// Overall accuracy over non-control tasks.
    #[must_use]
    pub fn accuracy(&self) -> f64 {
        if self.evaluated == 0 {
            1.0
        } else {
            self.correct as f64 / self.evaluated as f64
        }
    }
}

/// Measure the rule table on eval tasks. Controls are skipped for
/// accuracy but counted nowhere else — silence is measured at the
/// retrieval gate, not here.
#[must_use]
/// Measure the rule table on eval tasks. Controls are skipped for
/// accuracy but counted nowhere else — silence is measured at the
/// retrieval gate, not here.
pub fn measure_intent_rules(tasks: &[EvalTask]) -> IntentMeasurement {
    use QueryIntent as Q;
    let order = [
        Q::SingleIdentifier,
        Q::CapabilityQuery,
        Q::FilePath,
        Q::HowTo,
        Q::Conceptual,
        Q::MultiKeyword,
        Q::GeneralSearch,
        Q::Graph,
        Q::Ownership,
        Q::Orientation,
        Q::Provenance,
    ];
    let mut classes: Vec<IntentClassScore> = order
        .iter()
        .map(|intent| IntentClassScore {
            intent: *intent,
            predicted: 0,
            correct: 0,
            golden: 0,
        })
        .collect();
    let mut evaluated = 0usize;
    let mut correct = 0usize;
    let mut ambiguous = 0usize;
    for task in tasks {
        if task.control {
            continue;
        }
        let predicted = crate::read::intent::classify(&task.query);
        if predicted == QueryIntent::GeneralSearch {
            ambiguous += 1;
        }
        evaluated += 1;
        let hit = golden_intent_matches(predicted, &task.intent);
        if hit {
            correct += 1;
        }
        if let Some(class) = classes.iter_mut().find(|class| class.intent == predicted) {
            class.predicted += 1;
            if hit {
                class.correct += 1;
            }
        }
        for class in &mut classes {
            if golden_intent_matches(class.intent, &task.intent) {
                class.golden += 1;
            }
        }
    }
    IntentMeasurement {
        evaluated,
        correct,
        ambiguous,
        model_calls: 0,
        classes,
    }
}

/// Predicted classes below the task-value bar with enough support to
/// judge: these route to `General` with a coverage gap instead of
/// their plane. Measured on the eval corpus (`measure_intent_rules`
/// over `doc/guidance/eval/questions.jsonl`): Ownership 0.667 (n=3),
/// Provenance 0.50 (n=2), HowTo 0.875 (n=8) — all below the 0.90 bar.
/// Graph and Orientation hold at 1.00. The `General` fallback is
/// excluded (it cannot demote; its precision is the ambiguity cost).
/// Order follows the class table; the pinning test recomputes this
/// from the corpus, so any corpus change forces re-examination
/// instead of silent drift.
pub const WEAK_INTENT_CLASSES: &[QueryIntent] = &[
    QueryIntent::HowTo,
    QueryIntent::Ownership,
    QueryIntent::Provenance,
];
#[must_use]
pub fn weak_intent_classes(measurement: &IntentMeasurement) -> Vec<QueryIntent> {
    measurement
        .classes
        .iter()
        .filter(|class| {
            // The fallback cannot demote: its precision is reported as
            // the ambiguity cost, never as a routing change.
            class.intent != QueryIntent::GeneralSearch
                && class.predicted >= INTENT_MIN_CLASS_SUPPORT
                && class.precision() < INTENT_MIN_CLASS_PRECISION
        })
        .map(|class| class.intent)
        .collect()
}

/// Route one table prediction through the measured bars: weak classes
/// fall back to `General`, everything else keeps its route.
#[must_use]
/// Route one table prediction through the measured bars: weak classes
/// fall back to `General`, everything else keeps its route.
pub fn demote_weak_intent(intent: QueryIntent, weak: &[QueryIntent]) -> QueryIntent {
    if weak.contains(&intent) {
        QueryIntent::GeneralSearch
    } else {
        intent
    }
}

/// Coverage gap for a demoted prediction (`None` when the table route
/// stands).
#[must_use]
/// Coverage gap for a demoted prediction (`None` when the table route
/// stands).
pub fn intent_coverage_gap(predicted: QueryIntent, weak: &[QueryIntent]) -> Option<String> {
    if weak.contains(&predicted) {
        Some(format!(
            "intent {predicted:?} below task-value bar: route kept"
        ))
    } else {
        None
    }
}

/// Whether one query may ever reach a decision model: exact
/// symbol/path evidence (and empty queries) resolve deterministically
/// and never call one. Only the ambiguous remainder is model-eligible.
/// Delegates to the single `read::intent::is_exact` rule.
#[must_use]
/// Whether one query may ever reach a decision model: exact
/// symbol/path evidence (and empty queries) resolve deterministically
/// and never call one. Only the ambiguous remainder is model-eligible.
/// Delegates to the single `read::intent::is_exact` rule.
pub fn model_callable(query: &str) -> bool {
    !crate::read::intent::is_exact(query)
}

/// One scored gate verdict: the shared sweep numbers plus the
/// task-value bars. `passed` requires every bar and zero control
/// fires; anything else stays `HELD` (nothing stored, nothing
/// promoted).
#[derive(Debug, Clone, Copy, PartialEq)]
/// One scored gate verdict: the shared sweep numbers plus the
/// task-value bars. `passed` requires every bar and zero control
/// fires; anything else stays `HELD` (nothing stored, nothing
/// promoted).
pub struct GateVerdict {
    pub precision: f64,
    pub recall: f64,
    pub fpr: f64,
    pub support: usize,
    pub control_fires: usize,
    pub passed: bool,
}

/// Gate scored cases against task-value bars at one threshold. Empty
/// corpora never pass (a never-firing gate proves nothing).
#[must_use]
/// Gate scored cases against task-value bars at one threshold. Empty
/// corpora never pass (a never-firing gate proves nothing).
pub fn gate_scored_cases(
    cases: &[fluent_llm::decision::ScoredCase],
    threshold: f64,
    bars: &TaskBars,
) -> GateVerdict {
    if cases.is_empty() {
        return GateVerdict {
            precision: 0.0,
            recall: 0.0,
            fpr: 0.0,
            support: 0,
            control_fires: 0,
            passed: false,
        };
    }
    let report = common_core::calibration::calibrate_threshold(
        cases,
        |case| case.score,
        |case| case.expected,
        threshold,
    );
    let fires = fluent_llm::decision::control_fires(cases, threshold);
    GateVerdict {
        precision: report.precision,
        recall: report.recall,
        fpr: report.fpr,
        support: report.support,
        control_fires: fires,
        passed: report.precision >= bars.min_precision
            && report.fpr <= bars.max_fpr
            && report.recall >= bars.min_recall
            && fires == 0,
    }
}

/// Judge-label score for finding relevance: `relevant` fires at the
/// 1.0 operating point, `insufficient` stays below it (gaps, never
/// evidence), `irrelevant` scores zero. Discrete by construction —
/// the gate measures judge accuracy, not a ranking sweep.
#[must_use]
/// Judge-label score for finding relevance: `relevant` fires at the
/// 1.0 operating point, `insufficient` stays below it (gaps, never
/// evidence), `irrelevant` scores zero. Discrete by construction —
/// the gate measures judge accuracy, not a ranking sweep.
pub fn finding_label_score(label: &crate::read::decide::FindingLabel) -> f64 {
    use crate::read::decide::FindingLabel;
    match label {
        FindingLabel::Relevant => 1.0,
        FindingLabel::Insufficient => 0.5,
        FindingLabel::Irrelevant => 0.0,
    }
}

/// Operating threshold for judge-label scores.
pub const FINDING_THRESHOLD: f64 = 1.0;

/// One hand-labelled finding: the task-value goldens for the finding
/// point. `expected_relevant` is the label; `control` rows must never
/// fire (a control firing vetoes promotion).
#[derive(Debug, Clone, PartialEq, Eq)]
/// One hand-labelled finding: the task-value goldens for the finding
/// point. `expected_relevant` is the label; `control` rows must never
/// fire (a control firing vetoes promotion).
pub struct LabeledFinding {
    /// Stable case id.
    pub id: String,
    /// Question the finding answers (or not).
    pub query: String,
    /// Finding under test (`source:excerpt` identity).
    pub finding_id: String,
    /// Excerpt text (empty on retrieval controls).
    pub excerpt: String,
    /// True when the finding answers the query.
    pub expected_relevant: bool,
    /// Control rows must never fire.
    pub control: bool,
}

/// Parse the findings eval corpus (one JSON object per line).
/// Unknown labels and missing ids/queries reject the file — a
/// mislabelled gold set must fail loudly, never calibrate quietly.
pub fn parse_labeled_findings(text: &str) -> Result<Vec<LabeledFinding>, String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|error| format!("line {}: {error}", index + 1))?;
        let id = value
            .get("id")
            .and_then(|id| id.as_str())
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            return Err(format!("line {}: missing `id`", index + 1));
        }
        if !seen.insert(id.clone()) {
            return Err(format!("line {}: duplicate id `{id}`", index + 1));
        }
        let query = value
            .get("query")
            .and_then(|query| query.as_str())
            .unwrap_or_default()
            .to_string();
        if query.trim().is_empty() {
            return Err(format!("line {}: missing `query`", index + 1));
        }
        let label = value
            .get("label")
            .and_then(|label| label.as_str())
            .unwrap_or_default();
        let expected_relevant = match label {
            "relevant" => true,
            "irrelevant" | "insufficient" => false,
            _ => return Err(format!("line {}: unknown label `{label}`", index + 1)),
        };
        out.push(LabeledFinding {
            id,
            query,
            finding_id: value
                .get("finding_id")
                .and_then(|finding| finding.as_str())
                .unwrap_or_default()
                .to_string(),
            excerpt: value
                .get("excerpt")
                .and_then(|excerpt| excerpt.as_str())
                .unwrap_or_default()
                .to_string(),
            expected_relevant,
            control: value
                .get("control")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        });
    }
    Ok(out)
}

/// Load the findings corpus from disk.
pub fn load_labeled_findings(path: &std::path::Path) -> Result<Vec<LabeledFinding>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("findings corpus read {}: {error}", path.display()))?;
    parse_labeled_findings(&raw)
}

/// Provenance for eval-derived scored cases: human-family labels from
/// the checked-in corpus, never model output.
fn eval_provenance() -> fluent_llm::decision::CaseProvenance {
    fluent_llm::decision::CaseProvenance::human("guidance-eval", crate::scorecard::today_utc())
}

/// Score labeled findings through one scorer (judge labels in
/// production calibration, the lexical proxy for harness checks).
/// Scores travel with expected/control flags into the shared gate.
#[must_use]
/// Score labeled findings through one scorer (judge labels in
/// production calibration, the lexical proxy for harness checks).
/// Scores travel with expected/control flags into the shared gate.
pub fn score_findings(
    rows: &[LabeledFinding],
    mut scorer: impl FnMut(&LabeledFinding) -> f64,
) -> Vec<fluent_llm::decision::ScoredCase> {
    let provenance = eval_provenance();
    rows.iter()
        .map(|row| fluent_llm::decision::ScoredCase {
            score: scorer(row),
            expected: row.expected_relevant,
            control: row.control,
            provenance: provenance.clone(),
        })
        .collect()
}

/// Deterministic lexical proxy: shared content-term overlap between
/// query and excerpt. A harness stand-in only — it never authorizes
/// (it measures below bars by construction on hard negatives), and
/// production calibration scores judge labels instead.
#[must_use]
/// Deterministic lexical proxy: shared content-term overlap between
/// query and excerpt. A harness stand-in only — it never authorizes
/// (it measures below bars by construction on hard negatives), and
/// production calibration scores judge labels instead.
pub fn proxy_finding_score(query: &str, excerpt: &str) -> f64 {
    if excerpt.trim().is_empty() {
        return 0.0;
    }
    let query_terms = crate::read::vocabulary::content_terms(query);
    if query_terms.is_empty() {
        return 0.0;
    }
    let excerpt_lower = excerpt.to_lowercase();
    let hits = query_terms
        .iter()
        .filter(|term| excerpt_lower.contains(term.as_str()))
        .count();
    hits as f64 / query_terms.len() as f64
}

/// Measure labeled findings through one scorer at the finding
/// operating point.
#[must_use]
/// Measure labeled findings through one scorer at the finding
/// operating point.
pub fn measure_findings(
    rows: &[LabeledFinding],
    scorer: impl FnMut(&LabeledFinding) -> f64,
) -> GateVerdict {
    let cases = score_findings(rows, scorer);
    gate_scored_cases(&cases, FINDING_THRESHOLD, &FINDING_BARS)
}

/// Build the registry corpus for a passing point: cases plus the
/// dataset hash that pins the measurement to its labels.
#[must_use]
/// Build the registry corpus for a passing point: cases plus the
/// dataset hash that pins the measurement to its labels.
pub fn finding_corpus(
    cases: Vec<fluent_llm::decision::ScoredCase>,
    model_fingerprint: &str,
    dataset_hash: &str,
    source: &str,
) -> fluent_llm::decision::Corpus {
    fluent_llm::decision::Corpus {
        point: FINDING_POINT.to_string(),
        model_fingerprint: model_fingerprint.to_string(),
        question_version: FINDING_QUESTION_VERSION,
        provenance: fluent_llm::decision::CorpusProvenance {
            source: source.to_string(),
            labeller: "guidance calibrate".to_string(),
            date: crate::scorecard::today_utc(),
            dataset: dataset_hash.to_string(),
        },
        cases,
    }
}

/// Store one registry entry on pass; report `HELD` without touching
/// the registry on fail. Measurement is never an error: the outcome
/// carries the verdict, and callers print it.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Store one registry entry on pass; report `HELD` without touching
/// the registry on fail. Measurement is never an error: the outcome
/// carries the verdict, and callers print it.
pub enum StoreOutcome {
    Authorized { path: String },
    Held { reason: String },
}

/// Upsert `entry` into the single calibration registry at
/// `registry_path` when `passed`; otherwise leave the registry
/// untouched. Parent directories are created as needed.
pub fn store_entry_if_passed(
    registry_path: &std::path::Path,
    entry: fluent_llm::decision::RegistryEntry,
    passed: bool,
    reason: &str,
) -> Result<StoreOutcome, String> {
    if !passed {
        return Ok(StoreOutcome::Held {
            reason: reason.to_string(),
        });
    }
    let mut registry = if registry_path.exists() {
        fluent_llm::decision::CalibrationRegistry::load(registry_path)?
    } else {
        fluent_llm::decision::CalibrationRegistry::empty()
    };
    registry.upsert(entry);
    registry.save(registry_path)?;
    Ok(StoreOutcome::Authorized {
        path: registry_path.display().to_string(),
    })
}

/// Expected support for one hand-labelled claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Expected support for one hand-labelled claim.
pub enum ExpectedSupport {
    Supported,
    Contradicted,
    Insufficient,
}

/// One hand-labelled claim: the task-value goldens for the support
/// point. Binding is structural (sentence names its citation);
/// support is the verdict a calibrated judge must return.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One hand-labelled claim: the task-value goldens for the support
/// point. Binding is structural (sentence names its citation);
/// support is the verdict a calibrated judge must return.
pub struct ClaimCase {
    /// Stable case id.
    pub id: String,
    /// Claim sentence as it would appear in an answer.
    pub sentence: String,
    /// Bound citation (`path:line[-line]`, empty when unbound).
    pub citation: String,
    /// Hand label.
    pub expected: ExpectedSupport,
    /// Control rows must never verify.
    pub control: bool,
}

/// Parse the claims eval corpus (one JSON object per line). Supported
/// rows must bind (non-empty citation named by the sentence) —
/// otherwise the corpus cannot measure binding; unknown labels,
/// missing ids, and empty sentences reject the file.
pub fn parse_claim_cases(text: &str) -> Result<Vec<ClaimCase>, String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|error| format!("line {}: {error}", index + 1))?;
        let id = value
            .get("id")
            .and_then(|id| id.as_str())
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            return Err(format!("line {}: missing `id`", index + 1));
        }
        if !seen.insert(id.clone()) {
            return Err(format!("line {}: duplicate id `{id}`", index + 1));
        }
        let sentence = value
            .get("sentence")
            .and_then(|sentence| sentence.as_str())
            .unwrap_or_default()
            .to_string();
        if sentence.trim().is_empty() {
            return Err(format!("line {}: missing `sentence`", index + 1));
        }
        let citation = value
            .get("citation")
            .and_then(|citation| citation.as_str())
            .unwrap_or_default()
            .to_string();
        let expected = match value
            .get("expected")
            .and_then(|expected| expected.as_str())
            .unwrap_or_default()
        {
            "supported" => ExpectedSupport::Supported,
            "contradicted" => ExpectedSupport::Contradicted,
            "insufficient" => ExpectedSupport::Insufficient,
            other => return Err(format!("line {}: unknown expected `{other}`", index + 1)),
        };
        if expected == ExpectedSupport::Supported
            && (citation.is_empty() || !sentence.contains(citation.as_str()))
        {
            return Err(format!(
                "line {}: supported claims must bind their citation",
                index + 1
            ));
        }
        if !citation.is_empty() && !sentence.contains(citation.as_str()) {
            return Err(format!(
                "line {}: citation is not named by the sentence",
                index + 1
            ));
        }
        out.push(ClaimCase {
            id,
            sentence,
            citation,
            expected,
            control: value
                .get("control")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        });
    }
    Ok(out)
}

/// Load the claims corpus from disk.
pub fn load_claim_cases(path: &std::path::Path) -> Result<Vec<ClaimCase>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("claims corpus read {}: {error}", path.display()))?;
    parse_claim_cases(&raw)
}

/// Judge-verdict score for claim support: only `Verified` fires at
/// the operating point. `Unverified` stays below it (labelled, never
/// settled); `Contradicted` and `Insufficient` score zero (dropped).
#[must_use]
/// Judge-verdict score for claim support: only `Verified` fires at
/// the operating point. `Unverified` stays below it (labelled, never
/// settled); `Contradicted` and `Insufficient` score zero (dropped).
pub fn support_verdict_score(support: &crate::read::claims::Support) -> f64 {
    use crate::read::claims::Support;
    match support {
        Support::Verified => 1.0,
        Support::Unverified => 0.5,
        Support::Contradicted | Support::Insufficient => 0.0,
    }
}

/// Operating threshold for support verdicts.
pub const SUPPORT_THRESHOLD: f64 = 1.0;

/// Binding accuracy over claim cases: fraction of sentences whose
/// structural binding is well-formed (a non-empty citation is named
/// by the sentence; empty citations bind nothing). Binding is
/// structural, never semantic — contradicted rows bind exactly like
/// supported ones, and support stays a verdict. Deterministic and
/// model-free: it reports a rate and never gates a registry entry.
#[must_use]
/// Binding accuracy over claim cases: fraction of sentences whose
/// structural binding is well-formed (a non-empty citation is named
/// by the sentence; empty citations bind nothing). Binding is
/// structural, never semantic — contradicted rows bind exactly like
/// supported ones, and support stays a verdict. Deterministic and
/// model-free: it reports a rate and never gates a registry entry.
pub fn binding_accuracy(cases: &[ClaimCase]) -> f64 {
    if cases.is_empty() {
        return 1.0;
    }
    let bound = cases
        .iter()
        .filter(|case| {
            let binds = !case.citation.is_empty() && case.sentence.contains(case.citation.as_str());
            binds != case.citation.is_empty()
        })
        .count();
    bound as f64 / cases.len() as f64
}

/// Score claim cases through one verdict scorer (judge verdicts in
/// production calibration, scripted scores in harness checks).
#[must_use]
/// Score claim cases through one verdict scorer (judge verdicts in
/// production calibration, scripted scores in harness checks).
pub fn score_claims(
    cases: &[ClaimCase],
    mut scorer: impl FnMut(&ClaimCase) -> f64,
) -> Vec<fluent_llm::decision::ScoredCase> {
    let provenance = eval_provenance();
    cases
        .iter()
        .map(|case| fluent_llm::decision::ScoredCase {
            score: scorer(case),
            expected: case.expected == ExpectedSupport::Supported,
            control: case.control,
            provenance: provenance.clone(),
        })
        .collect()
}

/// Measure claim cases through one scorer at the support operating
/// point.
#[must_use]
/// Measure claim cases through one scorer at the support operating
/// point.
pub fn measure_support(cases: &[ClaimCase], scorer: impl FnMut(&ClaimCase) -> f64) -> GateVerdict {
    gate_scored_cases(
        &score_claims(cases, scorer),
        SUPPORT_THRESHOLD,
        &SUPPORT_BARS,
    )
}

/// Build the registry corpus for the support point.
#[must_use]
/// Build the registry corpus for the support point.
pub fn support_corpus(
    cases: Vec<fluent_llm::decision::ScoredCase>,
    model_fingerprint: &str,
    dataset_hash: &str,
    source: &str,
) -> fluent_llm::decision::Corpus {
    fluent_llm::decision::Corpus {
        point: SUPPORT_POINT.to_string(),
        model_fingerprint: model_fingerprint.to_string(),
        question_version: SUPPORT_QUESTION_VERSION,
        provenance: fluent_llm::decision::CorpusProvenance {
            source: source.to_string(),
            labeller: "guidance calibrate".to_string(),
            date: crate::scorecard::today_utc(),
            dataset: dataset_hash.to_string(),
        },
        cases,
    }
}

#[cfg(test)]
#[path = "../../tests/calibration_intent.rs"]
mod calibration_intent_tests;

#[cfg(test)]
#[path = "../../tests/calibration_findings.rs"]
mod calibration_findings_tests;

#[cfg(test)]
#[path = "../../tests/calibration_support.rs"]
mod calibration_support_tests;

#[cfg(test)]
#[path = "../../tests/calibration_capability.rs"]
mod calibration_capability_tests;
