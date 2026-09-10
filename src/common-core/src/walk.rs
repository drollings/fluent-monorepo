//! Directory walker: `walk_files` (callback), `should_skip_dir`, `SOURCE_EXTENSIONS`.

use std::collections::HashSet;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};

/// Source file extensions recognized by the guidance pipeline.
pub const SOURCE_EXTENSIONS: &[&str] = &["zig", "zon", "py", "rs", "md"];

/// Default directories to skip during recursive walks.
const DEFAULT_SKIP: &[&str] = &["target", "fixtures"];

/// Recursively walk `root`, calling `callback` for each file whose extension
/// matches one of `extensions`.  Hidden directories (starting with `.`),
/// `target/`, and `fixtures/` are always skipped.  Additional directories can
/// be skipped via [`should_skip_dir`].
pub fn walk_files<F>(root: &Path, extensions: &[&str], mut callback: F)
where
    F: FnMut(&Path),
{
    let ext_set: HashSet<&str> = extensions.iter().copied().collect();
    let mut skip: HashSet<&str> = DEFAULT_SKIP.iter().copied().collect();
    walk_recursive(root, &ext_set, &mut skip, &mut callback);
}

fn walk_recursive<F>(
    dir: &Path,
    extensions: &HashSet<&str>,
    skip: &mut HashSet<&str>,
    callback: &mut F,
) where
    F: FnMut(&Path),
{
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if should_skip_dir(&path, skip) {
                continue;
            }
            walk_recursive(&path, extensions, skip, callback);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if extensions.contains(ext) {
                callback(&path);
            }
        }
    }
}

/// Returns `true` if `path` is a hidden directory (starts with `.`) or its
/// file name is in the `extra` skip set.
pub fn should_skip_dir<S: BuildHasher>(path: &Path, extra: &HashSet<&str, S>) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    name.starts_with('.') || extra.contains(name)
}

/// Collect every file extension found under `dirs`, returned as `[".ext", …]`.
/// Recurses into non-hidden, non-build directories.
pub fn collect_extensions(dirs: &[PathBuf]) -> HashSet<String> {
    let mut exts = HashSet::new();
    for dir in dirs {
        collect_ext_recursive(dir, &mut exts);
    }
    exts
}

fn collect_ext_recursive(dir: &Path, exts: &mut HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if should_skip_dir(&path, &HashSet::new()) {
                continue;
            }
            collect_ext_recursive(&path, exts);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            exts.insert(format!(".{ext}"));
        }
    }
}

/// Test helper: create a directory tree with the given files and directories.
///
/// Public so the cross-crate test-support crate (`fluent-wvr-testutil`) can
/// delegate to it — the single implementation of tree scaffolding.
pub fn make_tree(root: &Path, files: &[&str], dirs: &[&str]) {
    for d in dirs {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    for f in files {
        let p = root.join(f);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, "").unwrap();
    }
}

/// Number of leading bytes sniffed for binary detection (zvec parity).
pub const BINARY_SNIFF_BYTES: usize = 8192;

/// Control-character ratio above which a sniffed window counts as binary.
pub const BINARY_CONTROL_CHAR_RATIO: f64 = 0.3;

/// A discovered file with its stat metadata (single `stat` per file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkEntry {
    /// Absolute path.
    pub path: PathBuf,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Last-modified time (ms since epoch; 0 when unavailable).
    pub modified_ms: i64,
}

/// Why a file was excluded from a filtered walk (scan diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Extension not in the filter set.
    Extension,
    /// Exceeds `WalkFilter::max_bytes`.
    TooLarge,
    /// Binary content detected (`WalkFilter::skip_binary`).
    Binary,
    /// Matched a `.gitignore` rule (`WalkFilter::honor_gitignore`).
    Ignored,
}

/// Selection predicate for [`walk_files_filtered`] (P2 `FileSelection`
/// building block; domain-agnostic — no file-kind knowledge here).
#[derive(Debug, Clone, Default)]
pub struct WalkFilter {
    /// Extensions to visit (without the dot); empty visits every file.
    pub extensions: HashSet<String>,
    /// Per-file byte cap; larger files are reported as [`SkipReason::TooLarge`].
    pub max_bytes: Option<u64>,
    /// Sniff content and skip binaries ([`SkipReason::Binary`]).
    pub skip_binary: bool,
    /// Directory names to skip beyond hidden + `DEFAULT_SKIP`.
    pub extra_skip_dirs: HashSet<String>,
    /// Honor `.gitignore` files via the `ignore` crate matchers.
    pub honor_gitignore: bool,
}

