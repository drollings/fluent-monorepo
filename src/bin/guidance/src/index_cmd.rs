//! `index` command (P5): thin shell over the P2 incremental pipeline.
//!
//! Path-scoped indexing only — diff, extract, embed, and commit live in
//! `guidance-core` (`SyncEngine::gen_scoped`); this module parses flags and
//! renders counts. Failure is stored data: per-file failures are counted,
//! never thrown.

use std::path::{Path, PathBuf};

use guidance_core::graph_index::GraphIndex;
use guidance_core::query::ingest::{ingest_text_file, LazyNlp};
use guidance_core::selection::{select_files, FileSelection, ScanDiagnostics, SelectedFile};
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
    /// Files skipped because the committed fragment row is unchanged
    /// (size + mtime match; embedded when a backend is configured).
    pub skipped_unchanged: usize,
    /// Fragments written.
    pub fragments: usize,
    /// Image files discovered + kind-tagged but skipped (text-only parity,
    /// Gate 0 §5; a VL backend is P6+ work).
    pub skipped_images: usize,
    /// Files that failed to read or ingest (stored count, run continues).
    pub failed: usize,
    /// Absolute paths ingested this run (seeds for dependent propagation).
    pub changed: Vec<String>,
    /// Absolute paths purged this run (in the index but missing on disk).
    pub deleted: Vec<String>,
    /// Dependents of the purged paths, resolved while their graph rows
    /// still existed (extra propagation seeds for the caller — the
    /// post-purge closure cannot expand deleted seeds, so without this
    /// the dependents of a deletion would silently miss re-processing).
    pub deleted_dependents: Vec<String>,
}

/// Absolute paths the selection yielded (every kind, including skipped
/// images — presence on disk is what matters, not ingestibility).
fn selected_paths(files: &[SelectedFile]) -> std::collections::HashSet<String> {
    files
        .iter()
        .map(|f| f.path.to_string_lossy().into_owned())
        .collect()
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
    // index time at zero embedding cost. Built lazily on the first
    // file that actually ingests — fully-skipped syncs never pay it.
    let nlp = LazyNlp::new();
    // L3 embedder from the workspace project config (offline
    // construction; absent backend degrades to unembedded fragments,
    // counted in stats — same contract as before, now actually usable).
    let cfg =
        guidance_core::config::load_config(workspace).unwrap_or_default();
    let embedder = crate::embed::embedder_from_config(&cfg);
    let mut stats = FragmentIngestStats::default();
    // One parser for the whole sync: per-file graph harvest reuses the
    // harvest `assemble` consumes (never a second parser, never per-file
    // parser construction).
    let mut parser = guidance_core::ast_parser::AstParser::new();
    // Every path selection yields, across all src dirs: presence on disk
    // is what the deleted purge below diffs against (images included —
    // they exist even though they never ingest).
    let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
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
        present.extend(selected_paths(&files));
        for selected in files {
            if selected.kind == FileKind::Image {
                stats.skipped_images += 1;
                continue;
            }
            // Change gate: an unchanged file keeps its committed row —
            // skip the read + lemmatize + embed + upsert entirely. The
            // probe fails open (ingest as before) so a read error here
            // can never lose index content. A configured embedder still
            // re-ingests unembedded rows to backfill vectors.
            let file_id = selected.path.to_string_lossy().into_owned();
            let unchanged = match db.fragment_freshness(&file_id) {
                Ok(Some(fresh)) => {
                    fresh.size_bytes == selected.size_bytes
                        && fresh.last_modified_time == selected.modified_ms
                        && (embedder.is_none() || fresh.embedded)
                }
                _ => false,
            };
            if unchanged {
                stats.skipped_unchanged += 1;
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
                id: file_id,
                absolute_path: selected.path.to_string_lossy().into_owned(),
                relative_path: selected.relative.clone(),
                root_path: workspace.to_string_lossy().into_owned(),
                size_bytes: selected.size_bytes,
                last_modified_time: selected.modified_ms,
                // Stored source hash for the member-JSON clock gate (the
                // ambiguity-window tiebreak reads it back next sync).
                // Free: the bytes are already in hand — no extra read.
                content_hash: Some(common_core::hash::sha256_hex(text.as_bytes())),
                kind: Some(selected.kind),
                format,
                index_status: None,
            };
            match ingest_text_file(&db, &file, &text, embedder.as_deref(), nlp.get()) {
                Ok(ingested) => {
                    // Persist the file's graph inputs from the same bytes
                    // (changed files only — unchanged files keep their
                    // committed rows via the gate above). A graph-write
                    // failure rolls the fragment rows back so the next
                    // sync re-ingests the file instead of going stale.
                    let (specifiers, symbols) =
                        guidance_core::graph_index::harvest_graph_inputs(
                            &mut parser,
                            &file.format,
                            file.kind.unwrap_or(FileKind::Text),
                            &text,
                        );
                    if let Err(error) =
                        db.replace_file_graph(&file.absolute_path, &specifiers, &symbols)
                    {
                        eprintln!("graph persist failed for {}: {error}", file.absolute_path);
                        let _ = db.delete_file(&file.id);
                        stats.failed += 1;
                        continue;
                    }
                    stats.files += 1;
                    stats.fragments += ingested.fragments;
                    stats.changed.push(file.absolute_path.clone());
                }
                Err(_) => stats.failed += 1,
            }
        }
    }
    // Deleted purge: rows under the selection roots but missing on disk
    // go stale with no other signal (no fingerprint can fire for a file
    // that is gone). Reuses the fragment deleter — never a second one —
    // which also drops the file's graph rows. Dependents of the doomed
    // set are resolved BEFORE purging: the purge drops the graph rows
    // the dependents closure traverses, so a post-purge hydration cannot
    // expand deleted seeds (their dependents would silently miss
    // re-processing). The resolved dependents join the caller's seed set
    // via `deleted_dependents`; the doomed paths themselves stay in
    // `deleted` for sidecar removal.
    let mut doomed: Vec<(String, String)> = Vec::new();
    match db.zg_list_files() {
        Ok(rows) => {
            for row in rows {
                let under_scope = src_dirs
                    .iter()
                    .any(|dir| Path::new(&row.absolute_path).starts_with(dir));
                if under_scope && !present.contains(&row.absolute_path) {
                    doomed.push((row.id, row.absolute_path));
                }
            }
        }
        Err(error) => {
            eprintln!("deleted scan failed: {error}");
        }
    }
    if !doomed.is_empty() {
        let doomed_paths: Vec<String> =
            doomed.iter().map(|(_, path)| path.clone()).collect();
        let roots: Vec<String> = src_dirs
            .iter()
            .map(|dir| dir.to_string_lossy().into_owned())
            .collect();
        let pre = affected_for_sync(&db, &stats.changed, &doomed_paths, &roots);
        let doomed_set: std::collections::HashSet<&str> =
            doomed_paths.iter().map(String::as_str).collect();
        stats.deleted_dependents = pre
            .into_iter()
            .filter(|path| !doomed_set.contains(path.as_str()))
            .collect();
        for (id, path) in &doomed {
            if db.delete_file(id).is_ok() {
                stats.deleted.push(path.clone());
            }
        }
    }
    Ok(stats)
}

