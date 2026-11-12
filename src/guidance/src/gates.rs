//! Model-free certification gates P1-P10: every automated patch passes
//! each gate or is discarded with the failing gate named. Cheap pure
//! gates always run; build and test verdicts arrive pre-run from the
//! caller (the wave scheduler owns cargo in M8), recorded as pass,
//! fail, or explicit skip — never silent.
//!
//! The spliced-format primitive also lives here so the format rung and
//! the format-stability gate share one implementation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast_parser::{AstParser, TopItem};

/// Changed file under certification.
#[derive(Debug, Clone)]
/// Changed file under certification.
pub struct ChangedFile {
    /// Workspace-relative path, as cited.
    pub path: String,
    /// Bytes before the patch.
    pub before: String,
    /// Bytes after the patch.
    pub after: String,
}

/// Pre-run build verdict across the cfg matrix (caller-supplied; the
/// wave scheduler owns cargo).
#[derive(Debug, Clone)]
/// Pre-run build verdict across the cfg matrix (caller-supplied; the
/// wave scheduler owns cargo).
pub struct BuildReport {
    /// Clean across every matrix cell.
    pub clean: bool,
    /// Captured output for the ledger.
    pub output: String,
}

/// Pre-run test verdict against the recorded baseline.
#[derive(Debug, Clone)]
/// Pre-run test verdict against the recorded baseline.
pub struct TestReport {
    /// Failures beyond the baseline; empty means no regression.
    pub new_failures: Vec<String>,
    /// Captured output for the ledger.
    pub output: String,
}

/// Gate inputs. Slices borrow the caller's diagnosis; paths and flags
/// come from policy.
pub struct GateCtx<'a> {
    /// Language tag (`rust` is certified; others pass syntax gates
    /// through with an explicit note).
    pub language: &'static str,
    /// Content hash at analysis time (P1 optimistic-concurrency check).
    pub pre_hash: String,
    /// Content hash at apply time.
    pub live_hash: String,
    /// Protected path globs, `*` matching any substring.
    pub protected_globs: &'a [String],
    /// Policy flag for public-signature changes.
    pub allow_pub_change: bool,
    /// Policy flag for `cfg` attribute changes.
    pub allow_cfg_change: bool,
    /// Diagnostic fingerprints before the patch (P6 shrink check).
    pub before_diags: &'a [String],
    /// Diagnostic fingerprints after the patch.
    pub after_diags: &'a [String],
    /// P9 byte cap on the patched file.
    pub max_bytes: usize,
    /// P9 changed-line cap.
    pub max_changed_lines: usize,
    /// Declared fix class for the P10 shape check.
    pub fix_class: &'static str,
    /// Pre-run build verdict, when the caller ran it.
    pub build: Option<BuildReport>,
    /// Pre-run test verdict, when the caller ran it.
    pub tests: Option<TestReport>,
    /// Project rustfmt config for the stability gate, when known.
    pub rustfmt_config: Option<PathBuf>,
    /// Directory holding the file, for sibling-aware rustfmt scratch
    /// placement (`mod` resolution). `None` falls back to a temp dir.
    pub source_dir: Option<PathBuf>,
}

/// One gate's outcome: pass, fail with reason, or explicit skip.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// One gate's outcome: pass, fail with reason, or explicit skip.
pub enum GateOutcome {
    /// Certified.
    Pass,
    /// Rejected with the reason for the ledger.
    Fail(String),
    /// Not run, with why (missing runner, unsupported language).
    Skip(String),
}

/// One gate's verdict with its stable ID.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// One gate's verdict with its stable ID.
pub struct GateVerdict {
    /// Stable gate ID (`P1`–`P10`).
    pub id: String,
    /// Outcome.
    pub outcome: GateOutcome,
}

impl GateVerdict {
    /// Whether the gate certified.
    #[must_use]
    pub fn is_pass(&self) -> bool {
        self.outcome == GateOutcome::Pass
    }

    /// Whether the gate rejected.
    #[must_use]
    pub fn is_fail(&self) -> bool {
        matches!(self.outcome, GateOutcome::Fail(_))
    }

    /// Whether the gate did not run.
    #[must_use]
    pub fn is_skip(&self) -> bool {
        matches!(self.outcome, GateOutcome::Skip(_))
    }
}

