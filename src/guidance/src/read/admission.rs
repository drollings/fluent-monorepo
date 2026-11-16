//! Doc-admission measurement: the labelled corpus, the hermetic
//! scorer, and the deterministic subject-anchor report.
//!
//! Nothing here filters or promotes. The scorer reports precision,
//! recall, and control fires at the 1.0 operating point; the anchor
//! report measures MRR, golden retention, and the excluded-irrelevant
//! rate of the pure subject rule. Promotion stays an explicit human
//! `guidance calibrate` run; the free-term anchor additionally stays
//! shadow until its own bars pass.

use crate::eval::EvalTask;

/// CLI dispatch id for the doc-admission point (registry key
/// [`crate::read::calibration::ADMISSION_POINT`]).
pub const DOC_ADMISSION_POINT: &str = "dp.doc_admission";

/// CLI dispatch id for the deterministic subject-anchor report.
/// Never a registry entry: the anchor is measured, not authorized.
pub const SUBJECT_ANCHOR_POINT: &str = "dp.subject_anchor";

/// One hand-labelled admission row: a query plus one candidate
/// section and its relevance judgment. Controls name sections that
/// must never be admitted for the query.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One hand-labelled admission row: a query plus one candidate
/// section and its relevance judgment. Controls name sections that
/// must never be admitted for the query.
pub struct DocAdmissionRow {
    /// Stable row id.
    pub id: String,
    /// Question text.
    pub query: String,
    /// Candidate section (`path#anchor`).
    pub section_id: String,
    /// Section excerpt (judge input, human-written).
    pub excerpt: String,
    /// True when the section answers the query.
    pub relevant: bool,
    /// Tune or heldout split.
    pub split: String,
    /// Control rows must never be admitted.
    pub control: bool,
}

/// Parse the doc-admission eval corpus (one JSON object per line).
/// Missing ids/queries/sections and duplicate ids reject the file.
pub fn parse_doc_admission_rows(text: &str) -> Result<Vec<DocAdmissionRow>, String> {
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
        let section_id = value
            .get("section_id")
            .and_then(|section| section.as_str())
            .unwrap_or_default()
            .to_string();
        if section_id.trim().is_empty() {
            return Err(format!("line {}: missing `section_id`", index + 1));
        }
        let excerpt = value
            .get("excerpt")
            .and_then(|excerpt| excerpt.as_str())
            .unwrap_or_default()
            .to_string();
        out.push(DocAdmissionRow {
            id,
            query,
            section_id,
            excerpt,
            relevant: value
                .get("relevant")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            split: value
                .get("split")
                .and_then(|split| split.as_str())
                .unwrap_or("tune")
                .to_string(),
            control: value
                .get("control")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        });
    }
    Ok(out)
}

/// Load one doc-admission corpus file.
pub fn load_doc_admission_rows(path: &std::path::Path) -> Result<Vec<DocAdmissionRow>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("doc admission corpus read {}: {error}", path.display()))?;
    parse_doc_admission_rows(&raw)
}

/// Score rows at the 1.0 operating point: `score_of` maps a row to
/// its judge score (scripted or live). Expected fires exactly on
/// relevant rows; controls must never fire.
pub fn doc_admission_scores(
    rows: &[DocAdmissionRow],
    score_of: impl Fn(&DocAdmissionRow) -> f64,
) -> Vec<fluent_llm::decision::ScoredCase> {
    rows.iter()
        .map(|row| fluent_llm::decision::ScoredCase {
            score: score_of(row),
            expected: row.relevant,
            control: row.control,
            provenance: fluent_llm::decision::CaseProvenance::human("guidance-eval", "2026-11-16"),
        })
        .collect()
}

/// One anchor-measurement case: a query, its golden section texts,
/// and the distractor texts admission must cut.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One anchor-measurement case: a query, its golden section texts,
/// and the distractor texts admission must cut.
pub struct AnchorEvalCase {
    /// Question text.
    pub query: String,
    /// Golden section ids (must survive).
    pub golden: Vec<String>,
    /// Distractor section ids (should be cut).
    pub distractors: Vec<String>,
}

