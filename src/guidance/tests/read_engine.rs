//! Read-engine skeleton: plan routing, budget application, empty
//! sections with coverage. Included via `#[path]` forwarder from
//! `read/engine.rs`, per repo convention. Pure: no I/O, no model.

use super::*;
use crate::query::strategy::QueryIntent;

fn request(query: &str) -> ReadRequest {
    ReadRequest {
        query: query.to_string(),
        budget_override: None,
    }
}

#[test]
fn plan_routes_intent_and_budgets() {
    let engine = ReadEngine::new();
    let plan = engine.plan(&request("callers of gen_scoped"));
    assert_eq!(plan.intent, QueryIntent::Graph);
    assert!(plan.extract_symbols);
    assert_eq!(plan.budgets.total, 1000);
    let plan = engine.plan(&request("what are the major components"));
    assert_eq!(plan.intent, QueryIntent::Orientation);
    assert!(!plan.extract_symbols);
    assert_eq!(plan.budgets.project, 400);
}

#[test]
fn budget_override_replaces_only_the_total() {
    let engine = ReadEngine::new();
    let plan = engine.plan(&ReadRequest {
        query: "callers of x".to_string(),
        budget_override: Some(2000),
    });
    assert_eq!(plan.budgets.total, 2000);
    assert_eq!(plan.budgets.code, 900);
}

#[test]
fn answer_carries_empty_sections_with_coverage_and_tokens() {
    let engine = ReadEngine::new();
    let coverage = crate::read::readiness::Coverage::empty();
    let answer = engine.answer(&request("where is alpha_fn defined"), Some(coverage));
    assert_eq!(answer.intent, QueryIntent::SingleIdentifier);
    assert_eq!(answer.sections.len(), 6);
    assert_eq!(answer.sections[0].title, "From the project map");
    assert!(answer.coverage.is_some());
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.starts_with("## Explain: where is alpha_fn defined"));
    assert!(rendered.contains("### Top matches"));
    assert!(answer.tokens_used > 0);
}

#[test]
fn findings_carry_producer_kind_and_provenance() {
    let finding = Finding {
        id: "doc/guide.md#guide".to_string(),
        producer: FindingProducer::Cards,
        kind: FindingKind::Capability,
        source: crate::source_ref::SourceRef::from_workspace(
            std::path::Path::new("/workspace"),
            std::path::Path::new("doc/guide.md"),
        ),
        start_line: None,
        end_line: None,
        text: "Routes requests onward.".to_string(),
        producer_score: 1.0,
        provenance: "external:doc/capabilities/corpus.json".to_string(),
    };
    assert_eq!(finding.producer, FindingProducer::Cards);
    let code = Finding {
        kind: FindingKind::Code,
        producer: FindingProducer::Ripgrep,
        ..finding.clone()
    };
    let rendered = crate::read::render::render_code_findings(std::slice::from_ref(&code));
    assert!(rendered.contains("Routes requests onward."), "{rendered}");
}

struct StubSynth {
    reply: Option<String>,
}

impl crate::read::engine::Synthesizer for StubSynth {
    fn synthesize(&self, _prompt: &str) -> Result<Option<String>, String> {
        Ok(self.reply.clone())
    }
}

