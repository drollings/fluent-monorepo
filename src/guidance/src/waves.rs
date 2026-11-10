//! Wave scheduler for the deterministic fix loop: batch-format the
//! dirty set, run one clippy per affected package, apply the ladder,
//! and repeat to fixpoint, no-progress, or budget. The bespoke
//! coalescing driver underneath stays exactly as is; waves schedule
//! file lists, never jobs.
//!
//! One rung per file per wave (D1 format, then D2 suggestions); D2
//! only runs when D1 left the text untouched, so suggestion offsets
//! measured pre-wave still resolve. Certification rides the model-free
//! gates with write-ahead intent and journaling; build and test
//! verdicts arrive as explicit skips until waves own cargo results.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::clippy::{attribute_diagnostics, resolve_reported, ClippyReport};
use crate::maintenance::{BreakerConfig, LoopBreaker};

/// Clippy runner: one invocation per package directory. Production
/// passes the live driver; tests inject a counting stub.
pub type ClippyRunner = Arc<dyn Fn(&Path) -> std::io::Result<ClippyReport> + Send + Sync>;

/// Build the live runner over one clippy context.
#[must_use]
pub fn live_runner(ctx: crate::clippy::ClippyCtx) -> ClippyRunner {
    Arc::new(move |package: &Path| crate::clippy::run_clippy(package, &ctx))
}

/// One escalated file with its requeue fingerprint.
#[derive(Debug, Clone)]
pub struct Escalation {
    /// File path, as passed in.
    pub file: String,
    /// Stable fingerprint (file plus diagnostic codes) for requeue.
    pub fingerprint: String,
    /// Why the ladder stopped (`attempts exhausted`, `fmt_conflict`,
    /// `no_progress`).
    pub reason: String,
}

/// Wave-loop inputs.
pub struct WaveCtx<'a> {
    /// Journal directory for kept edits.
    pub journal_dir: PathBuf,
    /// Write-ahead intent directory.
    pub wal_dir: PathBuf,
    /// Protected path globs.
    pub protected_globs: &'a [String],
    /// Policy flag for public-signature changes.
    pub allow_pub_change: bool,
    /// Policy flag for `cfg` changes.
    pub allow_cfg_change: bool,
    /// P9 byte cap.
    pub max_bytes: usize,
    /// P9 changed-line cap.
    pub max_changed_lines: usize,
    /// Wave budget for one run.
    pub max_waves: usize,
    /// One clippy invocation per package per wave.
    pub clippy: ClippyRunner,
}

/// Wave-loop outcome for the run report.
#[derive(Debug, Clone, Default)]
pub struct WaveReport {
    /// Waves executed (the last is always the clean witness).
    pub waves_run: usize,
    /// Files formatted and kept.
    pub formatted: usize,
    /// Files fixed at any rung and kept.
    pub fixed: usize,
    /// Escalated files with fingerprints.
    pub escalated: Vec<Escalation>,
    /// Clippy invocations spent (one per affected package per wave).
    pub clippy_invocations: usize,
    /// Stop reason when the loop did not reach the fixpoint.
    pub stopped: Option<String>,
    /// Explicit witness verdict: the final wave kept nothing and
    /// nothing unowned remains. Set, never inferred from `stopped`.
    pub witness_clean: bool,
}

/// Diagnostic code to declared fix class. Codes without a shape never
/// auto-apply: the P10 shape check would skip and the patch must not
/// reach the tree on a verdict alone. Single home is the triage table.
fn class_for_code(code: &str) -> &'static str {
    crate::triage::fix_class_for_code(code).unwrap_or("generic")
}

/// Fingerprint one escalation for requeue.
fn fingerprint(file: &str, codes: &[String], reason: &str) -> String {
    let mut codes = codes.to_vec();
    codes.sort();
    codes.dedup();
    common_core::hash::blake3_hex(format!("{file}\0{}\0{reason}", codes.join(",")).as_bytes())
}

/// One file's diagnostics as `(package, report index)` pairs in package
/// order, so multi-package runs stay deterministic.
type FileDiags = HashMap<PathBuf, Vec<(PathBuf, usize)>>;

