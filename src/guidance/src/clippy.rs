//! Per-package clippy driver: one JSON invocation per affected package,
//! diagnostics attributed to files by span, plus resource shaping that
//! lowers build priority while interactive queries run.
//!
//! Unit tests cover parsing, attribution, shaping, and argv building;
//! live cargo runs ride the injected runner the waves compose, so no
//! test here spawns a toolchain.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::fix::Suggestion;

/// One clippy/rustc diagnostic attributed to a file.
#[derive(Debug, Clone, Default)]
pub struct ClippyDiag {
    /// File path as reported (often package-relative).
    pub file: Option<String>,
    /// Diagnostic code (`needless_borrow`, `E0432`, …).
    pub code: Option<String>,
    /// Human message.
    pub message: String,
    /// Severity (`warning`, `error`, …).
    pub level: String,
    /// Rendered rustc diagnostic with the source snippet.
    pub rendered: Option<String>,
    /// Machine-applicable replacements with byte spans.
    pub suggestions: Vec<Suggestion>,
}

/// One package invocation's diagnostics.
#[derive(Debug, Clone)]
pub struct ClippyReport {
    /// Parsed diagnostics in output order.
    pub diagnostics: Vec<ClippyDiag>,
    /// Whether cargo exited successfully.
    pub exit_ok: bool,
    /// Cargo's stderr (transport or build failure detail).
    pub stderr: String,
}

impl Default for ClippyReport {
    fn default() -> Self {
        Self {
            diagnostics: Vec::new(),
            exit_ok: true,
            stderr: String::new(),
        }
    }
}

/// Resource shaping for build invocations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildShaping {
    /// Cargo job cap, when reduced.
    pub jobs: Option<usize>,
    /// Lower CPU priority via `nice`.
    pub nice: bool,
}

/// Shaping decision: interactive queries never wait on maintenance, so
/// waves build small and nice while they are active.
#[must_use]
pub fn shaping_for(interactive_active: bool) -> BuildShaping {
    if interactive_active {
        BuildShaping {
            jobs: Some(2),
            nice: true,
        }
    } else {
        BuildShaping {
            jobs: None,
            nice: false,
        }
    }
}

/// Clippy invocation context.
#[derive(Debug, Clone)]
pub struct ClippyCtx {
    /// Dedicated target dir (no lock fights with the user's own cargo).
    pub target_dir: Option<PathBuf>,
    /// Resource shaping for this run.
    pub shaping: BuildShaping,
    /// Extra cargo args (feature flags for the cfg matrix cells).
    pub extra_args: Vec<String>,
}

/// Build the clippy argv plus environment: `cargo clippy
/// --message-format=json` over the package, with shaping applied.
/// Pure, so tests pin it without spawning anything.
#[must_use]
pub fn clippy_command(package_dir: &Path, ctx: &ClippyCtx) -> (Vec<String>, Vec<(String, String)>) {
    let mut argv = Vec::new();
    if ctx.shaping.nice {
        argv.extend(["nice".to_string(), "-n".to_string(), "10".to_string()]);
    }
    argv.push("cargo".to_string());
    argv.push("clippy".to_string());
    argv.push("--message-format=json".to_string());
    if let Some(target) = &ctx.target_dir {
        argv.push("--target-dir".to_string());
        argv.push(target.to_string_lossy().into_owned());
    }
    argv.extend(ctx.extra_args.iter().cloned());
    argv.push("--manifest-path".to_string());
    argv.push(
        package_dir
            .join("Cargo.toml")
            .to_string_lossy()
            .into_owned(),
    );
    let mut env = Vec::new();
    if let Some(jobs) = ctx.shaping.jobs {
        env.push(("CARGO_BUILD_JOBS".to_string(), jobs.to_string()));
    }
    (argv, env)
}

