//! P2 file selection: `FileSelection` (roots + globs + policy) over the
//! extended `common_core::walk`. Ports the zvec scanner discovery rules
//! (`scanner/index.ts` + `file-type.ts` + `file-size-policy.ts`):
//! kind/format tables, type-aware size caps, binary sniff, default ignores,
//! and raster handling (discover + kind-tag; explicit selection restores).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use common_core::walk::{walk_files_filtered, SkipReason, WalkEntry, WalkFilter};

use crate::offload::is_test_path;
use crate::query::glob::{
    compile_path_glob, has_path_glob, is_absolute_path_pattern, normalize_path_for_match,
    normalize_path_pattern,
};
use crate::search_constants::{
    MAX_CODE_FILE_SIZE_BYTES, MAX_DATA_FILE_SIZE_BYTES, MAX_IMAGE_FILE_SIZE_BYTES,
    MAX_TEXT_FILE_SIZE_BYTES,
};
use crate::search_types::FileKind;

/// Selection roots + filters for one index pass.
#[derive(Debug, Clone)]
/// Selection roots + filters for one index pass.
pub struct FileSelection {
    /// Workspace root used for the display/citation/glob identity.
    pub workspace_root: PathBuf,
    /// Workspace roots (files or directories).
    pub roots: Vec<PathBuf>,
    /// Include globs (rg-style); empty selects everything.
    pub include_globs: Vec<String>,
    /// Exclude globs (rg-style).
    pub exclude_globs: Vec<String>,
    /// Extra directory names to skip.
    pub extra_skip_dirs: Vec<String>,
    /// Honor `.gitignore` files.
    pub honor_gitignore: bool,
    /// Include checked-in fixture trees in this selection (tests and
    /// scorecard staging only; production retrieval never opts in).
    pub include_fixtures: bool,
    /// Override every per-kind size cap (tests).
    pub max_bytes_override: Option<u64>,
}

/// A discovered file with its kind tag.
#[derive(Debug, Clone, PartialEq, Eq)]
/// A discovered file with its kind tag.
pub struct SelectedFile {
    /// Absolute path.
    pub path: PathBuf,
    /// Owning root.
    pub root: PathBuf,
    /// Workspace-relative path (`path` stripped of `root`).
    pub relative: String,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Last-modified time (ms epoch).
    pub modified_ms: i64,
    /// Kind tag.
    pub kind: FileKind,
    /// Shared retrieval/maintenance scope.
    pub scope: crate::source_scope::SourceScope,
    /// Format tag.
    pub format: String,
}

/// Discovery counters (zvec `FileScanDiagnostics` equivalent).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Discovery counters (zvec `FileScanDiagnostics` equivalent).
pub struct ScanDiagnostics {
    /// Files accepted into the selection.
    pub scanned: usize,
    /// Skipped: over the type-aware size cap.
    pub skipped_large: usize,
    /// Skipped: binary content.
    pub skipped_binary: usize,
    /// Skipped: `.gitignore` / default ignores.
    pub skipped_ignored: usize,
    /// Skipped: binary extension / unknown kind.
    pub skipped_kind: usize,
    /// Skipped: include/exclude globs.
    pub skipped_glob: usize,
    /// Skipped: unit-test or e2e-test path (never indexed, never described).
    pub skipped_test: usize,
    /// Image files in the selection (kind-tagged, skipped at embed time).
    pub image_files: usize,
}

/// Selection failure (missing / overlapping roots).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
/// Selection failure (missing / overlapping roots).
pub enum SelectionError {
    /// A root does not exist.
    #[error("selection root does not exist: {0}")]
    MissingRoot(String),
    /// Two roots overlap (one contains the other).
    #[error("selection roots overlap: {0} contains {1}")]
    OverlappingRoots(String, String),
}

/// Classify a path into `(kind, format)` (port of `detectFileType`).
/// Binary extensions return `None`; unknown extensions fall back to text.
/// Format tables are canonical in `fluent_types::file_kind` — called directly
/// (no local wrappers) so the two maps cannot drift.
#[must_use]
/// Classify a path into `(kind, format)` (port of `detectFileType`).
/// Binary extensions return `None`; unknown extensions fall back to text.
/// Format tables are canonical in `fluent_types::file_kind` — called directly
/// (no local wrappers) so the two maps cannot drift.
pub fn detect_file_type(path: &Path) -> Option<(FileKind, String)> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if let Some((kind, format)) = fluent_types::file_kind::named_file_kind(name) {
        return Some((kind, format.to_string()));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if let Some(format) = fluent_types::file_kind::code_kind_format(&ext) {
        return Some((FileKind::Code, format.to_string()));
    }
    if let Some(format) = fluent_types::file_kind::data_kind_format(&ext) {
        return Some((FileKind::Data, format.to_string()));
    }
    if let Some(format) = fluent_types::file_kind::text_kind_format(&ext) {
        return Some((FileKind::Text, format.to_string()));
    }
    if let Some(format) = fluent_types::file_kind::image_kind_format(&ext) {
        return Some((FileKind::Image, format.to_string()));
    }
    if fluent_types::file_kind::is_binary_extension(&ext) {
        return None;
    }
    Some((FileKind::Text, ext))
}

