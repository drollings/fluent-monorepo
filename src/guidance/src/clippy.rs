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
#[derive(Debug, Clone)]
pub struct ClippyDiag {
    /// File path as reported (often package-relative).
    pub file: Option<String>,
    /// Diagnostic code (`needless_borrow`, `E0432`, …).
    pub code: Option<String>,
    /// Human message.
    pub message: String,
    /// Severity (`warning`, `error`, …).
    pub level: String,
    /// Machine-applicable replacements with byte spans.
    pub suggestions: Vec<Suggestion>,
}

/// One package invocation's diagnostics.
#[derive(Debug, Clone, Default)]
pub struct ClippyReport {
    /// Parsed diagnostics in output order.
    pub diagnostics: Vec<ClippyDiag>,
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

/// Run one clippy invocation over a package directory. Env control
/// needs `CARGO_BUILD_JOBS`, which the shared capture helper cannot
/// set, so this driver spawns directly.
pub fn run_clippy(package_dir: &Path, ctx: &ClippyCtx) -> std::io::Result<ClippyReport> {
    let (argv, env) = clippy_command(package_dir, ctx);
    let mut command = std::process::Command::new(&argv[0]);
    command.args(&argv[1..]);
    for (key, value) in &env {
        command.env(key, value);
    }
    let output = command.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(parse_clippy_json(&stdout))
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
        for span in &spans {
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
            suggestions,
        });
    }
    ClippyReport { diagnostics }
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
    let mut dir = path.parent()?.to_path_buf();
    for _ in 0..10 {
        if dir.join("Cargo.toml").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
    None
}

#[cfg(test)]
#[path = "../tests/clippy_driver.rs"]
mod clippy_driver_tests;
