//! Deterministic doc chunking and external capability-corpus import.
//!
//! The chunker is the single markdown section splitter: the jump index,
//! the capability import, and (later) the doc plane all resolve sections
//! through it, so anchors agree everywhere. The corpus import is
//! model-free: it resolves each card to a live section, verifies the
//! section hash, and stores `ready` or a labelled `gap` — never
//! generated text.

use std::path::{Path, PathBuf};

use crate::config::ProjectConfig;
use crate::summary::SummaryGap;

/// One doc section: a markdown section (or manifest description) with
/// its deterministic decomposition. Nothing here is model-written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocChunk {
    /// Stable chunk id (`source_path#anchor`).
    pub id: String,
    /// Workspace-relative doc path.
    pub source_path: String,
    /// Section heading title (breadcrumb joined); empty for preamble.
    pub title: String,
    /// Section anchor within the doc.
    pub anchor: String,
    /// Chunk text.
    pub text: String,
    /// Blake3 of the chunk text (incremental cache key input).
    pub content_hash: String,
    /// Workspace identity (root directory name).
    pub project: String,
    /// Owning crate from the path decomposition.
    pub crate_name: String,
    /// Doc domain from the path decomposition.
    pub domain: String,
}

/// Slug for anchors: lowercase alphanumerics, runs of the rest fold
/// to one dash, edge dashes trimmed. Empty input stays empty.
fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in text.to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Max chunk characters per section piece (char-boundary splits).
const MAX_CHUNK_INPUT_CHARS: usize = 8000;

/// Split one section's text at char boundaries into input-sized pieces.
fn split_chunk_text(text: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut rest = text;
    while rest.len() > MAX_CHUNK_INPUT_CHARS {
        let mut end = MAX_CHUNK_INPUT_CHARS.min(rest.len());
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        pieces.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    if !rest.is_empty() || pieces.is_empty() {
        pieces.push(rest.to_string());
    }
    pieces
}

/// Split one doc into addressed sections. Empty sections become gaps,
/// never chunks: there is nothing to cite and nothing to verify.
#[must_use]
pub fn chunk_markdown_sections(source_path: &str, text: &str) -> (Vec<DocChunk>, Vec<SummaryGap>) {
    let lines: Vec<&str> = text.lines().collect();
    let headings = crate::extractor::markdown::scan_headings(&lines);
    let sections = if headings.is_empty() {
        vec![crate::extractor::markdown::Section {
            heading: None,
            start_index: 0,
            end_index: lines.len().saturating_sub(1),
            breadcrumb: Vec::new(),
        }]
    } else {
        crate::extractor::markdown::build_sections(&headings, &lines)
    };
    let (crate_name, domain) = decompose_chunk_source(source_path);
    let mut chunks = Vec::new();
    let mut gaps = Vec::new();
    let mut used: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for section in &sections {
        let base = match &section.heading {
            Some(heading) => {
                let mut parts: Vec<String> = section
                    .breadcrumb
                    .iter()
                    .map(|part| slug(part))
                    .filter(|part| !part.is_empty())
                    .collect();
                let own = slug(&heading.text);
                if !own.is_empty() {
                    parts.push(own);
                }
                parts.join("-")
            }
            None => "preamble".to_string(),
        };
        let title = match &section.heading {
            Some(heading) => {
                let mut parts = section.breadcrumb.clone();
                parts.push(heading.text.clone());
                parts.join(" > ")
            }
            None => "preamble".to_string(),
        };
        let base = if base.is_empty() {
            "section".to_string()
        } else {
            base
        };
        let section_text = if lines.is_empty() || section.start_index > section.end_index {
            String::new()
        } else {
            lines[section.start_index..=section.end_index.min(lines.len() - 1)].join("\n")
        };
        // Emptiness reads the body: a bare heading with no prose is
        // not a citable chunk.
        let body = match &section.heading {
            Some(_) => section_text.split_once('\n').map_or("", |(_, rest)| rest),
            None => section_text.as_str(),
        };
        if body.trim().is_empty() {
            let count = used.entry(base.clone()).or_insert(0);
            *count += 1;
            let anchor = if *count == 1 {
                base
            } else {
                format!("{base}-{}", *count)
            };
            gaps.push(SummaryGap {
                node_key: format!("{source_path}#{anchor}"),
                reason: "empty",
                detail: "section is empty; nothing to cite".to_string(),
            });
            continue;
        }
        for piece in split_chunk_text(&section_text) {
            let count = used.entry(base.clone()).or_insert(0);
            *count += 1;
            let anchor = if *count == 1 {
                base.clone()
            } else {
                format!("{base}-{}", *count)
            };
            chunks.push(DocChunk {
                id: format!("{source_path}#{anchor}"),
                source_path: source_path.to_string(),
                title: title.clone(),
                anchor,
                content_hash: common_core::hash::blake3_hex(piece.as_bytes()),
                text: piece,
                project: String::new(),
                crate_name: crate_name.clone(),
                domain: domain.clone(),
            });
        }
    }
    (chunks, gaps)
}

/// Deterministic path decomposition: crate from the longest
/// boundary-matching table prefix (`doc/<name>/` maps through the
/// same table as `src/<name>`), domain from the first `doc/` segment
/// or the crate name for crate-owned files, `meta` for workspace
/// roots. Fallbacks are `unknown`, never a guess.
#[must_use]
pub fn decompose_chunk_source(path: &str) -> (String, String) {
    let normalized = path.trim().trim_start_matches("./");
    let probe = normalized
        .strip_prefix("doc/")
        .and_then(|rest| rest.split('/').next())
        .map_or_else(|| normalized.to_string(), |name| format!("src/{name}"));
    let gate = crate::component_map::component_gate_for_path(&probe);
    let crate_name = if gate.package.is_empty() {
        "unknown".to_string()
    } else {
        gate.package
    };
    let domain = if let Some(rest) = normalized.strip_prefix("doc/") {
        rest.split('/')
            .next()
            .filter(|segment| !segment.is_empty())
            .unwrap_or("unknown")
            .to_string()
    } else if crate_name != "unknown" {
        crate_name.clone()
    } else {
        "meta".to_string()
    };
    (crate_name, domain)
}

/// The `[package] name` of a manifest: same basic-string rule as
/// the description reader. Anything else reads as absent.
#[must_use]
pub fn manifest_package_name(manifest: &str) -> Option<String> {
    package_basic_string(manifest, "name")
}

/// The `[package] description` of a manifest: basic single-line
/// strings with `\"`/`\\` unescaped. Anything else reads as absent.
#[must_use]
pub fn manifest_description(manifest: &str) -> Option<String> {
    package_basic_string(manifest, "description")
}

/// One basic-string field from a manifest's `[package]` section.
fn package_basic_string(manifest: &str, key: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix(key) else {
            continue;
        };
        let value = rest.trim_start().strip_prefix('=')?.trim_start();
        if !value.starts_with('"') {
            return None;
        }
        let mut out = String::new();
        let mut chars = value[1..].chars();
        loop {
            match chars.next()? {
                '"' => return Some(out),
                '\\' => match chars.next()? {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                },
                c => out.push(c),
            }
        }
    }
    None
}