/// Returns `true` when `window` looks binary: the control-character ratio
/// over the sniffed prefix exceeds [`BINARY_CONTROL_CHAR_RATIO`].
/// Control characters are bytes below `0x20` except `\t`, `\n`, `\r`,
/// plus `0x7f`.
#[must_use]
pub fn sniff_is_binary(window: &[u8]) -> bool {
    let end = window.len().min(BINARY_SNIFF_BYTES);
    let window = &window[..end];
    if window.is_empty() {
        return false;
    }
    let control = window
        .iter()
        .filter(|b| **b == 0x7f || (**b < 0x20 && **b != b'\t' && **b != b'\n' && **b != b'\r'))
        .count();
    control as f64 / window.len() as f64 > BINARY_CONTROL_CHAR_RATIO
}

/// Walk `root` like [`walk_files`], additionally enforcing `filter`.
///
/// Every visited file produces exactly one callback: `visit` for accepted
/// files (with stat metadata), `on_skip` with the [`SkipReason`] otherwise.
/// Directory skipping matches [`walk_files`] plus `extra_skip_dirs`.
pub fn walk_files_filtered<F, S>(
    root: &Path,
    filter: &WalkFilter,
    visit: &mut F,
    on_skip: &mut S,
) where
    F: FnMut(WalkEntry),
    S: FnMut(&Path, SkipReason),
{
    let mut skip: HashSet<&str> = DEFAULT_SKIP.iter().copied().collect();
    let extra: Vec<&str> = filter.extra_skip_dirs.iter().map(String::as_str).collect();
    for name in &extra {
        skip.insert(name);
    }
    let mut gi = GitignoreStack::new(root, filter.honor_gitignore);
    walk_filtered_recursive(root, filter, &skip, &mut gi, visit, on_skip);
}

#[allow(clippy::too_many_arguments)]
fn walk_filtered_recursive<F, S>(
    dir: &Path,
    filter: &WalkFilter,
    skip: &HashSet<&str>,
    gi: &mut GitignoreStack,
    visit: &mut F,
    on_skip: &mut S,
) where
    F: FnMut(WalkEntry),
    S: FnMut(&Path, SkipReason),
{
    let depth = gi.depth();
    gi.push_dir(dir);
    let Ok(entries) = std::fs::read_dir(dir) else {
        gi.pop_to(depth);
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if should_skip_dir(&path, skip) {
                continue;
            }
            walk_filtered_recursive(&path, filter, skip, gi, visit, on_skip);
        } else {
            visit_or_skip(&path, filter, gi, visit, on_skip);
        }
    }
    gi.pop_to(depth);
}

fn visit_or_skip<F, S>(
    path: &Path,
    filter: &WalkFilter,
    gi: &GitignoreStack,
    visit: &mut F,
    on_skip: &mut S,
) where
    F: FnMut(WalkEntry),
    S: FnMut(&Path, SkipReason),
{
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !filter.extensions.is_empty() && !filter.extensions.contains(ext) {
        return;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if let Some(cap) = filter.max_bytes {
        if meta.len() > cap {
            on_skip(path, SkipReason::TooLarge);
            return;
        }
    }
    if gi.is_ignored(path) {
        on_skip(path, SkipReason::Ignored);
        return;
    }
    if filter.skip_binary && is_binary_path(path) {
        on_skip(path, SkipReason::Binary);
        return;
    }
    visit(WalkEntry {
        path: path.to_path_buf(),
        size_bytes: meta.len(),
        modified_ms: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
    });
}

fn is_binary_path(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; BINARY_SNIFF_BYTES];
    use std::io::Read as _;
    let Ok(n) = std::io::BufReader::new(file).read(&mut buf) else {
        return false;
    };
    sniff_is_binary(&buf[..n])
}

/// Stack of per-directory `.gitignore` matchers along the current walk path.
///
/// Traversal stays in this module (no second walker); only the gitignore
/// pattern semantics come from the `ignore` crate.
struct GitignoreStack {
    enabled: bool,
    stack: Vec<ignore::gitignore::Gitignore>,
}

impl GitignoreStack {
    fn new(_root: &Path, enabled: bool) -> Self {
        Self {
            enabled,
            stack: Vec::new(),
        }
    }

    fn push_dir(&mut self, dir: &Path) {
        if !self.enabled {
            return;
        }
        let ignore_file = dir.join(".gitignore");
        if ignore_file.is_file() {
            let mut builder = ignore::gitignore::GitignoreBuilder::new(dir);
            builder.add(&ignore_file);
            match builder.build() {
                Ok(gi) => self.stack.push(gi),
                Err(e) => eprintln!("walk: gitignore build failed for {dir:?}: {e}"),
            }
        }
    }

    fn pop_to(&mut self, depth: usize) {
        if !self.enabled {
            return;
        }
        while self.stack.len() > depth {
            self.stack.pop();
        }
    }

    fn depth(&self) -> usize {
        self.stack.len()
    }

    fn is_ignored(&self, path: &Path) -> bool {
        if !self.enabled {
            return false;
        }
        // Every matcher on the stack is anchored to the directory whose
        // `.gitignore` built it; all of them apply to paths below it.
        self.stack
            .iter()
            .any(|gi| gi.matched(path, path.is_dir()).is_ignore())
    }
}

