//! Doc-admission measurement pins: corpus shape, seed shape,
//! scripted-judge bars, top-k admission mechanics, and the
//! deterministic anchor report. Included via `#[path]` forwarder
//! from `read/admission.rs`, per repo convention. Hermetic: no
//! endpoint, scripted verdicts only.

use super::*;
use crate::read::calibration::ADMISSION_BARS;
use crate::read::decide::{CandidateVerdict, DocLabel};

fn corpus_rows() -> Vec<DocAdmissionRow> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../doc/guidance/eval/doc_admission.jsonl");
    load_doc_admission_rows(&path).expect("doc admission corpus loads")
}

#[test]
fn corpus_parses_with_named_failure_modes() {
    let rows = corpus_rows();
    assert_eq!(rows.len(), 44, "{} rows", rows.len());
    let labelled = rows.iter().filter(|row| !row.control).count();
    assert!(labelled >= 30, "{labelled} labelled rows");
    let controls = rows.iter().filter(|row| row.control).count();
    assert!(controls >= 10, "{controls} controls");
    // The observed failure modes are pinned as controls.
    for section in [
        "doc/skills/common-core/SKILL.md#consolidation-rule",
        "doc/guidance/VISION.md#fixpoint",
        "src/guidance/src/capability.rs#calibrate-capability-corpus",
    ] {
        assert!(
            rows.iter()
                .any(|row| row.section_id == section && row.control),
            "control pins {section}"
        );
    }
    // Malformed rows reject loudly.
    assert!(parse_doc_admission_rows("{\"id\":\"a\",\"query\":\"q\"}").is_err());
    assert!(parse_doc_admission_rows(
        "{\"id\":\"a\",\"query\":\"q\",\"section_id\":\"s\"}\n{\"id\":\"a\",\"query\":\"q\",\"section_id\":\"t\"}"
    )
    .is_err());
}

#[test]
fn seed_shape_loads_tune_cases_with_controls() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpora/doc_admission_seed.json");
    let raw = std::fs::read_to_string(&path).expect("seed reads");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("seed parses");
    let briefs = value
        .get("briefs")
        .and_then(|ids| ids.as_array())
        .expect("briefs");
    let cases = value
        .get("cases")
        .and_then(|cases| cases.as_array())
        .expect("cases");
    assert!(!briefs.is_empty() && !cases.is_empty());
    let ids: std::collections::HashSet<&str> = briefs
        .iter()
        .filter_map(|brief| brief.get("id").and_then(|id| id.as_str()))
        .collect();
    assert_eq!(ids.len(), briefs.len(), "brief ids unique");
    assert!(
        cases.iter().any(|case| case
            .get("control")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)),
        "seed carries controls"
    );
    for case in cases {
        let relevant = case
            .get("relevant")
            .and_then(|relevant| relevant.as_array())
            .expect("relevant");
        for id in relevant {
            let id = id.as_str().expect("id string");
            assert!(ids.contains(id), "relevant {id} names a brief");
        }
        if case
            .get("control")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            assert!(relevant.is_empty(), "controls admit nothing");
        }
    }
}

fn scripted_scores(
    rows: &[DocAdmissionRow],
    strong: impl Fn(&DocAdmissionRow) -> bool,
) -> Vec<fluent_llm::decision::ScoredCase> {
    doc_admission_scores(rows, |row| if strong(row) { 1.0 } else { 0.0 })
}