/// Propagation seeds → affected set over the hydrated graph (the same
/// `dependents_closure` the watch path calls — one traversal, two
/// callers). Seeds are fingerprint-changed files plus files missing on
/// disk; the closure conservatively over-approximates on the correctness
/// axis (over-invalidation costs time, under-invalidation costs silent
/// staleness). Fail-open: when hydration fails, the seeds themselves
/// still re-process — direct changes never silently skip.
pub fn affected_for_sync(
    db: &GuidanceDb,
    changed: &[String],
    deleted: &[String],
    roots: &[String],
) -> Vec<String> {
    let mut seeds: Vec<String> = changed.to_vec();
    seeds.extend(deleted.iter().cloned());
    seeds.sort();
    seeds.dedup();
    if seeds.is_empty() {
        return Vec::new();
    }
    match hydrated_graph(db, roots) {
        Ok(graph) => graph.dependents_closure(&seeds),
        Err(error) => {
            eprintln!("graph hydration failed ({error}); re-processing seeds only");
            seeds
        }
    }
}

/// Row-read the persisted graph inputs into a `GraphIndex` (no file
/// reads, no parsing — the M2 hydrated constructor). Shared by sync
/// propagation and the explain port (one hydration helper, two callers).
pub fn hydrated_graph(db: &GuidanceDb, roots: &[String]) -> Result<GraphIndex, String> {
    let files =
        db.zg_list_files().map_err(|error| format!("list files: {error}"))?;
    let known: Vec<String> =
        files.into_iter().map(|file| file.absolute_path).collect();
    let edges: Vec<(String, String)> = db
        .graph_edge_rows()
        .map_err(|error| format!("edge rows: {error}"))?
        .into_iter()
        .map(|row| (row.importer, row.specifier))
        .collect();
    let symbols: Vec<(String, String, Vec<String>)> = db
        .graph_symbol_rows()
        .map_err(|error| format!("symbol rows: {error}"))?
        .into_iter()
        .map(|row| (row.name, row.file, row.calls))
        .collect();
    GraphIndex::build_from_rows(&known, &edges, &symbols, roots)
        .map_err(|error| format!("assemble: {error}"))
}

#[cfg(test)]
#[path = "../tests/index_cmd.rs"]
mod tests;
