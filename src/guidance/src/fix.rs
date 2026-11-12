//! Deterministic fix rungs D1-D3 plus the verified-apply loop: patches
//! are produced in scratch, certified by the model-free gates, and only
//! then written back. A refused patch restores byte-identically (which,
//! for an unwritten patch, means the file is simply never touched).

use std::path::{Path, PathBuf};

use crate::gates::{
    gates_passed, run_gates, BuildReport, ChangedFile, GateCtx, GateVerdict, TestReport,
};

/// Outcome of the format rung: formatted text plus an optional
/// `fmt_conflict` finding. Text is only appliable when no conflict is
/// reported; a conflict routes to human review instead of the loop.
#[derive(Debug, Clone)]
/// Outcome of the format rung: formatted text plus an optional
/// `fmt_conflict` finding. Text is only appliable when no conflict is
/// reported; a conflict routes to human review instead of the loop.
pub struct FormatOutcome {
    /// Spliced formatted text (test regions preserved).
    pub text: String,
    /// Prose-pressure finding, when rustfmt config would alter prose.
    pub conflict: Option<String>,
}

/// Rustfmt keys that alter prose: with any enabled, formatted comments
/// may move, so hunks touching prose are reported, never silently kept.
fn prose_pressure_keys(config_text: &str) -> Vec<String> {
    [
        "wrap_comments",
        "normalize_comments",
        "format_code_in_doc_comments",
        "format_strings",
    ]
    .into_iter()
    .filter(|key| {
        config_text.lines().any(|line| {
            let line = line.trim_start();
            !line.starts_with('#') && line.starts_with(key) && line.contains("true")
        })
    })
    .map(str::to_string)
    .collect()
}

/// Find the project rustfmt config by walking up from the source file.
/// The project's own config formats; scratch never invents one.
pub(crate) fn find_rustfmt_config(source_path: &Path) -> Option<PathBuf> {
    let mut dir = source_path.parent()?.to_path_buf();
    for _ in 0..8 {
        let candidate = dir.join("rustfmt.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
    None
}

/// D1 rustfmt on a scratch copy with test-preserving splice-back. Never
/// touches the source file; the caller gates and writes back.
#[must_use]
/// D1 rustfmt on a scratch copy with test-preserving splice-back. Never
/// touches the source file; the caller gates and writes back.
pub fn format_rust(source_path: &Path, text: &str) -> FormatOutcome {
    let config = find_rustfmt_config(source_path);
    let Some((spliced, formatted)) =
        crate::gates::rustfmt_and_splice(text, config.as_deref(), source_path.parent())
    else {
        return FormatOutcome {
            text: text.to_string(),
            conflict: Some("fmt_conflict: rustfmt unavailable".to_string()),
        };
    };
    let conflict = config
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| prose_pressure_keys(&text))
        .filter(|keys| !keys.is_empty())
        .and_then(|keys| {
            let mut parser = crate::ast_parser::AstParser::new();
            let mut formatted_units = parser.rust_prose_units(&formatted);
            let mut spliced_units = parser.rust_prose_units(&spliced);
            formatted_units.sort_unstable();
            spliced_units.sort_unstable();
            (formatted_units != spliced_units).then(|| {
                format!(
                    "fmt_conflict: prose config ({}) alters prose",
                    keys.join(", ")
                )
            })
        });
    FormatOutcome {
        text: spliced,
        conflict,
    }
}

/// One machine-applicable replacement from clippy JSON diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One machine-applicable replacement from clippy JSON diagnostics.
pub struct Suggestion {
    /// Byte start of the replaced span.
    pub start: u64,
    /// Byte end (exclusive) of the replaced span.
    pub end: u64,
    /// Replacement text.
    pub replacement: String,
}

/// One span object to a suggestion: only byte-spanned,
/// `MachineApplicable` replacements convert; everything else is `None`.
#[must_use]
/// One span object to a suggestion: only byte-spanned,
/// `MachineApplicable` replacements convert; everything else is `None`.
pub fn suggestion_from_span(span: &serde_json::Value) -> Option<Suggestion> {
    let applicable = span
        .get("suggestion_applicability")
        .and_then(|applicability| applicability.as_str())
        == Some("MachineApplicable");
    if !applicable {
        return None;
    }
    let (Some(start), Some(end), Some(replacement)) = (
        span.get("byte_start").and_then(serde_json::Value::as_u64),
        span.get("byte_end").and_then(serde_json::Value::as_u64),
        span.get("suggested_replacement")
            .and_then(|replacement| replacement.as_str()),
    ) else {
        return None;
    };
    if end < start {
        return None;
    }
    Some(Suggestion {
        start,
        end,
        replacement: replacement.to_string(),
    })
}

/// Parse `MachineApplicable` suggestions from `cargo clippy
/// --message-format=json` output (one JSON object per line, `rendered`
/// lines ignored). Only byte-spanned, applicable suggestions parse;
/// everything else is skipped without error.
#[must_use]
/// Parse `MachineApplicable` suggestions from `cargo clippy
/// --message-format=json` output (one JSON object per line, `rendered`
/// lines ignored). Only byte-spanned, applicable suggestions parse;
/// everything else is skipped without error.
pub fn parse_clippy_suggestions(json_lines: &str) -> Vec<Suggestion> {
    let mut out = Vec::new();
    for line in json_lines.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let spans = value
            .get("spans")
            .and_then(|spans| spans.as_array())
            .cloned()
            .unwrap_or_default();
        for span in spans {
            if let Some(suggestion) = suggestion_from_span(&span) {
                out.push(suggestion);
            }
        }
    }
    out
}

