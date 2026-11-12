//! `index` command (P5): thin shell over the P2 incremental pipeline.
//!
//! Path-scoped indexing only — diff, extract, embed, and commit live in
//! `guidance-core` (`SyncEngine::gen_scoped`); this module parses flags and
//! renders counts. Failure is stored data: per-file failures are counted,
//! never thrown.

use std::path::{Path, PathBuf};
use std::time::Instant;

use guidance_core::graph_index::GraphIndex;
use guidance_core::query::ingest::{tree_sitter_format, LazyNlp};
use guidance_core::search_types::FileKind;
use guidance_core::selection::{select_files, FileSelection, ScanDiagnostics, SelectedFile};
use guidance_core::sync_engine::SyncEngine;
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
/// Fragment-ingestion counts (stored data, never thrown).
pub struct FragmentIngestStats {
    /// Files ingested into the fragment index (FTS + lemmas; embeddings skipped without
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
    /// Changed files whose committed interface moved (new, deleted, or
    /// rehashed symbols): the only seeds whose dependents re-process.
    /// Missing stored rows fail open toward regeneration.
    pub sig_moved: Vec<String>,
    /// Changed files defining macros: syntactic edges are unreliable, so
    /// their package re-processes conservatively.
    pub macro_moved: Vec<String>,
}

/// Absolute paths the selection yielded (every kind, including skipped
/// images — presence on disk is what matters, not ingestibility).
fn selected_paths(files: &[SelectedFile]) -> std::collections::HashSet<String> {
    files
        .iter()
        .map(|f| f.path.to_string_lossy().into_owned())
        .collect()
}

