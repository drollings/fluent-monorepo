//! Capability-tier calibration: the `dp.capability` point
//! calibrates on the external corpus's golden `cases` when present;
//! without cases every card stays `considered` and never feeds
//! synthesis or traversal. Only a passing entry may promote `strong`.
//! Included via `#[path]` forwarder from `read/calibration.rs`, per
//! repo convention. Hermetic: temp workspaces and corpora, rule
//! pipeline only, no endpoint.

use super::*;
use crate::capability::{calibrate_capability_corpus, CAPABILITY_POINT};
use crate::read::engine::{ExplainContext, ExplainRequest, ReadEngine, Synthesizer};

fn corpus_file(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir.join("doc/capabilities")).expect("mkdir corpus");
    let path = dir.join("doc/capabilities/corpus.json");
    std::fs::write(&path, body).expect("write corpus");
    path
}

fn doc_file(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("doc/router")).expect("mkdir doc");
    std::fs::write(
        dir.join("doc/router/guide.md"),
        "# Guide\n\nRoutes requests onward through the table.\n",
    )
    .expect("write doc");
}

fn passing_corpus() -> String {
    r#"{"briefs": [
        {"id": "doc/router/guide.md#guide", "title": "Routing",
         "purpose": "Routes requests onward through the table.",
         "crate": "fluent-router", "domain": "router",
         "source_path": "doc/router/guide.md", "anchor": "guide",
         "keywords": ["routing"]},
        {"id": "doc/other.md#other", "title": "Baking",
         "purpose": "Bakes bread slowly overnight.",
         "crate": "bakery", "domain": "food",
         "source_path": "doc/other.md", "anchor": "other",
         "keywords": ["baking"]}],
      "cases": [
        {"query": "how are requests routed onward", "relevant": ["doc/router/guide.md#guide"]},
        {"query": "zzqy nonexistent wobble", "relevant": [], "control": true}]}"#
        .to_string()
}

#[test]
fn without_cases_calibration_refuses_and_cards_stay_considered() {
    let dir = tempfile::tempdir().expect("tempdir");
    doc_file(dir.path());
    let path = corpus_file(
        dir.path(),
        r#"{"briefs": [{"id": "doc/router/guide.md#guide", "title": "Routing",
            "purpose": "Routes requests onward through the table.",
            "crate": "fluent-router", "domain": "router",
            "source_path": "doc/router/guide.md", "anchor": "guide"}]}"#,
    );
    // No `cases`: calibration is impossible, loudly.
    let report = calibrate_capability_corpus(&path, None);
    assert!(
        report.is_err(),
        "caseless corpus cannot calibrate: {report:?}"
    );
    // Import still stores the card; the read plane renders it
    // `considered` and never lets it feed synthesis or traversal.
    let db = crate::query::search_db::GuidanceSearchDb::open_in_memory().expect("db");
    crate::read::doc_cards::import_capability_corpus(
        dir.path(),
        &crate::config::ProjectConfig::default(),
        &db,
    )
    .expect("import");
    struct CountingSynth {
        called: std::cell::Cell<bool>,
    }
    impl Synthesizer for CountingSynth {
        fn synthesize(&self, _prompt: &str) -> Result<Option<String>, String> {
            self.called.set(true);
            Ok(Some("synthesized".to_string()))
        }
    }
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let synth = CountingSynth {
        called: std::cell::Cell::new(false),
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "how are requests routed onward".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: false,
            include_fixtures: false,
            answer_mode: crate::read::engine::AnswerMode::Auto,
        },
        &ctx,
        Some(&synth),
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.contains("[considered]"), "{rendered}");
    assert!(!rendered.contains("[strong]"), "{rendered}");
    // No code evidence exists, so synthesis never runs: capabilities
    // alone cannot trigger an answer, and no branch-2 pull merges
    // their scopes into Top matches.
    assert!(!synth.called.get(), "capabilities never feed synthesis");
    assert!(!rendered.contains("### Answer"), "{rendered}");
    assert!(!rendered.contains("via capability"), "{rendered}");
}

#[test]
fn with_cases_the_point_calibrates_for_promotion() {
    let dir = tempfile::tempdir().expect("tempdir");
    doc_file(dir.path());
    std::fs::write(dir.path().join("doc/other.md"), "# Other\n\nBaking.\n").expect("write");
    let path = corpus_file(dir.path(), &passing_corpus());
    let report = calibrate_capability_corpus(&path, None).expect("calibrates");
    assert_eq!(report.support, 1, "{report:?}");
    assert_eq!(report.control_total, 1);
    assert_eq!(report.control_fires, 0, "{report:?}");
    assert!(report.passed, "{report:?}");
    assert!(
        !report.dataset_hash.is_empty(),
        "rotation detection needs the hash"
    );
    // A corpus whose golden expects a brief the ranker does not put
    // first fails the recall bar: HELD, nothing stored, promotion
    // stays manual.
    let mismatch = passing_corpus().replace(
        "\"relevant\": [\"doc/router/guide.md#guide\"]",
        "\"relevant\": [\"doc/other.md#other\"]",
    );
    let mismatch_path = corpus_file(dir.path(), &mismatch);
    let _ = std::fs::write(&path, &mismatch);
    let held = calibrate_capability_corpus(&mismatch_path, None).expect("measures");
    assert!(!held.passed, "wrong rank-1 must hold: {held:?}");
    assert!(held.recall_at_1 < 1.0);
}

#[test]
fn capability_point_name_is_stable() {
    assert_eq!(CAPABILITY_POINT, "dp.capability");
    assert_eq!(FINDING_POINT, "finding");
    assert_eq!(SUPPORT_POINT, "support");
}
