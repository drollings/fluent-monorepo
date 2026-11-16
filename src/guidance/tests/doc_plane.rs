//! Documentation-plane retrieval pins.

#[test]
fn anchored_retrieval_scopes_to_the_named_crate_and_fails_open() {
    use crate::read::vocabulary::{AnchorSource, SubjectAnchor};
    let dir = tempfile::tempdir().expect("tempdir");
    let router_text = "# Router\n\nRoutes requests onward.\n";
    let sync_text = "# Sync\n\nReprocesses stale files.\n";
    std::fs::write(dir.path().join("router.md"), router_text).expect("write");
    std::fs::write(dir.path().join("sync.md"), sync_text).expect("write");
    let db = search_vector::GuidanceDb::open_in_memory().expect("db");
    let seed = |key: &str, source: &str, anchor: &str, text: &str, codes: &[&str]| {
        let (chunks, _) = crate::read::doc_cards::chunk_markdown_sections(source, text);
        let chunk = chunks
            .iter()
            .find(|chunk| chunk.anchor == anchor)
            .expect("anchor chunks");
        search_vector::db::ProjectCard {
            key: key.to_string(),
            kind: "doc".to_string(),
            title: key.to_string(),
            body: chunk.text.clone(),
            source_path: source.to_string(),
            anchor: anchor.to_string(),
            aliases_json: "[]".to_string(),
            code_paths_json: serde_json::to_string(codes).expect("codes json"),
            symbols_json: "[]".to_string(),
            content_hash: chunk.content_hash.clone(),
            generated_at: 0,
        }
    };
    db.replace_project_cards(&[
        seed(
            "doc:router.md#router",
            "router.md",
            "router",
            router_text,
            &["src/router/dispatch.rs"],
        ),
        seed(
            "doc:sync.md#sync",
            "sync.md",
            "sync",
            sync_text,
            &["src/guidance/src/sync_engine.rs"],
        ),
    ])
    .expect("cards seed");
    // Unanchored retrieval is the baseline both arms agree on.
    let baseline =
        crate::read::planes::docs::retrieve_doc_sections("router sync", Some(dir.path()), &db, 5);
    assert_eq!(baseline.len(), 2, "{baseline:?}");
    let (passthrough, excluded) = crate::read::planes::docs::retrieve_doc_sections_anchored(
        "router sync",
        Some(dir.path()),
        &db,
        5,
        None,
    );
    assert_eq!(passthrough, baseline, "None anchors read byte-identically");
    assert_eq!(excluded, 0);
    // Exact-crate anchor keeps the router section and drops sync.
    let anchor = SubjectAnchor {
        terms: vec!["fluent-router".to_string(), "router".to_string()],
        source: AnchorSource::Crate,
    };
    let (hits, excluded) = crate::read::planes::docs::retrieve_doc_sections_anchored(
        "router sync",
        Some(dir.path()),
        &db,
        5,
        Some(&anchor),
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].source_path, "router.md");
    assert_eq!(excluded, 1);
    // Shadow identifiers filter nothing, however selective.
    let shadow = SubjectAnchor {
        terms: vec!["sync".to_string()],
        source: AnchorSource::Identifier,
    };
    let (hits, excluded) = crate::read::planes::docs::retrieve_doc_sections_anchored(
        "router sync",
        Some(dir.path()),
        &db,
        5,
        Some(&shadow),
    );
    assert_eq!(hits, baseline, "free terms stay shadow");
    assert_eq!(excluded, 0);
    // An anchor naming nothing fails open with the full set.
    let stray = SubjectAnchor {
        terms: vec!["qqq-nosuch-crate".to_string()],
        source: AnchorSource::Crate,
    };
    let (hits, excluded) = crate::read::planes::docs::retrieve_doc_sections_anchored(
        "router sync",
        Some(dir.path()),
        &db,
        5,
        Some(&stray),
    );
    assert_eq!(hits, baseline, "fail-open keeps every candidate");
    assert_eq!(excluded, 0);
}

#[test]
fn doc_cards_resolve_verbatim_and_point_at_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src/guidance")).expect("mkdir");
    std::fs::write(dir.path().join("src/guidance/lib.rs"), "pub fn x() {}\n").expect("write");
    std::fs::create_dir_all(dir.path().join("doc/guidance")).expect("mkdir doc");
    std::fs::write(
        dir.path().join("doc/guidance/GUIDE.md"),
        "# Root\n\n## Guidance\n\nSync reprocesses stale files from docs.\n",
    )
    .expect("write");
    let db = search_vector::GuidanceDb::open_in_memory().expect("db");
    let cfg = crate::config::ProjectConfig::default();
    let cards = crate::read::project_map::build_doc_cards(dir.path(), &cfg, None);
    db.replace_project_cards(&cards).expect("cards");
    let hits = crate::read::planes::docs::retrieve_doc_sections(
        "sync reprocesses stale",
        Some(dir.path()),
        &db,
        5,
    );
    assert!(!hits.is_empty(), "{hits:?}");
    assert!(hits.iter().all(|hit| hit.verified), "{hits:?}");
    assert!(
        hits.iter().any(|hit| hit
            .body
            .as_deref()
            .is_some_and(|body| body.contains("stale files"))),
        "{hits:?}"
    );
    assert!(
        hits.iter()
            .any(|hit| hit.code_paths.iter().any(|path| path == "src/guidance")),
        "{hits:?}"
    );
}

#[test]
fn drifted_doc_section_is_unverified() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("README.md"), "# Root\n\nOriginal.\n").expect("write");
    let db = search_vector::GuidanceDb::open_in_memory().expect("db");
    let cfg = crate::config::ProjectConfig::default();
    let cards = crate::read::project_map::build_doc_cards(dir.path(), &cfg, None);
    db.replace_project_cards(&cards).expect("cards");
    std::fs::write(dir.path().join("README.md"), "# Root\n\nChanged bytes.\n").expect("write");
    let hits =
        crate::read::planes::docs::retrieve_doc_sections("original", Some(dir.path()), &db, 5);
    assert!(hits.iter().any(|hit| !hit.verified), "{hits:?}");
}