/// Run all ten gates in order, collecting every verdict. Callers decide
/// on [`gates_passed`]; the ledger records the full list either way.
#[must_use]
/// Run all ten gates in order, collecting every verdict. Callers decide
/// on [`gates_passed`]; the ledger records the full list either way.
pub fn run_gates(file: &ChangedFile, ctx: &GateCtx) -> Vec<GateVerdict> {
    let mut parser = AstParser::new();
    vec![
        GateVerdict {
            id: "P1".to_string(),
            outcome: check_precondition(ctx),
        },
        GateVerdict {
            id: "P2".to_string(),
            outcome: check_scope(file, ctx, &mut parser),
        },
        GateVerdict {
            id: "P3".to_string(),
            outcome: check_prose(file, ctx, &mut parser),
        },
        GateVerdict {
            id: "P4".to_string(),
            outcome: check_parse(file, ctx, &mut parser),
        },
        GateVerdict {
            id: "P5".to_string(),
            outcome: check_format_stable(file, ctx),
        },
        GateVerdict {
            id: "P6".to_string(),
            outcome: check_diagnostics(ctx),
        },
        GateVerdict {
            id: "P7".to_string(),
            outcome: check_build(ctx),
        },
        GateVerdict {
            id: "P8".to_string(),
            outcome: check_tests(ctx),
        },
        GateVerdict {
            id: "P9".to_string(),
            outcome: check_size(file, ctx, &mut parser),
        },
        GateVerdict {
            id: "P10".to_string(),
            outcome: check_edit_shape(file, ctx),
        },
    ]
}

/// Certification rule: every gate passes, except P6-P8 which may skip
/// (no diagnostics in scope, no runner supplied). A skip anywhere else
/// reverts, exactly like a fail.
#[must_use]
/// Certification rule: every gate passes, except P6-P8 which may skip
/// (no diagnostics in scope, no runner supplied). A skip anywhere else
/// reverts, exactly like a fail.
pub fn gates_passed(verdicts: &[GateVerdict]) -> bool {
    verdicts.iter().all(|verdict| match verdict.id.as_str() {
        "P6" | "P7" | "P8" => !verdict.is_fail(),
        _ => verdict.is_pass(),
    })
}

/// P1 precondition: the target is unchanged since analysis. A human
/// editing mid-run discards the patch and requeues the item, never an
/// overwrite.
fn check_precondition(ctx: &GateCtx) -> GateOutcome {
    if ctx.pre_hash == ctx.live_hash {
        GateOutcome::Pass
    } else {
        GateOutcome::Fail("target changed since analysis".to_string())
    }
}

/// Whether a path is a test, config, or toolchain file the loop never
/// writes: test modules, `tests/`, `benches/`, Cargo files, build
/// scripts, and toolchain or lint/fmt configs.
fn is_refused_path(path: &str) -> bool {
    let segments: Vec<&str> = path.split(['/', '\\']).collect();
    if segments.iter().any(|s| *s == "tests" || *s == "benches") {
        return true;
    }
    match segments.last().copied().unwrap_or("") {
        "Cargo.toml" | "Cargo.lock" | "build.rs" | "clippy.toml" | "rustfmt.toml" => return true,
        name if name.starts_with("rust-toolchain") => return true,
        _ => {}
    }
    false
}

/// Whether a glob (`*` matches any substring) or exact pattern hits.
fn glob_hits(pattern: &str, path: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == path;
    }
    pattern.split('*').all(|part| path.contains(part))
}

/// Count non-overlapping occurrences of a token.
fn count_token(text: &str, token: &str) -> usize {
    text.match_indices(token).count()
}

/// Blank one prose unit wherever it occurs: comments and string literals
/// may cite a watched token without adding a code site. Multiset removal
/// matches the P3 prose comparison, so both gates see the same prose.
fn strip_prose_unit(surface: &mut String, unit: &str) {
    if unit.is_empty() {
        return;
    }
    while let Some(at) = surface.find(unit) {
        surface.replace_range(at..at + unit.len(), "");
    }
}

