//! Read-tier scorecard: the fast tier measures the shipping path —
//! `ReadEngine` over a real `GuidanceDb` and the eval corpus — not
//! fixture docs. Included via `#[path]` forwarder from
//! `scorecard.rs`, per repo convention. Hermetic: staged fixture,
//! in-memory index, no model.

use super::*;

fn read_metric_names(measured: &[Measured]) -> Vec<String> {
    let mut names: Vec<String> = measured
        .iter()
        .filter(|metric| metric.name.starts_with("read."))
        .map(|metric| metric.name.clone())
        .collect();
    names.sort();
    names
}

#[test]
fn read_tier_uses_the_shipping_engine_over_a_real_db() {
    // Same engine, same database kind as production: the staged
    // fixture indexes through the real pipeline into a real
    // `GuidanceDb` (FTS answers), and `ReadEngine` answers over it.
    let (staged, db) = index_read_fixture();
    let hits = db
        .search_fts(
            "converge_mirror",
            10,
            &search_vector::db::FragmentFilter::default(),
        )
        .expect("real FTS over the staged index");
    assert!(!hits.is_empty(), "staged content is retrievable");
    let engine = crate::read::engine::ReadEngine::new();
    let answer = answer_read_question(&engine, &staged, &db, "callers of gen_scoped");
    assert!(
        answer
            .rendered
            .starts_with("## Explain: callers of gen_scoped\n"),
        "ReadEngine renders the answer"
    );
    for section in [
        "Coverage",
        "From the project map",
        "From the docs",
        "From the capabilities",
        "Top matches",
        "Citations",
    ] {
        assert!(
            answer.rendered.contains(&format!("### {section}")),
            "{section}"
        );
    }
    assert!(
        answer
            .citations
            .iter()
            .any(|citation| citation.contains("sync_engine.rs")),
        "graph callers resolve through the shipping path: {:?}",
        answer.citations
    );
    let _ = staged;
}

#[test]
fn read_tier_never_surfaces_measurement_paths() {
    let (staged, db) = index_read_fixture();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../doc/guidance/eval/questions.jsonl");
    let tasks = crate::eval::load_eval_tasks(&path).expect("eval corpus loads");
    let engine = crate::read::engine::ReadEngine::new();
    for task in &tasks {
        let answer = answer_read_question(&engine, &staged, &db, &task.query);
        assert!(
            !answer.rendered.contains("doc/guidance/eval")
                && !answer.rendered.contains("src/guidance/corpora"),
            "leakage in {:?}: {}",
            task.id,
            answer.rendered.chars().take(300).collect::<String>()
        );
        assert!(
            answer
                .citations
                .iter()
                .all(|citation| !citation.contains("doc/guidance/eval")
                    && !citation.contains("src/guidance/corpora")),
            "leakage in citations for {:?}: {:?}",
            task.id,
            answer.citations
        );
    }
    let _ = staged;
}

#[test]
fn read_metric_names_are_stable() {
    // The ratchet, dashboards, and the eval alias all key on these
    // names: renaming a metric is a baseline break, never a drive-by.
    let measured = common_core::runtime::block_on_shared(measure_read());
    assert_eq!(
        read_metric_names(&measured),
        vec![
            "read.corpus_leakage".to_string(),
            "read.degraded_rate".to_string(),
            "read.fact_recall.heldout".to_string(),
            "read.fact_recall.tune".to_string(),
            "read.file_recall_at_5.heldout".to_string(),
            "read.file_recall_at_5.tune".to_string(),
            "read.no_result_false".to_string(),
            "read.p50_ms".to_string(),
            "read.p95_ms".to_string(),
            "read.tokens_per_correct".to_string(),
            "read.unsupported_claims".to_string(),
        ]
    );
}
