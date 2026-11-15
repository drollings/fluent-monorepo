//! `callers of` goldens: the answer leads with caller edges naming
//! the calling scope — the definition file never presents as a caller.
use crate::read::engine::{ExplainContext, ExplainRequest, ReadEngine};

fn workspace_with_caller() -> (tempfile::TempDir, search_vector::db::GuidanceDb) {
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
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let record = |name: &str| {
        let absolute = dir.path().join("src").join(name);
        let meta = std::fs::metadata(&absolute).expect("seeded file reads");
        search_vector::db::FileRecord {
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
        search_vector::db::FragmentRecord {
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
        },
        &ctx,
        None,
    );
    let rendered = crate::read::render::render_answer(&answer);
    // Caller edges render apart in their own section, never fused
    // into code rows: the definition cannot present as a caller.
    let graph = rendered
        .split("### Call graph")
        .nth(1)
        .unwrap_or("")
        .split("### ")
        .next()
        .unwrap_or("");
    assert!(
        graph.contains("caller.rs") && graph.contains("calls target_sym in caller_fn"),
        "call graph lists the caller edge: {rendered}"
    );
    assert!(
        !graph.contains("callee.rs"),
        "the definition never presents as a caller: {graph}"
    );
    let top = top_matches_blocks(&rendered).join("\n");
    assert!(
        !top.contains("calls target_sym in "),
        "no edge rows hide inside Top matches: {rendered}"
    );
}