/// Resolve one live section by anchor through the shared chunker.
/// Returns the live text when the anchor exists now.
#[must_use]
pub fn resolve_live_section(source_path: &str, anchor: &str, live_text: &str) -> Option<String> {
    let (chunks, _) = chunk_markdown_sections(source_path, live_text);
    chunks
        .into_iter()
        .find(|chunk| chunk.anchor == anchor)
        .map(|chunk| chunk.text)
}

/// Totals for one external-corpus import (stored data, never thrown).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilityImportReport {
    /// Cards present in the manifest.
    pub cards: usize,
    /// Cards stored `ready` this run.
    pub ready: usize,
    /// Cards stored as labelled gaps this run.
    pub gaps: usize,
    /// Cards skipped on matching stored rows.
    pub skipped_unchanged: usize,
    /// Stored external rows dropped (left the manifest).
    pub removed: usize,
    /// Briefs refused for pointing at measurement fixtures (eval
    /// goldens or the benchmark task corpus). Never stored.
    pub skipped_measurement: usize,
    /// Whether a manifest was found at all.
    pub present: bool,
}

/// Manifest filename inside `capabilities_dir`.
pub const CAPABILITY_CORPUS_FILE: &str = "corpus.json";

/// Import the external capability corpus: resolve each card to its live
/// doc section, verify the section hash, and store `ready` or a
/// labelled gap. No model call on any path; unchanged cards skip by
/// content and corpus hash. An absent manifest skips (`present: false`);
/// a malformed one fails loudly.
pub fn import_capability_corpus(
    workspace: &Path,
    cfg: &ProjectConfig,
    db: &crate::query::search_db::GuidanceSearchDb,
) -> Result<CapabilityImportReport, String> {
    use crate::query::search_db::{CapabilityLemma, CapabilityRecord, CAPABILITY_SCHEMA_VERSION};
    let dir = cfg
        .capabilities_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("doc/capabilities"));
    let dir = if dir.is_absolute() {
        dir
    } else {
        workspace.join(&dir)
    };
    let manifest = dir.join(CAPABILITY_CORPUS_FILE);
    if !manifest.is_file() {
        return Ok(CapabilityImportReport {
            present: false,
            ..CapabilityImportReport::default()
        });
    }
    let raw = std::fs::read(&manifest)
        .map_err(|error| format!("capability corpus read {}: {error}", manifest.display()))?;
    let corpus_hash = common_core::hash::blake3_hex(&raw);
    // One parser for import and calibration: unknown-field and shape
    // rules live in `load_capability_corpus`, never here.
    let corpus = crate::capability::load_capability_corpus(&manifest)?;
    let project = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace")
        .to_string();
    let origin = format!(
        "external:{}",
        manifest
            .strip_prefix(workspace)
            .unwrap_or(&manifest)
            .to_string_lossy()
            .replace('\\', "/")
    );
    let generated_at = crate::summary::now_ms();
    let mut report = CapabilityImportReport {
        present: true,
        cards: corpus.briefs.len(),
        ..CapabilityImportReport::default()
    };
    let mut rows = Vec::with_capacity(corpus.briefs.len());
    let mut postings = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    for brief in &corpus.briefs {
        // Measurement fixtures are never cards: a brief pointing at
        // the eval goldens or the benchmark task corpus is refused
        // before any read, never stored (not even as a gap — the path
        // itself must not enter the index).
        if crate::eval::is_eval_path(&brief.source_path)
            || crate::selection::is_benchmark_corpus_path(&brief.source_path)
        {
            report.skipped_measurement += 1;
            continue;
        }
        seen_ids.insert(brief.id.clone());
        let stored = db
            .capabilities_for_ids(std::slice::from_ref(&brief.id))
            .map_err(|error| format!("capability cache read: {error}"))?;
        let (crate_name, domain) = decompose_chunk_source(&brief.source_path);
        let live_path = workspace.join(&brief.source_path);
        let live_text = std::fs::read_to_string(&live_path).ok();
        let (purpose, content_hash, status, gap_reason) = match live_text {
            None => (
                brief.purpose.clone(),
                String::new(),
                "gap".to_string(),
                Some("missing-source: corpus source_path has no file".to_string()),
            ),
            Some(text) => {
                let section = resolve_live_section(&brief.source_path, &brief.anchor, &text);
                match section {
                    None => (
                        brief.purpose.clone(),
                        String::new(),
                        "gap".to_string(),
                        Some("anchor-missing: live doc has no such section".to_string()),
                    ),
                    Some(section) => {
                        let live_hash = common_core::hash::blake3_hex(section.as_bytes());
                        let matches = brief
                            .content_hash
                            .as_deref()
                            .is_none_or(|expected| expected == live_hash);
                        if matches {
                            (brief.purpose.clone(), live_hash, "ready".to_string(), None)
                        } else {
                            (
                                brief.purpose.clone(),
                                live_hash,
                                "gap".to_string(),
                                Some(
                                    "hash-mismatch: live section drifted from the card".to_string(),
                                ),
                            )
                        }
                    }
                }
            }
        };
        let keywords_json =
            serde_json::to_string(&brief.keywords).unwrap_or_else(|_| "[]".to_string());
        let code_paths_json =
            serde_json::to_string(&brief.code_paths).unwrap_or_else(|_| "[]".to_string());
        let symbols_json =
            serde_json::to_string(&brief.symbols).unwrap_or_else(|_| "[]".to_string());
        let unchanged = stored.first().is_some_and(|existing| {
            existing.status == status
                && existing.content_hash == content_hash
                && existing.corpus_hash == corpus_hash
                && existing.prompt_version == CAPABILITY_SCHEMA_VERSION
                && existing.purpose == purpose
                && existing.title == brief.title
                && existing.keywords_json == keywords_json
                && existing.code_paths_json == code_paths_json
                && existing.symbols_json == symbols_json
        });
        if unchanged {
            report.skipped_unchanged += 1;
            continue;
        }
        let (predicate, roles) = crate::capability::brief_svo(&purpose, None);
        if status == "ready" {
            for lemma in crate::query::ingest::query_lemmas(None, &purpose) {
                postings.push(CapabilityLemma {
                    capability_id: brief.id.clone(),
                    lemma,
                    confidence: 1.0,
                });
            }
        }
        rows.push(CapabilityRecord {
            id: brief.id.clone(),
            project: project.clone(),
            crate_name,
            domain,
            title: brief.title.clone(),
            purpose,
            predicate,
            roles_json: serde_json::to_string(&roles).unwrap_or_else(|_| "[]".to_string()),
            keywords_json,
            code_paths_json,
            symbols_json,
            source_path: brief.source_path.clone(),
            anchor: brief.anchor.clone(),
            content_hash,
            model: "external".to_string(),
            prompt_version: CAPABILITY_SCHEMA_VERSION,
            origin: origin.clone(),
            corpus_hash: corpus_hash.clone(),
            status: status.clone(),
            gap_reason,
            generated_at,
        });
        if status == "ready" {
            report.ready += 1;
        } else {
            report.gaps += 1;
        }
    }
    if !rows.is_empty() {
        db.replace_capabilities(&rows)
            .map_err(|error| format!("capability store: {error}"))?;
        db.insert_capability_lemmas(&postings)
            .map_err(|error| format!("capability lemma store: {error}"))?;
    }
    let stale: Vec<String> = db
        .capability_ids_with_origin(&origin)
        .map_err(|error| format!("capability sweep: {error}"))?
        .into_iter()
        .filter(|id| !seen_ids.contains(id))
        .collect();
    if !stale.is_empty() {
        db.delete_capabilities(&stale)
            .map_err(|error| format!("capability sweep: {error}"))?;
        report.removed = stale.len();
    }
    Ok(report)
}

#[cfg(test)]
#[path = "../../tests/capability_import.rs"]
mod capability_import_tests;

#[cfg(test)]
#[path = "../../tests/capability_corpus.rs"]
mod capability_corpus_tests;