/// Blank byte spans with spaces (length-preserving, boundary-safe):
/// test regions leave the counting surface without shifting the rest.
fn blank_spans(text: &str, spans: &[(u64, u64)]) -> String {
    let mut bytes = text.as_bytes().to_vec();
    for (start, end) in spans {
        let end = (*end as usize).min(bytes.len());
        let start = (*start as usize).min(end);
        for byte in &mut bytes[start..end] {
            *byte = b' ';
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| text.to_string())
}

/// Code-only surface for suppression and panic counting: test regions are
/// already out of `remaining`; prose units come out here. Added code
/// tokens survive; added prose does not.
fn code_surface(parser: &mut AstParser, preserved_after: &str, full_text: &str) -> String {
    let mut surface = preserved_after.to_string();
    for unit in parser.rust_prose_units(full_text) {
        strip_prose_unit(&mut surface, &unit);
    }
    surface
}

/// P2 scope: permitted ranges only. Test regions survive
/// byte-identically (coordinate-free: every before-span appears in the
/// after text), configs and protected paths refuse, suppression and
/// new panic sites refuse by token count, and public-signature moves
/// need the policy flag.
fn check_scope(file: &ChangedFile, ctx: &GateCtx, parser: &mut AstParser) -> GateOutcome {
    if is_refused_path(&file.path) {
        return GateOutcome::Fail(format!("refused path: {}", file.path));
    }
    if ctx
        .protected_globs
        .iter()
        .any(|glob| glob_hits(glob, &file.path))
    {
        return GateOutcome::Fail(format!("protected path: {}", file.path));
    }
    if ctx.language != "rust" {
        return GateOutcome::Skip("scope syntax checks are Rust-only".to_string());
    }
    let test_spans: Vec<String> = parser
        .rust_test_spans(&file.before)
        .into_iter()
        .filter_map(|(start, end)| {
            file.before
                .as_bytes()
                .get(start as usize..end as usize)
                .and_then(|slice| std::str::from_utf8(slice).ok())
                .map(str::to_string)
        })
        .collect();
    let mut remaining = file.after.clone();
    for span in &test_spans {
        let Some(at) = remaining.find(span.as_str()) else {
            return GateOutcome::Fail("test region altered".to_string());
        };
        remaining.replace_range(at..at + span.len(), "");
    }
    // Count on code surfaces: test regions blanked on both sides and
    // prose stripped from both sides, so a new comment or string literal
    // citing a token is not a new code site, and test-region tokens
    // cannot mask a real addition.
    let before_spans = parser.rust_test_spans(&file.before);
    let before_code = code_surface(
        parser,
        &blank_spans(&file.before, &before_spans),
        &file.before,
    );
    let after_code = code_surface(parser, &remaining, &file.after);
    for (token, name) in [
        ("unsafe", "unsafe"),
        ("#[allow", "allow"),
        ("#![allow", "allow"),
        ("todo!(", "todo"),
        ("unimplemented!(", "unimplemented"),
        ("unreachable!(", "unreachable"),
        ("panic!(", "panic"),
        ("unwrap(", "unwrap"),
        ("expect(", "expect"),
    ] {
        if count_token(&after_code, token) > count_token(&before_code, token) {
            return GateOutcome::Fail(format!("new {name} site"));
        }
    }
    if !ctx.allow_cfg_change
        && count_token(&file.after, "#[cfg(") != count_token(&file.before, "#[cfg(")
    {
        return GateOutcome::Fail("cfg attribute change needs approval".to_string());
    }
    if !ctx.allow_pub_change
        && parser.rust_pub_headers(&file.before) != parser.rust_pub_headers(&file.after)
    {
        return GateOutcome::Fail("public signature change needs approval".to_string());
    }
    GateOutcome::Pass
}

/// P3 prose: comments, doc attributes, and string literals match as
/// multisets, ignoring leading indentation. Code may move freely.
fn check_prose(file: &ChangedFile, ctx: &GateCtx, parser: &mut AstParser) -> GateOutcome {
    if ctx.language != "rust" {
        return GateOutcome::Skip("prose units are Rust-only".to_string());
    }
    let mut before = parser.rust_prose_units(&file.before);
    let mut after = parser.rust_prose_units(&file.after);
    before.sort_unstable();
    after.sort_unstable();
    if before == after {
        GateOutcome::Pass
    } else {
        GateOutcome::Fail("prose units differ".to_string())
    }
}

/// P4 parse: the patch adds no new error nodes.
fn check_parse(file: &ChangedFile, ctx: &GateCtx, parser: &mut AstParser) -> GateOutcome {
    if ctx.language != "rust" {
        return GateOutcome::Skip("parse gate is Rust-only".to_string());
    }
    let before = parser.rust_error_count(&file.before);
    let after = parser.rust_error_count(&file.after);
    if after == usize::MAX {
        return GateOutcome::Fail("after text unparseable".to_string());
    }
    if after <= before {
        GateOutcome::Pass
    } else {
        GateOutcome::Fail(format!("new parse errors: {before} to {after}"))
    }
}

/// Format one text through rustfmt on a scratch copy: project config
/// when found, edition 2021 otherwise. `None` when rustfmt is missing.
/// The scratch lives beside the source when its directory is known, so
/// `mod` declarations resolve exactly as they do for the real file; a
/// global temp dir would break every multi-module crate. Only the named
/// scratch file is ever formatted — siblings are read-only resolution
/// context, never rewritten. Best-effort cleanup on the way out.
fn run_rustfmt(text: &str, config: Option<&Path>, source_dir: Option<&Path>) -> Option<String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let slot = COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = format!(
        ".gate-{}.guidance-fmt-{}-{slot}.rs",
        std::process::id(),
        &common_core::hash::blake3_hex(text.as_bytes())[..16]
    );
    // Beside-source first (module resolution), temp dir as fallback.
    // A caller-supplied directory that cannot hold the scratch is not
    // an error by itself — the fallback decides.
    let beside = source_dir
        .map(|dir| dir.join(&stem))
        .filter(|path| std::fs::write(path, text).is_ok());
    let mut owned_dir = None;
    let scratch = if let Some(path) = beside {
        path
    } else {
        let dir = std::env::temp_dir().join(format!(
            "guidance-gate-{}-{slot}-{}",
            std::process::id(),
            &common_core::hash::blake3_hex(text.as_bytes())[..16]
        ));
        if common_core::io::ensure_dir(&dir).is_err() {
            return None;
        }
        let path = dir.join("gate.rs");
        std::fs::write(&path, text).ok()?;
        owned_dir = Some(dir);
        path
    };
    let path = scratch.to_string_lossy().into_owned();
    let mut argv: Vec<&str> = vec!["rustfmt", "--edition", "2021"];
    let config_str;
    if let Some(cfg) = config {
        config_str = cfg.to_string_lossy().into_owned();
        argv.extend(["--config-path", config_str.as_str()]);
    }
    argv.push(path.as_str());
    let ran = common_core::shell::run_capture(&argv).ok()?;
    let out = if ran.success {
        std::fs::read_to_string(&scratch).ok()
    } else {
        None
    };
    // Best-effort cleanup: the scratch file always, the temp dir only
    // when this call created it (never the source tree).
    let _ = std::fs::remove_file(&scratch);
    if let Some(dir) = owned_dir {
        let _ = std::fs::remove_dir(&dir);
    }
    out
}

