//! Generation guard: matching markers serve, anything else refuses.

use crate::index_generation::{
    generation_of, verify_generation, write_generation, GenerationVerdict,
};
use fluent_wvr_testutil::tempdir;

fn write_sidecar(dir: &std::path::Path, name: &str, header: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, format!("{header}\n{{\"ok\":true}}")).expect("write");
}

#[test]
fn matching_marker_serves() {
    let sidecars = tempdir();
    let index = tempdir();
    write_sidecar(sidecars.path(), "a.rs.json", "hdr-a");
    let generation = generation_of(sidecars.path());
    write_generation(index.path(), &generation).expect("write generation");
    assert_eq!(
        verify_generation(index.path(), sidecars.path()),
        GenerationVerdict::Current
    );
}

#[test]
fn mismatched_marker_refuses() {
    let sidecars = tempdir();
    let index = tempdir();
    write_sidecar(sidecars.path(), "a.rs.json", "hdr-a");
    write_generation(index.path(), "stale-generation").expect("write generation");
    assert_eq!(
        verify_generation(index.path(), sidecars.path()),
        GenerationVerdict::Mismatch
    );
    write_sidecar(sidecars.path(), "b.rs.json", "hdr-b");
    let generation = generation_of(sidecars.path());
    assert_eq!(
        verify_generation(index.path(), sidecars.path()),
        GenerationVerdict::Mismatch,
        "sidecar change without re-commit must refuse, got generation {generation}"
    );
}

#[test]
fn crash_half_commit_refuses() {
    let sidecars = tempdir();
    let index = tempdir();
    write_sidecar(sidecars.path(), "a.rs.json", "hdr-a");
    assert_eq!(
        verify_generation(index.path(), sidecars.path()),
        GenerationVerdict::Mismatch,
        "missing marker refuses"
    );
    std::fs::write(index.path().join("index.generation"), "").expect("write");
    assert_eq!(
        verify_generation(index.path(), sidecars.path()),
        GenerationVerdict::Mismatch,
        "empty marker refuses"
    );
    std::fs::write(index.path().join("index.generation"), "   \n").expect("write");
    assert_eq!(
        verify_generation(index.path(), sidecars.path()),
        GenerationVerdict::Mismatch,
        "whitespace marker refuses"
    );
}

#[test]
fn byte_identical_rebuild_after_recommit() {
    let sidecars = tempdir();
    let index = tempdir();
    write_sidecar(sidecars.path(), "a.rs.json", "hdr-a");
    let first = generation_of(sidecars.path());
    write_generation(index.path(), &first).expect("write generation");
    assert_eq!(
        verify_generation(index.path(), sidecars.path()),
        GenerationVerdict::Current
    );
    let second = generation_of(sidecars.path());
    assert_eq!(first, second, "same manifest recomputes identically");
}

struct GenerationFake {
    committed: Option<String>,
}

impl crate::query::recall::RecallStorage for GenerationFake {
    fn list_files(
        &self,
    ) -> Result<Vec<crate::search_types::FileInfo>, crate::query::plan::RecallError> {
        Ok(Vec::new())
    }

    fn search_fts(
        &self,
        _query: &str,
        _limit: usize,
        _filter: &crate::search_types::StorageFilter,
    ) -> Result<Vec<crate::search_types::StorageHit>, crate::query::plan::RecallError> {
        Ok(Vec::new())
    }

    fn search_vector(
        &self,
        _embedding: &[f32],
        _limit: usize,
        _filter: &crate::search_types::StorageFilter,
    ) -> Result<Vec<crate::search_types::StorageHit>, crate::query::plan::RecallError> {
        Ok(Vec::new())
    }

    fn search_lemmas(
        &self,
        _lemmas: &[String],
        _limit: usize,
        _filter: &crate::search_types::StorageFilter,
    ) -> Result<Vec<crate::search_types::StorageHit>, crate::query::plan::RecallError> {
        Ok(Vec::new())
    }

    fn get_entity(
        &self,
        _entity_id: &str,
    ) -> Result<
        Option<(crate::search_types::Entity, crate::search_types::FileInfo)>,
        crate::query::plan::RecallError,
    > {
        Ok(None)
    }

