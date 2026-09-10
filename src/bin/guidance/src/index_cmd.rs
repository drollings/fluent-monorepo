//! `index` command (P5): thin shell over the P2 incremental pipeline.
//!
//! Path-scoped indexing only — diff, extract, embed, and commit live in
//! `guidance-core` (`SyncEngine::gen_scoped`); this module parses flags and
//! renders counts. Failure is stored data: per-file failures are counted,
//! never thrown.

use std::path::{Path, PathBuf};

use guidance_core::query::ingest::{default_en_pipeline, ingest_text_file};
use guidance_core::selection::{select_files, FileSelection, ScanDiagnostics};
use guidance_core::sync_engine::SyncEngine;
use guidance_core::zg_types::{FileInfo, FileKind};
use search_vector::GuidanceDb;

/// Run the `index` command over `path` (file or directory; defaults to the
/// workspace). Prints counts and returns `(generated, failed)`.
pub fn cmd_index(
    workspace: &str,
    json_dir: &str,
    path: Option<&str>,
    force: bool,
) -> (usize, usize) {
    match run_index(workspace, json_dir, path) {
        Ok((generated, failed)) => {
            let target = path.unwrap_or(workspace);
            if force {
                println!("Indexed {target} (force): {generated} generated, {failed} failed");
            } else {
                println!("Indexed {target}: {generated} generated, {failed} failed");
            }
            (generated, failed)
        }
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
    }
}

/// Execute path-scoped indexing (testable; no printing, no process exit).
pub fn run_index(
    workspace: &str,
    json_dir: &str,
    path: Option<&str>,
) -> Result<(usize, usize), String> {
    let workspace_path = PathBuf::from(workspace);
    let guidance_dir = PathBuf::from(json_dir);
    if !workspace_path.is_dir() {
        return Err(format!("workspace not found: {workspace}"));
    }
    let target = path.map_or_else(|| workspace_path.clone(), PathBuf::from);
    if !target.exists() {
        return Err(format!("path not found: {}", target.display()));
    }
    common_core::ensure_dir_or_panic(&guidance_dir);
    let mut engine = SyncEngine::new(guidance_dir, workspace_path);
    engine
        .gen_scoped(&target)
        .map_err(|error| format!("index failed: {error}"))
}

/// Fragment-ingestion counts (stored data, never thrown).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FragmentIngestStats {
    /// Files ingested into `zg_*` (FTS + lemmas; embeddings skipped without
    /// a configured backend).
    pub files: usize,
    /// Fragments written.
    pub fragments: usize,
    /// Image files discovered + kind-tagged but skipped (text-only parity,
    /// Gate 0 §5; a VL backend is P6+ work).
    pub skipped_images: usize,
    /// Files that failed to read or ingest (stored count, run continues).
    pub failed: usize,
}

/// Ingest workspace source files into the fragment index (`zg_*` FTS5 +
/// `fragment_lemmas`; no embedder, no NLP pipeline — L1+L2 only, L3 arrives
/// with a configured embedding backend). Thin shell over
/// `ingest_text_file`; selection honors `.gitignore` (search-index noise
/// discipline, matching `zg`).
pub fn ingest_workspace_fragments(
    db_path: &Path,
    workspace: &Path,
    src_dirs: &[PathBuf],
) -> Result<FragmentIngestStats, String> {
    let db = GuidanceDb::open(db_path)
        .map_err(|error| format!("cannot open index at {}: {error}", db_path.display()))?;
    // Rule-lemmatizer pipeline (no model): L2 inflections collapse at
    // index time at zero embedding cost. Built once per sync.
    let nlp = default_en_pipeline();
    let mut stats = FragmentIngestStats::default();
    for src_dir in src_dirs {
        if !src_dir.is_dir() {
            continue;
        }
        let selection = FileSelection {
            roots: vec![src_dir.clone()],
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            extra_skip_dirs: Vec::new(),
            honor_gitignore: true,
            max_bytes_override: None,
        };
        let mut diag = ScanDiagnostics::default();
        let files = select_files(&selection, &mut diag)
            .map_err(|error| format!("selection failed: {error}"))?;
        for selected in files {
            if selected.kind == FileKind::Image {
                stats.skipped_images += 1;
                continue;
            }
            let text = match std::fs::read_to_string(&selected.path) {
                Ok(text) => text,
                Err(_) => {
                    stats.failed += 1;
                    continue;
                }
            };
            let format = selected
                .path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("")
                .to_string();
            let file = FileInfo {
                id: selected.path.to_string_lossy().into_owned(),
                absolute_path: selected.path.to_string_lossy().into_owned(),
                relative_path: selected.relative.clone(),
                root_path: workspace.to_string_lossy().into_owned(),
                size_bytes: selected.size_bytes,
                last_modified_time: selected.modified_ms,
                content_hash: None,
                kind: Some(selected.kind),
                format,
                index_status: None,
            };
            match ingest_text_file(&db, &file, &text, None, nlp.as_ref()) {
                Ok(ingested) => {
                    stats.files += 1;
                    stats.fragments += ingested.fragments;
                }
                Err(_) => stats.failed += 1,
            }
        }
    }
    Ok(stats)
}

#[cfg(test)]
#[path = "../tests/index_cmd.rs"]
mod tests;
