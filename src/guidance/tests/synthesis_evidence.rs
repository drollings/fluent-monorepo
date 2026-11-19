//! Evidence-complete synthesis: the prompt carries the project
//! map, verified doc excerpts, and code findings with citable ids;
//! answers bind to shown evidence and refuse only with no support.
use crate::read::engine::{ExplainContext, ExplainRequest, ReadEngine};
use crate::tests::common::{db_file_record, db_project_card, db_text_fragment};

/// Workspace with a manifest, one code file, and one doc file, all
/// indexed plus project cards (workspace card + verified doc card).
fn evidence_workspace() -> (tempfile::TempDir, crate::query::search_db::GuidanceSearchDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("write");
    std::fs::write(dir.path().join("lib.rs"), "pub fn alpha_sync() {}\n").expect("write");
    std::fs::create_dir(dir.path().join("doc")).expect("mkdir");
    let doc_text = "# Alpha\n\nAlpha sync engine overview.\n";
    std::fs::write(dir.path().join("doc").join("alpha.md"), doc_text).expect("write");
    let db = crate::query::search_db::GuidanceSearchDb::open_in_memory().expect("db");
    db.upsert_fragments(
        &db_file_record(dir.path(), "lib.rs"),
        &[db_text_fragment(
            "lib.rs",
            "alpha_sync",
            "pub fn alpha_sync() {}",
            1,
        )],
        &[],
    )
    .expect("seed");
    let (chunks, _) = crate::read::doc_cards::chunk_markdown_sections("doc/alpha.md", doc_text);
    let chunk = chunks.first().expect("doc chunks");
    db.replace_project_cards(&[
        db_project_card(
            "workspace:ws",
            "workspace",
            "Workspace",
            "Ships Coral Router and Guidance.",
            "Cargo.toml",
            "",
        ),
        crate::query::search_db::ProjectCard {
            content_hash: chunk.content_hash.clone(),
            ..db_project_card(
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
fn prompt_uses_self_describing_tokens_only() {
    // Verification-first protocol: every evidence row carries a
    // `[Cn] path:start-end` or `[Dn] path#anchor` token; opaque
    // `[card-doc-N]`, `[card-map-N]`, and `[symbol:...]` ids are gone.
    // The token-bound answer resolves through the manifest.
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
            answer_mode: crate::read::engine::AnswerMode::Synthesis,
        },
        &ctx,
        Some(&PromptAsserter {
            wants: vec!["[C1] lib.rs:1-1".to_string(), "[D1] Cargo.toml".to_string()],
            forbids: vec![
                "[card-doc-".to_string(),
                "[card-map-".to_string(),
                "[symbol:".to_string(),
            ],
            answer: "Alpha syncs here [C1]. The workspace ships both [D1].".to_string(),
        }),
    );
    assert!(
        answer.claims.iter().all(|claim| claim.citation.is_some()),
        "token-bound sentences resolve: {:?}",
        answer.claims
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.contains("### Answer"), "{rendered}");
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
            answer_mode: crate::read::engine::AnswerMode::Synthesis,
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
    let db = crate::query::search_db::GuidanceSearchDb::open_in_memory().expect("db");
    db.replace_project_cards(&[db_project_card(
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
            answer_mode: crate::read::engine::AnswerMode::Synthesis,
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
            answer_mode: crate::read::engine::AnswerMode::Synthesis,
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
            answer_mode: crate::read::engine::AnswerMode::Synthesis,
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
    let db = crate::query::search_db::GuidanceSearchDb::open_in_memory().expect("db");
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
            answer_mode: crate::read::engine::AnswerMode::Synthesis,
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
            answer_mode: crate::read::engine::AnswerMode::Synthesis,
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
            answer_mode: crate::read::engine::AnswerMode::Synthesis,
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
        timeout: std::time::Duration::from_secs(2),
        fingerprint: None,
    };
    let answer = engine.explain_with_judge(
        &ExplainRequest {
            query: "How does Alpha sync work".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: crate::read::engine::AnswerMode::Synthesis,
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

#[test]
fn synthesis_mode_preserves_the_prose_path() {
    // Escape hatch and regression comparator: `Synthesis` answers
    // prose over admitted evidence exactly like before, with the
    // support label and the full later sections intact.
    let (dir, db) = evidence_workspace();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    struct Prose;
    impl crate::read::engine::Synthesizer for Prose {
        fn synthesize(&self, _prompt: &str) -> Result<Option<String>, String> {
            Ok(Some("Alpha syncs here (lib.rs:1-1).".to_string()))
        }
    }
    let answer = engine.explain(
        &ExplainRequest {
            query: "How does Alpha sync work".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: false,
            include_fixtures: false,
            answer_mode: crate::read::engine::AnswerMode::Synthesis,
        },
        &ctx,
        Some(&Prose),
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.contains("### Answer"), "{rendered}");
    assert!(rendered.contains("Alpha syncs here"), "{rendered}");
    assert!(rendered.contains("[unverified]"), "{rendered}");
    assert!(
        rendered.contains("pub fn alpha_sync() {}"),
        "used rows stay in Top matches on the prose path: {rendered}"
    );
}

struct CountingSynth {
    calls: std::cell::Cell<u32>,
}

impl crate::read::engine::Synthesizer for CountingSynth {
    fn synthesize(&self, prompt: &str) -> Result<Option<String>, String> {
        assert!(prompt.contains("Evidence:"), "{prompt}");
        self.calls.set(self.calls.get() + 1);
        Ok(Some("Alpha syncs here (lib.rs:1-1).".to_string()))
    }
}

fn cached_workspace() -> (tempfile::TempDir, crate::query::search_db::GuidanceSearchDb) {
    // Two verified docs plus one code finding: the synthesis arm has
    // something to key on, and an edit soils exactly one row.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("write");
    std::fs::write(dir.path().join("lib.rs"), "pub fn alpha_sync() {}\n").expect("write");
    std::fs::write(
        dir.path().join("doc_one.md"),
        "# One\n\nAlpha syncs here.\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("doc_two.md"),
        "# Two\n\nAlpha syncs twice.\n",
    )
    .expect("write");
    let db = crate::query::search_db::GuidanceSearchDb::open_in_memory().expect("db");
    db.upsert_fragments(
        &db_file_record(dir.path(), "lib.rs"),
        &[db_text_fragment(
            "lib.rs",
            "alpha_sync",
            "pub fn alpha_sync() {}",
            1,
        )],
        &[],
    )
    .expect("seed");
    for (name, text) in [
        ("doc_one.md", "# One\n\nAlpha syncs here.\n"),
        ("doc_two.md", "# Two\n\nAlpha syncs twice.\n"),
    ] {
        let (chunks, _) = crate::read::doc_cards::chunk_markdown_sections(name, text);
        let chunk = chunks.first().expect("doc chunks");
        db.replace_project_cards(&[crate::query::search_db::ProjectCard {
            content_hash: chunk.content_hash.clone(),
            ..db_project_card(
                &format!("doc:{name}#{}", chunk.anchor),
                "doc",
                name,
                &chunk.text,
                name,
                &chunk.anchor,
            )
        }])
        .expect("cards seed");
    }
    (dir, db)
}

fn how_to_request(query: &str) -> ExplainRequest {
    ExplainRequest {
        query: query.to_string(),
        limit: 5,
        budget_override: None,
        no_llm: false,
        include_fixtures: false,
        answer_mode: crate::read::engine::AnswerMode::Synthesis,
    }
}

#[test]
fn manifest_cache_serves_identical_bytes_without_a_second_call() {
    // Same evidence yields the same bytes: the second identical run
    // is byte-identical modulo latency, with zero model calls.
    let (dir, db) = cached_workspace();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let synth = CountingSynth {
        calls: std::cell::Cell::new(0),
    };
    let first = engine.explain(
        &how_to_request("How does Alpha sync work"),
        &ctx,
        Some(&synth),
    );
    assert_eq!(synth.calls.get(), 1, "miss synthesizes once");
    assert!(!first.cache_hit);
    let second = engine.explain(
        &how_to_request("How does Alpha sync work"),
        &ctx,
        Some(&synth),
    );
    assert_eq!(synth.calls.get(), 1, "hit makes zero model calls");
    assert!(second.cache_hit);
    let rendered = crate::read::render::render_answer(&first);
    let rendered_again = crate::read::render::render_answer(&second);
    assert_eq!(rendered, rendered_again, "byte-identical modulo latency");
    assert!(rendered.contains("### Answer"), "{rendered}");
    // The stored support status rides along with the hit.
    assert_eq!(first.claims, second.claims);
}

#[test]
fn edited_source_misses_the_manifest() {
    // Changed source always wins: editing the cited file changes the
    // manifest, so the next run synthesizes again.
    let (dir, db) = cached_workspace();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let synth = CountingSynth {
        calls: std::cell::Cell::new(0),
    };
    let _ = engine.explain(
        &how_to_request("How does Alpha sync work"),
        &ctx,
        Some(&synth),
    );
    assert_eq!(synth.calls.get(), 1);
    std::fs::write(dir.path().join("lib.rs"), "pub fn alpha_sync() {  }\n")
        .expect("edit inside the cited span without re-sync");
    let _ = engine.explain(
        &how_to_request("How does Alpha sync work"),
        &ctx,
        Some(&synth),
    );
    assert_eq!(synth.calls.get(), 2, "edited bytes miss");
}

#[test]
fn manifest_hash_keys_evidence_model_and_prompt_version() {
    // One hash protocol: same inputs hash identically, and any of
    // evidence, model, or prompt version rotates the key.
    let (dir, db) = cached_workspace();
    let hits = crate::read::planes::docs::retrieve_doc_sections(
        "How does Alpha sync work",
        Some(dir.path()),
        &db,
        5,
    );
    let cards = vec![];
    let findings: Vec<crate::read::engine::Finding> = Vec::new();
    let base =
        crate::grounding::evidence_manifest_hash(&findings, &hits, &cards, "code", 1, "auto");
    let again =
        crate::grounding::evidence_manifest_hash(&findings, &hits, &cards, "code", 1, "auto");
    assert_eq!(base, again, "deterministic");
    assert_ne!(
        base,
        crate::grounding::evidence_manifest_hash(&findings, &hits, &cards, "other", 1, "auto"),
        "model rotation misses"
    );
    assert_ne!(
        base,
        crate::grounding::evidence_manifest_hash(&findings, &hits, &cards, "code", 2, "auto"),
        "prompt bump misses"
    );
    let changed: Vec<crate::read::planes::docs::DocHit> = hits
        .iter()
        .map(|hit| {
            let mut hit = hit.clone();
            hit.body = Some("changed bytes".to_string());
            hit
        })
        .collect();
    assert_ne!(
        base,
        crate::grounding::evidence_manifest_hash(&findings, &changed, &cards, "code", 1, "auto"),
        "edited evidence misses"
    );
}

#[test]
fn answer_cache_table_roundtrips_on_open() {
    // Old databases gain the table on open (IF NOT EXISTS); the
    // generation guard still refuses a mismatched pair.
    let db = crate::query::search_db::GuidanceSearchDb::open_in_memory().expect("db");
    let row = crate::query::search_db::AnswerCacheRecord {
        manifest_hash: "abc".to_string(),
        answer: "Alpha syncs here (lib.rs:1-1) [unverified]".to_string(),
        claims_json: "[]".to_string(),
        citations_json: "[\"lib.rs:1-1\"]".to_string(),
        support: "unverified".to_string(),
        model: "code".to_string(),
        prompt_version: 1,
        generation: "g1".to_string(),
        created_at: 0,
    };
    db.answer_cache_put(&row).expect("put");
    let back = db.answer_cache_get("abc", "g1").expect("get").expect("row");
    assert_eq!(back, row);
    assert!(db.answer_cache_get("missing", "g1").expect("get").is_none());
    // A generation change invalidates the row before it is served.
    assert!(db.answer_cache_get("abc", "g2").expect("get").is_none());
    assert!(
        db.answer_cache_get("abc", "g1").expect("get").is_none(),
        "the stale row was dropped"
    );
    db.answer_cache_put(&row).expect("put again");
    assert_eq!(db.answer_cache_invalidate("abc").expect("invalidate"), 1);
    assert!(db.answer_cache_get("abc", "g1").expect("get").is_none());
}
