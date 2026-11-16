//! Orientation battery characterization: pins today's read behavior so
//! later work flips expectations deliberately. Battery shape plus the
//! three known gaps (file-level citation repeats, unbound claims,
//! budget table contract) are asserted as they behave now.
use super::*;
use std::collections::HashSet;

fn battery_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../doc/guidance/eval/orientation_battery.jsonl")
}

#[test]
fn battery_loads_six_orientation_plus_drilldowns() {
    let tasks = load_eval_tasks(&battery_path()).expect("battery loads");
    assert!(tasks.len() >= 10, "{} tasks", tasks.len());
    let orientation: Vec<_> = tasks.iter().filter(|t| t.intent == "orientation").collect();
    assert!(orientation.len() >= 6, "{} orientation", orientation.len());
    assert!(tasks.iter().any(|t| t.id == "drill-callers-clean-cache"));
    assert!(tasks.iter().any(|t| t.id == "drill-glob-guidance-crate"));
    for task in &tasks {
        assert!(!task.query.trim().is_empty(), "{:?}", task.id);
        assert!(!task.relevant.is_empty(), "{:?}", task.id);
        assert!(!task.required_sections.is_empty(), "{:?}", task.id);
    }
}

#[test]
fn battery_records_known_failure_signatures() {
    let raw = std::fs::read_to_string(battery_path()).expect("battery reads");
    for signature in ["duplicate_citations", "refusal", "wrong_component"] {
        assert!(raw.contains(signature), "missing {signature}");
    }
    assert!(raw.contains("clean_build_cache"));
    assert!(raw.contains("src/guidance/**") || raw.contains("src/router/**"));
}

#[test]
fn battery_ids_are_unique_and_splits_present() {
    let tasks = load_eval_tasks(&battery_path()).expect("battery loads");
    let mut seen = HashSet::new();
    for task in &tasks {
        assert!(seen.insert(task.id.clone()), "duplicate {}", task.id);
    }
    assert!(
        tasks.iter().any(|t| t.split == "tune"),
        "tune split present"
    );
}

#[test]
fn file_level_repeats_collapse_to_first_range() {
    use crate::read::engine::{Finding, FindingKind, FindingProducer};
    use crate::read::evidence::EvidenceSet;
    use crate::source_ref::SourceRef;
    let mk = |id: &str, start: u32| Finding {
        id: id.to_string(),
        producer: FindingProducer::Database,
        kind: FindingKind::Code,
        source: SourceRef::from_workspace(
            std::path::Path::new("/workspace"),
            std::path::Path::new("src/guidance/src/sync_engine.rs"),
        ),
        start_line: Some(start),
        end_line: Some(start + 5),
        text: format!("excerpt {id}"),
        producer_score: 1.0,
        provenance: "code".to_string(),
    };
    let mut set = EvidenceSet::new();
    set.extend(vec![mk("a", 1), mk("b", 40)]);
    set.dedupe();
    assert_eq!(set.len(), 1, "one file cites once after file-level dedupe");
}

#[test]
fn unbound_sentences_bind_to_nothing_today() {
    let manifest = vec![(
        "[C1]".to_string(),
        "src/guidance/src/sync_engine.rs:10-20".to_string(),
    )];
    let claims = crate::read::claims::bind_claims(
        "Sync reprocesses stale files. See nothing else.",
        &manifest,
    );
    assert!(!claims.is_empty());
    assert!(
        claims.iter().all(|c| c.citation.is_none()),
        "no sentence names the citation today: {claims:?}"
    );
    assert!(
        crate::read::claims::gate_answer(&claims).is_none(),
        "unbound sentences never render"
    );
}

#[test]
fn budget_table_contract_is_published() {
    use crate::query::strategy::QueryIntent;
    use crate::read::budget::budgets_for;
    let orientation = budgets_for(QueryIntent::Orientation);
    assert_eq!(orientation.total, 1000);
    assert_eq!(orientation.project, 400);
    assert_eq!(orientation.code, 200);
    let graph = budgets_for(QueryIntent::Graph);
    assert_eq!(graph.total, 1000);
}
