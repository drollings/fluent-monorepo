//! P2 file selection: `FileSelection` (roots + globs + policy) over the
//! extended `common_core::walk`. Ports the zvec scanner discovery rules
//! (`scanner/index.ts` + `file-type.ts` + `file-size-policy.ts`):
//! kind/format tables, type-aware size caps, binary sniff, default ignores,
//! and raster handling (discover + kind-tag; explicit selection restores).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use common_core::walk::{SkipReason, WalkEntry, WalkFilter, walk_files_filtered};

use crate::query::glob::path_pattern_matches;
use crate::zg_constants::{
    MAX_CODE_FILE_SIZE_BYTES, MAX_DATA_FILE_SIZE_BYTES, MAX_IMAGE_FILE_SIZE_BYTES,
    MAX_TEXT_FILE_SIZE_BYTES,
};
use crate::zg_types::FileKind;

/// Selection roots + filters for one index pass.
#[derive(Debug, Clone)]
pub struct FileSelection {
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
    /// Override every per-kind size cap (tests).
    pub max_bytes_override: Option<u64>,
}

/// A discovered file with its kind tag.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Format tag.
    pub format: String,
}

/// Discovery counters (zvec `FileScanDiagnostics` equivalent).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// Image files in the selection (kind-tagged, skipped at embed time).
    pub image_files: usize,
}

/// Selection failure (missing / overlapping roots).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
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
#[must_use]
pub fn detect_file_type(path: &Path) -> Option<(FileKind, String)> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if let Some(tagged) = named_file_type(name) {
        return Some(tagged);
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if let Some(format) = code_format(&ext) {
        return Some((FileKind::Code, format));
    }
    if let Some(format) = data_format(&ext) {
        return Some((FileKind::Data, format));
    }
    if let Some(format) = text_format(&ext) {
        return Some((FileKind::Text, format));
    }
    if let Some(format) = image_format(&ext) {
        return Some((FileKind::Image, format));
    }
    if BINARY_EXTENSIONS.contains(&ext.as_str()) {
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
    for root in &selection.roots {
        if root.is_file() {
            select_single_file(selection, root, root, diag, &mut out);
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
                .chain([HARD_SKIP_ZVEC.to_string()])
                .collect(),
            honor_gitignore: selection.honor_gitignore,
        };
        let mut skipped: Vec<(PathBuf, SkipReason)> = Vec::new();
        walk_files_filtered(
            root,
            &filter,
            &mut |entry: WalkEntry| {
                select_walk_entry(selection, root, &entry, diag, &mut out);
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
        modified_ms: 0,
    };
    select_walk_entry(selection, root, &entry, diag, out);
}

fn select_walk_entry(
    selection: &FileSelection,
    root: &Path,
    entry: &WalkEntry,
    diag: &mut ScanDiagnostics,
    out: &mut Vec<SelectedFile>,
) {
    let Some((kind, format)) = detect_file_type(&entry.path) else {
        diag.skipped_kind += 1;
        return;
    };
    let relative = entry
        .path
        .strip_prefix(root)
        .unwrap_or(&entry.path)
        .to_string_lossy()
        .replace('\\', "/");
    let display = format!("{}/{}", root.display(), relative);
    // Glob matching runs against the root-relative path (rg-style
    // relative patterns); absolute patterns match the absolute path.
    let match_path = |pattern: &str| {
        if crate::query::glob::is_absolute_path_pattern(pattern) {
            path_pattern_matches(pattern, &display)
        } else {
            path_pattern_matches(pattern, &relative)
        }
    };
    let explicit = !selection.include_globs.is_empty()
        && selection.include_globs.iter().any(|g| match_path(g));
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
        if !selection.include_globs.is_empty()
            && !selection.include_globs.iter().any(|g| match_path(g))
        {
            diag.skipped_glob += 1;
            return;
        }
        if selection.exclude_globs.iter().any(|g| match_path(g)) {
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
fn is_hidden_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
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
            .map(|pattern| crate::query::glob::compile_path_glob(pattern, false))
            .collect()
    });
    let candidate = crate::query::glob::normalize_path_for_match(relative);
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

/// `.zvec-grep` state is never indexed (G0.4); `.git` is covered by the
/// hidden-directory rule in `walk`.
const HARD_SKIP_ZVEC: &str = ".zvec-grep";

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

fn named_file_type(name: &str) -> Option<(FileKind, String)> {
    match name {
        "Dockerfile" => Some((FileKind::Code, "dockerfile".to_string())),
        "Makefile" => Some((FileKind::Code, "makefile".to_string())),
        _ => None,
    }
}

fn code_format(ext: &str) -> Option<String> {
    match ext {
        "c" => Some("c"),
        "cc" | "cpp" | "cxx" | "h" | "hpp" => Some("cpp"),
        "go" => Some("go"),
        "java" => Some("java"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "jsx" => Some("jsx"),
        "ts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "py" => Some("python"),
        "rs" => Some("rust"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "swift" => Some("swift"),
        "kt" | "kts" => Some("kotlin"),
        "cs" => Some("csharp"),
        "scala" => Some("scala"),
        "sh" | "bash" | "zsh" => Some("bash"),
        "sql" => Some("sql"),
        "css" => Some("css"),
        "scss" => Some("scss"),
        "less" => Some("less"),
        "vue" => Some("vue"),
        "svelte" => Some("svelte"),
        _ => None,
    }
    .map(ToString::to_string)
}

fn data_format(ext: &str) -> Option<String> {
    match ext {
        "csv" => Some("csv"),
        "json" | "jsonc" => Some("json"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        _ => None,
    }
    .map(ToString::to_string)
}

fn text_format(ext: &str) -> Option<String> {
    match ext {
        "md" | "mdx" => Some("markdown"),
        "rst" => Some("rst"),
        "txt" => Some("text"),
        "html" | "htm" => Some("html"),
        "xml" => Some("xml"),
        _ => None,
    }
    .map(ToString::to_string)
}

fn image_format(ext: &str) -> Option<String> {
    match ext {
        "gif" => Some("gif"),
        "jpeg" | "jpg" => Some("jpeg"),
        "png" => Some("png"),
        "webp" => Some("webp"),
        _ => None,
    }
    .map(ToString::to_string)
}

/// Extension groups whose files are never classified (port of
/// `BINARY_EXTENSION_GROUPS`).
const BINARY_EXTENSIONS: &[&str] = &[
    "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "exe", "dll", "dylib", "so", "a", "o",
    "obj", "wasm", "class", "jar", "pdf", "doc", "docx", "ppt", "pptx", "xls", "xlsx",
    "mp3", "mp4", "mov", "avi", "mkv", "db", "sqlite",
];

#[cfg(test)]
#[path = "../tests/index_selection.rs"]
mod tests;
