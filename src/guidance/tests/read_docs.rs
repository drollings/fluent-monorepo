//! Docs plane: capability-card retrieval over FTS + lemmas, live
//! section excerpts with hash verification, native judge labels, and
//! branch-2 scoped pulls. Included via `#[path]` forwarders from
//! `read/planes`, per repo convention. Hermetic: temp workspaces,
//! real index, scripted judge, no endpoint.

use super::docs::{ranked_briefs_for_legacy, retrieve_cards};

fn workspace_with_card() -> (tempfile::TempDir, search_vector::db::GuidanceDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("doc/router")).expect("mkdir doc");
    std::fs::write(
        root.join("doc/router/guide.md"),
        "# Guide\n\nRoutes requests onward.\n",
    )
    .expect("write doc");
    std::fs::create_dir_all(root.join("doc/capabilities")).expect("mkdir corpus");
    std::fs::write(
        root.join("doc/capabilities/corpus.json"),
        r#"{"briefs": [{"id": "doc/router/guide.md#guide", "title": "Routing",
            "purpose": "Routes requests onward through the table.",
            "crate": "fluent-router", "domain": "router",
            "source_path": "doc/router/guide.md", "anchor": "guide",
            "keywords": ["routing"], "code_paths": ["src/router/dispatch.rs"],
            "symbols": ["dispatch_routes"]}]}"#,
    )
    .expect("write corpus");
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    crate::read::doc_cards::import_capability_corpus(
        root,
        &crate::config::ProjectConfig::default(),
        &db,
    )
    .expect("import");
    assert_eq!(db.capability_ready_count().expect("count"), 1);
    (dir, db)
}

#[test]
fn cards_rank_resolve_and_verify() {
    let (dir, db) = workspace_with_card();
    let hits = retrieve_cards(
        "how are requests routed onward",
        Some(dir.path()),
        &db,
        None,
        None,
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
    let hit = &hits[0];
    assert_eq!(hit.id, "doc/router/guide.md#guide");
    assert_eq!(hit.title, "Routing");
    assert_eq!(hit.code_paths, vec!["src/router/dispatch.rs".to_string()]);
    assert_eq!(hit.symbols, vec!["dispatch_routes".to_string()]);
    assert!(hit.label.is_none(), "unjudged without a judge: {hit:?}");
    let excerpt = hit.live_excerpt.as_deref().expect("live excerpt");
    assert!(excerpt.contains("Routes requests onward."), "{excerpt}");
    assert!(hit.verified, "import-time hash still matches");
}

#[test]
fn drifted_sections_are_unverified_gaps_not_trusted_text() {
    let (dir, db) = workspace_with_card();
    std::fs::write(
        dir.path().join("doc/router/guide.md"),
        "# Guide\n\nRoutes rewritten entirely.\n",
    )
    .expect("drift doc");
    let hits = retrieve_cards(
        "how are requests routed onward",
        Some(dir.path()),
        &db,
        None,
        None,
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(!hits[0].verified, "drifted section must not verify");
    let excerpt = hits[0]
        .live_excerpt
        .as_deref()
        .expect("excerpt still resolves");
    assert!(excerpt.contains("rewritten"), "{excerpt}");
}

#[test]
fn missing_anchors_resolve_to_no_excerpt() {
    let (dir, db) = workspace_with_card();
    std::fs::write(
        dir.path().join("doc/router/guide.md"),
        "# Changed\n\nNo guide section anymore.\n",
    )
    .expect("rewrite doc");
    let hits = retrieve_cards(
        "how are requests routed onward",
        Some(dir.path()),
        &db,
        None,
        None,
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].live_excerpt.is_none());
    assert!(!hits[0].verified);
}

#[test]
fn legacy_mapping_is_lossless() {
    let (dir, db) = workspace_with_card();
    let hits = retrieve_cards(
        "how are requests routed onward",
        Some(dir.path()),
        &db,
        None,
        None,
    );
    let briefs = ranked_briefs_for_legacy(&hits);
    assert_eq!(briefs.len(), hits.len());
    for (hit, brief) in hits.iter().zip(briefs.iter()) {
        assert_eq!(hit.id, brief.id);
        assert_eq!(hit.purpose, brief.purpose);
        assert_eq!(hit.crate_name, brief.crate_name);
        assert_eq!(hit.domain, brief.domain);
        assert_eq!(hit.source_path, brief.source_path);
        assert_eq!(hit.anchor, brief.anchor);
        assert_eq!(hit.overlap, brief.overlap);
        assert_eq!(hit.predicate, brief.predicate);
        assert_eq!(hit.roles, brief.roles);
    }
}

#[test]
fn branch_two_pulls_scope_code_by_card_paths() {
    use super::docs::{branch2_pulls, branch_provenance};
    // The card's code_paths scope the follow-up pull: matching code in
    // scope surfaces with via-capability provenance, out-of-scope code
    // never enters even when it matches the text.
    let (dir, db) = workspace_with_card();
    let hits = retrieve_cards(
        "how are requests routed onward",
        Some(dir.path()),
        &db,
        None,
        None,
    );
    let hit = &hits[0];
    let code = |rel: &str, text: &str| {
        let abs = dir.path().join(rel).to_string_lossy().into_owned();
        db.upsert_fragments(
            &search_vector::db::FileRecord {
                id: abs.clone(),
                absolute_path: abs.clone(),
                relative_path: rel.to_string(),
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
                id: format!("{abs}#fn"),
                group: None,
                file_id: abs,
                range_json: "{\"kind\":\"item\"}".to_string(),
                content_kind: "text".to_string(),
                content_text: Some(text.to_string()),
                cjk_text: String::new(),
                symbol_type: Some("function".to_string()),
                symbol_name: Some("dispatch_routes".to_string()),
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
    };
    code(
        "src/router/dispatch.rs",
        "pub fn dispatch_routes() {}\n// Routes requests onward through the table.\n",
    );
    code(
        "src/other/unrelated.rs",
        "pub fn unrelated() {}\n// Routes requests onward through the table.\n",
    );
    let pulls = branch2_pulls(hit, &db, None);
    assert_eq!(pulls.len(), 1, "{pulls:?}");
    assert_eq!(pulls[0].source.display(), "src/router/dispatch.rs");
    assert_eq!(pulls[0].kind, crate::read::engine::FindingKind::Code);
    assert_eq!(
        pulls[0].producer,
        crate::read::engine::FindingProducer::Database
    );
    assert_eq!(pulls[0].provenance, branch_provenance(hit));
    assert!(pulls[0].provenance.contains("via capability"));
}
