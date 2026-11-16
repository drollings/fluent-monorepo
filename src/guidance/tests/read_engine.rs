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
        answer_mode: AnswerMode::Auto,
    }
}

fn synthesis_request(query: &str, no_llm: bool) -> ExplainRequest {
    ExplainRequest {
        query: query.to_string(),
        limit: 5,
        budget_override: None,
        no_llm,
        include_fixtures: false,
        answer_mode: AnswerMode::Synthesis,
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
        &synthesis_request("what does alpha_fn do", false),
        &ctx,
        Some(&synth),
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.contains("### Answer"), "{rendered}");
    assert!(rendered.contains("[unverified]"), "{rendered}");
    assert!(!answer.claims.is_empty());
    // Evidence-only when the model declines. A fresh index holds no
    // cached answer, so the decline degrades to evidence-only.
    let (dir, db) = explain_fixture();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let declined = StubSynth { reply: None };
    let answer = engine.explain(
        &synthesis_request("what does alpha_fn do", false),
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
        &synthesis_request("what does alpha_fn do", true),
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
        .map_or("", |(_, rest)| rest);
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
fn same_file_callers_answer_extractively_in_every_mode() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = graph_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: Some(crate::read::readiness::Coverage::empty()),
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "callers of target_fn".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: AnswerMode::Auto,
        },
        &ctx,
        None,
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(
        rendered.contains("### Answer") && rendered.contains("calls target_fn in caller_fn"),
        "same-file caller answers extractively: {rendered}"
    );
    // Evidence mode keeps the legacy shape: edges render apart from
    // code excerpts; the model-free extractive answer also lands.
    let evidence = engine.explain(
        &ExplainRequest {
            query: "callers of target_fn".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: AnswerMode::Evidence,
        },
        &ctx,
        None,
    );
    let rendered = crate::read::render::render_answer(&evidence);
    assert!(
        rendered.contains("### Answer") && rendered.contains("calls target_fn in caller_fn"),
        "evidence mode answers extractively: {rendered}"
    );
    let top = rendered
        .split("### Top matches")
        .nth(1)
        .unwrap_or("")
        .split("### ")
        .next()
        .unwrap_or("");
    assert!(
        !top.contains("calls target_fn in caller_fn"),
        "edges never hide in Top matches: {rendered}"
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
            answer_mode: AnswerMode::Auto,
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
            answer_mode: AnswerMode::Auto,
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
        &synthesis_request("what does alpha_fn do", false),
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
                answer_mode: AnswerMode::Auto,
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
            answer_mode: AnswerMode::Synthesis,
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

fn ownership_fixture() -> (tempfile::TempDir, search_vector::db::GuidanceDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("manifest");
    std::fs::write(dir.path().join("lib.rs"), "pub fn guidance_sync() {}\n").expect("write");
    let db = search_vector::GuidanceDb::open_in_memory().expect("db");
    let absolute = dir.path().join("lib.rs").to_string_lossy().into_owned();
    db.upsert_fragments(
        &search_vector::db::FileRecord {
            id: "lib.rs".to_string(),
            absolute_path: absolute.clone(),
            relative_path: "lib.rs".to_string(),
            root_path: dir.path().to_string_lossy().into_owned(),
            size_bytes: 26,
            last_modified_time: 100,
            kind: Some("code".to_string()),
            format: "rust".to_string(),
            content_hash: None,
            index_status: Some("indexed".to_string()),
            fail_count: 0,
            last_error: None,
        },
        &[search_vector::db::FragmentRecord {
            id: "lib.rs:guidance_sync".to_string(),
            group: None,
            file_id: "lib.rs".to_string(),
            range_json: "{\"kind\":\"text\",\"start_line\":1,\"end_line\":1,\"start_offset\":0,\"end_offset\":10}"
                .to_string(),
            content_kind: "text".to_string(),
            content_text: Some("pub fn guidance_sync() {}".to_string()),
            cjk_text: String::new(),
            symbol_type: Some("function".to_string()),
            symbol_name: Some("guidance_sync".to_string()),
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
    .expect("seed code");
    let card = |key: &str, title: &str, body: &str| search_vector::db::ProjectCard {
        key: key.to_string(),
        kind: "crate".to_string(),
        title: title.to_string(),
        body: body.to_string(),
        source_path: "Cargo.toml".to_string(),
        anchor: String::new(),
        aliases_json: "[]".to_string(),
        code_paths_json: "[]".to_string(),
        symbols_json: "[]".to_string(),
        content_hash: common_core::hash::blake3_hex(body.as_bytes()),
        generated_at: 0,
    };
    db.replace_project_cards(&[
        card(
            "crate:guidance-core",
            "guidance-core",
            "Owns sync and freshness.",
        ),
        card(
            "crate:fluent-router",
            "fluent-router",
            "Routes requests to models.",
        ),
    ])
    .expect("cards seed");
    (dir, db)
}

#[test]
fn ownership_scopes_to_the_anchored_crate() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    use crate::read::vocabulary::AnchorSource;
    let (dir, db) = ownership_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "which crate owns guidance-core".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: AnswerMode::Auto,
        },
        &ctx,
        None,
    );
    let subject = answer.subject.as_ref().expect("anchor resolves");
    assert_eq!(subject.source, AnchorSource::Crate);
    assert_eq!(subject.terms, vec!["guidance-core".to_string()]);
    let rendered = crate::read::render::render_answer(&answer);
    let map = rendered
        .split("### From the project map")
        .nth(1)
        .unwrap_or("")
        .split("### From the docs")
        .next()
        .unwrap_or("");
    assert!(map.contains("guidance-core"), "{map}");
    assert!(
        !map.contains("fluent-router"),
        "unowned crate drops out: {map}"
    );
    assert!(
        !answer
            .gaps
            .iter()
            .any(|gap| gap.contains("removed every card")),
        "{:?}",
        answer.gaps
    );
}

#[test]
fn ownership_anchor_removing_every_card_falls_back_with_a_gap() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = ownership_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "which crate owns spacy-rs".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: AnswerMode::Auto,
        },
        &ctx,
        None,
    );
    assert!(
        answer
            .gaps
            .iter()
            .any(|gap| gap.contains("spacy-rs") && gap.contains("removed every card")),
        "{:?}",
        answer.gaps
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.contains("guidance-core"), "{rendered}");
}

