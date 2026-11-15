//! Evidence-complete synthesis: the prompt carries the project
//! map, verified doc excerpts, and code findings with citable ids;
//! answers bind to shown evidence and refuse only with no support.
use crate::read::engine::{ExplainContext, ExplainRequest, ReadEngine};

fn file_record(workspace: &std::path::Path, name: &str) -> search_vector::db::FileRecord {
    let absolute = workspace.join(name);
    let meta = std::fs::metadata(&absolute).expect("seeded file reads");
    search_vector::db::FileRecord {
        id: name.to_string(),
        absolute_path: absolute.to_string_lossy().into_owned(),
        relative_path: name.to_string(),
        root_path: workspace.to_string_lossy().into_owned(),
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
    }
}

fn code_fragment(file_id: &str, symbol: &str, text: &str) -> search_vector::db::FragmentRecord {
    search_vector::db::FragmentRecord {
        id: format!("{file_id}:{symbol}"),
        group: None,
        file_id: file_id.to_string(),
        range_json: "{\"kind\":\"text\",\"start_line\":1,\"end_line\":1,\"start_offset\":0,\"end_offset\":5}"
            .to_string(),
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
    }
}

fn project_card(
    key: &str,
    kind: &str,
    title: &str,
    body: &str,
    source_path: &str,
    anchor: &str,
) -> search_vector::db::ProjectCard {
    search_vector::db::ProjectCard {
        key: key.to_string(),
        kind: kind.to_string(),
        title: title.to_string(),
        body: body.to_string(),
        source_path: source_path.to_string(),
        anchor: anchor.to_string(),
        aliases_json: "[]".to_string(),
        code_paths_json: "[]".to_string(),
        symbols_json: "[]".to_string(),
        content_hash: common_core::hash::blake3_hex(body.as_bytes()),
        generated_at: 0,
    }
}

/// Workspace with a manifest, one code file, and one doc file, all
/// indexed plus project cards (workspace card + verified doc card).
fn evidence_workspace() -> (tempfile::TempDir, search_vector::db::GuidanceDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("write");
    std::fs::write(dir.path().join("lib.rs"), "pub fn alpha_sync() {}\n").expect("write");
    std::fs::create_dir(dir.path().join("doc")).expect("mkdir");
    let doc_text = "# Alpha\n\nAlpha sync engine overview.\n";
    std::fs::write(dir.path().join("doc").join("alpha.md"), doc_text).expect("write");
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    db.upsert_fragments(
        &file_record(dir.path(), "lib.rs"),
        &[code_fragment(
            "lib.rs",
            "alpha_sync",
            "pub fn alpha_sync() {}",
        )],
        &[],
    )
    .expect("seed");
    let (chunks, _) = crate::read::doc_cards::chunk_markdown_sections("doc/alpha.md", doc_text);
    let chunk = chunks.first().expect("doc chunks");
    db.replace_project_cards(&[
        project_card(
            "workspace:ws",
            "workspace",
            "Workspace",
            "Ships Coral Router and Guidance.",
            "Cargo.toml",
            "",
        ),
        search_vector::db::ProjectCard {
            content_hash: chunk.content_hash.clone(),
            ..project_card(
                "doc:doc/alpha.md#alpha",
                "doc",
                "Alpha",
                &chunk.text,
                "doc/alpha.md",
                &chunk.anchor,
            )
        },
    ])
    .expect("cards seed");
    assert_eq!(chunk.anchor, "alpha", "anchor shape");
    (dir, db)
}

struct PromptAsserter {
    wants: Vec<String>,
    forbids: Vec<String>,
    answer: String,
}

impl crate::read::engine::Synthesizer for PromptAsserter {
    fn synthesize(&self, prompt: &str) -> Result<Option<String>, String> {
        for want in &self.wants {
            assert!(
                prompt.contains(want.as_str()),
                "prompt covers {want}: {prompt}"
            );
        }
        for forbid in &self.forbids {
            assert!(
                !prompt.contains(forbid.as_str()),
                "prompt omits {forbid}: {prompt}"
            );
        }
        Ok(Some(self.answer.clone()))
    }
}

#[test]
fn prompt_covers_map_and_verified_doc_and_code() {
    // The model sees verbatim excerpts from every evidence kind, so a
    // project-map or doc-backed answer is grounded, not recalled.
    let (dir, db) = evidence_workspace();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "How does Alpha sync work".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: false,
            include_fixtures: false,
        },
        &ctx,
        Some(&PromptAsserter {
            wants: vec![
                "Ships Coral Router and Guidance.".to_string(),
                "Alpha sync engine overview.".to_string(),
                "pub fn alpha_sync() {}".to_string(),
            ],
            forbids: vec![],
            answer: "Alpha syncs here (lib.rs:1-1).".to_string(),
        }),
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.contains("### Answer"), "{rendered}");
}

#[test]
fn orientation_answer_comes_from_project_map() {
    // Cards-only evidence still synthesizes: the map answers when code
    // recall is empty, instead of refusing for lack of code findings.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("write");
    std::fs::write(dir.path().join("lib.rs"), "pub fn unrelated_quux() {}\n").expect("write");
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    db.replace_project_cards(&[project_card(
        "workspace:ws",
        "workspace",
        "Workspace",
        "Ships Coral Router and Guidance.",
        "Cargo.toml",
        "",
    )])
    .expect("cards seed");
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "What are the major components of this codebase".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: false,
            include_fixtures: false,
        },
        &ctx,
        Some(&PromptAsserter {
            wants: vec!["Ships Coral Router and Guidance.".to_string()],
            forbids: vec!["### Code".to_string()],
            answer: "The workspace ships Coral Router and Guidance (Cargo.toml).".to_string(),
        }),
    );
    // Map-only evidence: no code section in the prompt, yet the map
    // still answers instead of refusing for empty code findings.
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.contains("### Answer"), "{rendered}");
    assert!(rendered.contains("Coral Router"), "{rendered}");
}

