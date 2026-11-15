//! External capability-corpus import: manifest shapes, live-section
//! resolution, hash verification, incremental re-import, and removal.
//! Included via `#[path]` forwarder from `read/doc_cards.rs`, per repo
//! convention. Hermetic: temp workspaces, hand-written manifests, no
//! model call on any path (the import takes no backend).

use super::*;
use crate::config::ProjectConfig;

fn project_config() -> ProjectConfig {
    ProjectConfig::default()
}

fn workspace_with_doc(dir: &tempfile::TempDir, rel: &str, text: &str) {
    let path = dir.path().join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, text).expect("write doc");
}

fn write_manifest(dir: &tempfile::TempDir, body: &str) -> std::path::PathBuf {
    let manifest_dir = dir.path().join("doc/capabilities");
    std::fs::create_dir_all(&manifest_dir).expect("mkdir corpus");
    let path = manifest_dir.join("corpus.json");
    std::fs::write(&path, body).expect("write corpus");
    path
}

fn open_db(dir: &tempfile::TempDir) -> search_vector::db::GuidanceDb {
    search_vector::db::GuidanceDb::open(&dir.path().join("import.db")).expect("open db")
}

const GUIDE: &str = "# Guide\n\nRoutes requests onward.\n\n## Detail\n\nExtra depth.\n";

fn guide_card(id: &str, anchor: &str, purpose: &str, extra: &str) -> String {
    format!(
        r#"{{"id": "{id}", "title": "Routing", "purpose": "{purpose}", "crate": "c", "domain": "d", "source_path": "doc/guide.md", "anchor": "{anchor}", "keywords": ["routing"], "code_paths": ["src/router"], "symbols": ["dispatch"]{extra}}}"#
    )
}

fn card(id: &str, anchor: &str, purpose: &str) -> String {
    guide_card(id, anchor, purpose, "")
}

