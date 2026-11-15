//! Doc→code jump records: per-chunk crate attribution plus the shared
//! funnel vocabulary, derived from the doc tree. Chunking and
//! attribution compose the capability ingest's shared seams (no second
//! scanner, no second table); terms compose the query-side funnel rule
//! so the two vocabularies cannot drift. Freshness composes the single
//! home (`FreshnessGate` matrix: stat sweep first, content hash only
//! inside the ambiguity window). The manifest is derived state:
//! rebuildable, never trusted without its marker, and an unreadable or
//! stale manifest reads as absent, never as evidence.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Manifest filename inside the guidance dir.
pub const JUMPS_FILE: &str = "doc_jumps.json";

/// Manifest schema version: a mismatch rebuilds, never misreads.
pub const JUMPS_MANIFEST_VERSION: u32 = 1;

/// Per-file read cap, mirroring the explain doc walk.
pub const JUMP_MAX_FILE_BYTES: u64 = 100_000;

/// One jump: a doc chunk, its owning crate, and its funnel terms.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// One jump: a doc chunk, its owning crate, and its funnel terms.
pub struct JumpRecord {
    /// Chunk identity (`source_path#anchor`).
    pub chunk_id: String,
    /// Workspace-relative doc path.
    pub source_path: String,
    /// Section anchor within the doc.
    pub anchor: String,
    /// Owning crate from the shared table (`unknown` fallback).
    pub crate_name: String,
    /// Doc domain from the shared decomposition.
    pub domain: String,
    /// Funnel terms of the chunk text (shared query vocabulary).
    pub terms: Vec<String>,
}

/// One manifest source row: content hash behind the ambiguity window.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// One manifest source row: content hash behind the ambiguity window.
pub struct JumpSource {
    /// Workspace-relative doc path.
    pub path: String,
    /// Content hash at derivation time.
    pub hash: String,
}

/// Derived manifest: jumps plus the source hashes that gate freshness.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Derived manifest: jumps plus the source hashes that gate freshness.
pub struct JumpManifest {
    /// Schema version (`JUMPS_MANIFEST_VERSION`).
    pub version: u32,
    /// Jump records, derivation order.
    pub jumps: Vec<JumpRecord>,
    /// Source hashes at derivation time.
    pub sources: Vec<JumpSource>,
}

/// Outcome of one jumps sync.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Outcome of one jumps sync.
pub struct JumpSyncReport {
    /// Whether the manifest was rewritten.
    pub rederived: bool,
    /// Chunk count in the manifest afterwards.
    pub chunks: usize,
}

/// Query-side index: crate → sorted deduped funnel terms.
#[derive(Debug, Clone, Default)]
/// Query-side index: crate → sorted deduped funnel terms.
pub struct JumpIndex {
    by_crate: HashMap<String, Vec<String>>,
}

impl JumpIndex {
    /// Funnel terms attributed to one crate (empty when unattributed).
    #[must_use]
    pub fn terms_for_crate(&self, crate_name: &str) -> &[String] {
        self.by_crate.get(crate_name).map_or(&[], Vec::as_slice)
    }
}

/// Derive jump records for one doc: shared chunker, shared
/// attribution, shared funnel terms. Pure.
#[must_use]
/// Derive jump records for one doc: shared chunker, shared
/// attribution, shared funnel terms. Pure.
pub fn derive_jumps_for_doc(source_path: &str, text: &str) -> Vec<JumpRecord> {
    let (chunks, _gaps) = crate::read::doc_cards::chunk_markdown_sections(source_path, text);
    chunks
        .into_iter()
        .map(|chunk| {
            let terms = crate::query::funnel::significant_terms(&chunk.text);
            JumpRecord {
                chunk_id: chunk.id,
                source_path: chunk.source_path,
                anchor: chunk.anchor,
                crate_name: chunk.crate_name,
                domain: chunk.domain,
                terms,
            }
        })
        .collect()
}