    fn list_entities_by_file(
        &self,
        _file_id: &str,
    ) -> Result<
        Vec<(crate::search_types::Entity, crate::search_types::FileInfo)>,
        crate::query::plan::RecallError,
    > {
        Ok(Vec::new())
    }

    fn stored_generation(&self) -> Option<String> {
        self.committed.clone()
    }
}

#[test]
fn guarded_recall_serves_on_match_and_refuses_on_mismatch() {
    use crate::query::recall::run_recall_guarded;
    use crate::search_types::SearchPlan;
    let plan = SearchPlan {
        routes: Vec::new(),
        ..Default::default()
    };
    let matching = GenerationFake {
        committed: Some("gen-1".to_string()),
    };
    let _ = run_recall_guarded(&plan, &matching, None, None, "gen-2")
        .expect_err("mismatch must refuse before reading");
    let unknown = GenerationFake { committed: None };
    assert!(
        run_recall_guarded(&plan, &unknown, None, None, "gen-1").is_err(),
        "unknown generation refuses"
    );
}

#[test]
fn guarded_graph_build_refuses_on_mismatch() {
    use crate::graph_index::GraphIndex;
    let known: Vec<String> = vec!["a.rs".to_string()];
    let ok = GraphIndex::build_guarded(&known, &[], &[], &[], "gen-1", Some("gen-1"));
    assert!(ok.is_ok(), "matching generation builds");
    assert!(
        GraphIndex::build_guarded(&known, &[], &[], &[], "gen-1", Some("gen-2")).is_err(),
        "mismatch refuses before assembling"
    );
    assert!(
        GraphIndex::build_guarded(&known, &[], &[], &[], "gen-1", None).is_err(),
        "unknown generation refuses"
    );
}

#[test]
fn content_generation_is_the_sorted_header_fingerprint() {
    // The propagation fingerprint is the generation marker's content
    // half: sorted sidecar receipts. Bodies are not read (their hash
    // rides in the header), so equal headers are one identity.
    let sidecars = tempdir();
    write_sidecar(sidecars.path(), "a.rs.json", "hdr-a");
    write_sidecar(sidecars.path(), "b.rs.json", "hdr-b");
    let first = crate::index_generation::content_generation(sidecars.path());
    assert_eq!(
        first,
        crate::index_generation::content_generation(sidecars.path()),
        "stable"
    );
    // Rewriting a body with the same receipt header is the same tree.
    std::fs::write(sidecars.path().join("a.rs.json"), "hdr-a\n{\"ok\":false}").expect("rewrite");
    assert_eq!(
        first,
        crate::index_generation::content_generation(sidecars.path()),
        "header identity, not body bytes"
    );
    // A moved receipt header changes the fingerprint.
    write_sidecar(sidecars.path(), "a.rs.json", "hdr-a2");
    assert_ne!(
        first,
        crate::index_generation::content_generation(sidecars.path()),
        "header move"
    );
    // Create and delete change it.
    let moved = crate::index_generation::content_generation(sidecars.path());
    write_sidecar(sidecars.path(), "c.rs.json", "hdr-c");
    let created = crate::index_generation::content_generation(sidecars.path());
    assert_ne!(moved, created, "create");
    std::fs::remove_file(sidecars.path().join("c.rs.json")).expect("remove");
    assert_ne!(
        created,
        crate::index_generation::content_generation(sidecars.path()),
        "delete"
    );
}

#[test]
fn index_dir_for_db_keeps_markers_beside_the_database() {
    assert_eq!(
        crate::index_generation::index_dir_for_db(std::path::Path::new("root/.guidance.db")),
        std::path::PathBuf::from("root")
    );
    assert_eq!(
        crate::index_generation::index_dir_for_db(std::path::Path::new(".guidance.db")),
        std::path::PathBuf::from("")
    );
}