/// Validate selection roots: every root exists; no root contains another.
pub fn validate_roots(selection: &FileSelection) -> Result<(), SelectionError> {
    for root in &selection.roots {
        if !root.exists() {
            return Err(SelectionError::MissingRoot(root.display().to_string()));
        }
    }
    for (i, a) in selection.roots.iter().enumerate() {
        for b in selection.roots.iter().skip(i + 1) {
            if a == b || a.starts_with(b) {
                return Err(SelectionError::OverlappingRoots(
                    b.display().to_string(),
                    a.display().to_string(),
                ));
            }
            if b.starts_with(a) {
                return Err(SelectionError::OverlappingRoots(
                    a.display().to_string(),
                    b.display().to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// Discover selected files under every root, accumulating diagnostics.
pub fn select_files(
    selection: &FileSelection,
    diag: &mut ScanDiagnostics,
) -> Result<Vec<SelectedFile>, SelectionError> {
    validate_roots(selection)?;
    let mut out = Vec::new();
    // Glob matchers compile once per selection, not once per file.
    let compiled = compile_selection(selection);
    for root in &selection.roots {
        if root.is_file() {
            select_single_file(selection, &compiled, root, root, diag, &mut out);
            continue;
        }
        let filter = WalkFilter {
            extensions: HashSet::new(),
            max_bytes: None,
            skip_binary: false,
            extra_skip_dirs: selection
                .extra_skip_dirs
                .iter()
                .cloned()
                .chain(DEFAULT_SKIP_DIRS.iter().map(ToString::to_string))
                .chain([HARD_SKIP_STATE_DIR.to_string()])
                .collect(),
            honor_gitignore: selection.honor_gitignore,
        };
        let mut skipped: Vec<(PathBuf, SkipReason)> = Vec::new();
        walk_files_filtered(
            root,
            &filter,
            &mut |entry: WalkEntry| {
                select_walk_entry(selection, &compiled, root, &entry, diag, &mut out);
            },
            &mut |path: &Path, reason: SkipReason| {
                skipped.push((path.to_path_buf(), reason));
            },
        );
        for (_, reason) in skipped {
            match reason {
                SkipReason::TooLarge => diag.skipped_large += 1,
                SkipReason::Binary => diag.skipped_binary += 1,
                SkipReason::Ignored => diag.skipped_ignored += 1,
                SkipReason::Extension => diag.skipped_kind += 1,
            }
        }
    }
    Ok(out)
}

fn select_single_file(
    selection: &FileSelection,
    compiled: &CompiledGlobs,
    root: &Path,
    path: &Path,
    diag: &mut ScanDiagnostics,
    out: &mut Vec<SelectedFile>,
) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    let entry = WalkEntry {
        path: path.to_path_buf(),
        size_bytes: meta.len(),
        // Real mtime like the walker records: the ingest change gate
        // compares it, and a zero here would re-ingest every pass.
        modified_ms: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as i64),
    };
    select_walk_entry(selection, compiled, root, &entry, diag, out);
}

fn select_walk_entry(
    selection: &FileSelection,
    compiled: &CompiledGlobs,
    root: &Path,
    entry: &WalkEntry,
    diag: &mut ScanDiagnostics,
    out: &mut Vec<SelectedFile>,
) {
    // Unit tests and e2e tests are never indexed and never described:
    // the skip is absolute (explicit globs do not override it) so test
    // code can neither pollute recall nor draw model-backed commentary.
    if is_test_path(&entry.path) {
        diag.skipped_test += 1;
        return;
    }
    let Some((kind, format)) = detect_file_type(&entry.path) else {
        diag.skipped_kind += 1;
        return;
    };
    let relative = entry
        .path
        .strip_prefix(&selection.workspace_root)
        .unwrap_or(&entry.path)
        .to_string_lossy()
        .replace('\\', "/");
    // A file root scopes the sync to itself: the strip above is empty,
    // so fall back to the file name — downstream stores and profile
    // lines expect a name, never "".
    let relative = if relative.is_empty() {
        entry
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string()
    } else {
        relative
    };
    if crate::eval::is_eval_path(&relative) || is_benchmark_corpus_path(&relative) {
        diag.skipped_ignored += 1;
        return;
    }
    let scope = crate::source_scope::classify(&relative, Some(kind));
    if scope == crate::source_scope::SourceScope::Fixture && !selection.include_fixtures {
        diag.skipped_test += 1;
        return;
    }
    let display = format!("{}/{}", root.display(), relative);
    // Glob matching runs against the root-relative path (rg-style
    // relative patterns); absolute patterns match the absolute path.
    // Both candidates normalize once per file; the compiled matchers
    // built for this selection do the rest with no per-file regex
    // construction.
    let relative_match = normalize_path_for_match(&relative);
    let display_match = normalize_path_for_match(&display);
    let match_compiled = |set: &[CompiledGlob]| {
        set.iter()
            .any(|glob| compiled_glob_matches(glob, &relative_match, &display_match))
    };
    let explicit = !compiled.include.is_empty() && match_compiled(&compiled.include);
    if !explicit {
        if matches_default_dir(&relative) {
            diag.skipped_ignored += 1;
            return;
        }
        // Hidden files are skipped unless explicitly included (rg parity —
        // hidden dirs are already skipped by the walker; without this the
        // index ingests its own dotfiles, e.g. `.guidance.db-wal`).
        if is_hidden_file(&entry.path) {
            diag.skipped_ignored += 1;
            return;
        }
        if matches_default_file_pattern(&relative) {
            diag.skipped_ignored += 1;
            return;
        }
        if !selection.include_globs.is_empty() && !match_compiled(&compiled.include) {
            diag.skipped_glob += 1;
            return;
        }
        if match_compiled(&compiled.exclude) {
            diag.skipped_glob += 1;
            return;
        }
    }
    let cap = selection
        .max_bytes_override
        .unwrap_or(size_cap_for_kind(kind));
    if entry.size_bytes > cap {
        diag.skipped_large += 1;
        return;
    }
    if kind != FileKind::Image && is_binary_extensionless(&entry.path) {
        diag.skipped_binary += 1;
        return;
    }
    if kind == FileKind::Image {
        diag.image_files += 1;
    }
    diag.scanned += 1;
    out.push(SelectedFile {
        path: entry.path.clone(),
        root: root.to_path_buf(),
        relative,
        size_bytes: entry.size_bytes,
        modified_ms: entry.modified_ms,
        kind,
        scope,
        format,
    });
}

fn size_cap_for_kind(kind: FileKind) -> u64 {
    match kind {
        FileKind::Code => MAX_CODE_FILE_SIZE_BYTES,
        FileKind::Text => MAX_TEXT_FILE_SIZE_BYTES,
        FileKind::Data => MAX_DATA_FILE_SIZE_BYTES,
        FileKind::Image => MAX_IMAGE_FILE_SIZE_BYTES,
    }
}

/// Binary sniff for files whose extension gave no verdict (extensionless /
/// unknown-as-text): read the sniff window and apply the shared ratio rule.
fn is_binary_extensionless(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; common_core::walk::BINARY_SNIFF_BYTES];
    use std::io::Read as _;
    let Ok(n) = std::io::BufReader::new(file).read(&mut buf) else {
        return false;
    };
    common_core::walk::sniff_is_binary(&buf[..n])
}

/// Hidden-file check mirroring the walker's hidden-directory rule
/// (`common_core::walk::should_skip_dir`): a leading `.` on the file
/// name. Explicit `include_globs` bypass this (discoverability).
/// One glob pattern compiled once per selection: regex for
/// metacharacter patterns, prefix fold otherwise — the same two
/// branches as the per-call path, but the regex builds once instead
/// of once per file.
struct CompiledGlob {
    regex: Option<regex::Regex>,
    prefix: String,
    absolute: bool,
}

/// Selection globs compiled once and shared by every walked file.
struct CompiledGlobs {
    include: Vec<CompiledGlob>,
    exclude: Vec<CompiledGlob>,
}

fn compile_selection_globs(patterns: &[String]) -> Vec<CompiledGlob> {
    patterns
        .iter()
        .filter_map(|pattern| {
            let normalized = normalize_path_pattern(pattern);
            // Empty patterns never match (same early-false as the
            // per-call path) — dropped at build, not per file.
            if normalized.is_empty() {
                return None;
            }
            let absolute = is_absolute_path_pattern(pattern);
            let regex = if has_path_glob(&normalized) {
                Some(compile_path_glob(pattern, false))
            } else {
                None
            };
            Some(CompiledGlob {
                regex,
                prefix: normalized,
                absolute,
            })
        })
        .collect()
}

fn compiled_glob_matches(compiled: &CompiledGlob, relative: &str, display: &str) -> bool {
    let candidate = if compiled.absolute { display } else { relative };
    match &compiled.regex {
        Some(matcher) => matcher.is_match(candidate),
        None => {
            candidate == compiled.prefix || candidate.starts_with(&format!("{}/", compiled.prefix))
        }
    }
}

fn compile_selection(selection: &FileSelection) -> CompiledGlobs {
    CompiledGlobs {
        include: compile_selection_globs(&selection.include_globs),
        exclude: compile_selection_globs(&selection.exclude_globs),
    }
}

fn is_hidden_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

/// Measurement fixtures are not retrieval inputs: the benchmark task
/// corpus must never be indexed, answered from, or cited. The eval
/// corpus has its own owner (`eval::is_eval_path`); this covers the
/// benchmark side. Workspace-relative, forward slashes.
#[must_use]
/// Measurement fixtures are not retrieval inputs: the benchmark task
/// corpus must never be indexed, answered from, or cited. The eval
/// corpus has its own owner (`eval::is_eval_path`); this covers the
/// benchmark side. Workspace-relative, forward slashes.
pub fn is_benchmark_corpus_path(relative: &str) -> bool {
    let normalized = relative.trim_start_matches("./").replace('\\', "/");
    normalized == "src/guidance/corpora" || normalized.starts_with("src/guidance/corpora/")
}

/// Directory rules apply to the root-relative path (never the absolute
/// path — system temp dirs like `/tmp` must not trip the `tmp` rule).
fn matches_default_dir(relative: &str) -> bool {
    relative
        .split('/')
        .any(|name| DEFAULT_SKIP_DIRS.contains(&name))
}

/// Default ignore patterns, compiled once and shared by every file
/// match (per-file recompilation cost ~39 ms/file in debug builds).
static DEFAULT_IGNORED_MATCHERS: std::sync::OnceLock<Vec<regex::Regex>> =
    std::sync::OnceLock::new();

fn matches_default_file_pattern(relative: &str) -> bool {
    let matchers = DEFAULT_IGNORED_MATCHERS.get_or_init(|| {
        DEFAULT_IGNORED_FILE_PATTERNS
            .iter()
            .map(|pattern| compile_path_glob(pattern, false))
            .collect()
    });
    let candidate = normalize_path_for_match(relative);
    // The defaults all carry glob metacharacters, so the per-call path
    // (`path_pattern_matches`) always took the regex branch — the cached
    // equivalent below matches it exactly, compiled once.
    matchers.iter().any(|matcher| matcher.is_match(&candidate))
}

#[cfg(test)]
pub(crate) fn default_ignored_matchers_initialized() -> bool {
    // Probe for the regression test: the shared matcher set is populated
    // after any match call, never recompiled per file.
    DEFAULT_IGNORED_MATCHERS
        .get()
        .is_some_and(|matchers| matchers.len() == DEFAULT_IGNORED_FILE_PATTERNS.len())
}

/// The `.guidance` state directory is never indexed; `.git` is covered by the
/// hidden-directory rule in `walk`.
const HARD_SKIP_STATE_DIR: &str = ".guidance";

/// Generated-dependency directory names (port of
/// `DEFAULT_IGNORED_DIRECTORY_NAMES`).
const DEFAULT_SKIP_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    "thirdparty",
    "third_party",
    "external",
    "deps",
    "dist",
    "build",
    "out",
    "target",
    "coverage",
    "generated",
    "__pycache__",
    "venv",
    ".venv",
    "env",
    ".tox",
    ".eggs",
    "Pods",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".vite",
    ".parcel-cache",
    ".cache",
    ".gradle",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    "tmp",
    "temp",
    "logs",
    "locale",
    "locales",
    "translations",
];

/// Low-signal file patterns (port of `DEFAULT_IGNORED_FILE_PATTERNS`).
const DEFAULT_IGNORED_FILE_PATTERNS: &[&str] = &[
    "*.lock",
    "*.lockb",
    "*-lock.json",
    "*-lock.yaml",
    "npm-shrinkwrap.json",
    "go.sum",
    "*.resolved",
    "*.po",
    "*.pot",
    "*.map",
    "*.min.*",
    "*.bundle.*",
    "*.generated.*",
    "*.gen.*",
    "*.designer.*",
    "*.pb.*",
    "*_pb2.*",
    "*.g.*",
    "*.gif",
    "*.jpeg",
    "*.jpg",
    "*.png",
    "*.webp",
];

#[cfg(test)]
#[path = "../tests/index_selection.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/eval_exclusion.rs"]
mod eval_exclusion_tests;
