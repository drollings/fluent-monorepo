//! L0 minimal: capability-gated managed-`rg` subprocess (P1).
//!
//! Always `--json --line-number --column --with-filename --color never`
//! with hard excludes (`!.git/**`, `!.zvec-grep/**`), NDJSON match-only
//! parsing, and limit early-kill. Queries run as literals
//! (`--fixed-strings`; full patterns arrive in P5). Independent of index
//! state from day one; never fused (exhaustive/standalone, as in zvec).
//!
//! Capability gate: `FsCapability` must be in the task-local set or the
//! search fails closed (`DeniedCapability`). Binary chain is ENOENT-only:
//! override → `PATH` lookup; only resolution failures fall through.

use std::io::BufRead as _;
use std::path::PathBuf;

use thiserror::Error;

/// One literal match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgHit {
    /// Workspace-relative path as printed by rg.
    pub path: String,
    /// 1-based line number.
    pub line: u32,
    /// 1-based column of the first submatch.
    pub column: u32,
    /// Matched line text.
    pub text: String,
    /// `-B` context lines above the match (file order).
    pub context_before: Vec<String>,
    /// `-A` context lines below the match (file order).
    pub context_after: Vec<String>,
}

/// Full L0 search options (P5): context capture plus mtime post-filter.
/// Modification times are ms epoch; files whose mtime cannot be read are
/// kept (best-effort — the filter excludes only proven-outside files).
#[derive(Debug, Clone, Default)]
pub struct RgOptions {
    /// `-B` lines of leading context.
    pub context_before: usize,
    /// `-A` lines of trailing context.
    pub context_after: usize,
    /// Modification-time floor (ms epoch).
    pub mtime_after_ms: Option<i64>,
    /// Modification-time ceiling (ms epoch).
    pub mtime_before_ms: Option<i64>,
}

/// Managed-rg failure.
#[derive(Debug, Error)]
pub enum RgError {
    /// No `FsCapability` in scope (fail-closed).
    #[error("rg search denied: no FsCapability in scope")]
    DeniedCapability,
    /// Workspace root does not exist (missing-path diagnostic).
    #[error("rg workspace not found: {path}")]
    MissingRoot {
        /// The missing root as given.
        path: String,
    },
    /// No usable rg binary on the ENOENT-only chain.
    #[error("no rg binary: override and PATH lookup both failed")]
    NoBinary,
    /// Subprocess I/O failure.
    #[error("rg subprocess I/O: {0}")]
    Io(String),
    /// rg exited non-zero.
    #[error("rg failed with status {status}: {stderr}")]
    Failed {
        /// Exit status code.
        status: i32,
        /// Stderr tail.
        stderr: String,
    },
    /// NDJSON parse failure.
    #[error("rg output parse error: {0}")]
    Parse(String),
}

/// Managed-`rg` runner rooted at a workspace directory.
pub struct RgBackend {
    /// Workspace root (rg runs with this as cwd).
    pub root: PathBuf,
    /// Bundled-binary override (checked first).
    pub exe_override: Option<PathBuf>,
    /// `PATH` override for lookup (tests pin an empty dir; `None` = ambient).
    pub path_override: Option<std::ffi::OsString>,
}

impl RgBackend {
    /// Search literally for `query` under the root, at most `limit` hits.
    /// Minimal shape (no context, no mtime filter); full options via
    /// [`RgBackend::search_with_options`].
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<RgHit>, RgError> {
        self.search_with_options(query, limit, &RgOptions::default())
    }

