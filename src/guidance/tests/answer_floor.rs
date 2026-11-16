//! Answer floor: an `Answer` renders only for evidence that names the
//! asked subject. Below the floor the contract text lands in `Gaps`
//! with the covered scope, and the ungrounded rows stay in their
//! labelled evidence sections.
use super::*;
use search_vector::db::GuidanceDb;

fn answer_for(query: &str) -> ReadAnswer {
    let (dir, db) = super::explain_golden_tests::golden_workspace();
    explain_in(dir.path(), &db, query)
}

fn explain_in(dir: &std::path::Path, db: &GuidanceDb, query: &str) -> ReadAnswer {
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir,
        db,
        nlp: None,
        coverage: None,
    };
    engine.explain(
        &ExplainRequest {
            query: query.to_string(),
            limit: 8,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: AnswerMode::Auto,
        },
        &ctx,
        None,
    )
}

fn has_answer(answer: &ReadAnswer) -> bool {
    answer
        .sections
        .iter()
        .any(|section| section.title == "Answer")
}

fn section(answer: &ReadAnswer, title: &str) -> String {
    answer
        .sections
        .iter()
        .find(|section| section.title == title)
        .map(|section| section.body.clone())
        .unwrap_or_default()
}

fn contract_gap(answer: &ReadAnswer) -> Option<&String> {
    answer
        .gaps
        .iter()
        .find(|gap| gap.starts_with("none found in covered index"))
}

const NONSENSE: &[&str] = &[
    "What is the zzqy nonexistent subsystem?",
    "airspeed velocity of an unladen swallow",
    "What is the quantum flux capacitor turbulator?",
    "How does the zorp gnarlification pipeline optimize?",
];

#[test]
fn nonsense_queries_abstain_with_the_scoped_contract() {
    for query in NONSENSE {
        let answer = answer_for(query);
        assert!(!has_answer(&answer), "{query}: {:?}", answer.sections);
        let gap = contract_gap(&answer)
            .unwrap_or_else(|| panic!("{query}: missing contract gap: {:?}", answer.gaps));
        assert!(
            gap.contains("scope:"),
            "{query}: covered scope named: {gap}"
        );
    }
}

#[test]
fn benchmark_controls_abstain() {
    let controls: Vec<crate::eval::EvalTask> = super::explain_golden_tests::battery()
        .into_iter()
        .filter(|task| task.control)
        .collect();
    assert_eq!(controls.len(), 3, "control battery");
    for task in controls {
        let answer = answer_for(&task.query);
        assert!(
            !has_answer(&answer),
            "{} must abstain: {:?}",
            task.id,
            answer.sections
        );
        assert!(
            contract_gap(&answer).is_some(),
            "{} carries the contract: {:?}",
            task.id,
            answer.gaps
        );
    }
}

#[test]
fn grounded_positive_keeps_its_extractive_answer() {
    let task = super::explain_golden_tests::battery()
        .into_iter()
        .find(|task| task.id == "q2-calibration")
        .expect("q2 present");
    let answer = answer_for(&task.query);
    assert!(has_answer(&answer), "{:?}", answer.sections);
    let body = section(&answer, "Answer");
    assert!(
        body.contains("calibration.md"),
        "extractive row cites the doc: {body}"
    );
    assert!(
        contract_gap(&answer).is_none(),
        "grounded answers carry no abstention: {:?}",
        answer.gaps
    );
}

#[test]
fn ungrounded_rows_stay_in_labelled_evidence() {
    let answer = answer_for("What is the zzqy nonexistent subsystem?");
    assert!(!has_answer(&answer));
    let top = section(&answer, "Top matches");
    assert!(
        top.contains("freshness.rs") || top.contains("stale_files"),
        "withheld rows render as labelled evidence: {:?}",
        answer.sections
    );
}

#[test]
fn scoped_queries_name_the_scope_in_the_contract() {
    let answer = answer_for("What is the guidance-core zzqy turbulator?");
    assert!(!has_answer(&answer), "{:?}", answer.sections);
    let gap = contract_gap(&answer).expect("contract present");
    assert!(
        gap.contains("guidance-core"),
        "narrowed scope is named: {gap}"
    );
}