/// Ingest selected files into the fragment index (FTS5 + lemmas; L3
/// vectors arrive via item chunks). NLP runs for natural-language files only.
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
    // Whole-file embeddings belong to the item-chunk pipeline; the live
    // writer's text exceeds the embed server batch and looped forever.
    let mut stats = FragmentIngestStats::default();
    // Accumulated phase time, reported once by `GUIDANCE_PROFILE=1`.
    let mut t_select = 0u128;
    let mut t_freshness = 0u128;
    let mut t_read = 0u128;
    let mut t_ingest = 0u128;
    let mut t_graph = 0u128;
    // One parser for the whole sync: fragments, comment blocks, and
    // graph inputs share one parse per file (never a second parser,
    // never per-file parser construction).
    let mut parser = guidance_core::ast_parser::AstParser::new();
    // Every path selection yields, across all src dirs: presence on disk
    // is what the deleted purge below diffs against (images included —
    // they exist even though they never ingest). A root may be a single
    // file (the `sync --file` scope) or a directory; missing roots stay
    // skipped, and the purge is scoped per root so a file root never
    // dooms the files beside it.
    let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
    // One fingerprint scan for the whole pass: per-file probes cost a
    // round-trip each (seconds on thousand-file trees). An empty map on
    // read failure keeps the old fail-open: everything re-ingests.
    let fingerprints = {
        let bulk_start = Instant::now();
        let map = db.all_fragment_freshness().unwrap_or_default();
        t_freshness += bulk_start.elapsed().as_millis();
        map
    };
    for src_dir in src_dirs {
        if !src_dir.exists() {
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
        let select_start = Instant::now();
        let files = select_files(&selection, &mut diag)
            .map_err(|error| format!("selection failed: {error}"))?;
        t_select += select_start.elapsed().as_millis();
        present.extend(selected_paths(&files));
        for selected in files {
            // Index metadata is never content: the generation marker
            // lives beside the database by design, and ingesting it
            // would re-index a bookkeeping write on every run.
            if selected
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == guidance_core::index_generation::GENERATION_FILE)
            {
                continue;
            }
            if selected.kind == FileKind::Image {
                stats.skipped_images += 1;
                continue;
            }
            // Change gate: an unchanged file keeps its committed row —
            // skip the read + lemmatize + upsert entirely. Fingerprints
            // load once per pass (one scan, not one probe per file).
            let file_id = selected.path.to_string_lossy().into_owned();
            let freshness_start = Instant::now();
            let unchanged = match fingerprints.get(&file_id) {
                Some(fresh) => {
                    fresh.size_bytes == selected.size_bytes
                        && fresh.last_modified_time == selected.modified_ms
                }
                None => false,
            };
            t_freshness += freshness_start.elapsed().as_millis();
            if unchanged {
                stats.skipped_unchanged += 1;
                continue;
            }
            let read_start = Instant::now();
            let text = match std::fs::read_to_string(&selected.path) {
                Ok(text) => text,
                Err(_) => {
                    t_read += read_start.elapsed().as_millis();
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
            let absolute_path = selected.path.to_string_lossy().into_owned();
            let defines_macros = guidance_core::invalidation::defines_macros(&text);
            let record = search_vector::db::FileRecord {
                id: file_id,
                absolute_path: absolute_path.clone(),
                relative_path: selected.relative.clone(),
                root_path: workspace.to_string_lossy().into_owned(),
                size_bytes: selected.size_bytes,
                last_modified_time: selected.modified_ms,
                // Stored source hash for the member-JSON clock gate (the
                // ambiguity-window tiebreak reads it back next sync).
                // Free: the bytes are already in hand — no extra read.
                content_hash: Some(common_core::hash::sha256_hex(text.as_bytes())),
                kind: Some(selected.kind.as_str().to_string()),
                format,
                index_status: Some("indexed".to_string()),
                fail_count: 0,
                last_error: None,
            };
            t_read += read_start.elapsed().as_millis();
            let ingest_start = Instant::now();
            // One extract pass per file: item chunks, comment blocks, and
            // graph inputs share a single parse (never a second parser).
            let source_format = tree_sitter_format(&record.format)
                .unwrap_or(record.format.as_str())
                .to_string();
            let source = guidance_core::extractor::ExtractSource {
                file_id: record.id.clone(),
                text,
                format: source_format,
                kind: selected.kind,
            };
            let chunk_options = match guidance_core::extractor::ChunkOptions::resolve(None, None) {
                Ok(options) => options,
                Err(error) => {
                    eprintln!("prepare failed for {}: {error}", record.absolute_path);
                    t_ingest += ingest_start.elapsed().as_millis();
                    stats.failed += 1;
                    continue;
                }
            };
            let (mut fragments, harvest, blocks) =
                match guidance_core::index_pipeline::prepare_fragments(
                    &mut parser,
                    &source,
                    chunk_options,
                ) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        eprintln!("prepare failed for {}: {error}", record.absolute_path);
                        t_ingest += ingest_start.elapsed().as_millis();
                        stats.failed += 1;
                        continue;
                    }
                };
            if let Some(module) =
                guidance_core::extractor::code::module_fragment_for(&record.id, &blocks)
            {
                fragments.push(module);
            }
            // Spacy pays off on prose, not code: whole-file lemmatization
            // cost seconds per source file and only added punctuation.
            let nlp_for_file = if selected.kind == FileKind::Text {
                nlp.get()
            } else {
                None
            };
            let before: Vec<(String, String)> = db
                .symbols_for_file(&record.id)
                .map(|rows| {
                    rows.into_iter()
                        // Module fragments carry comments, not interfaces:
                        // their appearance must not expand dependents.
                        .filter(|row| row.name.as_deref() != Some("$file"))
                        .map(|row| (row.symbol_id, row.interface_hash))
                        .collect()
                })
                .unwrap_or_default();
            match guidance_core::query::writer::FragmentWriter::write_items(
                &db,
                &record,
                selected.kind,
                &fragments,
                &blocks,
                &[],
                nlp_for_file,
                guidance_core::query::writer::VectorPolicy::Optional,
            ) {
                Ok(count) => {
                    let file_ms = ingest_start.elapsed().as_millis();
                    t_ingest += file_ms;
                    if crate::profile::enabled() && file_ms >= 50 {
                        eprintln!(
                            "[profile] slow ingest {}: {} ms ({} bytes, {:?}, {} fragments)",
                            selected.relative, file_ms, selected.size_bytes, selected.kind, count
                        );
                    }
                    let graph_start = Instant::now();
                    // Persist the file's graph inputs from the same pass
                    // (changed files only — unchanged files keep their
                    // committed rows via the gate above). A graph-write
                    // failure rolls the fragment rows back so the next
                    // sync re-ingests the file instead of going stale.
                    let symbols: Vec<(String, Vec<String>)> = harvest
                        .symbols
                        .iter()
                        .map(|symbol| {
                            (
                                symbol.name.clone().unwrap_or_default(),
                                symbol.calls.clone(),
                            )
                        })
                        .collect();
                    if let Err(error) =
                        db.replace_file_graph(&record.absolute_path, &harvest.imports, &symbols)
                    {
                        eprintln!("graph persist failed for {}: {error}", record.absolute_path);
                        let _ = db.delete_file(&record.id);
                        t_graph += graph_start.elapsed().as_millis();
                        stats.failed += 1;
                        continue;
                    }
                    t_graph += graph_start.elapsed().as_millis();
                    stats.files += 1;
                    stats.fragments += count;
                    stats.changed.push(record.absolute_path.clone());
                    let after: Vec<(String, String)> = db
                        .symbols_for_file(&record.id)
                        .map(|rows| {
                            rows.into_iter()
                                .filter(|row| row.name.as_deref() != Some("$file"))
                                .map(|row| (row.symbol_id, row.interface_hash))
                                .collect()
                        })
                        .unwrap_or_default();
                    // Sig gate: only interface moves expand to dependents.
                    // Missing stored rows (new or legacy files) fail open
                    // toward regeneration, never toward a silent skip.
                    if before.is_empty()
                        || guidance_core::invalidation::file_sig_changed(&before, &after)
                    {
                        stats.sig_moved.push(record.absolute_path.clone());
                    }
                    if defines_macros {
                        stats.macro_moved.push(record.absolute_path.clone());
                    }
                }
                Err(error) => {
                    eprintln!("commit failed for {}: {error}", record.absolute_path);
                    t_ingest += ingest_start.elapsed().as_millis();
                    stats.failed += 1;
                }
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
    let purge_start = Instant::now();
    let mut doomed: Vec<(String, String)> = Vec::new();
    match db.list_files() {
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
        let doomed_paths: Vec<String> = doomed.iter().map(|(_, path)| path.clone()).collect();
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
    let t_purge = purge_start.elapsed().as_millis();
    // Segment merge is meaningful only after a bulk pass; a 1-file
    // refresh should not pay a whole-index merge.
    if stats.files >= 64 {
        if let Err(error) = db.optimize_fts() {
            eprintln!("warning: fts optimize failed: {error}");
        }
    }
    crate::profile::report("index.ingest.select", t_select);
    crate::profile::report("index.ingest.freshness", t_freshness);
    crate::profile::report("index.ingest.read", t_read);
    crate::profile::report("index.ingest.lemma_db", t_ingest);
    crate::profile::report("index.ingest.graph", t_graph);
    crate::profile::report("index.ingest.purge", t_purge);
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
    let files = db
        .list_files()
        .map_err(|error| format!("list files: {error}"))?;
    let known: Vec<String> = files.into_iter().map(|file| file.absolute_path).collect();
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