/// Outcome of suggestion application.
#[derive(Debug, Clone)]
/// Outcome of suggestion application.
pub struct ApplyOutcome {
    /// Patched text.
    pub text: String,
    /// Suggestions applied.
    pub applied: usize,
    /// Overlapping suggestions deferred to the next wave.
    pub deferred: usize,
}

/// D2 suggestion application: range-filtered, reverse-offset order,
/// overlaps deferred. `cargo fix` is never invoked: it rewrites tests
/// and prose-adjacent ranges, violating the oracle guarantees.
#[must_use]
/// D2 suggestion application: range-filtered, reverse-offset order,
/// overlaps deferred. `cargo fix` is never invoked: it rewrites tests
/// and prose-adjacent ranges, violating the oracle guarantees.
pub fn apply_suggestions(
    text: &str,
    suggestions: &[Suggestion],
    excluded: &[(u64, u64)],
) -> ApplyOutcome {
    let mut ordered: Vec<&Suggestion> = suggestions.iter().collect();
    ordered.sort_by(|a, b| b.start.cmp(&a.start).then(b.end.cmp(&a.end)));
    let mut bytes = text.as_bytes().to_vec();
    let mut applied = 0usize;
    let mut deferred = 0usize;
    let mut taken: Vec<(u64, u64)> = Vec::new();
    for suggestion in ordered {
        let in_excluded = excluded
            .iter()
            .any(|(start, end)| suggestion.start < *end && *start < suggestion.end);
        let overlaps = taken
            .iter()
            .any(|(start, end)| suggestion.start < *end && *start < suggestion.end);
        if in_excluded || overlaps {
            deferred += 1;
            continue;
        }
        let (Some(head), Some(tail)) = (
            bytes.get(..suggestion.start as usize),
            bytes.get(suggestion.end as usize..),
        ) else {
            deferred += 1;
            continue;
        };
        let mut next = Vec::with_capacity(bytes.len());
        next.extend_from_slice(head);
        next.extend_from_slice(suggestion.replacement.as_bytes());
        next.extend_from_slice(tail);
        bytes = next;
        taken.push((suggestion.start, suggestion.end));
        applied += 1;
    }
    ApplyOutcome {
        text: String::from_utf8(bytes).unwrap_or_else(|_| text.to_string()),
        applied,
        deferred,
    }
}

/// Whether a diagnostic names an unresolved-import error.
fn is_unresolved_import(diagnostic: &str) -> bool {
    diagnostic.contains("E0432") || diagnostic.contains("E0433")
}

/// D3 unambiguous-import recipe: exactly one index candidate drafts one
/// `use` line. Ambiguous, empty, unlisted, or policy-disabled inputs
/// return `None` without touching anything. Insertion point is the
/// caller's job (M8); the recipe only drafts the line.
#[must_use]
/// D3 unambiguous-import recipe: exactly one index candidate drafts one
/// `use` line. Ambiguous, empty, unlisted, or policy-disabled inputs
/// return `None` without touching anything. Insertion point is the
/// caller's job (M8); the recipe only drafts the line.
pub fn recipe_import(diagnostic: &str, candidates: &[String], allowed: bool) -> Option<String> {
    if !allowed || !is_unresolved_import(diagnostic) || candidates.len() != 1 {
        return None;
    }
    Some(format!("use {};\n", candidates[0]))
}

