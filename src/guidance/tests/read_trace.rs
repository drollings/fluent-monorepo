//! Per-candidate trace: anchor decision, judge label, and
//! admission/budget outcome for every doc/code candidate. Included
//! via `#[path]` forwarder from `read/engine.rs`, per repo
//! convention. Hermetic: tempdirs, no model, no network.

use crate::read::engine::{AnswerMode, ExplainContext, ExplainRequest, ReadEngine};

fn file_record(
    workspace: &std::path::Path,
    name: &str,
    text: &str,
) -> crate::query::search_db::FileRecord {
    let absolute = workspace.join(name).to_string_lossy().into_owned();
    crate::query::search_db::FileRecord {
        id: name.to_string(),
        absolute_path: absolute,
        relative_path: name.to_string(),
        root_path: workspace.to_string_lossy().into_owned(),
        size_bytes: text.len() as u64,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    }
}

fn code_fragment(name: &str, symbol: &str, text: &str) -> crate::query::search_db::FragmentRecord {
    crate::query::search_db::FragmentRecord {
        id: format!("{name}:{symbol}"),
        group: None,
        file_id: name.to_string(),
        range_json: "{\"kind\":\"text\",\"start_line\":1,\"end_line\":2,\"start_offset\":0,\"end_offset\":10}"
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

fn seed_doc(db: &crate::query::search_db::GuidanceSearchDb, name: &str, anchor: &str, body: &str) {
    let (chunks, _) = crate::read::doc_cards::chunk_markdown_sections(name, body);
    let chunk = chunks
        .iter()
        .find(|chunk| chunk.anchor == anchor)
        .expect("anchor chunks");
    db.replace_project_cards(&[crate::query::search_db::ProjectCard {
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

fn trace_fixture() -> (tempfile::TempDir, crate::query::search_db::GuidanceSearchDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("manifest");
    let db = crate::query::search_db::GuidanceSearchDb::open_in_memory().expect("db");
    for (name, symbol, text) in [
        ("a1.rs", "alpha_probe", "pub fn alpha_probe() {}\nalpha\n"),
        ("b2.rs", "beta_probe", "pub fn beta_probe() {}\nbeta\n"),
    ] {
        std::fs::write(dir.path().join(name), text).expect("write");
        db.upsert_fragments(
            &file_record(dir.path(), name, text),
            &[code_fragment(name, symbol, text)],
            &[],
        )
        .expect("seed code");
    }
    for (name, anchor, body) in [
        ("d1.md", "alpha", "# Alpha\n\nAlpha syncs here.\n"),
        ("d2.md", "beta", "# Beta\n\nBeta ships there.\n"),
    ] {
        std::fs::write(dir.path().join(name), body).expect("write");
        seed_doc(&db, name, anchor, body);
    }
    (dir, db)
}

fn anchored_fixture() -> (tempfile::TempDir, crate::query::search_db::GuidanceSearchDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("manifest");
    let db = crate::query::search_db::GuidanceSearchDb::open_in_memory().expect("db");
    let text = "pub fn guidance_sync() {}\n";
    std::fs::write(dir.path().join("lib.rs"), text).expect("write");
    db.upsert_fragments(
        &file_record(dir.path(), "lib.rs", text),
        &[code_fragment("lib.rs", "guidance_sync", text)],
        &[],
    )
    .expect("seed code");
    // One doc names the anchored crate, the other shares query terms
    // but not the anchor term, so the anchor excludes exactly it.
    for (name, anchor, body) in [
        (
            "d1.md",
            "sync",
            "# Sync\n\nguidance-core owns sync and freshness.\n",
        ),
        ("d2.md", "paths", "# Paths\n\nsync owns freshness paths.\n"),
    ] {
        std::fs::write(dir.path().join(name), body).expect("write");
        seed_doc(&db, name, anchor, body);
    }
    (dir, db)
}

fn request(query: &str) -> ExplainRequest {
    ExplainRequest {
        query: query.to_string(),
        limit: 5,
        budget_override: None,
        no_llm: true,
        include_fixtures: false,
        answer_mode: AnswerMode::Auto,
    }
}

#[test]
fn trace_reports_anchor_judge_outcome_per_candidate() {
    // No judge, no anchor: every surviving candidate traces with
    // `none`/`unjudged` and a rendered outcome. Deterministic, no
    // model call on any path.
    let (dir, db) = trace_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(&request("How does Alpha sync work"), &ctx, None);
    assert!(
        !answer.trace.is_empty(),
        "trace covers the candidates: {:?}",
        answer.gaps
    );
    for row in &answer.trace {
        assert!(!row.plane.is_empty(), "{row:?}");
        assert!(!row.candidate.is_empty(), "{row:?}");
        assert!(!row.anchor.is_empty(), "{row:?}");
        assert!(!row.judge.is_empty(), "{row:?}");
        assert!(!row.outcome.is_empty(), "{row:?}");
        assert!(
            row.plane == "docs" || row.plane == "code",
            "known plane: {row:?}"
        );
    }
    let docs: Vec<&crate::read::engine::TraceRow> = answer
        .trace
        .iter()
        .filter(|row| row.plane == "docs")
        .collect();
    assert!(!docs.is_empty(), "doc candidates trace");
    for row in &docs {
        // The fixture query carries free content terms, so the anchor
        // stays shadow (never enforced) until its calibration.
        assert_eq!(row.anchor, "free-term shadow", "{row:?}");
        assert_eq!(row.judge, "unjudged", "{row:?}");
        assert!(
            row.outcome == "rendered" || row.outcome == "rendered:unverified",
            "{row:?}"
        );
    }
    let code: Vec<&crate::read::engine::TraceRow> = answer
        .trace
        .iter()
        .filter(|row| row.plane == "code")
        .collect();
    assert!(!code.is_empty(), "code candidates trace");
    for row in &code {
        assert_eq!(
            row.anchor, "none",
            "no scope hypothesis without a judge: {row:?}"
        );
        assert_eq!(row.judge, "unjudged", "{row:?}");
        assert_eq!(row.outcome, "rendered", "{row:?}");
    }
}

#[test]
fn trace_marks_extractive_consumption() {
    // Factual intents consume admitted rows into the Answer: the trace
    // marks them `extractive` instead of `rendered`.
    let (dir, db) = trace_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(&request("which crate owns alpha"), &ctx, None);
    let rendered = crate::read::render::render_answer(&answer);
    assert!(rendered.contains("### Answer"), "{rendered}");
    let extractive: Vec<&crate::read::engine::TraceRow> = answer
        .trace
        .iter()
        .filter(|row| row.outcome == "extractive")
        .collect();
    assert!(
        !extractive.is_empty(),
        "consumed rows trace as extractive: {:?}",
        answer.trace
    );
}

#[test]
fn trace_marks_anchor_excluded() {
    // An enforced crate anchor filters doc candidates: the excluded
    // count rides a synthetic trace row, survivors name the anchor.
    let (dir, db) = anchored_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(&request("which crate owns guidance-core"), &ctx, None);
    let excluded: Vec<&crate::read::engine::TraceRow> = answer
        .trace
        .iter()
        .filter(|row| row.candidate == "(anchor-filtered)")
        .collect();
    assert_eq!(excluded.len(), 1, "one exclusion row: {:?}", answer.trace);
    assert!(
        excluded[0].anchor.starts_with("crate:"),
        "enforced anchor named: {:?}",
        excluded[0]
    );
    assert!(excluded[0].anchor.contains("enforced"), "{:?}", excluded[0]);
    assert_eq!(excluded[0].judge, "-", "{:?}", excluded[0]);
    assert!(
        excluded[0].outcome.starts_with("excluded:"),
        "{:?}",
        excluded[0]
    );
    for row in answer.trace.iter().filter(|row| row.plane == "docs") {
        if row.candidate == "(anchor-filtered)" {
            continue;
        }
        assert!(
            row.anchor.contains("enforced"),
            "survivors name the anchor: {row:?}"
        );
    }
}

#[test]
fn trace_shows_judge_labels_without_filtering() {
    // A scripted judge labels candidates without authorizing any
    // filter: the trace carries the verdict labels while every
    // outcome stays `rendered`.
    let (dir, db) = trace_fixture();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    fn decide(
        label: &str,
    ) -> Result<fluent_llm::decision::Decision, fluent_llm::decision::DecisionError> {
        fluent_llm::decision::Decision::from_distribution(
            label,
            vec![(label.to_string(), 0.9)],
            "d".to_string(),
            1,
            0,
            false,
        )
        .map_err(|_| fluent_llm::decision::DecisionError::Unavailable("bad".to_string()))
    }
    // Consumption order for this query: doc (1 hit), scope (1),
    // findings (1 hit). The scope verdict is cached process-wide by
    // query, so this test uses a query no other test asks.
    let script = vec![decide("strong"), decide("unknown"), decide("relevant")];
    let backend = fluent_llm::decision::ScriptedDecideBackend::new(script);
    let judge = crate::read::engine::JudgeCtx {
        backend: &backend,
        model: "d".to_string(),
        timeout_ms: 2000,
        fingerprint: None,
    };
    let answer = engine.explain_with_judge(
        &request("How does Alpha sync behave under trace"),
        &ctx,
        None,
        Some(&judge),
    );
    let docs: Vec<&crate::read::engine::TraceRow> = answer
        .trace
        .iter()
        .filter(|row| row.plane == "docs")
        .collect();
    assert!(!docs.is_empty(), "doc candidates trace");
    for row in &docs {
        assert_eq!(row.judge, "strong", "verdict label rides the row: {row:?}");
        assert!(
            row.outcome == "rendered" || row.outcome == "rendered:unverified",
            "shadow never filters: {row:?}"
        );
    }
    let code: Vec<&crate::read::engine::TraceRow> = answer
        .trace
        .iter()
        .filter(|row| row.plane == "code" && row.outcome != "budget-cap")
        .collect();
    assert!(!code.is_empty(), "code candidates trace");
    for row in &code {
        assert_eq!(
            row.judge, "relevant",
            "verdict label rides the row: {row:?}"
        );
    }
}

#[test]
fn trace_marks_budget_cap() {
    // More findings than the code token budget fits: the first rows
    // render, the rest trace as `budget-cap` victims of the cost cap.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("manifest");
    let db = crate::query::search_db::GuidanceSearchDb::open_in_memory().expect("db");
    for index in 0..10 {
        let name = format!("m{index}.rs");
        let text = format!(
            "pub fn probe{index}() -> u64 {{\n    // probe behave weight one\n    // probe behave weight two\n    // probe behave weight three\n    // probe behave weight four\n    // probe behave weight five\n    // probe behave weight six\n    // probe behave weight seven\n    // probe behave weight eight\n    // probe behave weight nine\n    // probe behave weight ten\n    // probe behave weight eleven\n    // probe behave weight twelve\n    {index}\n}}\n"
        );
        std::fs::write(dir.path().join(&name), &text).expect("write");
        db.upsert_fragments(
            &file_record(dir.path(), &name, &text),
            &[code_fragment(&name, &format!("probe{index}"), &text)],
            &[],
        )
        .expect("seed code");
    }
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            limit: 10,
            ..request("How does the probe behave")
        },
        &ctx,
        None,
    );
    let capped: Vec<&crate::read::engine::TraceRow> = answer
        .trace
        .iter()
        .filter(|row| row.outcome == "budget-cap")
        .collect();
    assert!(
        !capped.is_empty(),
        "budget victims trace: {:?}",
        answer.trace
    );
    for row in &capped {
        assert_eq!(row.plane, "code", "{row:?}");
        assert_eq!(row.judge, "unjudged", "{row:?}");
    }
}