/// Splice formatted non-test items over original test regions: test
/// bytes, interstitial layout, and leading/trailing text survive
/// byte-identically while code takes the formatted shape. Items match
/// by index with kind equality (rustfmt preserves order, count, and
/// kind); any structural disagreement falls back to the original, so a
/// mis-splice is impossible, only a no-op. Shared by the format rung
/// and the stability gate, so the loop converges instead of
/// oscillating.
pub fn splice_formatted(
    original: &str,
    formatted: &str,
    original_items: &[TopItem],
    formatted_items: &[TopItem],
) -> String {
    if original_items.is_empty() || original_items.len() != formatted_items.len() {
        return original.to_string();
    }
    let mut out = String::new();
    let mut original_cursor = 0u64;
    for (before, after) in original_items.iter().zip(formatted_items.iter()) {
        if before.kind != after.kind {
            return original.to_string();
        }
        let pre = original
            .as_bytes()
            .get(original_cursor as usize..before.start as usize);
        let body = if before.is_test || after.is_test {
            original
                .as_bytes()
                .get(before.start as usize..before.end as usize)
        } else {
            formatted
                .as_bytes()
                .get(after.start as usize..after.end as usize)
        };
        match (pre, body) {
            (Some(pre), Some(body)) => {
                out.push_str(std::str::from_utf8(pre).unwrap_or(""));
                out.push_str(std::str::from_utf8(body).unwrap_or(""));
            }
            _ => return original.to_string(),
        }
        original_cursor = before.end;
    }
    match original.as_bytes().get(original_cursor as usize..) {
        Some(tail) => out.push_str(std::str::from_utf8(tail).unwrap_or("")),
        None => return original.to_string(),
    }
    out
}

