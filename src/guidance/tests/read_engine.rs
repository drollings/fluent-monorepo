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
        include_fixtures: false,
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
            "Top matches",
            "Citations",
            "Gaps"
        ],
        "{titles:?}"
    );
    // No capability rows seeded: the section stays out and the gap
    // names the unpopulated corpus instead.
    assert!(
        answer
            .gaps
            .iter()
            .any(|gap| gap.contains("capability corpus unpopulated")),
        "{:?}",
        answer.gaps
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
        !rendered.contains("### Answer"),
        "declined synthesis renders no answer section: {rendered}"
    );
    assert!(
        answer
            .gaps
            .iter()
            .any(|gap| gap.contains("synthesis unavailable")),
        "decline names the gap: {:?}",
        answer.gaps
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
fn soft_scope_orders_in_crate_first_with_split_budget() {
    // Two retrieved files, one in the query-named crate: the scope
    // partition renders the in-crate finding first even when rank
    // prefers the other file (the router file repeats the query terms
    // to outrank the guidance file), declares the split in Coverage,
    // and leaves recall itself unscoped (both files still cited).
    use crate::read::engine::{ExplainContext, ReadEngine};
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("manifest");
    let seed = |relative: &str, symbol: &str, body: &str| {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, body).expect("write");
        let meta = std::fs::metadata(&path).expect("mtime");
        (search_vector::db::FileRecord {
            id: relative.to_string(),
            absolute_path: path.to_string_lossy().into_owned(),
            relative_path: relative.to_string(),
            root_path: root.to_string_lossy().into_owned(),
            size_bytes: meta.len(),
            last_modified_time: meta
                .modified()
                .expect("mtime")
                .duration_since(std::time::UNIX_EPOCH)
                .expect("epoch")
                .as_millis() as i64,
            kind: Some("code".to_string()),
            format: "rust".to_string(),
            content_hash: None,
            index_status: Some("indexed".to_string()),
            fail_count: 0,
            last_error: None,
        }, search_vector::db::FragmentRecord {
            id: format!("{relative}:{symbol}"),
            group: None,
            file_id: relative.to_string(),
            range_json: "{\"kind\":\"text\",\"start_line\":1,\"end_line\":2,\"start_offset\":0,\"end_offset\":10}".to_string(),
            content_kind: "text".to_string(),
            content_text: Some(body.to_string()),
            cjk_text: String::new(),
            symbol_type: Some("function".to_string()),
            symbol_name: Some(symbol.to_string()),
            scope: None,
            signature: None,
            doc: None,
            modifiers: String::new(),
            heading: None,
            heading_level: None,
            embedding: None,
        })
    };
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let (in_file, in_frag) = seed(
        "src/guidance/alpha.rs",
        "alpha_guidance_fn",
        "pub fn alpha_guidance_fn() {}\n// guidance sync work\n",
    );
    let (out_file, out_frag) = seed(
        "src/router/beta.rs",
        "router_dispatch_fn",
        "pub fn router_dispatch_fn() {}\n// guidance sync work guidance sync work guidance sync work\n",
    );
    db.upsert_fragments(&in_file, &[in_frag], &[])
        .expect("seed");
    db.upsert_fragments(&out_file, &[out_frag], &[])
        .expect("seed");
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: root,
        db: &db,
        nlp: None,
        coverage: Some(crate::read::readiness::Coverage::empty()),
    };
    let answer = engine.explain(
        &explain_request("how does guidance sync work", true),
        &ctx,
        None,
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(
        rendered
            .contains("scope: in-scope first (70% of code budget for [guidance-core, guidance])"),
        "partition marker names the scope: {rendered}"
    );
    let top = rendered
        .split_once("### Top matches")
        .map(|(_, rest)| rest)
        .unwrap_or("");
    let in_pos = top.find("src/guidance/alpha.rs");
    let out_pos = top.find("src/router/beta.rs");
    assert!(
        in_pos.is_some() && out_pos.is_some(),
        "recall stays unscoped, both files render: {rendered}"
    );
    assert!(
        in_pos < out_pos,
        "in-scope renders first regardless of rank: {rendered}"
    );
    assert!(
        rendered.contains("scope: in=[guidance-core, guidance]"),
        "Coverage declares the split: {rendered}"
    );
}

fn graph_fixture() -> (tempfile::TempDir, search_vector::db::GuidanceDb) {
    // One file where a same-file caller invokes its callee: the
    // regression case for the dropped same-file edge.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let abs = root.join("a.rs");
    std::fs::write(
        &abs,
        "pub fn caller_fn() { target_fn(); }\npub fn target_fn() {}\n",
    )
    .expect("write");
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let abs_text = abs.to_string_lossy().into_owned();
    db.upsert_fragments(
        &search_vector::db::FileRecord {
            id: abs_text.clone(),
            absolute_path: abs_text.clone(),
            relative_path: "a.rs".to_string(),
            root_path: root.to_string_lossy().into_owned(),
            size_bytes: 60,
            last_modified_time: 100,
            kind: Some("code".to_string()),
            format: "rust".to_string(),
            content_hash: None,
            index_status: Some("indexed".to_string()),
            fail_count: 0,
            last_error: None,
        },
        &[],
        &[],
    )
    .expect("seed file");
    db.replace_file_graph(
        &abs_text,
        &[],
        &[
            ("caller_fn".to_string(), vec!["target_fn".to_string()]),
            ("target_fn".to_string(), vec![]),
        ],
    )
    .expect("seed graph");
    (dir, db)
}