    /// Full L0 search: `-A`/`-B` context capture, mtime post-filter, and a
    /// missing-root diagnostic. Still index-independent and never fused.
    pub fn search_with_options(
        &self,
        query: &str,
        limit: usize,
        options: &RgOptions,
    ) -> Result<Vec<RgHit>, RgError> {
        fluent_wvr::capability::check_capability(&fluent_wvr::capability::FsCapability::new())
            .map_err(|_| RgError::DeniedCapability)?;
        if !self.root.is_dir() {
            return Err(RgError::MissingRoot {
                path: self.root.display().to_string(),
            });
        }
        let exe = self.resolve_exe().ok_or(RgError::NoBinary)?;
        let mut command = std::process::Command::new(exe);
        command.current_dir(&self.root).args([
            "--json",
            "--line-number",
            "--column",
            "--with-filename",
            "--color",
            "never",
            "--no-heading",
            "--fixed-strings",
            "--glob",
            "!.git/**",
            "--glob",
            "!.zvec-grep/**",
        ]);
        if options.context_before > 0 {
            command.args(["--before-context", &options.context_before.to_string()]);
        }
        if options.context_after > 0 {
            command.args(["--after-context", &options.context_after.to_string()]);
        }
        command
            .args(["--", query, "."])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                RgError::NoBinary
            } else {
                RgError::Io(error.to_string())
            }
        })?;
        let mut hits = Vec::new();
        // Buffered context per path plus the last match per path. rg
        // streams one file at a time: context lines buffer until the next
        // match on the same path claims them as `-B` before-context; the
        // file `end` marker flushes the remainder as `-A` after-context of
        // the last match. Lines between two close matches read as the next
        // match's before-context (documented asymmetry; each hit stays
        // self-contained).
        let mut pending_before: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let mut last_match: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        if let Some(stdout) = child.stdout.take() {
            for line in std::io::BufReader::new(stdout).lines() {
                let line = line.map_err(|error| RgError::Parse(error.to_string()))?;
                match parse_stream_line(&line)? {
                    StreamLine::Match(hit) => {
                        let path = hit.path.clone();
                        let mut hit = hit;
                        if let Some(pending) = pending_before.remove(&path) {
                            hit.context_before = pending;
                        }
                        hits.push(hit);
                        last_match.insert(path, hits.len() - 1);
                        if hits.len() >= limit {
                            break;
                        }
                    }
                    StreamLine::Context { path, text } => {
                        pending_before.entry(path).or_default().push(text);
                    }
                    StreamLine::End { path } => {
                        if let Some(pending) = pending_before.remove(&path) {
                            if let Some(index) = last_match.get(&path) {
                                if let Some(hit) = hits.get_mut(*index) {
                                    hit.context_after.extend(pending);
                                }
                            }
                        }
                        last_match.remove(&path);
                    }
                    StreamLine::Other => {}
                }
            }
        }
        // Flush trailing context when early-kill skipped the `end` marker.
        for (path, pending) in std::mem::take(&mut pending_before) {
            if let Some(index) = last_match.get(&path) {
                if let Some(hit) = hits.get_mut(*index) {
                    hit.context_after.extend(pending);
                }
            }
        }
        // Deterministic order: parallel rg streams files in scheduling
        // order, so arrival order varies run to run. Sorting here (before
        // the mtime post-filter, which preserves order) makes every
        // consumer — CLI render, coverage, tests — byte-stable. The
        // explicit-limit kill still fires on arrival count above; only
        // the surviving subset is ordered.
        sort_rg_hits(&mut hits);
        // Early-kill past the limit, then reap (exit status: 0 match, 1
        // none, 2 error — only 2 fails).
        let _ = child.kill();
        let output = child
            .wait_with_output()
            .map_err(|error| RgError::Io(error.to_string()))?;
        let status = output.status.code().unwrap_or(-1);
        if status == 2 {
            return Err(RgError::Failed {
                status,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(apply_mtime_filter(&self.root, hits, options))
    }

    /// ENOENT-only chain: override (when present on disk) → `PATH` lookup.
    fn resolve_exe(&self) -> Option<PathBuf> {
        if let Some(override_exe) = &self.exe_override {
            if override_exe.is_file() {
                return Some(override_exe.clone());
            }
        }
        match &self.path_override {
            Some(path) => which::which_in("rg", Some(path), &self.root).ok(),
            None => which::which("rg").ok(),
        }
    }
}

/// One NDJSON stream item: a match, a context line, a file-end marker, or
/// anything else (begin/summary/stats — match-only parse otherwise).
enum StreamLine {
    Match(RgHit),
    Context { path: String, text: String },
    End { path: String },
    Other,
}

fn stream_path(value: &serde_json::Value) -> Option<String> {
    value
        .get("path")
        .and_then(|path| path.get("text"))
        .and_then(|text| text.as_str())
        .map(|path| path.strip_prefix("./").unwrap_or(path).to_string())
}

fn stream_text(data: &serde_json::Value) -> String {
    data.get("lines")
        .and_then(|lines| lines.get("text"))
        .and_then(|text| text.as_str())
        .unwrap_or("")
        .to_string()
}

fn parse_stream_line(line: &str) -> Result<StreamLine, RgError> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|error| RgError::Parse(error.to_string()))?;
    let kind = value
        .get("type")
        .and_then(|kind| kind.as_str())
        .unwrap_or("");
    let data = value.get("data");
    match kind {
        "match" => {
            let data = data.ok_or_else(|| RgError::Parse("match without data".to_string()))?;
            let path = stream_path(data)
                .ok_or_else(|| RgError::Parse("match without path".to_string()))?;
            let line_number = data
                .get("line_number")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| RgError::Parse("match without line_number".to_string()))?;
            let column = data
                .get("submatches")
                .and_then(|submatches| submatches.as_array())
                .and_then(|submatches| submatches.first())
                .and_then(|submatch| submatch.get("start"))
                .and_then(serde_json::Value::as_u64)
                .map_or(1, |start| start + 1);
            Ok(StreamLine::Match(RgHit {
                path,
                line: line_number as u32,
                column: column as u32,
                text: stream_text(data),
                context_before: Vec::new(),
                context_after: Vec::new(),
            }))
        }
        "context" => {
            let data = data.ok_or_else(|| RgError::Parse("context without data".to_string()))?;
            let path = stream_path(data)
                .ok_or_else(|| RgError::Parse("context without path".to_string()))?;
            Ok(StreamLine::Context {
                path,
                text: stream_text(data),
            })
        }
        "end" => {
            let path = data
                .and_then(stream_path)
                .ok_or_else(|| RgError::Parse("end without path".to_string()))?;
            Ok(StreamLine::End { path })
        }
        _ => Ok(StreamLine::Other),
    }
}