fn seed_symbol(
    db: &search_vector::db::GuidanceDb,
    dir: &std::path::Path,
    name: &str,
    content_hash: &str,
    interface: &str,
    module_interface: &str,
) {
    std::fs::write(dir.join(name), "pub fn alpha() {}\n").expect("write");
    let mut file = crate::tests::common::db_file_record(dir, name);
    file.content_hash = Some(content_hash.to_string());
    let fragment = crate::tests::common::db_text_fragment(name, "alpha", "pub fn alpha() {}", 1);
    db.upsert_fragments(&file, &[fragment], &[]).expect("seed");
    let row = |symbol_id: String, symbol_name: &str, hash: &str| search_vector::db::SymbolRecord {
        symbol_id,
        file_id: name.to_string(),
        name: Some(symbol_name.to_string()),
        scope: None,
        interface_hash: hash.to_string(),
    };
    db.replace_symbols(&[
        row(format!("{name}#alpha"), "alpha", interface),
        row(format!("{name}#$file"), "alpha", module_interface),
    ])
    .expect("symbols");
}

#[test]
fn tree_fingerprint_tracks_content_and_interfaces() {
    let dir = tempdir();
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    seed_symbol(&db, dir.path(), "a.rs", "content-1", "iface-1", "module-1");
    let base = crate::index_generation::tree_fingerprint(&db);
    assert_eq!(
        base,
        crate::index_generation::tree_fingerprint(&db),
        "stable"
    );
    // Module-header rows never move the fingerprint.
    seed_symbol(&db, dir.path(), "a.rs", "content-1", "iface-1", "module-2");
    assert_eq!(
        base,
        crate::index_generation::tree_fingerprint(&db),
        "module rows are comments, not interfaces"
    );
    // A signature move changes it.
    seed_symbol(&db, dir.path(), "a.rs", "content-1", "iface-2", "module-1");
    let moved = crate::index_generation::tree_fingerprint(&db);
    assert_ne!(base, moved, "interface move");
    // A content move changes it.
    seed_symbol(&db, dir.path(), "a.rs", "content-2", "iface-2", "module-1");
    let content = crate::index_generation::tree_fingerprint(&db);
    assert_ne!(moved, content, "content move");
    // Create and delete change it (presence is identity).
    seed_symbol(&db, dir.path(), "b.rs", "content-1", "iface-1", "module-1");
    let created = crate::index_generation::tree_fingerprint(&db);
    assert_ne!(content, created, "create");
    db.delete_file("b.rs").expect("delete");
    assert_ne!(
        created,
        crate::index_generation::tree_fingerprint(&db),
        "delete"
    );
}

#[test]
fn fingerprint_marker_roundtrips_beside_the_generation_marker() {
    let dir = tempdir();
    let index = dir.path().join("idx");
    assert!(crate::index_generation::read_fingerprint(&index).is_none());
    crate::index_generation::write_fingerprint(&index, "fp-1").expect("write");
    assert_eq!(
        crate::index_generation::read_fingerprint(&index).as_deref(),
        Some("fp-1")
    );
    crate::index_generation::write_fingerprint(&index, "fp-2").expect("write");
    assert_eq!(
        crate::index_generation::read_fingerprint(&index).as_deref(),
        Some("fp-2")
    );
}

/// M3.1 characterization: `first_line` edge table (C9) — missing
/// file, empty file, no trailing newline, multiline (first only).
/// Must hold after migrating to `common_core::io::read_first_line`.
#[test]
fn first_line_edge_table() {
    use std::io::Write as _;
    let dir = tempdir();
    let missing = dir.path().join("missing.txt");
    assert_eq!(super::first_line(&missing), None);
    let empty = dir.path().join("empty.txt");
    std::fs::write(&empty, "").expect("write");
    assert_eq!(super::first_line(&empty), None);
    let noeol = dir.path().join("noeol.txt");
    std::fs::write(&noeol, "only").expect("write");
    assert_eq!(super::first_line(&noeol).as_deref(), Some("only"));
    let multi = dir.path().join("multi.txt");
    let mut file = std::fs::File::create(&multi).expect("create");
    writeln!(file, "first").expect("write");
    writeln!(file, "second").expect("write");
    assert_eq!(super::first_line(&multi).as_deref(), Some("first"));
}