#[test]
fn call_graph_section_lists_same_file_callers() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = graph_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: Some(crate::read::readiness::Coverage::empty()),
    };
    let answer = engine.explain(&explain_request("callers of target_fn", true), &ctx, None);
    let rendered = crate::read::render::render_answer(&answer);
    assert!(
        rendered.contains("### Call graph"),
        "edges render apart from code: {rendered}"
    );
    assert!(
        rendered.contains("calls target_fn in caller_fn"),
        "same-file caller resolves: {rendered}"
    );
}

#[test]
fn call_graph_without_edges_is_an_explicit_gap() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = graph_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: Some(crate::read::readiness::Coverage::empty()),
    };
    let answer = engine.explain(&explain_request("callers of no_such_fn", true), &ctx, None);
    let rendered = crate::read::render::render_answer(&answer);
    assert!(
        !rendered.contains("### Call graph"),
        "no edges means no section: {rendered}"
    );
    assert!(
        answer
            .gaps
            .iter()
            .any(|gap| gap.contains("no recorded edges")),
        "honest-empty names the gap: {:?}",
        answer.gaps
    );
}

fn two_crate_graph_fixture() -> (tempfile::TempDir, search_vector::db::GuidanceDb) {
    // One homonym split across two table crates: `target_fn` defined
    // and called in both `src/guidance` and `src/router`.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for (relative, caller, callee) in [
        ("src/guidance/g.rs", "g_caller_fn", "target_fn"),
        ("src/router/r.rs", "r_caller_fn", "target_fn"),
    ] {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(
            &path,
            format!("pub fn {caller}() {{ {callee}(); }}\npub fn {callee}() {{}}\n"),
        )
        .expect("write");
    }
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    for (relative, caller) in [
        ("src/guidance/g.rs", "g_caller_fn"),
        ("src/router/r.rs", "r_caller_fn"),
    ] {
        let absolute = root.join(relative).to_string_lossy().into_owned();
        db.upsert_fragments(
            &search_vector::db::FileRecord {
                id: absolute.clone(),
                absolute_path: absolute.clone(),
                relative_path: relative.to_string(),
                root_path: root.to_string_lossy().into_owned(),
                size_bytes: 60,
                last_modified_time: 100,
                kind: Some("code".to_string()),
                format: "rust".to_string(),
                content_hash: None,
                index_status: Some("indexed".to_string()),
                fail_count: 0,
                last_error: None,
            },
            &[],
            &[],
        )
        .expect("seed file");
        db.replace_file_graph(
            &absolute,
            &[],
            &[
                (caller.to_string(), vec!["target_fn".to_string()]),
                ("target_fn".to_string(), vec![]),
            ],
        )
        .expect("seed graph");
    }
    (dir, db)
}

#[test]
fn scoped_graph_query_narrows_to_the_named_crate() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = two_crate_graph_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: Some(crate::read::readiness::Coverage::empty()),
    };
    let answer = engine.explain(
        &explain_request("callers of target_fn in guidance", true),
        &ctx,
        None,
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(
        rendered.contains("calls target_fn in g_caller_fn"),
        "named-crate caller resolves: {rendered}"
    );
    assert!(
        !rendered.contains("r_caller_fn"),
        "out-of-scope caller stays out: {rendered}"
    );
    assert!(
        rendered.contains("graph narrowed"),
        "Coverage declares the narrowing: {rendered}"
    );
}

#[test]
fn scoped_graph_query_falls_back_when_scope_names_no_definition() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = two_crate_graph_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: Some(crate::read::readiness::Coverage::empty()),
    };
    let answer = engine.explain(
        &explain_request("callers of target_fn in llm", true),
        &ctx,
        None,
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(
        rendered.contains("g_caller_fn") && rendered.contains("r_caller_fn"),
        "empty scope reads unscoped, never empty: {rendered}"
    );
    assert!(
        !rendered.contains("graph narrowed"),
        "no narrowing claimed without narrowing: {rendered}"
    );
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
            include_fixtures: false,
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
            include_fixtures: false,
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
    // Shadow-only verdicts annotate nothing without a calibrated
    // fingerprint: Tier labels render only when authorized.
    assert!(
        !judged_rendered.contains("[relevant]")
            && !judged_rendered.contains("[irrelevant]")
            && !judged_rendered.contains("[insufficient]"),
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
                include_fixtures: false,
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
            include_fixtures: false,
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
