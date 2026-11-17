//! Eval/corpus exclusion: measurement fixtures are not retrieval
//! inputs. The test proves the shipping composition — selection skips
//! them before the index, so no recall, symbol, graph, or card arm
//! can surface them — over a real indexed workspace. Included via
//! `#[path]` forwarder, per repo convention. Hermetic: tempdir
//! workspace, in-memory index, no endpoint.

use super::{is_benchmark_corpus_path, FileSelection};
use crate::eval::is_eval_path;

#[test]
fn measurement_paths_are_recognized() {
    assert!(is_eval_path("doc/guidance/eval/questions.jsonl"));
    assert!(is_eval_path("doc/guidance/eval/claims.jsonl"));
    assert!(is_eval_path("./doc/guidance/eval/findings.jsonl"));
    assert!(is_benchmark_corpus_path(
        "src/guidance/corpora/benchmark_tasks.jsonl"
    ));
    assert!(is_benchmark_corpus_path(
        "src/guidance/corpora/nav_seed.json"
    ));
    assert!(!is_benchmark_corpus_path("src/guidance/corpus_loader.rs"));
    assert!(!is_benchmark_corpus_path("src/guidance/src/code.rs"));
    assert!(!is_eval_path("doc/guidance/overview.md"));
    assert!(!is_eval_path("src/guidance/corpora/nav_seed.json"));
}

fn stage_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    // Same distinctive content in three files: a normal source file, an
    // eval golden, and a benchmark-corpus fixture. Only the normal file
    // may ever surface through any arm.
    for rel in [
        "src/beam.rs",
        "doc/guidance/eval/zzbeam.md",
        "src/guidance/corpora/zzbeam.jsonl",
    ] {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(
            &path,
            "fn zzbeam_calibrator() {}\nCalibrates the zzbeam flux drive.\n",
        )
        .expect("write");
    }
    // A capability corpus pointing at the eval golden: import must
    // refuse the brief before any read, never store it.
    std::fs::write(
        dir.path().join("doc/beam.md"),
        "# Beam\n\nCalibrates the zzbeam flux drive in beam source.\n",
    )
    .expect("write beam doc");
    std::fs::create_dir_all(dir.path().join("doc/capabilities")).expect("mkdir corpus");
    std::fs::write(
        dir.path().join("doc/capabilities/corpus.json"),
        r#"{"briefs": [
            {"id": "doc/guidance/eval/zzbeam.md#zzbeam", "title": "Zzbeam",
             "purpose": "Calibrates the zzbeam flux drive.",
             "crate": "beam", "domain": "drive",
             "source_path": "doc/guidance/eval/zzbeam.md", "anchor": "zzbeam"},
            {"id": "doc/beam.md#beam", "title": "Beam",
             "purpose": "Calibrates the zzbeam flux drive in beam source.",
             "crate": "beam", "domain": "drive",
             "source_path": "doc/beam.md", "anchor": "beam"}]}"#,
    )
    .expect("write corpus");
    dir
}

fn index_staged(
    root: &std::path::Path,
) -> std::sync::Arc<crate::query::search_db::GuidanceSearchDb> {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    let db = Arc::new(crate::query::search_db::GuidanceSearchDb::open_in_memory().expect("db"));
    let selection = FileSelection {
        workspace_root: root.to_path_buf(),
        include_fixtures: false,
        roots: vec![root.to_path_buf()],
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        extra_skip_dirs: Vec::new(),
        honor_gitignore: false,
        max_bytes_override: None,
    };
    crate::index_pipeline::index_workspace(
        Arc::clone(&db),
        fluent_llm::embeddings::NoopEmbedding::new(4),
        &selection,
        &crate::index_pipeline::IndexOptions::default(),
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .expect("index");
    // Capability import refuses the eval-pointing brief before any
    // read; the beam-pointing brief imports normally.
    let report = crate::read::doc_cards::import_capability_corpus(
        root,
        &crate::config::ProjectConfig::default(),
        &db,
    )
    .expect("import");
    assert_eq!(report.skipped_measurement, 1, "{report:?}");
    assert_eq!(db.capability_ready_count().expect("count"), 1);
    db
}

fn leaked(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| is_eval_path(path) || is_benchmark_corpus_path(path))
        .cloned()
        .collect()
}