/// Group admission rows by query: relevant sections are golden,
/// every other listed section — irrelevant rows and controls alike —
/// distracts. Controls pin the must-cut junk.
pub fn anchor_cases_from_doc_admission(rows: &[DocAdmissionRow]) -> Vec<AnchorEvalCase> {
    let mut order: Vec<String> = Vec::new();
    let mut by_query: std::collections::HashMap<String, (Vec<String>, Vec<String>)> =
        std::collections::HashMap::new();
    for row in rows {
        by_query.entry(row.query.clone()).or_insert_with(|| {
            order.push(row.query.clone());
            (Vec::new(), Vec::new())
        });
        let entry = by_query.get_mut(&row.query).expect("grouped");
        if row.relevant {
            if !entry.0.contains(&row.section_id) {
                entry.0.push(row.section_id.clone());
            }
        } else if !entry.1.contains(&row.section_id) {
            entry.1.push(row.section_id.clone());
        }
    }
    order
        .into_iter()
        .filter_map(|query| {
            by_query
                .remove(&query)
                .map(|(golden, distractors)| AnchorEvalCase {
                    query,
                    golden,
                    distractors,
                })
        })
        .collect()
}

/// Anchor cases from a cold-start corpus file: golden files, golden
/// sections, and golden facts join as the survival set. Controls
/// stay out of the measurement.
pub fn anchor_cases_from_coldstart(path: &std::path::Path) -> Result<Vec<AnchorEvalCase>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("cold-start corpus read {}: {error}", path.display()))?;
    let mut out = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let task: EvalTask =
            serde_json::from_str(line).map_err(|error| format!("line {}: {error}", index + 1))?;
        if task.control {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|error| format!("line {}: {error}", index + 1))?;
        let mut golden: Vec<String> = task.relevant.clone();
        if let Some(sections) = value.get("golden_sections").and_then(|ids| ids.as_array()) {
            for section in sections {
                if let Some(text) = section.as_str() {
                    golden.push(text.to_string());
                }
            }
        }
        for fact in &task.golden_facts {
            golden.push(fact.clone());
        }
        golden.sort();
        golden.dedup();
        out.push(AnchorEvalCase {
            query: task.query,
            golden,
            distractors: Vec::new(),
        });
    }
    Ok(out)
}

/// Deterministic anchor report over goldens plus distractors.
#[derive(Debug, Clone, PartialEq)]
/// Deterministic anchor report over goldens plus distractors.
pub struct SubjectAnchorReport {
    /// Mean reciprocal rank of the first golden under anchor-mention
    /// ranking (mention count desc, id asc).
    pub mrr: f64,
    /// Fraction of cases keeping at least one golden. Fail-open
    /// anchors (none or shadow) retain by construction.
    pub retention: f64,
    /// Fraction of distractor-bearing cases cutting at least one
    /// distractor.
    pub excluded_rate: f64,
    /// Cases measured.
    pub cases: usize,
    /// Cases keeping a golden.
    pub retained: usize,
    /// Cases bearing distractors.
    pub distractors_support: usize,
    /// Cases cutting a distractor.
    pub excluded: usize,
}

/// Whether one candidate text mentions the anchor subject.
fn mentions(anchor: &crate::read::vocabulary::SubjectAnchor, text: &str) -> bool {
    let fold = text.to_lowercase();
    anchor.terms.iter().any(|term| fold.contains(term.as_str()))
}