#[test]
fn valid_manifest_imports_ready_cards_incrementally() {
    let dir = tempfile::tempdir().expect("tempdir");
    workspace_with_doc(&dir, "doc/guide.md", GUIDE);
    write_manifest(
        &dir,
        &format!(
            "{{\"briefs\": [{}, {}]}}",
            card("doc/guide.md#guide", "guide", "Routes requests onward."),
            card("doc/guide.md#guide-detail", "guide-detail", "Extra depth.")
        ),
    );
    let db = open_db(&dir);
    let first = import_capability_corpus(dir.path(), &project_config(), &db).expect("import");
    assert!(first.present, "{first:?}");
    assert_eq!(first.cards, 2, "{first:?}");
    assert_eq!(first.ready, 2, "{first:?}");
    assert_eq!(first.gaps, 0, "{first:?}");
    let stored = db
        .capabilities_for_source("doc/guide.md")
        .expect("stored rows");
    assert_eq!(stored.len(), 2, "{stored:?}");
    for row in &stored {
        assert_eq!(row.status, "ready", "{row:?}");
        assert_eq!(
            row.origin, "external:doc/capabilities/corpus.json",
            "{row:?}"
        );
        assert!(!row.corpus_hash.is_empty(), "{row:?}");
        assert_eq!(
            row.prompt_version,
            search_vector::db::CAPABILITY_SCHEMA_VERSION
        );
    }
    let guide = stored
        .iter()
        .find(|row| row.id == "doc/guide.md#guide")
        .expect("guide row");
    assert_eq!(guide.title, "Routing", "{guide:?}");
    assert_eq!(guide.keywords_json, r#"["routing"]"#, "{guide:?}");
    assert_eq!(guide.code_paths_json, r#"["src/router"]"#, "{guide:?}");
    let second = import_capability_corpus(dir.path(), &project_config(), &db).expect("re-import");
    assert_eq!(second.skipped_unchanged, 2, "{second:?}");
    assert_eq!(second.ready, 0, "{second:?}");
    assert_eq!(second.gaps, 0, "{second:?}");
}

#[test]
fn missing_source_anchor_mismatch_and_hash_drift_are_gaps() {
    let dir = tempfile::tempdir().expect("tempdir");
    workspace_with_doc(&dir, "doc/guide.md", GUIDE);
    write_manifest(
        &dir,
        &format!(
            "{{\"briefs\": [{}, {}, {}, {}]}}",
            guide_card("missing.md#absent", "absent", "Absent purpose.", "").replace(
                "\"source_path\": \"doc/guide.md\"",
                "\"source_path\": \"missing.md\""
            ),
            card("doc/guide.md#nope", "nope", "Nope purpose."),
            guide_card(
                "doc/guide.md#guide",
                "guide",
                "Routes requests onward.",
                r#", "content_hash": "deadbeef""#
            ),
            card("doc/guide.md#guide-detail", "guide-detail", "Extra depth."),
        ),
    );
    // Distinct ids for each gap shape.
    let db = open_db(&dir);
    let report = import_capability_corpus(dir.path(), &project_config(), &db).expect("import");
    assert_eq!(report.cards, 4, "{report:?}");
    assert_eq!(report.gaps, 3, "{report:?}");
    assert_eq!(report.ready, 1, "{report:?}");
    let mut rows = db
        .capabilities_for_source("doc/guide.md")
        .expect("guide rows");
    rows.extend(
        db.capabilities_for_source("missing.md")
            .expect("missing rows"),
    );
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|row| {
            row.id == "missing.md#absent"
                || row.id == "doc/guide.md#nope"
                || row.id == "doc/guide.md#guide"
        })
        .collect();
    // Gaps persist but never enter recall.
    assert_eq!(rows.len(), 3, "{rows:?}");
    let mut reasons: Vec<&str> = rows
        .iter()
        .map(|row| {
            assert_eq!(row.status, "gap", "{row:?}");
            row.gap_reason.as_deref().unwrap_or("")
        })
        .collect();
    reasons.sort_unstable();
    assert!(reasons[0].starts_with("anchor-missing"), "{reasons:?}");
    assert!(reasons[1].starts_with("hash-mismatch"), "{reasons:?}");
    assert!(reasons[2].starts_with("missing-source"), "{reasons:?}");
    let depth = db.search_capabilities("depth", 10).expect("search");
    assert_eq!(depth.len(), 1, "{depth:?}");
    assert_eq!(depth[0].id, "doc/guide.md#guide-detail", "{depth:?}");
}

#[test]
fn malformed_manifest_fails_loudly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open_db(&dir);
    // Absent manifest skips without error.
    let skipped =
        import_capability_corpus(dir.path(), &project_config(), &db).expect("absent skips");
    assert!(!skipped.present, "{skipped:?}");
    // Malformed JSON fails.
    write_manifest(&dir, "{not json");
    assert!(import_capability_corpus(dir.path(), &project_config(), &db).is_err());
    // Unknown top-level and brief fields fail.
    write_manifest(&dir, r#"{"briefs": [], "bogus": 1}"#);
    assert!(import_capability_corpus(dir.path(), &project_config(), &db).is_err());
    write_manifest(
        &dir,
        r#"{"briefs": [{"id": "a#b", "purpose": "P.", "crate": "c", "domain": "d", "source_path": "s", "anchor": "a", "bogus": 1}]}"#,
    );
    assert!(import_capability_corpus(dir.path(), &project_config(), &db).is_err());
    // A card without a purpose fails.
    write_manifest(
        &dir,
        r#"{"briefs": [{"id": "a#b", "crate": "c", "domain": "d", "source_path": "s", "anchor": "a"}]}"#,
    );
    assert!(import_capability_corpus(dir.path(), &project_config(), &db).is_err());
}

#[test]
fn removed_cards_leave_the_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    workspace_with_doc(&dir, "doc/guide.md", GUIDE);
    write_manifest(
        &dir,
        &format!(
            "{{\"briefs\": [{}, {}]}}",
            card("doc/guide.md#guide", "guide", "Routes requests onward."),
            card("doc/guide.md#guide-detail", "guide-detail", "Extra depth.")
        ),
    );
    let db = open_db(&dir);
    import_capability_corpus(dir.path(), &project_config(), &db).expect("import");
    assert_eq!(db.capability_ready_count().expect("count"), 2);
    write_manifest(
        &dir,
        &format!(
            "{{\"briefs\": [{}]}}",
            card("doc/guide.md#guide", "guide", "Routes requests onward.")
        ),
    );
    let report = import_capability_corpus(dir.path(), &project_config(), &db).expect("re-import");
    assert_eq!(report.removed, 1, "{report:?}");
    assert_eq!(db.capability_ready_count().expect("count"), 1);
    assert!(db
        .capabilities_for_ids(&["doc/guide.md#guide-detail".to_string()])
        .expect("rows")
        .is_empty());
}