/// Run one clippy invocation over a package directory. Environment
/// shaping rides the shared env-aware capture helper; the package
/// travels in argv via `--manifest-path`, so no working directory
/// override is needed.
pub fn run_clippy(package_dir: &Path, ctx: &ClippyCtx) -> std::io::Result<ClippyReport> {
    let (argv, env) = clippy_command(package_dir, ctx);
    let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
    let output = common_core::shell::run_capture_with_env(&borrowed, &env)?;
    let mut report = parse_clippy_json(&output.stdout);
    report.exit_ok = output.success;
    report.stderr = output.stderr;
    Ok(report)
}

/// Parse `cargo clippy --message-format=json` output: compiler messages
/// become diagnostics, build-finished and rendered lines drop out.
/// Never fails: unparsable lines are skipped, not fatal.
#[must_use]
pub fn parse_clippy_json(output: &str) -> ClippyReport {
    let mut diagnostics = Vec::new();
    for line in output.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("reason").and_then(|reason| reason.as_str()) != Some("compiler-message") {
            continue;
        }
        let message = value.get("message");
        let code = message
            .and_then(|message| message.get("code"))
            .and_then(|code| code.get("code"))
            .and_then(|code| code.as_str())
            .map(str::to_string);
        let text = message
            .and_then(|message| message.get("message"))
            .and_then(|message| message.as_str())
            .unwrap_or("")
            .to_string();
        let rendered = message
            .and_then(|message| message.get("rendered"))
            .and_then(|rendered| rendered.as_str())
            .map(str::to_string);
        let level = message
            .and_then(|message| message.get("level"))
            .and_then(|level| level.as_str())
            .unwrap_or("warning")
            .to_string();
        let spans = message
            .and_then(|message| message.get("spans"))
            .and_then(|spans| spans.as_array())
            .cloned()
            .unwrap_or_default();
        let mut suggestions = Vec::new();
        let mut file = None;
        // Suggestion spans ride the primary spans and the child notes:
        // rustc/clippy carry the MachineApplicable replacement on the
        // child note (e.g. "change this to"), never the primary span,
        // so primary-only harvesting leaves the D2 rung with nothing.
        let mut span_refs: Vec<&serde_json::Value> = spans.iter().collect();
        if let Some(children) = message
            .and_then(|message| message.get("children"))
            .and_then(|children| children.as_array())
        {
            for child in children {
                if let Some(child_spans) = child.get("spans").and_then(|spans| spans.as_array()) {
                    span_refs.extend(child_spans.iter());
                }
            }
        }
        for span in span_refs {
            if file.is_none() {
                file = span
                    .get("file_name")
                    .and_then(|name| name.as_str())
                    .map(str::to_string);
            }
            if let Some(suggestion) = crate::fix::suggestion_from_span(span) {
                suggestions.push(suggestion);
            }
        }
        diagnostics.push(ClippyDiag {
            file,
            code,
            message: text,
            level,
            rendered,
            suggestions,
        });
    }
    ClippyReport {
        diagnostics,
        exit_ok: true,
        stderr: String::new(),
    }
}

/// Attribute diagnostics to files: package-relative names resolve
/// against the package dir, absolute names stand alone.
#[must_use]
pub fn attribute_diagnostics(report: &ClippyReport) -> HashMap<String, Vec<usize>> {
    let mut by_file: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, diagnostic) in report.diagnostics.iter().enumerate() {
        if let Some(file) = &diagnostic.file {
            by_file.entry(file.clone()).or_default().push(index);
        }
    }
    by_file
}

/// Resolve one reported file name against its package directory.
#[must_use]
pub fn resolve_reported(package_dir: &Path, reported: &str) -> PathBuf {
    let path = Path::new(reported);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        package_dir.join(path)
    }
}

/// Owning cargo package of a file: nearest ancestor with `Cargo.toml`.
/// `None` past the search cap (detached files format without clippy).
#[must_use]
pub fn package_root(path: &Path) -> Option<PathBuf> {
    let manifest = common_core::config::find_ancestor_file(path, "Cargo.toml", 10)?;
    manifest.parent().map(Path::to_path_buf)
}

#[cfg(test)]
#[path = "../tests/clippy_driver.rs"]
mod clippy_driver_tests;