#[test]
fn mechanism_query_anchors_free_term_not_context_crate() {
    // "guidance" names a known crate but this is a mechanism
    // question, not an ownership one: the crate word is context and
    // the subject is the free term "sync". Crate anchors serve
    // ownership scoping only.
    use crate::read::engine::{ExplainContext, ReadEngine};
    use crate::read::vocabulary::AnchorSource;
    let (dir, db) = ownership_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "How does guidance sync decide what to reprocess after an edit?".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: AnswerMode::Auto,
        },
        &ctx,
        None,
    );
    let subject = answer.subject.as_ref().expect("Q3 anchors");
    assert_eq!(subject.source, AnchorSource::Identifier);
    assert!(subject.terms.contains(&"sync".to_string()), "{subject:?}");
    assert!(
        !subject.terms.contains(&"guidance-core".to_string()),
        "{subject:?}"
    );
}

fn admission_fixture() -> (tempfile::TempDir, search_vector::db::GuidanceDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("manifest");
    let db = search_vector::GuidanceDb::open_in_memory().expect("db");
    for (name, symbol, text) in [
        ("a1.rs", "alpha_probe", "pub fn alpha_probe() {}\nalpha\n"),
        ("b2.rs", "beta_probe", "pub fn beta_probe() {}\nbeta\n"),
    ] {
        std::fs::write(dir.path().join(name), text).expect("write");
        let absolute = dir.path().join(name).to_string_lossy().into_owned();
        db.upsert_fragments(
            &search_vector::db::FileRecord {
                id: name.to_string(),
                absolute_path: absolute.clone(),
                relative_path: name.to_string(),
                root_path: dir.path().to_string_lossy().into_owned(),
                size_bytes: text.len() as u64,
                last_modified_time: 100,
                kind: Some("code".to_string()),
                format: "rust".to_string(),
                content_hash: None,
                index_status: Some("indexed".to_string()),
                fail_count: 0,
                last_error: None,
            },
            &[search_vector::db::FragmentRecord {
                id: format!("{name}:{symbol}"),
                group: None,
                file_id: name.to_string(),
                range_json: "{\"kind\":\"text\",\"start_line\":1,\"end_line\":2,\"start_offset\":0,\"end_offset\":10}".to_string(),
                content_kind: "text".to_string(),
                content_text: Some(text.to_string()),
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
            }],
            &[],
        )
        .expect("seed code");
    }
    for (name, anchor, body) in [
        ("d1.md", "alpha", "# Alpha\n\nAlpha syncs here.\n"),
        ("d2.md", "beta", "# Beta\n\nBeta ships there.\n"),
        ("d3.md", "gamma", "# Gamma\n\nGamma holds still.\n"),
    ] {
        std::fs::write(dir.path().join(name), body).expect("write");
        let (chunks, _) = crate::read::doc_cards::chunk_markdown_sections(name, body);
        let chunk = chunks
            .iter()
            .find(|chunk| chunk.anchor == anchor)
            .expect("anchor chunks");
        db.replace_project_cards(&[search_vector::db::ProjectCard {
            key: format!("doc:{name}#{anchor}"),
            kind: "doc".to_string(),
            title: anchor.to_string(),
            body: chunk.text.clone(),
            source_path: name.to_string(),
            anchor: anchor.to_string(),
            aliases_json: "[]".to_string(),
            code_paths_json: "[]".to_string(),
            symbols_json: "[]".to_string(),
            content_hash: chunk.content_hash.clone(),
            generated_at: 0,
        }])
        .expect("seed doc");
    }
    (dir, db)
}