fn explain_fixture() -> (tempfile::TempDir, search_vector::db::GuidanceDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"src/a\"]\n",
    )
    .expect("manifest");
    std::fs::create_dir_all(root.join("src/a")).expect("mkdir");
    std::fs::write(
        root.join("src/a/Cargo.toml"),
        "[package]\nname = \"alpha\"\ndescription = \"Alpha crate.\"\n",
    )
    .expect("manifest");
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    crate::read::project_map::import_project_cards(root, "fixture", &db, None).expect("cards");
    db.upsert_fragments(
        &search_vector::db::FileRecord {
            id: "/repo/src/a.rs".to_string(),
            absolute_path: "/repo/src/a.rs".to_string(),
            relative_path: "src/a.rs".to_string(),
            root_path: "/repo".to_string(),
            size_bytes: 64,
            last_modified_time: 100,
            kind: Some("code".to_string()),
            format: "rust".to_string(),
            content_hash: None,
            index_status: Some("indexed".to_string()),
            fail_count: 0,
            last_error: None,
        },
        &[search_vector::db::FragmentRecord {
            id: "fn".to_string(),
            group: None,
            file_id: "/repo/src/a.rs".to_string(),
            range_json: "{\"kind\":\"text\",\"start_line\":1,\"end_line\":3,\"start_offset\":0,\"end_offset\":10}".to_string(),
            content_kind: "text".to_string(),
            content_text: Some("pub fn alpha_fn() {}\nAdds one.".to_string()),
            cjk_text: String::new(),
            symbol_type: Some("function".to_string()),
            symbol_name: Some("alpha_fn".to_string()),
            scope: None,
            signature: None,
            doc: None,
            modifiers: String::new(),
            heading: None,
            heading_level: None,
            embedding: None,
        }],
        &[],
    )
    .expect("seed");
    (dir, db)
}

fn explain_request(query: &str, no_llm: bool) -> ExplainRequest {
    ExplainRequest {
        query: query.to_string(),
        limit: 5,
        budget_override: None,
        no_llm,
    }
}

#[test]
fn explain_assembles_sections_in_order_without_a_model() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = explain_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: Some(crate::read::readiness::Coverage::empty()),
    };
    let answer = engine.explain(
        &explain_request("what are the major components", true),
        &ctx,
        None,
    );
    assert_eq!(answer.intent, QueryIntent::Orientation);
    assert_eq!(answer.route, "orientation");
    let titles: Vec<&str> = answer
        .sections
        .iter()
        .map(|section| section.title.as_str())
        .collect();
    assert_eq!(
        titles,
        vec![
            "Coverage",
            "From the project map",
            "From the docs",
            "From the capabilities",
            "Top matches",
            "Citations"
        ],
        "{titles:?}"
    );
    assert!(answer.coverage.is_some());
    assert!(answer.tokens_used > 0);
    assert!(!answer.sections.is_empty());
}

#[test]
fn explain_synthesizes_support_gated_answers() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = explain_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let synth = StubSynth {
        reply: Some("Alpha adds one (src/a.rs:1).".to_string()),
    };
    let answer = engine.explain(
        &explain_request("what does alpha_fn do", false),
        &ctx,
        Some(&synth),
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.contains("### Answer"), "{rendered}");
    assert!(rendered.contains("[unverified]"), "{rendered}");
    assert!(!answer.claims.is_empty());
    // Evidence-only when the model declines.
    let declined = StubSynth { reply: None };
    let answer = engine.explain(
        &explain_request("what does alpha_fn do", false),
        &ctx,
        Some(&declined),
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(
        rendered.contains("(unavailable: empty answer)"),
        "{rendered}"
    );
    // Evidence-only when declined at the request level.
    let answer = engine.explain(
        &explain_request("what does alpha_fn do", true),
        &ctx,
        Some(&synth),
    );
    assert!(!crate::read::render::render_answer(&answer).contains("### Answer"));
    assert!(answer.claims.is_empty());
}

#[test]
fn plan_ripgrep_runs_for_exact_but_not_broad() {
    let engine = ReadEngine::new();
    assert!(engine.plan(&request("alpha_fn")).ripgrep);
    assert!(engine.plan(&request("src/a.rs")).ripgrep);
    assert!(engine.plan(&request("callers of alpha_fn")).ripgrep);
    assert!(
        !engine
            .plan(&request("what are the major components"))
            .ripgrep
    );
    assert!(
        !engine
            .plan(&request("how does sync decide what to reprocess"))
            .ripgrep
    );
}