/// Run the wave loop over dirty files to the fixpoint.
pub fn run_fix_waves(files: &[PathBuf], ctx: &WaveCtx) -> WaveReport {
    let mut report = WaveReport::default();
    let mut attempts: HashMap<PathBuf, u32> = HashMap::new();
    let mut converged: HashSet<PathBuf> = HashSet::new();
    let mut kept_patch: HashMap<PathBuf, String> = HashMap::new();
    let mut breaker = LoopBreaker::new(BreakerConfig::strict());
    let mut ast_parser = crate::ast_parser::AstParser::new();
    let mut last_wave_kept = false;

    while report.waves_run < ctx.max_waves {
        let dirty: Vec<PathBuf> = files
            .iter()
            .filter(|path| attempts.get(*path).copied().unwrap_or(0) < 3 && !converged.contains(*path))
            .cloned()
            .collect();
        if dirty.is_empty() {
            break;
        }
        report.waves_run += 1;
        let mut kept_this_wave = false;

        // One clippy per affected package; detached files format only.
        let mut packages: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        for path in &dirty {
            if let Some(root) = crate::clippy::package_root(path) {
                packages.entry(root).or_default().push(path.clone());
            }
        }
        let mut package_names: Vec<PathBuf> = packages.keys().cloned().collect();
        package_names.sort();
        let mut reports: HashMap<PathBuf, ClippyReport> = HashMap::new();
        let mut diags: FileDiags = HashMap::new();
        for package in &package_names {
            report.clippy_invocations += 1;
            if let Ok(parsed) = (ctx.clippy)(package) {
                for (name, indices) in attribute_diagnostics(&parsed) {
                    let absolute = resolve_reported(package, &name);
                    if dirty.contains(&absolute) {
                        diags
                            .entry(absolute)
                            .or_default()
                            .extend(indices.iter().map(|i| (package.clone(), *i)));
                    }
                }
                reports.insert(package.clone(), parsed);
            }
        }

        for path in &dirty {
            let live = std::fs::read_to_string(path).unwrap_or_default();
            // D1 format on scratch; conflicts hold the file for review.
            let formatted = crate::fix::format_rust(path, &live);
            if let Some(conflict) = formatted.conflict {
                escalate(&mut report, path, &file_codes(path, &diags, &reports), conflict);
                attempts.insert(path.clone(), 3);
                continue;
            }
            if formatted.text != live {
                if apply_kept(path, &formatted.text, "rustfmt", "D1", ctx) {
                    report.formatted += 1;
                    report.fixed += 1;
                    kept_this_wave = true;
                    kept_patch.insert(path.clone(), formatted.text);
                } else {
                    bump(attempts.entry(path.clone()).or_insert(0));
                }
                continue;
            }
            // D2 suggestions only on untouched text, so clippy offsets
            // measured pre-wave still resolve.
            match apply_suggestions_for(path, &live, &diags, &reports, ctx, &mut ast_parser) {
                Act::Kept(patch) => {
                    if kept_patch.get(path).is_some_and(|last| *last == patch) {
                        escalate(
                            &mut report,
                            path,
                            &file_codes(path, &diags, &reports),
                            "no_progress".to_string(),
                        );
                        attempts.insert(path.clone(), 3);
                    } else {
                        report.fixed += 1;
                        kept_this_wave = true;
                        kept_patch.insert(path.clone(), patch);
                    }
                }
                Act::None => {
                    if diags.get(path).is_none_or(Vec::is_empty) {
                        converged.insert(path.clone());
                    } else {
                        bump(attempts.entry(path.clone()).or_insert(0));
                    }
                }
                Act::NeedsCert => {
                    bump(attempts.entry(path.clone()).or_insert(0));
                }
            }
            if attempts.get(path).copied().unwrap_or(0) >= 3
                && !report.escalated.iter().any(|esc| esc.file == path.to_string_lossy())
            {
                escalate(
                    &mut report,
                    path,
                    &file_codes(path, &diags, &reports),
                    "attempts exhausted".to_string(),
                );
            }
        }

        if let Some(stop) = breaker.observe(kept_this_wave) {
            report.stopped = Some(stop);
            last_wave_kept = kept_this_wave;
            break;
        }
        last_wave_kept = kept_this_wave;
    }
    if report.waves_run >= ctx.max_waves
        && files
            .iter()
            .any(|path| attempts.get(path).copied().unwrap_or(0) < 3 && !converged.contains(path))
    {
        report.stopped = Some("max waves reached".to_string());
    }
    // Explicit witness: the final wave kept nothing, the loop was not
    // stopped, and every file converged or carries an owned escalation.
    let unowned = files
        .iter()
        .filter(|path| {
            !converged.contains(*path)
                && !report.escalated.iter().any(|esc| esc.file == path.to_string_lossy())
        })
        .count();
    report.witness_clean =
        crate::pipeline::witness_clean(last_wave_kept, unowned, report.stopped.is_some());
    report
}