/// D3 missing-`mut` recipe: inserts `mut` after the `let` binding that
/// owns the byte offset. Out-of-range offsets and disabled policy
/// return `None`.
#[must_use]
/// D3 missing-`mut` recipe: inserts `mut` after the `let` binding that
/// owns the byte offset. Out-of-range offsets and disabled policy
/// return `None`.
pub fn recipe_mut(text: &str, name_offset: u64, allowed: bool) -> Option<String> {
    if !allowed {
        return None;
    }
    let offset = name_offset as usize;
    if offset >= text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    let head = &text[..offset];
    let let_at = head.rfind("let ")?;
    if text[let_at..offset]
        .trim_start_matches("let ")
        .contains([';', '{', '}', '\n'])
    {
        return None;
    }
    let mut patched = String::with_capacity(text.len() + 4);
    patched.push_str(&text[..let_at + 4]);
    patched.push_str("mut ");
    patched.push_str(&text[let_at + 4..]);
    Some(patched)
}

/// Verified-apply inputs.
pub struct ApplyCtx<'a> {
    /// Content hash at analysis time (P1 optimistic concurrency).
    pub pre_hash: String,
    /// Language tag for the syntax gates.
    pub language: &'static str,
    /// Protected path globs.
    pub protected_globs: &'a [String],
    /// Policy flag for public-signature changes.
    pub allow_pub_change: bool,
    /// Policy flag for `cfg` changes.
    pub allow_cfg_change: bool,
    /// Declared fix class for the P10 shape check.
    pub fix_class: &'static str,
    /// P9 byte cap.
    pub max_bytes: usize,
    /// P9 changed-line cap.
    pub max_changed_lines: usize,
    /// Project rustfmt config for P5.
    pub rustfmt_config: Option<PathBuf>,
    /// Pre-run build verdict, when the caller ran it.
    pub build: Option<BuildReport>,
    /// Pre-run test verdict, when the caller ran it.
    pub tests: Option<TestReport>,
    /// Diagnostic fingerprints before/after (P6 shrink check).
    pub before_diags: &'a [String],
    /// Diagnostic fingerprints after the patch.
    pub after_diags: &'a [String],
}

/// Verified-apply disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Verified-apply disposition.
pub enum FixVerdict {
    /// Gates green: the write-back stands.
    Kept,
    /// Any gate red: original bytes stand, nothing was written.
    Reverted,
}

/// Verified-apply report for the ledger (M7 journals it).
#[derive(Debug, Clone)]
/// Verified-apply report for the ledger (M7 journals it).
pub struct FixReport {
    /// Disposition.
    pub verdict: FixVerdict,
    /// Every gate verdict, in order.
    pub gates: Vec<GateVerdict>,
    /// Captured gate output for review.
    pub gate_output: String,
}

impl FixReport {
    /// Whether the patch was kept.
    #[must_use]
    pub fn is_kept(&self) -> bool {
        self.verdict == FixVerdict::Kept
    }
}

/// Apply one full-text patch under gates: read the live file (P1),
/// certify, and write back only when every required gate passes. A
/// refusal or a mid-run human edit leaves the file byte-identical.
pub fn apply_verified_patch(path: &Path, new_text: &str, ctx: &ApplyCtx) -> FixReport {
    let live = std::fs::read_to_string(path).unwrap_or_default();
    let live_hash = common_core::hash::sha256_hex(live.as_bytes());
    let gate_ctx = GateCtx {
        language: ctx.language,
        pre_hash: ctx.pre_hash.clone(),
        live_hash,
        protected_globs: ctx.protected_globs,
        allow_pub_change: ctx.allow_pub_change,
        allow_cfg_change: ctx.allow_cfg_change,
        before_diags: ctx.before_diags,
        after_diags: ctx.after_diags,
        max_bytes: ctx.max_bytes,
        max_changed_lines: ctx.max_changed_lines,
        fix_class: ctx.fix_class,
        build: ctx.build.clone(),
        tests: ctx.tests.clone(),
        rustfmt_config: ctx.rustfmt_config.clone(),
        source_dir: path.parent().map(std::path::Path::to_path_buf),
    };
    let file = ChangedFile {
        path: path.to_string_lossy().into_owned(),
        before: live.clone(),
        after: new_text.to_string(),
    };
    let gates = run_gates(&file, &gate_ctx);
    let mut gate_output = String::new();
    for verdict in &gates {
        use std::fmt::Write as _;
        let _ = writeln!(gate_output, "{}: {:?}", verdict.id, verdict.outcome);
    }
    if !gates_passed(&gates) {
        return FixReport {
            verdict: FixVerdict::Reverted,
            gates,
            gate_output,
        };
    }
    if common_core::io::write_atomic(path, new_text.as_bytes()).is_err() {
        return FixReport {
            verdict: FixVerdict::Reverted,
            gates,
            gate_output,
        };
    }
    FixReport {
        verdict: FixVerdict::Kept,
        gates,
        gate_output,
    }
}

#[cfg(test)]
#[path = "../tests/m6_fix.rs"]
mod m6_fix_tests;

#[cfg(test)]
#[path = "../tests/m12_redteam.rs"]
mod m12_redteam_tests;