#[test]
fn scripted_judge_clears_bars_and_bad_judge_held() {
    use crate::read::calibration::gate_scored_cases;
    let rows = corpus_rows();
    // Perfect verdicts: Strong exactly on relevant rows.
    let verdict = gate_scored_cases(
        &scripted_scores(&rows, |row| row.relevant),
        1.0,
        &ADMISSION_BARS,
    );
    assert!(verdict.passed, "{verdict:?}");
    assert!((verdict.precision - 1.0).abs() < 1e-9, "{verdict:?}");
    assert!((verdict.recall - 1.0).abs() < 1e-9, "{verdict:?}");
    assert_eq!(verdict.control_fires, 0);
    // Every control scores below the firing threshold.
    for row in rows.iter().filter(|row| row.control) {
        assert!(!row.relevant, "controls never relevant: {:?}", row.id);
    }
    // Flat judge: nothing fires, recall collapses, HELD.
    let flat = gate_scored_cases(&scripted_scores(&rows, |_| false), 1.0, &ADMISSION_BARS);
    assert!(!flat.passed, "flat judge holds");
    assert!(flat.recall.abs() < 1e-9, "{flat:?}");
}

#[test]
fn admitted_top_k_keeps_goldens_and_names_drops() {
    use crate::query::rerank::admit_top;
    let rows = corpus_rows();
    // Per-query candidates: every section listed for the query.
    let mut order: Vec<String> = Vec::new();
    let mut by_query: std::collections::HashMap<String, Vec<&DocAdmissionRow>> =
        std::collections::HashMap::new();
    for row in &rows {
        by_query.entry(row.query.clone()).or_insert_with(|| {
            order.push(row.query.clone());
            Vec::new()
        });
        by_query.get_mut(&row.query).expect("grouped").push(row);
    }
    for query in &order {
        let candidates = &by_query[query];
        let ids: Vec<String> = candidates
            .iter()
            .map(|row| row.section_id.clone())
            .collect();
        // Scripted perfect judge: Strong on relevant rows only.
        let verdicts: Vec<CandidateVerdict<DocLabel>> = candidates
            .iter()
            .map(|row| CandidateVerdict {
                id: row.section_id.clone(),
                label: if row.relevant {
                    DocLabel::Strong
                } else {
                    DocLabel::Considered
                },
                confidence: 1.0,
            })
            .collect();
        let admitted = admit_top(&ids, &verdicts, 2);
        let goldens: Vec<String> = candidates
            .iter()
            .filter(|row| row.relevant)
            .map(|row| row.section_id.clone())
            .collect();
        assert!(
            goldens.iter().all(|golden| admitted.contains(golden)),
            "{query}: goldens admitted: {admitted:?}"
        );
        // Controls never admit, whatever the query.
        for row in candidates.iter().filter(|row| row.control) {
            assert!(
                !admitted.contains(&row.section_id),
                "{query}: control admitted: {:?}",
                row.section_id
            );
        }
        // Every drop names its label.
        for id in ids.iter().filter(|id| !admitted.contains(id)) {
            let label = verdicts
                .iter()
                .find(|verdict| &verdict.id == id)
                .map_or("unjudged", |verdict| verdict.label.as_str());
            assert!(!label.is_empty(), "{query}: drop names a label");
        }
    }
}

#[test]
fn anchor_report_measures_the_rule() {
    let rows = corpus_rows();
    let mut cases = anchor_cases_from_doc_admission(&rows);
    let coldstart = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../doc/guidance/eval/coldstart.jsonl");
    cases.extend(anchor_cases_from_coldstart(&coldstart).expect("coldstart cases"));
    assert_eq!(cases.len(), 14, "8 admission + 6 cold-start queries");
    let report = report_subject_anchor(&cases);
    assert_eq!(report.cases, 14);
    // Exclusion clears its bar; retention documents the Q1 lexical
    // gap (the free term "components" names no golden file), so the
    // free-term anchor stays shadow until the rule improves.
    assert!(
        report.excluded_rate >= 0.80,
        "excluded-irrelevant rate: {report:?}"
    );
    assert_eq!(report.retained, 13, "Q1 misses: {report:?}");
    assert!((report.retention - 13.0 / 14.0).abs() < 1e-9, "{report:?}");
    assert!(report.mrr > 0.0 && report.mrr <= 1.0, "{report:?}");
}