fn bump(attempt: &mut u32) {
    *attempt += 1;
}

fn file_codes(
    path: &Path,
    diags: &FileDiags,
    reports: &HashMap<PathBuf, ClippyReport>,
) -> Vec<String> {
    diags
        .get(path)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(package, index)| {
                    reports
                        .get(package)
                        .and_then(|report| report.diagnostics.get(*index))
                        .and_then(|diag| diag.code.clone())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn escalate(report: &mut WaveReport, path: &Path, codes: &[String], reason: String) {
    let file = path.to_string_lossy().into_owned();
    if report.escalated.iter().any(|esc| esc.file == file) {
        return;
    }
    let fingerprint = fingerprint(&file, codes, &reason);
    report.escalated.push(Escalation {
        file,
        fingerprint,
        reason,
    });
}

/// D2 outcome for one file in one wave.
enum Act {
    /// A patch was kept; carries the kept text for no-progress checks.
    Kept(String),
    /// Nothing acted (no shaped suggestions, or converged).
    None,
    /// Suggestions existed but certification refused them.
    NeedsCert,
}

/// D2 over one file's diagnostics: first shaped code group applies.
fn apply_suggestions_for(
    path: &Path,
    live: &str,
    diags: &FileDiags,
    reports: &HashMap<PathBuf, ClippyReport>,
    ctx: &WaveCtx,
    parser: &mut crate::ast_parser::AstParser,
) -> Act {
    let mut by_code: HashMap<String, Vec<crate::fix::Suggestion>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    if let Some(entries) = diags.get(path) {
        for (package, index) in entries {
            let Some(diag) = reports
                .get(package)
                .and_then(|report| report.diagnostics.get(*index))
            else {
                continue;
            };
            let code = diag.code.clone().unwrap_or_default();
            if class_for_code(&code) == "generic" {
                continue;
            }
            if !by_code.contains_key(&code) {
                order.push(code.clone());
            }
            by_code.entry(code).or_default().extend(diag.suggestions.iter().cloned());
        }
    }
    order.sort();
    if order.is_empty() {
        return Act::None;
    }
    let excluded = parser.rust_test_spans(live);
    for code in order {
        let class = class_for_code(&code);
        let Some(suggestions) = by_code.get(&code) else {
            continue;
        };
        let outcome = crate::fix::apply_suggestions(live, suggestions, &excluded);
        if outcome.applied == 0 || outcome.text == live {
            continue;
        }
        if apply_kept(path, &outcome.text, class, "D2", ctx) {
            return Act::Kept(outcome.text);
        }
        return Act::NeedsCert;
    }
    Act::None
}

/// Certify one patch through gates with intent and journal; P7/P8
/// verdicts arrive as explicit skips until waves own cargo results.
fn apply_kept(path: &Path, text: &str, fix_class: &'static str, actor: &str, ctx: &WaveCtx) -> bool {
    let apply = crate::fix::ApplyCtx {
        pre_hash: String::new(),
        language: "rust",
        protected_globs: ctx.protected_globs,
        allow_pub_change: ctx.allow_pub_change,
        allow_cfg_change: ctx.allow_cfg_change,
        fix_class,
        max_bytes: ctx.max_bytes,
        max_changed_lines: ctx.max_changed_lines,
        rustfmt_config: crate::fix::find_rustfmt_config(path),
        build: None,
        tests: None,
        before_diags: &[],
        after_diags: &[],
    };
    crate::journal::apply_journaled(path, text, &apply, &ctx.journal_dir, &ctx.wal_dir, "wave", actor)
        .is_kept()
}

#[cfg(test)]
#[path = "../tests/m8_waves.rs"]
mod m8_waves_tests;