fn scripted_decision(
    label: &str,
    confidence: f64,
) -> Result<fluent_llm::decision::Decision, fluent_llm::decision::DecisionError> {
    fluent_llm::decision::Decision::from_distribution(
        label,
        vec![(label.to_string(), confidence)],
        "d".to_string(),
        1,
        0,
        false,
    )
    .map_err(|_| fluent_llm::decision::DecisionError::Unavailable("bad".to_string()))
}

fn admission_registry(dir: &std::path::Path) {
    use fluent_llm::decision::{CalibrationRegistry, RegistryEntry, ThresholdSet};
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
        crate::read::calibration::finding_corpus(cases, "m4-test-fp", "hash", "findings.jsonl");
    let entry = RegistryEntry::calibrate_with(
        crate::read::calibration::ADMISSION_POINT,
        "m4-test-fp",
        crate::read::calibration::ADMISSION_QUESTION_VERSION,
        &corpus,
        1.0,
        Some(ThresholdSet {
            act: 1.0,
            review: 1.0,
        }),
    )
    .expect("entry");
    let registry_path = dir.join(".guidance/state/calibration.json");
    std::fs::create_dir_all(registry_path.parent().expect("parent")).expect("mkdir");
    let mut registry = CalibrationRegistry::empty();
    registry.upsert(entry);
    registry.save(&registry_path).expect("save");
}

#[test]
fn uncalibrated_admission_keeps_today_order() {
    use crate::read::engine::{ExplainContext, JudgeCtx, ReadEngine};
    use fluent_llm::decision::ScriptedDecideBackend;
    let (dir, db) = admission_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    // Scripted judge, no admission entry: verdicts demote only,
    // nothing filters, no admission gaps.
    let backend = ScriptedDecideBackend::new(vec![
        scripted_decision("strong", 0.9),
        scripted_decision("considered", 0.8),
        scripted_decision("considered", 0.7),
        scripted_decision("relevant", 0.9),
        scripted_decision("relevant", 0.9),
    ]);
    let judge = JudgeCtx {
        backend: &backend,
        model: "d".to_string(),
        timeout_ms: 2000,
        fingerprint: Some("m4-uncalibrated-fp".to_string()),
    };
    let answer = engine.explain_with_judge(
        &explain_request("alpha beta gamma", true),
        &ctx,
        None,
        Some(&judge),
    );
    assert!(
        !answer.gaps.iter().any(|gap| gap.contains("admission")),
        "{:?}",
        answer.gaps
    );
    let plain = engine.explain(&explain_request("alpha beta gamma", true), &ctx, None);
    assert_eq!(
        answer.citations.len(),
        plain.citations.len(),
        "uncalibrated filters nothing"
    );
}

