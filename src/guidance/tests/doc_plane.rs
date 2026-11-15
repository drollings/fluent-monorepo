//! Documentation-plane retrieval pins.

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