/// Format through rustfmt plus the test-preserving splice: the pipeline
/// fixed point the stability gate checks. `None` when rustfmt cannot
/// run (missing binary or unformattable input).
pub fn format_rust_spliced(
    text: &str,
    config: Option<&Path>,
    source_dir: Option<&Path>,
) -> Option<String> {
    rustfmt_and_splice(text, config, source_dir).map(|(spliced, _)| spliced)
}

/// Raw rustfmt output plus the spliced fixed point. The rung needs both:
/// prose pressure is the difference between what rustfmt wants and what
/// the splice keeps.
pub(crate) fn rustfmt_and_splice(
    text: &str,
    config: Option<&Path>,
    source_dir: Option<&Path>,
) -> Option<(String, String)> {
    let formatted = run_rustfmt(text, config, source_dir)?;
    let mut parser = AstParser::new();
    let original_items = parser.rust_top_items(text);
    let formatted_items = parser.rust_top_items(&formatted);
    Some((
        splice_formatted(text, &formatted, &original_items, &formatted_items),
        formatted,
    ))
}

/// P5 format-stable: the spliced pipeline is at a fixed point on the
/// patched text. Missing rustfmt fails closed: an uncertifiable patch
/// never applies.
fn check_format_stable(file: &ChangedFile, ctx: &GateCtx) -> GateOutcome {
    if ctx.language != "rust" {
        return GateOutcome::Skip("format gate is Rust-only".to_string());
    }
    match format_rust_spliced(
        &file.after,
        ctx.rustfmt_config.as_deref(),
        ctx.source_dir.as_deref(),
    ) {
        Some(stable) if stable == file.after => GateOutcome::Pass,
        Some(_) => GateOutcome::Fail("rustfmt not stable on the patch".to_string()),
        None => GateOutcome::Fail("rustfmt unavailable".to_string()),
    }
}

/// P6 diagnostics: the fingerprint set strictly shrinks with an empty
/// added set. No diagnostics in scope skips: a pure-format patch has
/// nothing to shrink.
fn check_diagnostics(ctx: &GateCtx) -> GateOutcome {
    if ctx.before_diags.is_empty() && ctx.after_diags.is_empty() {
        return GateOutcome::Skip("no diagnostics in scope".to_string());
    }
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut counts: HashMap<&str, (usize, usize)> = HashMap::new();
    for diag in ctx.before_diags {
        counts.entry(diag.as_str()).or_default().0 += 1;
    }
    for diag in ctx.after_diags {
        counts.entry(diag.as_str()).or_default().1 += 1;
    }
    for (_, (before, after)) in counts {
        if after > before {
            added += after - before;
        } else {
            removed += before - after;
        }
    }
    if added == 0 && removed > 0 {
        GateOutcome::Pass
    } else {
        GateOutcome::Fail(format!("diagnostics added {added}, removed {removed}"))
    }
}

/// P7 build: the pre-run package check and clippy verdict across the
/// cfg matrix. No runner ran means skipped, never assumed clean.
fn check_build(ctx: &GateCtx) -> GateOutcome {
    match &ctx.build {
        Some(report) if report.clean => GateOutcome::Pass,
        Some(report) => GateOutcome::Fail(format!("build unclean: {}", report.output)),
        None => GateOutcome::Skip("no build verdict supplied".to_string()),
    }
}