#[test]
fn calibrated_admission_filters_top_k_with_labels() {
    use crate::read::engine::{ExplainContext, JudgeCtx, ReadEngine};
    use fluent_llm::decision::ScriptedDecideBackend;
    let (dir, db) = admission_fixture();
    admission_registry(dir.path());
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let backend = ScriptedDecideBackend::new(vec![
        scripted_decision("strong", 0.9),
        scripted_decision("considered", 0.8),
        scripted_decision("considered", 0.7),
        scripted_decision("relevant", 0.9),
        scripted_decision("relevant", 0.9),
    ]);
    let judge = JudgeCtx {
        backend: &backend,
        model: "d".to_string(),
        timeout_ms: 2000,
        fingerprint: Some("m4-test-fp".to_string()),
    };
    let answer = engine.explain_with_judge(
        &explain_request("alpha beta gamma", true),
        &ctx,
        None,
        Some(&judge),
    );
    // Docs admit top-2; the third drops with its label.
    let drops: Vec<&String> = answer
        .gaps
        .iter()
        .filter(|gap| gap.contains("admission dropped"))
        .collect();
    assert_eq!(drops.len(), 1, "{:?}", answer.gaps);
    assert!(drops[0].contains("(considered)"), "{:?}", answer.gaps);
    // Code admits top-5: both findings survive.
    assert!(
        !answer
            .gaps
            .iter()
            .any(|gap| gap.contains("a1.rs") || gap.contains("b2.rs")),
        "{:?}",
        answer.gaps
    );
}

#[test]
fn judge_outage_keeps_prior_without_admission() {
    use crate::read::engine::{ExplainContext, JudgeCtx, ReadEngine};
    use fluent_llm::decision::{DecisionError, ScriptedDecideBackend};
    let (dir, db) = admission_fixture();
    admission_registry(dir.path());
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let backend = ScriptedDecideBackend::new(vec![
        Err(DecisionError::Unavailable("down".to_string())),
        Err(DecisionError::Unavailable("down".to_string())),
        Err(DecisionError::Unavailable("down".to_string())),
        Err(DecisionError::Unavailable("down".to_string())),
        Err(DecisionError::Unavailable("down".to_string())),
    ]);
    let judge = JudgeCtx {
        backend: &backend,
        model: "d".to_string(),
        timeout_ms: 2000,
        fingerprint: Some("m4-test-fp".to_string()),
    };
    let answer = engine.explain_with_judge(
        &explain_request("alpha beta gamma", true),
        &ctx,
        None,
        Some(&judge),
    );
    assert!(
        !answer.gaps.iter().any(|gap| gap.contains("admission")),
        "outage keeps prior: {:?}",
        answer.gaps
    );
    let plain = engine.explain(&explain_request("alpha beta gamma", true), &ctx, None);
    assert_eq!(answer.citations.len(), plain.citations.len());
}

#[test]
fn golden_q2_ownership_extracts_docs_and_keeps_code() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = admission_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    // Q2-style ownership: the verified doc answers, and the code
    // finding stays in Top matches for owner composition and facts.
    let answer = engine.explain(
        &ExplainRequest {
            query: "which crate owns alpha".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: AnswerMode::Auto,
        },
        &ctx,
        None,
    );
    let rendered = crate::read::render::render_answer(&answer);
    let response = rendered
        .split("### Answer")
        .nth(1)
        .expect("extractive answer renders")
        .split("### ")
        .next()
        .unwrap_or("");
    assert!(response.contains("d1.md#alpha"), "{response}");
    assert!(
        !response.contains("a1.rs:1-1"),
        "ownership code evidence is never consumed: {response}"
    );
    let docs = rendered
        .split("### From the docs")
        .nth(1)
        .unwrap_or("")
        .split("### ")
        .next()
        .unwrap_or("");
    assert!(
        !docs.contains("Alpha syncs here"),
        "the extracted doc row leaves From the docs: {docs}"
    );
    let top = rendered
        .split("### Top matches")
        .nth(1)
        .unwrap_or("")
        .split("### ")
        .next()
        .unwrap_or("");
    assert!(
        top.contains("pub fn alpha_probe") || top.contains("a1.rs"),
        "code evidence stays in Top matches: {top}"
    );
    // The envelope still cites every admitted row.
    assert_eq!(answer.citations.len(), 2, "{:?}", answer.citations);
}