/// Measure the shipped anchor rule (crate names anchor ownership
/// questions only; free terms stay shadow): MRR, golden retention,
/// and the excluded-irrelevant rate. No endpoint, no registry, no
/// stored entry — a pure function of the corpora.
pub fn report_subject_anchor(cases: &[AnchorEvalCase]) -> SubjectAnchorReport {
    use crate::query::strategy::QueryIntent;
    let known = crate::component_map::known_crate_names();
    let mut mrr_sum = 0.0;
    let mut retained = 0usize;
    let mut excluded = 0usize;
    let mut distractors_support = 0usize;
    for case in cases {
        let intent = crate::read::intent::classify(&case.query);
        let empty: Vec<String> = Vec::new();
        let table = if intent == QueryIntent::Ownership {
            &known
        } else {
            &empty
        };
        let anchor = crate::read::vocabulary::subject_anchor(&case.query, table);
        let Some(anchor) = anchor else {
            retained += 1;
            mrr_sum += 1.0;
            continue;
        };
        if case.golden.iter().any(|golden| mentions(&anchor, golden)) {
            retained += 1;
        }
        // Mention-count ranking over goldens plus distractors.
        let mut candidates: Vec<(&String, bool, usize)> = case
            .golden
            .iter()
            .map(|golden| (golden, true, 0))
            .chain(
                case.distractors
                    .iter()
                    .map(|distractor| (distractor, false, 0)),
            )
            .collect();
        for candidate in &mut candidates {
            let fold = candidate.0.to_lowercase();
            candidate.2 = anchor
                .terms
                .iter()
                .filter(|term| fold.contains(term.as_str()))
                .count();
        }
        candidates.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(right.0)));
        let rank = candidates
            .iter()
            .position(|candidate| candidate.1)
            .map_or(0, |index| index + 1);
        if rank > 0 {
            mrr_sum += 1.0 / rank as f64;
        }
        if !case.distractors.is_empty() {
            distractors_support += 1;
            if case
                .distractors
                .iter()
                .any(|distractor| !mentions(&anchor, distractor))
            {
                excluded += 1;
            }
        }
    }
    let cases_total = cases.len().max(1) as f64;
    SubjectAnchorReport {
        mrr: mrr_sum / cases_total,
        retention: retained as f64 / cases_total,
        excluded_rate: if distractors_support == 0 {
            1.0
        } else {
            excluded as f64 / distractors_support as f64
        },
        cases: cases.len(),
        retained,
        distractors_support,
        excluded,
    }
}

#[cfg(test)]
#[path = "../../tests/doc_admission.rs"]
mod doc_admission_tests;

/// One sufficiency case: a query plus the evidence at hand. The
/// abstain decision routes to an answer exactly when usable evidence
/// exists; fixture and eval rows never ground, so nonsense and leak
/// queries refuse even when they name a section.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One sufficiency case: a query plus the evidence at hand. The
/// abstain decision routes to an answer exactly when usable evidence
/// exists; fixture and eval rows never ground, so nonsense and leak
/// queries refuse even when they name a section.
pub struct SufficiencyCase {
    /// Stable case id.
    pub id: String,
    /// Question text.
    pub query: String,
    /// Section ids at hand (`path#anchor`).
    pub evidence: Vec<String>,
    /// True when the evidence answers the query.
    pub expected_answer: bool,
    /// Control rows must refuse.
    pub control: bool,
}

/// Whether one evidence section can ground an answer: fixture and
/// eval rows never do (fixtures are excluded from recall, eval rows
/// from the index).
fn evidence_grounds(section_id: &str) -> bool {
    !(section_id.contains("fixture") || section_id.starts_with("doc/guidance/eval"))
}

/// Parse the sufficiency eval corpus (one JSON object per line).
/// Unknown ids/queries reject the file; controls must expect refusal.
pub fn parse_sufficiency_cases(text: &str) -> Result<Vec<SufficiencyCase>, String> {
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
        let evidence: Vec<String> = value
            .get("evidence")
            .and_then(|evidence| evidence.as_array())
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let expected_answer = match value.get("expected").and_then(|e| e.as_str()) {
            Some("answer") => true,
            Some("refuse") => false,
            other => {
                return Err(format!(
                    "line {}: unknown expected `{other:?}` (answer|refuse)",
                    index + 1
                ))
            }
        };
        let control = value
            .get("control")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if control && expected_answer {
            return Err(format!("line {}: controls must refuse", index + 1));
        }
        out.push(SufficiencyCase {
            id,
            query,
            evidence,
            expected_answer,
            control,
        });
    }
    Ok(out)
}

