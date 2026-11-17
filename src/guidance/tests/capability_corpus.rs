//! Capability corpus gate: every authored card resolves to a live doc
//! section, import is idempotent, and the major-components query renders
//! ranked briefs from the populated corpus.
use super::*;
use crate::config::ProjectConfig;
use crate::query::search_db::GuidanceSearchDb;

fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn corpus_path() -> std::path::PathBuf {
    workspace_root().join("doc/capabilities/corpus.json")
}

#[test]
fn every_authored_card_resolves_to_a_live_section() {
    let corpus = crate::capability::load_capability_corpus(&corpus_path()).expect("corpus loads");
    assert!(corpus.briefs.len() >= 8, "{} briefs", corpus.briefs.len());
    let root = workspace_root();
    let mut ids = std::collections::HashSet::new();
    for brief in &corpus.briefs {
        assert!(ids.insert(brief.id.clone()), "duplicate id {}", brief.id);
        assert!(!brief.purpose.trim().is_empty(), "{}: purpose", brief.id);
        let source = root.join(&brief.source_path);
        let text = std::fs::read_to_string(&source)
            .unwrap_or_else(|error| panic!("{}: source read: {error}", brief.id));
        let section = resolve_live_section(&brief.source_path, &brief.anchor, &text)
            .unwrap_or_else(|| panic!("{}: anchor `{}` missing", brief.id, brief.anchor));
        assert!(
            !section.trim().is_empty(),
            "{}: resolved section is empty",
            brief.id
        );
    }
}

#[test]
fn corpus_import_is_idempotent() {
    let root = workspace_root();
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    let first =
        import_capability_corpus(&root, &ProjectConfig::default(), &db).expect("first import");
    assert!(first.present, "manifest found");
    assert_eq!(first.gaps, 0, "every card resolves: {first:?}");
    assert_eq!(first.ready, first.cards, "{first:?}");
    assert_eq!(first.skipped_measurement, 0, "{first:?}");
    let second =
        import_capability_corpus(&root, &ProjectConfig::default(), &db).expect("second import");
    assert_eq!(second.ready, 0, "no row changes: {second:?}");
    assert_eq!(second.gaps, 0, "{second:?}");
    assert_eq!(second.removed, 0, "{second:?}");
    assert_eq!(
        second.skipped_unchanged, second.cards,
        "second import is a no-op: {second:?}"
    );
}

fn imported_answer(query: &str) -> crate::read::engine::ReadAnswer {
    let root = workspace_root();
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    let report =
        import_capability_corpus(&root, &ProjectConfig::default(), &db).expect("capability import");
    assert_eq!(report.ready, report.cards, "{report:?}");
    let engine = crate::read::engine::ReadEngine::new();
    let ctx = crate::read::engine::ExplainContext {
        workspace: &root,
        db: &db,
        nlp: None,
        coverage: None,
    };
    engine.explain(
        &crate::read::engine::ExplainRequest {
            query: query.to_string(),
            limit: 10,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: crate::read::engine::AnswerMode::Auto,
        },
        &ctx,
        None,
    )
}

#[test]
fn major_components_renders_ranked_capability_briefs() {
    let answer = imported_answer("What are the major components of this codebase?");
    let section = answer
        .sections
        .iter()
        .find(|section| section.title == "From the capabilities")
        .map(|section| section.body.clone())
        .unwrap_or_default();
    assert!(!section.is_empty(), "{:?}", answer.sections);
    assert!(section.contains("[considered]"), "{section}");
    let named = ["guidance-core", "fluent-router", "fluent-wvr"]
        .iter()
        .filter(|crate_name| section.contains(*crate_name))
        .count();
    assert!(named >= 2, "major components named: {section}");
}

#[test]
fn a_specific_purpose_ranks_its_brief_first() {
    let answer = imported_answer(
        "How does the router pipeline route requests over local and remote models?",
    );
    let section = answer
        .sections
        .iter()
        .find(|section| section.title == "From the capabilities")
        .map(|section| section.body.clone())
        .unwrap_or_default();
    let first = section
        .lines()
        .find(|line| line.starts_with("- [considered]"))
        .unwrap_or("");
    assert!(
        first.contains("Coral Router") || first.contains("doc/router/VISION.md"),
        "the router brief ranks first: {section}"
    );
}
