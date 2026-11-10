//! Generation guard: matching markers serve, anything else refuses.

use crate::index_generation::{generation_of, verify_generation, write_generation, GenerationVerdict};
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
    fn list_files(&self) -> Result<Vec<crate::search_types::FileInfo>, crate::query::plan::RecallError> {
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
        Option<(
            crate::search_types::Entity,
            crate::search_types::FileInfo,
        )>,
        crate::query::plan::RecallError,
    > {
        Ok(None)
    }

    fn list_entities_by_file(
        &self,
        _file_id: &str,
    ) -> Result<
        Vec<(
            crate::search_types::Entity,
            crate::search_types::FileInfo,
        )>,
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