/// Doc tree behind the jumps: `doc/**/*.md` plus the two workspace
/// root orientation files, sorted, size-capped per file.
#[must_use]
/// Doc tree behind the jumps: `doc/**/*.md` plus the two workspace
/// root orientation files, sorted, size-capped per file.
pub fn doc_tree_files(workspace: &Path) -> Vec<PathBuf> {
    doc_tree_files_with(workspace, Path::new("doc"), true, &[])
}

/// Doc tree with explicit config: docs dir, root docs, and source-root
/// markdown (non-test/non-fixture). Shared by jumps and doc cards.
#[must_use]
/// Doc tree with explicit config: docs dir, root docs, and source-root
/// markdown (non-test/non-fixture). Shared by jumps and doc cards.
pub fn doc_tree_files_with(
    workspace: &Path,
    docs_dir: &Path,
    root_docs: bool,
    src_dirs: &[PathBuf],
) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let doc_dir = workspace.join(docs_dir);
    let mut stack = vec![doc_dir];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .collect();
        names.sort();
        for path in names {
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("md")
                && std::fs::metadata(&path).is_ok_and(|meta| meta.len() <= JUMP_MAX_FILE_BYTES)
            {
                files.push(path);
            }
        }
    }
    for src_dir in src_dirs {
        let root = if src_dir.is_absolute() {
            src_dir.clone()
        } else {
            workspace.join(src_dir)
        };
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut names: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .collect();
            names.sort();
            for path in names {
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("md")
                    && std::fs::metadata(&path).is_ok_and(|meta| meta.len() <= JUMP_MAX_FILE_BYTES)
                {
                    let rel = path
                        .strip_prefix(workspace)
                        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_default();
                    if !crate::source_scope::is_fixture_path(&rel)
                        && !crate::offload::is_test_path(&path)
                        && !crate::eval::is_eval_path(&rel)
                    {
                        files.push(path);
                    }
                }
            }
        }
    }
    if root_docs {
        for root in ["README.md", "AGENTS.md"] {
            let path = workspace.join(root);
            if path.is_file()
                && std::fs::metadata(&path).is_ok_and(|meta| meta.len() <= JUMP_MAX_FILE_BYTES)
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

/// Manifest path inside the guidance dir.
#[must_use]
/// Manifest path inside the guidance dir.
pub fn manifest_path(guidance_dir: &Path) -> PathBuf {
    guidance_dir.join(JUMPS_FILE)
}

fn read_manifest(guidance_dir: &Path) -> Option<JumpManifest> {
    let bytes = std::fs::read(manifest_path(guidance_dir)).ok()?;
    let manifest: JumpManifest = serde_json::from_slice(&bytes).ok()?;
    (manifest.version == JUMPS_MANIFEST_VERSION).then_some(manifest)
}

fn workspace_rel(workspace: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(workspace)
        .ok()
        .map(|rel| rel.to_string_lossy().into_owned())
}

/// Manifest staleness: missing/unparseable/version-skewed reads stale;
/// otherwise a stat sweep — a doc strictly newer than the manifest
/// restales, an ambiguity-window doc restales only when its content
/// hash moved. Single home: `mtime_skew` plus `hash_decides_fresh`.
#[must_use]
/// Manifest staleness: missing/unparseable/version-skewed reads stale;
/// otherwise a stat sweep — a doc strictly newer than the manifest
/// restales, an ambiguity-window doc restales only when its content
/// hash moved. Single home: `mtime_skew` plus `hash_decides_fresh`.
pub fn manifest_stale(workspace: &Path, guidance_dir: &Path) -> bool {
    let Some(manifest) = read_manifest(guidance_dir) else {
        return true;
    };
    let manifest_path = manifest_path(guidance_dir);
    let Some(manifest_mtime) = common_core::io::mtime(&manifest_path) else {
        return true;
    };
    let known: HashMap<&str, &str> = manifest
        .sources
        .iter()
        .map(|source| (source.path.as_str(), source.hash.as_str()))
        .collect();
    let mut seen = std::collections::HashSet::new();
    for path in doc_tree_files(workspace) {
        let Some(rel) = workspace_rel(workspace, &path) else {
            continue;
        };
        seen.insert(rel.clone());
        let Some(known_hash) = known.get(rel.as_str()) else {
            return true;
        };
        let Ok(doc_mtime) = std::fs::metadata(&path).and_then(|meta| meta.modified()) else {
            return true;
        };
        match crate::freshness::mtime_skew(manifest_mtime, doc_mtime) {
            crate::freshness::MtimeSkew::SourceNewer => return true,
            crate::freshness::MtimeSkew::SourceOlder => {}
            crate::freshness::MtimeSkew::Ambiguous => {
                let current = crate::diff::hash_file(&path);
                if !crate::freshness::hash_decides_fresh(
                    current.as_deref().unwrap_or(""),
                    Some(known_hash),
                ) {
                    return true;
                }
            }
        }
    }
    seen.len() != known.len()
}

/// Converge the manifest: skip untouched trees (no rewrite), else
/// derive every chunk and atomically replace the manifest.
pub fn sync_jumps(workspace: &Path, guidance_dir: &Path) -> Result<JumpSyncReport, String> {
    if !manifest_stale(workspace, guidance_dir) {
        let chunks = read_manifest(guidance_dir).map_or(0, |manifest| manifest.jumps.len());
        return Ok(JumpSyncReport {
            rederived: false,
            chunks,
        });
    }
    let mut jumps = Vec::new();
    let mut sources = Vec::new();
    for path in doc_tree_files(workspace) {
        let Some(rel) = workspace_rel(workspace, &path) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        jumps.extend(derive_jumps_for_doc(&rel, &text));
        sources.push(JumpSource {
            path: rel,
            // Same hash the staleness check recomputes
            // (`diff::hash_file`); a mixed algorithm would restale
            // every same-second sync.
            hash: crate::diff::hash_file_bytes(&bytes),
        });
    }
    sources.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = JumpManifest {
        version: JUMPS_MANIFEST_VERSION,
        jumps,
        sources,
    };
    std::fs::create_dir_all(guidance_dir)
        .map_err(|error| format!("cannot create {}: {error}", guidance_dir.display()))?;
    let body = serde_json::to_string(&manifest)
        .map_err(|error| format!("cannot encode jumps: {error}"))?;
    let tmp = manifest_path(guidance_dir).with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|error| format!("cannot stage jumps: {error}"))?;
    std::fs::rename(&tmp, manifest_path(guidance_dir))
        .map_err(|error| format!("cannot publish jumps: {error}"))?;
    let chunks = read_manifest(guidance_dir).map_or(0, |manifest| manifest.jumps.len());
    Ok(JumpSyncReport {
        rederived: true,
        chunks,
    })
}

/// Load the query-side index: `None` when absent or stale (fail-open —
/// recall works without jumps, never from them).
#[must_use]
/// Load the query-side index: `None` when absent or stale (fail-open —
/// recall works without jumps, never from them).
pub fn load_jump_index(workspace: &Path, guidance_dir: &Path) -> Option<JumpIndex> {
    if manifest_stale(workspace, guidance_dir) {
        return None;
    }
    let manifest = read_manifest(guidance_dir)?;
    let mut by_crate: HashMap<String, std::collections::BTreeSet<String>> = HashMap::new();
    for jump in &manifest.jumps {
        let entry = by_crate.entry(jump.crate_name.clone()).or_default();
        entry.extend(jump.terms.iter().cloned());
    }
    let by_crate: BTreeMap<String, Vec<String>> = by_crate
        .into_iter()
        .map(|(crate_name, terms)| (crate_name, terms.into_iter().collect()))
        .collect();
    Some(JumpIndex {
        by_crate: by_crate.into_iter().collect(),
    })
}

#[cfg(test)]
#[path = "../tests/jumps.rs"]
mod tests;
