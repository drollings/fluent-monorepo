//! `callers of` goldens: the answer leads with caller edges naming
//! the calling scope — the definition file never presents as a caller.
use crate::read::engine::{ExplainContext, ExplainRequest, ReadEngine};

fn workspace_with_caller() -> (tempfile::TempDir, crate::query::search_db::GuidanceSearchDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("src")).expect("mkdir");
    std::fs::write(
        dir.path().join("src").join("callee.rs"),
        "pub fn target_sym() -> u64 {\n    7\n}\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("src").join("caller.rs"),
        "use crate::callee::target_sym;\npub fn caller_fn() -> u64 {\n    target_sym()\n}\n",
    )
    .expect("write");
    let db = crate::query::search_db::GuidanceSearchDb::open_in_memory().expect("db");
    let record = |name: &str| {
        let absolute = dir.path().join("src").join(name);
        let meta = std::fs::metadata(&absolute).expect("seeded file reads");
        crate::query::search_db::FileRecord {
            id: name.to_string(),
            absolute_path: absolute.to_string_lossy().into_owned(),
            relative_path: format!("src/{name}"),
            root_path: dir.path().to_string_lossy().into_owned(),
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
    };
    let frag = |id: &str, file_id: &str, symbol: &str, text: &str| {
        crate::query::search_db::FragmentRecord {
        id: id.to_string(),
        group: None,
        file_id: file_id.to_string(),
        range_json: "{\"kind\":\"text\",\"start_line\":1,\"end_line\":3,\"start_offset\":0,\"end_offset\":5}"
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
    };
    db.upsert_fragments(
        &record("callee.rs"),
        &[frag(
            "callee",
            "callee.rs",
            "target_sym",
            "pub fn target_sym() -> u64 {\n    7\n}",
        )],
        &[],
    )
    .expect("seed callee");
    db.upsert_fragments(
        &record("caller.rs"),
        &[frag(
            "caller",
            "caller.rs",
            "caller_fn",
            "pub fn caller_fn() -> u64 {\n    target_sym()\n}",
        )],
        &[],
    )
    .expect("seed caller");
    let caller_abs = dir
        .path()
        .join("src")
        .join("caller.rs")
        .to_string_lossy()
        .into_owned();
    let callee_abs = dir
        .path()
        .join("src")
        .join("callee.rs")
        .to_string_lossy()
        .into_owned();
    db.replace_file_graph(
        &caller_abs,
        &["crate::callee::target_sym".to_string()],
        &[("caller_fn".to_string(), vec!["target_sym".to_string()])],
    )
    .expect("graph caller");
    db.replace_file_graph(&callee_abs, &[], &[("target_sym".to_string(), vec![])])
        .expect("graph callee");
    (dir, db)
}

fn top_matches_blocks(rendered: &str) -> Vec<&str> {
    rendered
        .split("### Top matches")
        .nth(1)
        .unwrap_or("")
        .split("### Call graph")
        .next()
        .unwrap_or("")
        .split("### Citations")
        .next()
        .unwrap_or("")
        .split("## Code")
        .skip(1)
        .collect()
}

#[test]
fn callers_answer_leads_with_the_caller_not_the_definition() {
    let (dir, db) = workspace_with_caller();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: "callers of target_sym".to_string(),
            limit: 10,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: crate::read::engine::AnswerMode::Auto,
        },
        &ctx,
        None,
    );
    let rendered = crate::read::render::render_answer(&answer);
    // Factual graph answers are extractive: the caller edge leads in
    // `Answer`, never fused into code rows, and the definition file
    // never presents as a caller.
    let response = rendered
        .split("### Answer")
        .nth(1)
        .unwrap_or("")
        .split("### ")
        .next()
        .unwrap_or("");
    assert!(
        response.contains("caller.rs") && response.contains("calls target_sym in caller_fn"),
        "answer leads with the caller edge: {rendered}"
    );
    for line in response.lines() {
        if line.contains("calls target_sym in ") {
            assert!(
                line.contains("caller.rs"),
                "the definition never presents as a caller: {line}"
            );
        }
    }
    let top = top_matches_blocks(&rendered).join("\n");
    assert!(
        !top.contains("calls target_sym in "),
        "no edge rows hide inside Top matches: {rendered}"
    );
    // Evidence mode keeps the legacy shape: edges render apart in
    // their own section; the model-free extractive answer also lands
    // with the caller first.
    let evidence = engine.explain(
        &ExplainRequest {
            query: "callers of target_sym".to_string(),
            limit: 10,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: crate::read::engine::AnswerMode::Evidence,
        },
        &ctx,
        None,
    );
    let rendered = crate::read::render::render_answer(&evidence);
    assert!(rendered.contains("### Answer"), "{rendered}");
    let answer_body = rendered
        .split("### Answer")
        .nth(1)
        .unwrap_or("")
        .split("### ")
        .next()
        .unwrap_or("");
    let first = answer_body
        .lines()
        .find(|line| line.starts_with("- "))
        .unwrap_or("");
    assert!(
        first.contains("calls target_sym"),
        "the caller leads the evidence answer: {answer_body}"
    );
    // A one-edge query answers extractively, so the edge lives in the
    // Answer; a larger edge set leaves the remainder in `Call graph`.
    let graph = rendered
        .split("### Call graph")
        .nth(1)
        .unwrap_or("")
        .split("### ")
        .next()
        .unwrap_or("");
    assert!(
        answer_body.contains("calls target_sym in caller_fn")
            || (graph.contains("caller.rs") && graph.contains("calls target_sym in caller_fn")),
        "the caller edge is visible: {rendered}"
    );
}