/// Sort hits into deterministic `(path, line, column)` order (stable —
/// fully-equal hits keep arrival order). Whole structs move, so context
/// lines stay glued to their own match.
fn sort_rg_hits(hits: &mut [RgHit]) {
    hits.sort_by(|a, b| {
        (a.path.as_str(), a.line, a.column).cmp(&(b.path.as_str(), b.line, b.column))
    });
}

/// Mtime post-filter: drop hits whose file mtime falls outside
/// `[mtime_after_ms, mtime_before_ms]`. Files that cannot be read are kept
/// (best-effort — the filter excludes only proven-outside files).
fn apply_mtime_filter(root: &std::path::Path, hits: Vec<RgHit>, options: &RgOptions) -> Vec<RgHit> {
    if options.mtime_after_ms.is_none() && options.mtime_before_ms.is_none() {
        return hits;
    }
    hits.into_iter()
        .filter(|hit| {
            let mtime_ms = std::fs::metadata(root.join(&hit.path))
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|age| age.as_millis() as i64);
            match mtime_ms {
                None => true,
                Some(mtime) => {
                    options.mtime_after_ms.is_none_or(|floor| mtime >= floor)
                        && options
                            .mtime_before_ms
                            .is_none_or(|ceiling| mtime <= ceiling)
                }
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "../../tests/query_rg_backend.rs"]
mod tests;
