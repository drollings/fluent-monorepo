//! Envelope fields on exact, homonym, and missing-symbol fixtures.

use super::{citations_of, fit_stages_to_budget, gaps_of, AnswerEnvelope, ConfidenceAxis};
use crate::query::strategy::{FsmEngine, QueryIntent};
use crate::query::synthesize::Stage;
use fluent_types::StageKind;

fn code_stage(source: &str, line: u32, name: &str) -> Stage {
    Stage {
        kind: StageKind::Code,
        content: format!("fn {name}()"),
        source: source.to_string(),
        line: Some(line),
        end_line: None,
        member_name: Some(name.to_string()),
        member_type: None,
        trace: None,
    }
}

fn prose_stage(content: &str) -> Stage {
    Stage {
        kind: StageKind::Prose,
        content: content.to_string(),
        source: "doc.md".to_string(),
        line: None,
        end_line: None,
        member_name: None,
        member_type: None,
        trace: None,
    }
}

fn classify(query: &str) -> crate::query::strategy::QueryClass {
    FsmEngine::new().run(query)
}

#[test]
fn exact_query_carries_cited_task_certified_envelope() {
    let class = classify("helloWorld");
    let stages = vec![code_stage("src/test.zig", 1, "helloWorld")];
    let envelope = AnswerEnvelope::from_stages(&class, "word_index", stages);
    assert_eq!(envelope.intent, QueryIntent::SingleIdentifier);
    assert_eq!(envelope.citations, vec!["src/test.zig:1".to_string()]);
    assert!(envelope.gaps.is_empty());
    assert!(envelope.is_answerable());
    assert_eq!(envelope.confidence.axis, ConfidenceAxis::TaskCertified);
    assert!(envelope.resolution.contains("word_index"));
    let rendered = envelope.render_markdown();
    assert!(rendered.contains("src/test.zig:1"));
}

#[test]
fn homonym_members_raise_an_ambiguity_gap() {
    let class = classify("render");
    let stages = vec![
        code_stage("src/a.rs", 3, "render"),
        code_stage("src/b.rs", 9, "render"),
    ];
    let envelope = AnswerEnvelope::from_stages(&class, "word_index", stages);
    assert!(envelope
        .gaps
        .iter()
        .any(|gap| gap.contains("ambiguous: 2 candidates")));
    assert!(envelope.is_answerable());
    assert_eq!(envelope.citations.len(), 2);
}

#[test]
fn missing_symbol_reports_no_results_and_stays_producer_only() {
    let class = classify("noSuchThingAnywhere");
    let envelope = AnswerEnvelope::from_stages(&class, "word_index", Vec::new());
    assert!(envelope
        .gaps
        .iter()
        .any(|gap| gap.starts_with("no results")));
    assert!(!envelope.is_answerable());
    assert_eq!(envelope.confidence.axis, ConfidenceAxis::ProducerOrderOnly);
    assert!(envelope.citations.is_empty());
}

#[test]
fn ungrounded_prose_withholds_synthesis() {
    let class = classify("how does caching work here");
    let envelope = AnswerEnvelope::from_stages(&class, "hybrid", vec![prose_stage("notes")]);
    assert!(envelope
        .gaps
        .iter()
        .any(|gap| gap.starts_with("synthesis withheld")));
    assert!(!envelope.is_answerable());
    assert_eq!(envelope.citations, Vec::<String>::new());
    assert!(citations_of(&[]).is_empty());
    assert_eq!(
        gaps_of(&[], false),
        vec!["no results for query (none found in covered index)".to_string()]
    );
}

#[test]
fn budget_fit_preserves_rank_order_and_bounds_tokens() {
    let stages = vec![
        code_stage("src/a.rs", 1, "aaa"),
        code_stage("src/b.rs", 2, "bbb"),
        code_stage("src/c.rs", 3, "ccc"),
    ];
    let all = fit_stages_to_budget(&stages, 1_000_000);
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].source, "src/a.rs");
    let none = fit_stages_to_budget(&stages, 0);
    assert!(none.is_empty());
    let one = fit_stages_to_budget(&stages, 3);
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].source, "src/a.rs");
}

#[test]
fn summary_sourced_stages_cite_source_only() {
    use crate::query::synthesize::StageTrace;
    // A hit fused from the summary route still cites its source range:
    // generated text never enters the citation set.
    let mut stage = code_stage("src/a.rs", 2, "alpha");
    stage.trace = Some(StageTrace {
        matched_by: "summary".to_string(),
        tier: Some(
            crate::search_types::RetrievalTier::Model
                .as_str()
                .to_string(),
        ),
        score: crate::search_types::RrfScore::new(0.05),
        rank: 1,
        recall: Vec::new(),
    });
    let class = classify("adds");
    let envelope = AnswerEnvelope::from_stages(&class, "fuse", vec![stage]);
    assert_eq!(envelope.citations, vec!["src/a.rs:2".to_string()]);
    assert!(envelope.is_answerable());
}

#[test]
fn synthesis_prompt_shows_only_shown_ids_and_drops_invented_paths() {
    use crate::read::engine::{ExplainContext, ExplainRequest, ReadEngine};
    // Shown set is the evidence set: an invented path from the model
    // must not render, and the prompt carries ids for the shown rows.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), "pub fn shown_fn() {}\n").expect("write");
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    db.upsert_fragments(
        &search_vector::db::FileRecord {
            id: "f".to_string(),
            absolute_path: dir.path().join("lib.rs").to_string_lossy().into_owned(),
            relative_path: "lib.rs".to_string(),
            root_path: dir.path().to_string_lossy().into_owned(),
            size_bytes: 10,
            last_modified_time: 1,
            kind: Some("code".to_string()),
            format: "rust".to_string(),
            content_hash: None,
            index_status: Some("indexed".to_string()),
            fail_count: 0,
            last_error: None,
        },
        &[search_vector::db::FragmentRecord {
            id: "s".to_string(),
            group: None,
            file_id: "f".to_string(),
            range_json: "{\"kind\":\"text\",\"start_line\":1,\"end_line\":1,\"start_offset\":0,\"end_offset\":5}".to_string(),
            content_kind: "text".to_string(),
            content_text: Some("pub fn shown_fn() {}".to_string()),
            cjk_text: String::new(),
            symbol_type: Some("function".to_string()),
            symbol_name: Some("shown_fn".to_string()),
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
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    struct Invented;
    impl crate::read::engine::Synthesizer for Invented {
        fn synthesize(&self, prompt: &str) -> Result<Option<String>, String> {
            assert!(prompt.contains("shown_fn"), "{prompt}");
            assert!(prompt.contains("Evidence:"), "{prompt}");
            Ok(Some("Lives in invented.rs:99.".to_string()))
        }
    }
    let answer = engine.explain(
        &ExplainRequest {
            query: "where is shown_fn".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: false,
            include_fixtures: false,
        },
        &ctx,
        Some(&Invented),
    );
    let rendered = crate::read::render::render_answer(&answer);
    assert!(!rendered.contains("invented.rs:99"), "{rendered}");
}

#[test]
fn envelope_intent_equals_routed_intent_and_coverage_mirrors() {
    use crate::read::engine::{ExplainContext, ExplainRequest, ReadEngine};
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), "pub fn env_fn() {}\n").expect("write");
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: Some(crate::read::readiness::Coverage::empty()),
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "env_fn".to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
        },
        &ctx,
        None,
    );
    let envelope = crate::read::engine::route_for(answer.intent);
    assert_eq!(envelope, answer.route);
    assert!(answer.coverage.is_some());
}