#[test]
fn no_recall_arm_surfaces_measurement_paths() {
    use crate::query::db_storage::GuidanceDbStorage;
    use crate::query::recall::run_recall;
    use crate::search_types::{SearchPlan, SearchPlanRoute, SearchPlanRouteMode};
    let dir = stage_workspace();
    let db = index_staged(dir.path());
    let storage = GuidanceDbStorage::new(&db);
    // FTS finds the normal file; the vector route has no stored
    // vectors (harness is embedder-free) but must still leak nothing.
    let plan = SearchPlan {
        routes: vec![SearchPlanRoute {
            mode: SearchPlanRouteMode::Fts,
            query: "zzbeam calibrator flux drive".to_string(),
        }],
        limit: Some(10),
        ..Default::default()
    };
    let output = run_recall(&plan, &storage, None, None).expect("recall");
    let paths: Vec<String> = output
        .hits
        .iter()
        .map(|hit| hit.file.relative_path.clone())
        .collect();
    assert!(
        leaked(&paths).is_empty(),
        "fts leaked measurement paths: {paths:?}"
    );
    assert!(
        paths.iter().any(|path| path == "src/beam.rs"),
        "fts finds the normal file: {paths:?}"
    );
    for mode in [SearchPlanRouteMode::Fts, SearchPlanRouteMode::Vector] {
        let plan = SearchPlan {
            routes: vec![SearchPlanRoute {
                mode,
                query: "zzbeam calibrator flux drive".to_string(),
            }],
            limit: Some(10),
            ..Default::default()
        };
        let output = run_recall(&plan, &storage, None, None).expect("recall");
        let paths: Vec<String> = output
            .hits
            .iter()
            .map(|hit| hit.file.relative_path.clone())
            .collect();
        assert!(
            leaked(&paths).is_empty(),
            "{mode:?} leaked measurement paths: {paths:?}"
        );
    }
}

#[test]
fn no_symbol_or_graph_arm_surfaces_measurement_paths() {
    let dir = stage_workspace();
    let db = index_staged(dir.path());
    let store = crate::read::planes::symbol::GuidanceStructureStore::new(&db);
    let sites =
        crate::read::planes::symbol::StructureStore::exact_symbols(&store, "zzbeam_calibrator");
    let files: Vec<String> = sites.iter().map(|site| site.file.clone()).collect();
    assert!(
        leaked(&files).is_empty(),
        "symbol arm leaked measurement paths: {files:?}"
    );
    let plane = crate::read::planes::graph::GraphPlane::hydrate(
        &db,
        &[dir.path().to_string_lossy().into_owned()],
    )
    .expect("hydrate");
    let callers = plane.callers("zzbeam_calibrator");
    let caller_files: Vec<String> = callers.iter().map(|caller| caller.file.clone()).collect();
    assert!(
        leaked(&caller_files).is_empty(),
        "graph arm leaked measurement paths: {caller_files:?}"
    );
}

#[test]
fn no_card_arm_surfaces_measurement_paths() {
    let dir = stage_workspace();
    let db = index_staged(dir.path());
    let hits = crate::read::planes::docs::retrieve_cards(
        "zzbeam calibrator flux drive",
        Some(dir.path()),
        &db,
        None,
        None,
    );
    for hit in &hits {
        assert!(
            !is_eval_path(&hit.source_path) && !is_benchmark_corpus_path(&hit.source_path),
            "card arm leaked measurement path: {hit:?}"
        );
        assert!(
            hit.code_paths
                .iter()
                .all(|path| !is_eval_path(path) && !is_benchmark_corpus_path(path)),
            "card scopes leaked measurement paths: {hit:?}"
        );
    }
    // The eval-pointing brief was refused at import: only the beam
    // card ranks.
    assert!(
        hits.iter().all(|hit| hit.id == "doc/beam.md#beam"),
        "{hits:?}"
    );
}

#[test]
fn selection_skips_measurement_paths_before_the_index() {
    use crate::selection::{select_files, ScanDiagnostics};
    let dir = stage_workspace();
    let selection = FileSelection {
        workspace_root: dir.path().to_path_buf(),
        include_fixtures: false,
        roots: vec![dir.path().to_path_buf()],
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        extra_skip_dirs: Vec::new(),
        honor_gitignore: false,
        max_bytes_override: None,
    };
    let mut diag = ScanDiagnostics::default();
    let selected = select_files(&selection, &mut diag).expect("select");
    let names: Vec<String> = selected
        .iter()
        .map(|entry| {
            entry
                .path
                .strip_prefix(dir.path())
                .expect("relative")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert!(
        names.contains(&"src/beam.rs".to_string()),
        "normal file selected: {names:?}"
    );
    assert!(
        !names.iter().any(|name| is_eval_path(name)),
        "eval paths skipped: {names:?}"
    );
    assert!(
        !names.iter().any(|name| is_benchmark_corpus_path(name)),
        "benchmark corpus skipped: {names:?}"
    );
}
