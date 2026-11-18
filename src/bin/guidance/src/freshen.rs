//! Read-path freshness: converge the files behind an answer,
//! then re-run once. Sidecars plus index only — never fix waves.

use std::path::{Path, PathBuf};

/// Stale-cited files under the workspace. Missing files stay listed:
/// convergence fails loudly instead of silently dropping the citation.
#[must_use]
pub fn stale_citation_files(
    answer: &guidance_core::read::engine::ReadAnswer,
    workspace: &Path,
) -> Vec<PathBuf> {
    use guidance_core::sync_engine::absolute_or;
    let root = absolute_or(workspace.to_path_buf());
    let mut out = Vec::new();
    for cite in &answer.citations {
        if !cite.stale {
            continue;
        }
        let live = absolute_or(cite.source.live_path());
        if live.is_dir() || !live.starts_with(&root) || out.contains(&live) {
            continue;
        }
        out.push(live);
    }
    out
}

/// Converge `files` (sidecar gen, stale-only) and re-ingest them.
/// Gen failure aborts first, so the caller serves stale-labelled.
pub fn freshen_files(
    workspace: &Path,
    guidance_dir: &Path,
    db: &guidance_core::query::search_db::GuidanceSearchDb,
    db_path: &Path,
    src_dirs: &[PathBuf],
    files: &[PathBuf],
) -> Result<(usize, usize), String> {
    use guidance_core::sync_engine::SyncEngine;
    let hashes = db.source_content_hashes().unwrap_or_default();
    // One engine per source root, mirroring the status walk: sidecars
    // mirror the root, never the file's parent.
    let mut generated = 0usize;
    for file in files {
        let src_dir =
            crate::find_source_dir(file, src_dirs).unwrap_or_else(|| workspace.to_path_buf());
        let mut engine =
            SyncEngine::new(guidance_dir.to_path_buf(), src_dir).with_source_hashes(hashes.clone());
        generated += usize::from(
            engine
                .gen_if_stale(file)
                .map_err(|error| format!("freshen {}: {error}", file.display()))?,
        );
    }
    let stats = crate::index_cmd::ingest_workspace_fragments(db_path, workspace, files)
        .map_err(|error| format!("freshen index: {error}"))?;
    Ok((generated, stats.changed.len() + stats.deleted.len()))
}

/// Search-side entry: default guidance dir and config roots, hit
/// files in. Returns true when anything moved (caller re-runs once).
pub fn freshen_search_hits(
    workspace: &Path,
    db: &guidance_core::query::search_db::GuidanceSearchDb,
    db_path: &Path,
    files: &[PathBuf],
) -> Result<bool, String> {
    let cfg = crate::load_project_config(workspace);
    let guidance_dir = workspace.join(".guidance");
    let src_dirs = crate::src_dirs_from_config_with(workspace, &cfg);
    let (generated, changed) =
        freshen_files(workspace, &guidance_dir, db, db_path, &src_dirs, files)?;
    Ok(generated > 0 || changed > 0)
}