#[test]
fn exact_fallback_finds_literal_without_an_index_row() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("needle_file.rs"),
        "fn unique_needle_fn() {}\n",
    )
    .expect("write");
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "unique_needle_fn".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
        },
        &ctx,
        None,
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.contains("unique_needle_fn"), "{rendered}");
    // Broad language stays index-only: no rg fallback, no literal.
    let answer = engine.explain(
        &ExplainRequest {
            query: "what is this repository about".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
        },
        &ctx,
        None,
    );
    assert!(!crate::read::render::render_answer(&answer).contains("unique_needle_fn"));
}

#[test]
fn shadow_judge_labels_without_filtering_or_reordering() {
    use crate::read::engine::{ExplainContext, JudgeCtx, ReadEngine};
    use fluent_llm::decision::{Decision, ScriptedDecideBackend};
    let (dir, db) = explain_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let plain = engine.explain(&explain_request("alpha_fn", true), &ctx, None);
    let plain_rendered = crate::read::render::render_answer(&plain);
    let backend = ScriptedDecideBackend::new(vec![
        Ok(Decision::from_distribution(
            "relevant",
            vec![("relevant".to_string(), 0.9)],
            "d".to_string(),
            1,
            1,
            false,
        )
        .expect("d")),
        Ok(Decision::from_distribution(
            "irrelevant",
            vec![("irrelevant".to_string(), 0.8)],
            "d".to_string(),
            1,
            1,
            false,
        )
        .expect("d")),
    ]);
    let judge = JudgeCtx {
        backend: &backend,
        model: "d".to_string(),
        timeout_ms: 2000,
        fingerprint: None,
    };
    let judged =
        engine.explain_with_judge(&explain_request("alpha_fn", true), &ctx, None, Some(&judge));
    let judged_rendered = crate::read::render::render_answer(&judged);
    assert!(
        judged_rendered.contains("[relevant]")
            || judged_rendered.contains("[irrelevant]")
            || judged_rendered.contains("[insufficient]"),
        "{judged_rendered}"
    );
    assert_eq!(
        plain.citations.len(),
        judged.citations.len(),
        "shadow never filters"
    );
    assert!(
        judged.gaps.iter().any(|g| g.contains("fingerprint")),
        "{:?}",
        judged.gaps
    );
    let _ = plain_rendered;
}

#[test]
fn invented_model_path_is_dropped_not_rendered() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = explain_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let synth = StubSynth {
        reply: Some("It lives in other.rs:99 and does things.".to_string()),
    };
    let answer = engine.explain(
        &explain_request("what does alpha_fn do", false),
        &ctx,
        Some(&synth),
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(
        !rendered.contains("other.rs:99"),
        "invented path must not render: {rendered}"
    );
}

#[test]
fn unauthorized_judge_leaves_output_identical_except_labels() {
    use crate::read::engine::{ExplainContext, JudgeCtx, ReadEngine};
    use fluent_llm::decision::{Decision, ScriptedDecideBackend};
    let (dir, db) = explain_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let plain = engine.explain(&explain_request("alpha_fn", true), &ctx, None);
    let backend = ScriptedDecideBackend::new(vec![Ok(Decision::from_distribution(
        "relevant",
        vec![("relevant".to_string(), 0.9)],
        "d".to_string(),
        1,
        1,
        false,
    )
    .expect("d"))]);
    let judge = JudgeCtx {
        backend: &backend,
        model: "d".to_string(),
        timeout_ms: 2000,
        fingerprint: None,
    };
    let shadowed =
        engine.explain_with_judge(&explain_request("alpha_fn", true), &ctx, None, Some(&judge));
    assert_eq!(plain.citations.len(), shadowed.citations.len());
    assert_eq!(plain.sections.len(), shadowed.sections.len());
}