/// Load one sufficiency corpus file.
pub fn load_sufficiency_cases(path: &std::path::Path) -> Result<Vec<SufficiencyCase>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("sufficiency corpus read {}: {error}", path.display()))?;
    parse_sufficiency_cases(&raw)
}

/// Deterministic sufficiency report over must-answer and must-refuse
/// cases (plus controls): grounded evidence routes to `Answer`,
/// anything else refuses. No endpoint, no registry, never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Deterministic sufficiency report over must-answer and must-refuse
/// cases (plus controls): grounded evidence routes to `Answer`,
/// anything else refuses. No endpoint, no registry, never stored.
pub struct SufficiencyReport {
    /// Cases measured.
    pub support: usize,
    /// Correct routes.
    pub correct: usize,
    /// Controls correctly refused.
    pub controls_refused: usize,
    /// Controls measured.
    pub controls: usize,
    /// Every case routed as labelled with every control refused.
    pub passed: bool,
}

pub fn report_sufficiency(cases: &[SufficiencyCase]) -> SufficiencyReport {
    let thresholds = fluent_llm::decision::ThresholdSet {
        act: 0.9,
        review: 0.7,
    };
    let mut correct = 0usize;
    let mut controls = 0usize;
    let mut controls_refused = 0usize;
    for case in cases {
        let usable: Vec<&String> = case
            .evidence
            .iter()
            .filter(|id| evidence_grounds(id))
            .collect();
        let grounded = !usable.is_empty();
        let (route, _) = crate::query::decision_points::route_sufficiency(
            usable.len(),
            grounded,
            None,
            &thresholds,
        );
        let answered = route != crate::query::decision_points::Sufficiency::Unanswerable;
        if answered == case.expected_answer {
            correct += 1;
        }
        if case.control {
            controls += 1;
            if !answered {
                controls_refused += 1;
            }
        }
    }
    let passed = correct == cases.len() && controls_refused == controls;
    SufficiencyReport {
        support: cases.len(),
        correct,
        controls_refused,
        controls,
        passed,
    }
}

/// Whether one read answer may synthesize: the one home for the
/// question. Legacy `Stage` evidence delegates to the grounding
/// gate; the engine path decides on intent plus admitted evidence./// Prompt version for the synthesis evidence prompt: part of the
/// manifest cache key, so a prompt change misses instead of
/// replaying a stale answer.
pub const ANSWER_PROMPT_VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynthesisDecision {
    /// Generative prose over admitted evidence only.
    Synthesize,
    /// Evidence sections only, never prose.
    EvidenceOnly,
}

/// Generative synthesis is earned, never default: explanatory
/// intents (`HowTo`, `Conceptual`) with at least two admitted docs
/// synthesize; every other intent reads evidence-only. Legacy stages
/// keep the grounding gate as the single decider.
#[must_use]
/// Generative synthesis is earned, never default: explanatory
/// intents (`HowTo`, `Conceptual`) with at least two admitted docs
/// synthesize; every other intent reads evidence-only. Legacy stages
/// keep the grounding gate as the single decider.
pub fn synthesis_allowed(
    intent: crate::query::strategy::QueryIntent,
    admitted_docs: usize,
    admitted_code: usize,
    stages: &[crate::query::synthesize::Stage],
) -> SynthesisDecision {
    use crate::query::strategy::QueryIntent;
    let _ = admitted_code;
    if !stages.is_empty() {
        return if crate::grounding::can_synthesize(stages) {
            SynthesisDecision::Synthesize
        } else {
            SynthesisDecision::EvidenceOnly
        };
    }
    match intent {
        QueryIntent::HowTo | QueryIntent::Conceptual if admitted_docs >= 2 => {
            SynthesisDecision::Synthesize
        }
        _ => SynthesisDecision::EvidenceOnly,
    }
}