#[test]
fn clean_safety_answer_survives_gating() {
    // A code-backed sentence keeps its answer section with the support
    // label; gating never refuses a bound claim.
    let (dir, db) = evidence_workspace();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    struct Bound;
    impl crate::read::engine::Synthesizer for Bound {
        fn synthesize(&self, _prompt: &str) -> Result<Option<String>, String> {
            Ok(Some("Alpha sync runs here (lib.rs:1-1).".to_string()))
        }
    }
    let answer = engine.explain(
        &ExplainRequest {
            query: "How does Alpha sync work".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: false,
            include_fixtures: false,
        },
        &ctx,
        Some(&Bound),
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.contains("### Answer"), "{rendered}");
    assert!(rendered.contains("[unverified]"), "{rendered}");
}

#[test]
fn unshown_sync_citation_cannot_render() {
    // A sentence citing a path outside the shown set constrains to the
    // abstain template: the unbindable claim never renders.
    let (dir, db) = evidence_workspace();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    struct Invented;
    impl crate::read::engine::Synthesizer for Invented {
        fn synthesize(&self, _prompt: &str) -> Result<Option<String>, String> {
            Ok(Some("Sync lives in src/other/sync.rs:1-5.".to_string()))
        }
    }
    let answer = engine.explain(
        &ExplainRequest {
            query: "How does Alpha sync work".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: false,
            include_fixtures: false,
        },
        &ctx,
        Some(&Invented),
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(!rendered.contains("src/other/sync.rs"), "{rendered}");
    assert!(rendered.contains("lib.rs:1-1"), "{rendered}");
}

#[test]
fn refusal_needs_no_supporting_evidence() {
    // With nothing shown, synthesis is never attempted: the answer
    // refuses with the coverage gap, and the model is not consulted.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    struct Never;
    impl crate::read::engine::Synthesizer for Never {
        fn synthesize(&self, _prompt: &str) -> Result<Option<String>, String> {
            panic!("no evidence means no synthesis");
        }
    }
    let answer = engine.explain(
        &ExplainRequest {
            query: "qxvzxb thrum".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: false,
            include_fixtures: false,
        },
        &ctx,
        Some(&Never),
    );
    assert!(
        answer
            .sections
            .iter()
            .all(|section| section.title != "Answer"),
        "refusal renders no answer section"
    );
    assert!(
        answer.gaps.iter().any(|gap| gap.contains("none found")),
        "refusal names the gap: {:?}",
        answer.gaps
    );
}

#[test]
fn synthesis_failure_renders_gap_without_answer() {
    // An unreachable synthesizer is evidence-only: the error lands in
    // gaps, never as an "(unavailable: ...)" answer body.
    let (dir, db) = evidence_workspace();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    struct Down;
    impl crate::read::engine::Synthesizer for Down {
        fn synthesize(&self, _prompt: &str) -> Result<Option<String>, String> {
            Err("transport unavailable".to_string())
        }
    }
    let answer = engine.explain(
        &ExplainRequest {
            query: "How does Alpha sync work".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: false,
            include_fixtures: false,
        },
        &ctx,
        Some(&Down),
    );
    assert!(
        answer
            .sections
            .iter()
            .all(|section| section.title != "Answer"),
        "failed synthesis renders no answer section"
    );
    assert!(
        answer
            .gaps
            .iter()
            .any(|gap| gap.contains("synthesis unavailable")),
        "failure names the gap: {:?}",
        answer.gaps
    );
}

#[test]
fn empty_capability_corpus_renders_gap_without_section() {
    // Zero capability rows means no section, only a population gap.
    let (dir, db) = evidence_workspace();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "How does Alpha sync work".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
        },
        &ctx,
        None,
    );
    assert!(
        answer
            .sections
            .iter()
            .all(|section| section.title != "From the capabilities"),
        "empty corpus renders no capabilities section"
    );
    assert!(
        answer
            .gaps
            .iter()
            .any(|gap| gap.contains("capability corpus unpopulated")),
        "empty corpus names the gap: {:?}",
        answer.gaps
    );
}

#[test]
fn unauthorized_judge_labels_do_not_render() {
    // Shadow-only verdicts annotate nothing: without a calibrated
    // fingerprint the Top matches render plain, with no tier labels.
    use fluent_llm::decision::{Decision, ScriptedDecideBackend};
    let (dir, db) = evidence_workspace();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let decision = Decision::from_distribution(
        "irrelevant",
        vec![("irrelevant".to_string(), 0.8)],
        "decision".to_string(),
        1,
        3,
        false,
    )
    .expect("decision");
    let backend = ScriptedDecideBackend::canned(decision);
    let judge = crate::read::engine::JudgeCtx {
        backend: &backend,
        model: "decision".to_string(),
        timeout_ms: 2000,
        fingerprint: None,
    };
    let answer = engine.explain_with_judge(
        &ExplainRequest {
            query: "How does Alpha sync work".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
        },
        &ctx,
        None,
        Some(&judge),
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(!rendered.contains("[irrelevant]"), "{rendered}");
    assert!(!rendered.contains("[insufficient]"), "{rendered}");
    assert!(rendered.contains("alpha_sync"), "{rendered}");
}