/// P8 tests: affected packages show no new failures against the
/// recorded baseline. A regression reverts, because tests are the
/// oracle and never the fix target.
fn check_tests(ctx: &GateCtx) -> GateOutcome {
    match &ctx.tests {
        Some(report) if report.new_failures.is_empty() => GateOutcome::Pass,
        Some(report) => GateOutcome::Fail(format!(
            "new test failures: {}",
            report.new_failures.join(", ")
        )),
        None => GateOutcome::Skip("no test verdict supplied".to_string()),
    }
}

/// Changed lines between two texts, measured past the common prefix
/// and suffix. An approximation for caps, never a semantic diff.
fn changed_lines(before: &str, after: &str) -> usize {
    let before: Vec<&str> = before.lines().collect();
    let after: Vec<&str> = after.lines().collect();
    let mut prefix = 0usize;
    while prefix < before.len() && prefix < after.len() && before[prefix] == after[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < before.len() - prefix
        && suffix < after.len() - prefix
        && before[before.len() - 1 - suffix] == after[after.len() - 1 - suffix]
    {
        suffix += 1;
    }
    (before.len() - prefix - suffix) + (after.len() - prefix - suffix)
}

/// P9 size: the patched file and its changed-line count fit the caps.
/// Larger changes escalate instead of applying.
fn check_size(file: &ChangedFile, ctx: &GateCtx, parser: &mut AstParser) -> GateOutcome {
    if file.after.len() > ctx.max_bytes {
        return GateOutcome::Fail(format!("patched bytes exceed {}", ctx.max_bytes));
    }
    if changed_lines(&file.before, &file.after) > ctx.max_changed_lines {
        return GateOutcome::Fail(format!("changed lines exceed {}", ctx.max_changed_lines));
    }
    if ctx.language == "rust" {
        let mut items = |text: &str| parser.rust_top_items(text).len();
        if items(&file.after).saturating_sub(items(&file.before)) > 2 {
            return GateOutcome::Fail("new item count exceeds cap".to_string());
        }
    }
    GateOutcome::Pass
}

/// Line-multiset difference: lines added by the patch.
fn added_lines(before: &str, after: &str) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for line in before.lines() {
        *counts.entry(line).or_default() += 1;
    }
    let mut added = Vec::new();
    for line in after.lines() {
        match counts.get_mut(line) {
            Some(count) if *count > 0 => *count -= 1,
            _ => added.push(line.to_string()),
        }
    }
    added
}

/// P10 edit shape: the diff matches the declared fix class, with zero
/// tolerance in mechanical classes. Unknown classes skip instead of
/// claiming a shape they never declared.
fn check_edit_shape(file: &ChangedFile, ctx: &GateCtx) -> GateOutcome {
    match ctx.fix_class {
        "rustfmt" => {
            if crate::offload::is_whitespace_only(&file.before, &file.after) {
                GateOutcome::Pass
            } else {
                GateOutcome::Fail("rustfmt patch touches tokens".to_string())
            }
        }
        "needless_borrow" => {
            let strip = |text: &str| text.replace('&', "");
            if strip(&file.before) == strip(&file.after)
                && file.after.matches('&').count() < file.before.matches('&').count()
            {
                GateOutcome::Pass
            } else {
                GateOutcome::Fail("borrow patch touches more than `&`".to_string())
            }
        }
        "import" => {
            let added = added_lines(&file.before, &file.after);
            if added.len() == 1 && added[0].trim_start().starts_with("use ") {
                GateOutcome::Pass
            } else {
                GateOutcome::Fail("import patch is not exactly one `use`".to_string())
            }
        }
        "mut" => {
            let strip = |text: &str| text.replace("mut ", "");
            if strip(&file.before) == strip(&file.after)
                && file.after.matches("mut ").count() == file.before.matches("mut ").count() + 1
            {
                GateOutcome::Pass
            } else {
                GateOutcome::Fail("mut patch is not exactly one insertion".to_string())
            }
        }
        "removal" => {
            let added = added_lines(&file.before, &file.after);
            if added.is_empty() && file.after.len() < file.before.len() {
                GateOutcome::Pass
            } else {
                GateOutcome::Fail("removal patch adds or keeps bytes".to_string())
            }
        }
        _ => GateOutcome::Skip("no declared shape for this fix class".to_string()),
    }
}

#[cfg(test)]
#[path = "../tests/m6_gates.rs"]
mod m6_gates_tests;