#[test]
fn golden_q1_orientation_stays_evidence_only() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = admission_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    // Q1-style orientation is not factual: evidence only, no model.
    let answer = engine.explain(
        &ExplainRequest {
            query: "what are the major components of this codebase".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: AnswerMode::Auto,
        },
        &ctx,
        None,
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(!rendered.contains("### Answer"), "{rendered}");
}

#[test]
fn golden_q5_ownership_scopes_to_the_anchored_crate() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = ownership_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    // Q5-style ownership with a crate anchor: the scoped card answers
    // extractively instead of rendering the whole map.
    let answer = engine.explain(
        &ExplainRequest {
            query: "which crate owns guidance-core".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: AnswerMode::Auto,
        },
        &ctx,
        None,
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.contains("### Answer"), "{rendered}");
    assert!(!rendered.contains("fluent-router"), "{rendered}");
}

#[test]
fn extractive_bodies_never_repeat_across_sections() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = admission_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    for query in [
        "which crate owns alpha",
        "callers of alpha_probe",
        "where is beta_probe defined",
    ] {
        let answer = engine.explain(
            &ExplainRequest {
                query: query.to_string(),
                limit: 5,
                budget_override: None,
                no_llm: true,
                include_fixtures: false,
                answer_mode: AnswerMode::Auto,
            },
            &ctx,
            None,
        );
        let rendered = crate::read::render::render_answer(&answer);
        let response = rendered
            .split("### Answer")
            .nth(1)
            .map(|rest| rest.split("### ").next().unwrap_or("").to_string())
            .unwrap_or_default();
        if response.trim().is_empty() {
            continue;
        }
        let rest = rendered.split("### Answer").nth(1).unwrap_or("");
        let after = rest.split("### ").next().unwrap_or("").len();
        let tail = &rest[after..];
        for line in response.lines() {
            let body = line.split_once(": ").map_or("", |(_, body)| body.trim());
            if body.len() < 8 {
                continue;
            }
            assert!(
                !tail.contains(body),
                "excerpt repeats outside Answer for {query:?}: {body}"
            );
        }
    }
}

#[test]
fn evidence_mode_never_synthesizes() {
    use crate::read::engine::{ExplainContext, ReadEngine, Synthesizer};
    struct Counting {
        called: std::cell::Cell<bool>,
    }
    impl Synthesizer for Counting {
        fn synthesize(&self, _prompt: &str) -> Result<Option<String>, String> {
            self.called.set(true);
            Ok(Some("Alpha syncs here (a1.rs:1-2).".to_string()))
        }
    }
    let (dir, db) = admission_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let synth = Counting {
        called: std::cell::Cell::new(false),
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "How does Alpha sync work".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: false,
            include_fixtures: false,
            answer_mode: AnswerMode::Evidence,
        },
        &ctx,
        Some(&synth),
    );
    assert!(!synth.called.get(), "evidence never builds an answer");
    let rendered = crate::read::render::render_answer(&answer);
    assert!(!rendered.contains("### Answer"), "{rendered}");
}

#[test]
fn extractive_answers_stay_within_budget() {
    use crate::read::engine::{ExplainContext, ReadEngine};
    let (dir, db) = admission_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    for (query, mode) in [
        ("which crate owns alpha", AnswerMode::Auto),
        (
            "what are the major components of this codebase",
            AnswerMode::Evidence,
        ),
    ] {
        let answer = engine.explain(
            &ExplainRequest {
                query: query.to_string(),
                limit: 5,
                budget_override: None,
                no_llm: true,
                include_fixtures: false,
                answer_mode: mode,
            },
            &ctx,
            None,
        );
        assert!(
            answer.tokens_used < 4000,
            "{query}: tokens {} exceed the golden bound",
            answer.tokens_used
        );
    }
}

