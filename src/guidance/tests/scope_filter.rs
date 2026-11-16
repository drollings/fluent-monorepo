//! One fixture/scope rule wired everywhere: fixture rows drop by
//! default and return only through the explicit opt-in, eval rows
//! never pass. Pins the shared predicate and the engine wiring that
//! keeps fixture paths out of every rendered section.
use super::*;
use crate::tests::common::{db_file_record, db_project_card, db_text_fragment};
use search_vector::db::GuidanceDb;

#[test]
fn fixture_rows_need_the_opt_in_and_eval_rows_never_pass() {
    assert!(!crate::query::rerank::scope_row_admitted(
        "src/guidance/fixture_read/src/lib.rs",
        false
    ));
    assert!(crate::query::rerank::scope_row_admitted(
        "src/guidance/fixture_read/src/lib.rs",
        true
    ));
    assert!(!crate::query::rerank::scope_row_admitted(
        "benchmarks/fixtures/case.rs",
        false
    ));
    assert!(crate::query::rerank::scope_row_admitted(
        "benchmarks/fixtures/case.rs",
        true
    ));
    assert!(!crate::query::rerank::scope_row_admitted(
        "doc/guidance/eval/corpus.jsonl",
        true
    ));
    assert!(crate::query::rerank::scope_row_admitted(
        "src/guidance/src/freshness.rs",
        false
    ));
}

#[test]
fn exclude_fixtures_follows_the_shared_rule() {
    let mk = |path: &str| crate::read::engine::Finding {
        id: path.to_string(),
        producer: crate::read::engine::FindingProducer::Database,
        kind: crate::read::engine::FindingKind::Code,
        source: crate::source_ref::SourceRef::from_workspace(
            std::path::Path::new("/workspace"),
            std::path::Path::new(path),
        ),
        start_line: Some(1),
        end_line: Some(2),
        text: "row".to_string(),
        producer_score: 1.0,
        provenance: "database".to_string(),
    };
    let rows = vec![
        mk("src/guidance/src/freshness.rs"),
        mk("src/guidance/fixture_read/src/lib.rs"),
        mk("doc/guidance/eval/corpus.rs"),
    ];
    let kept = crate::query::rerank::exclude_fixtures(rows.clone(), false);
    assert_eq!(kept.len(), 1, "default keeps product rows only");
    let opted = crate::query::rerank::exclude_fixtures(rows, true);
    assert_eq!(opted.len(), 2, "opt-in restores fixtures, never eval rows");
}

/// Workspace where one fixture-scope manifest card and one
/// fixture-scope code row are the only sources naming the path.
fn fixture_leak_workspace() -> (tempfile::TempDir, GuidanceDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("write");
    std::fs::create_dir_all(dir.path().join("src/guidance/fixture_read/src")).expect("mkdir");
    let fixture_path = "src/guidance/fixture_read/src/lib.rs";
    std::fs::write(
        dir.path().join(fixture_path),
        "//! Fixture row: matches the query but must never render.\npub fn alpha() {}\n",
    )
    .expect("write");
    std::fs::write(dir.path().join("lib.rs"), "pub fn beta() {}\n").expect("write");
    let db = GuidanceDb::open_in_memory().expect("db");
    db.upsert_fragments(
        &db_file_record(dir.path(), fixture_path),
        &[db_text_fragment(
            fixture_path,
            "alpha",
            "Fixture row: matches the query but must never render.",
            1,
        )],
        &[],
    )
    .expect("seed fixture row");
    db.upsert_fragments(
        &db_file_record(dir.path(), "lib.rs"),
        &[db_text_fragment(
            "lib.rs",
            "beta",
            "beta row for grounding",
            1,
        )],
        &[],
    )
    .expect("seed row");
    db.replace_project_cards(&[
        db_project_card(
            "workspace:ws",
            "workspace",
            "Workspace",
            "Ships.",
            "Cargo.toml",
            "",
        ),
        db_project_card(
            "crate:fixture",
            "crate",
            "fixture",
            "Fixture manifest row naming the query terms.",
            "src/guidance/fixture_read/src/lib.rs",
            "",
        ),
    ])
    .expect("cards");
    (dir, db)
}

fn rendered(query: &str, include_fixtures: bool) -> String {
    let (dir, db) = fixture_leak_workspace();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: query.to_string(),
            limit: 8,
            budget_override: None,
            no_llm: true,
            include_fixtures,
            answer_mode: AnswerMode::Auto,
        },
        &ctx,
        None,
    );
    crate::read::render::render_answer(&answer)
}

#[test]
fn explain_output_carries_fixture_rows_only_when_opted_in() {
    let query = "What does the fixture manifest explain?";
    let hidden = rendered(query, false);
    assert!(
        !hidden.contains("Fixture manifest row"),
        "default output hides the fixture card: {hidden}"
    );
    let restored = rendered(query, true);
    assert!(
        restored.contains("Fixture manifest row"),
        "opt-in restores the fixture card: {restored}"
    );
}