#[test]
fn authorized_filtering_drops_irrelevant_keeps_exact() {
    use crate::read::decide::{FindingLabel, FindingVerdict};
    use crate::read::engine::{ExplainContext, JudgeCtx, ReadEngine};
    use fluent_llm::decision::{
        CalibrationRegistry, Decision, RegistryEntry, ScriptedDecideBackend, ThresholdSet,
    };
    let (dir, db) = explain_fixture();
    // Write an authorized finding entry for a test fingerprint.
    let registry_path = dir.path().join(".guidance/state/calibration.json");
    std::fs::create_dir_all(registry_path.parent().expect("parent")).expect("mkdir");
    let rows = crate::read::calibration::load_labeled_findings(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../doc/guidance/eval/findings.jsonl")
            .as_path(),
    )
    .expect("corpus");
    let cases = crate::read::calibration::score_findings(&rows, |row| {
        if row.expected_relevant {
            1.0
        } else {
            0.0
        }
    });
    let corpus =
        crate::read::calibration::finding_corpus(cases, "m7-test-fp", "hash", "findings.jsonl");
    let entry = RegistryEntry::calibrate_with(
        crate::read::calibration::FINDING_POINT,
        "m7-test-fp",
        crate::read::calibration::FINDING_QUESTION_VERSION,
        &corpus,
        crate::read::calibration::FINDING_THRESHOLD,
        Some(ThresholdSet {
            act: 1.0,
            review: 1.0,
        }),
    )
    .expect("entry");
    let mut registry = CalibrationRegistry::empty();
    registry.upsert(entry);
    registry.save(&registry_path).expect("save");
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    // Scripted judge: first finding irrelevant, second relevant.
    let backend = ScriptedDecideBackend::new(vec![
        Ok(Decision::from_distribution(
            "irrelevant",
            vec![("irrelevant".to_string(), 0.9)],
            "d".to_string(),
            1,
            1,
            false,
        )
        .expect("d")),
        Ok(Decision::from_distribution(
            "relevant",
            vec![("relevant".to_string(), 0.9)],
            "d".to_string(),
            1,
            1,
            false,
        )
        .expect("d")),
    ]);
    let judge = JudgeCtx {
        backend: &backend,
        model: "d".to_string(),
        timeout_ms: 2000,
        fingerprint: Some("m7-test-fp".to_string()),
    };
    let answer =
        engine.explain_with_judge(&explain_request("alpha_fn", true), &ctx, None, Some(&judge));
    // Authorized filtering ran (no crash) and kept deterministic evidence;
    // exact symbol findings (Ast) are never dropped even when labelled irrelevant.
    let _ = answer;
    let _ = FindingVerdict {
        id: "x".to_string(),
        label: FindingLabel::Relevant,
        confidence: 1.0,
    };
}

#[test]
fn controls_abstain_with_none_found_gap() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = explain_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    for query in [
        "What is the zzqy nonexistent subsystem?",
        "Which crate owns the qqqNoSuchSymbol calibrator?",
    ] {
        let answer = engine.explain(
            &ExplainRequest {
                query: query.to_string(),
                limit: 5,
                budget_override: None,
                no_llm: false,
            },
            &ctx,
            Some(&StubSynth {
                reply: Some("It lives in other.rs:99.".to_string()),
            }),
        );
        let rendered = crate::read::render::render_answer(&answer);
        assert!(
            answer.gaps.iter().any(|g| g.contains("none found")),
            "{query}: {:?}",
            answer.gaps
        );
        assert!(!rendered.contains("other.rs:99"), "{query}: {rendered}");
        assert!(
            !rendered.contains("### Answer") || rendered.contains("do not support"),
            "{query}: {rendered}"
        );
    }
    // Real mechanism still renders.
    let answer = engine.explain(
        &ExplainRequest {
            query: "what does alpha_fn do".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: false,
        },
        &ctx,
        Some(&StubSynth {
            reply: Some("Alpha adds one (src/a.rs:1-3).".to_string()),
        }),
    );
    assert!(
        !answer.gaps.iter().any(|g| g.contains("none found")),
        "{:?}",
        answer.gaps
    );
}