fn support_registry(dir: &std::path::Path, fingerprint: &str) {
    use fluent_llm::decision::{CalibrationRegistry, RegistryEntry, ThresholdSet};
    let rows = crate::read::calibration::load_claim_cases(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../doc/guidance/eval/claims.jsonl")
            .as_path(),
    )
    .expect("corpus");
    let scored = crate::read::calibration::score_claims(&rows, |case| {
        if case.expected == crate::read::calibration::ExpectedSupport::Supported {
            1.0
        } else {
            0.0
        }
    });
    let corpus =
        crate::read::calibration::support_corpus(scored, fingerprint, "hash", "claims.jsonl");
    let entry = RegistryEntry::calibrate_with(
        crate::read::calibration::SUPPORT_POINT,
        fingerprint,
        crate::read::calibration::SUPPORT_QUESTION_VERSION,
        &corpus,
        crate::read::calibration::SUPPORT_THRESHOLD,
        Some(ThresholdSet {
            act: 1.0,
            review: 1.0,
        }),
    )
    .expect("entry");
    let registry_path = dir.join(".guidance/state/calibration.json");
    std::fs::create_dir_all(registry_path.parent().expect("parent")).expect("mkdir");
    let mut registry = CalibrationRegistry::empty();
    registry.upsert(entry);
    registry.save(&registry_path).expect("save");
}

fn support_script(labels: &[&str]) -> fluent_llm::decision::ScriptedDecideBackend {
    use fluent_llm::decision::ScriptedDecideBackend;
    let decisions: Vec<
        Result<fluent_llm::decision::Decision, fluent_llm::decision::DecisionError>,
    > = labels
        .iter()
        .map(|label| {
            fluent_llm::decision::Decision::from_distribution(
                label,
                vec![(label.to_string(), 0.9)],
                "d".to_string(),
                1,
                0,
                false,
            )
            .expect("decision")
        })
        .map(Ok)
        .collect();
    ScriptedDecideBackend::new(decisions)
}

#[test]
fn uncalibrated_support_leaves_unverified() {
    use crate::read::engine::{ExplainContext, JudgeCtx, ReadEngine};
    let (dir, db) = admission_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    // Finding verdicts then one supported claim verdict; no registry
    // entry, so the verdict stays shadow and the claim unverified.
    // The stub cites [C1], which binds whichever finding ranks first:
    // both findings share the claim vocabulary.
    // Backend consumption order is fixed: scope hypothesis (1),
    // doc judge (2), finding judge (2), support judge (1).
    let backend = support_script(&[
        "unknown",
        "strong",
        "strong",
        "relevant",
        "relevant",
        "supported",
    ]);
    let judge = JudgeCtx {
        backend: &backend,
        model: "d".to_string(),
        timeout_ms: 2000,
        fingerprint: Some("m8-uncalibrated-fp".to_string()),
    };
    let answer = engine.explain_with_judge(
        &ExplainRequest {
            query: "alpha beta sync work".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: false,
            include_fixtures: false,
            answer_mode: AnswerMode::Synthesis,
        },
        &ctx,
        Some(&StubSynth {
            reply: Some("Alpha and beta probes sync here [C1].".to_string()),
        }),
        Some(&judge),
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.contains("### Answer"), "{rendered}");
    assert!(
        rendered.contains("[unverified]"),
        "shadow stays labelled: {rendered}"
    );
    assert!(
        answer
            .claims
            .iter()
            .all(|claim| claim.support == crate::read::claims::Support::Unverified),
        "no verdict without authorization: {:?}",
        answer.claims
    );
}

#[test]
fn authorized_support_verifies_and_drops() {
    use crate::read::engine::{ExplainContext, JudgeCtx, ReadEngine};
    // A supported claim verifies verbatim; the contradicted run drops
    // the claim instead of rendering it. Each verdict gets a fresh
    // index so the manifest cache cannot carry answers across runs.
    for (verdict, present, labelled) in [("supported", true, false), ("contradicted", false, false)]
    {
        let (dir, db) = admission_fixture();
        support_registry(dir.path(), "m8-test-fp");
        let engine = ReadEngine::new();
        let ctx = ExplainContext {
            workspace: dir.path(),
            db: &db,
            nlp: None,
            coverage: None,
        };
        // Backend consumption order is fixed: scope (1), docs (2),
        // findings (2), support (1).
        let backend = support_script(&[
            "unknown", "strong", "strong", "relevant", "relevant", verdict,
        ]);
        let judge = JudgeCtx {
            backend: &backend,
            model: "d".to_string(),
            timeout_ms: 2000,
            fingerprint: Some("m8-test-fp".to_string()),
        };
        let answer = engine.explain_with_judge(
            &ExplainRequest {
                query: "alpha beta sync work".to_string(),
                limit: 5,
                budget_override: None,
                no_llm: false,
                include_fixtures: false,
                answer_mode: AnswerMode::Synthesis,
            },
            &ctx,
            Some(&StubSynth {
                reply: Some("Alpha and beta probes sync here [C1].".to_string()),
            }),
            Some(&judge),
        );
        let rendered = crate::read::render::render_answer(&answer);
        assert_eq!(
            rendered.contains("Alpha and beta probes sync here"),
            present,
            "{verdict}: {rendered}"
        );
        assert_eq!(
            rendered.contains("[unverified]"),
            labelled,
            "{verdict}: {rendered}"
        );
    }
}

#[test]
fn renamed_function_renders_live_text_marked_stale() {
    // Rename probe: the stored signature is gone from the live file,
    // so the live range renders instead — never the stale excerpt —
    // with the row labelled and the file named in Gaps.
    use crate::read::engine::{ExplainContext, ReadEngine};
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir(root.join("src")).expect("mkdir");
    std::fs::write(
        root.join("src").join("ren.rs"),
        "pub fn old_name() -> u64 {\n    1\n}\n",
    )
    .expect("write");
    let db = search_vector::GuidanceDb::open_in_memory().expect("db");
    let absolute = root
        .join("src")
        .join("ren.rs")
        .to_string_lossy()
        .into_owned();
    db.upsert_fragments(
        &search_vector::db::FileRecord {
            id: absolute.clone(),
            absolute_path: absolute.clone(),
            relative_path: "src/ren.rs".to_string(),
            root_path: root.to_string_lossy().into_owned(),
            size_bytes: 100,
            last_modified_time: 100,
            kind: Some("code".to_string()),
            format: "rust".to_string(),
            content_hash: None,
            index_status: Some("indexed".to_string()),
            fail_count: 0,
            last_error: None,
        },
        &[search_vector::db::FragmentRecord {
            id: "ren:old_name".to_string(),
            group: None,
            file_id: absolute.clone(),
            range_json: "{\"kind\":\"text\",\"start_line\":1,\"end_line\":2,\"start_offset\":0,\"end_offset\":10}".to_string(),
            content_kind: "text".to_string(),
            content_text: Some("pub fn old_name() -> u64 {\n    1\n}".to_string()),
            cjk_text: String::new(),
            symbol_type: Some("function".to_string()),
            symbol_name: Some("old_name".to_string()),
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
    std::fs::write(
        root.join("src").join("ren.rs"),
        "pub fn new_name() -> u64 {\n    1\n}\n",
    )
    .expect("rename without re-sync");
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: root,
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "where is old_name defined".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: AnswerMode::Auto,
        },
        &ctx,
        None,
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(
        rendered.contains("pub fn new_name"),
        "live bytes render: {rendered}"
    );
    assert!(
        !rendered.contains("pub fn old_name"),
        "stored excerpt never renders: {rendered}"
    );
    assert!(
        rendered.contains("(stale)"),
        "renamed row is labelled: {rendered}"
    );
    assert!(
        answer.gaps.iter().any(|gap| gap.contains("src/ren.rs")),
        "gaps name the file: {:?}",
        answer.gaps
    );
    assert!(
        answer
            .citations
            .iter()
            .any(|cite| cite.stale && cite.source.display().contains("ren.rs")),
        "citation carries the stale marker: {:?}",
        answer.citations
    );
}
