//! Deterministic offload applier: the no-model tier of the maintenance loop.
//!
//! Stage a file copy in a temp dir, run the mechanical tool, require a
//! mechanical-class diff, write back, verify with the caller-supplied narrow
//! gate, restore byte-identically on red. Bounded attempts; the loop shape is
//! reused by the draft tier. The gate certifies (task value); the tool never
//! does.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use common_core::error::IoError;
use fluent_llm::decision::{decide_verdict, Decision, QuestionSpec, ThresholdSet, Verdict};
use thiserror::Error;

/// Default retry bound for one file (deterministic tools converge on the
/// first attempt; the bound matters once drafts reuse this loop).
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// Mechanical tools runnable on a staged file copy. `clippy --fix` needs
/// crate context a file copy cannot provide, so it is not offered here —
/// only tools that run hermetically on one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyTool {
    /// `rustfmt <file>`: formatting only, standalone-capable.
    Rustfmt,
}

impl ApplyTool {
    /// Stable tool name for reports.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Rustfmt => "rustfmt",
        }
    }
}

/// What happened to the source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApplyVerdict {
    /// Gate green: the write-back stands.
    Kept,
    /// Gate red every attempt: original bytes restored.
    Reverted,
    /// The tool reported no change: source untouched.
    Unchanged,
}

impl ApplyVerdict {
    /// Stable lowercase spelling for reports and rendering.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Kept => "kept",
            Self::Reverted => "reverted",
            Self::Unchanged => "unchanged",
        }
    }
}

/// Reviewable outcome of one file: file, tool, rewrite hashes, gate output.
#[derive(Debug, Clone)]
pub struct ApplyReport {
    /// Source file, as passed in.
    pub file: String,
    /// Tool name (`ApplyTool::name`).
    pub tool: &'static str,
    /// Whether the tool changed any bytes.
    pub changed: bool,
    /// Final disposition.
    pub verdict: ApplyVerdict,
    /// Apply attempts performed (1 when converged immediately).
    pub attempts_used: u32,
    /// Captured gate stdout/stderr of the deciding run.
    pub gate_output: String,
    /// sha256 of the source before the first attempt.
    pub before_hash: String,
    /// sha256 of the source as left (`before_hash` unless `Kept`).
    pub after_hash: String,
}

/// Applier failure. A red gate is not an error — it is `Reverted`.
#[derive(Debug, Error)]
pub enum OffloadError {
    /// Filesystem failure.
    #[error("IO error: {0}")]
    Io(#[from] IoError),
    /// The tool itself failed (missing binary, unparseable input).
    #[error("tool failed: {0}")]
    Tool(String),
    /// No verify command supplied.
    #[error("no verify command supplied")]
    EmptyCommand,
    /// Policy refused the draft before anything was staged.
    #[error("policy refused: {0}")]
    Policy(String),
}

common_core::impl_from_io_error!(OffloadError);

/// Whether `after` differs from `before` in whitespace only: the
/// mechanical-class check for formatting rewrites. Token changes fail.
#[must_use]
pub fn is_whitespace_only(before: &str, after: &str) -> bool {
    if before == after {
        return true;
    }
    let strip = |text: &str| {
        text.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    strip(before) == strip(after)
}

/// Whether `path` is a test target drafts must never touch: a `tests` or
/// `testing` directory, or a `test_`/`_test`/`_tests` file stem. Guards by
/// shape, never by content — a draft touching tests is refused outright.
#[must_use]
pub fn is_test_path(path: &Path) -> bool {
    if path
        .components()
        .any(|component| matches!(component.as_os_str().to_str(), Some("tests" | "testing")))
    {
        return true;
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    stem.starts_with("test_") || stem.ends_with("_test") || stem.ends_with("_tests")
}

/// Run the verify command, capturing combined output. Green is exit zero;
/// spawn failure is a tool error (the gate never ran).
fn capture_gate(argv: &[&str]) -> Result<(bool, String), OffloadError> {
    let output = common_core::shell::run_capture(argv)
        .map_err(|e| OffloadError::Tool(format!("gate failed to spawn: {e}")))?;
    Ok((
        output.success,
        format!("{}{}", output.stdout, output.stderr),
    ))
}
/// package gates split on whitespace. The command runs in the caller's
/// working directory (workspace root in production).
#[must_use]
/// Map an M2 gate string to argv: make targets run through `make`,
/// package gates split on whitespace. The command runs in the caller's
/// working directory (workspace root in production).
pub fn gate_argv(gate: &str) -> Vec<String> {
    if gate.ends_with("-smoke") || gate == "make smoke" || gate == "make test" {
        let target = gate.strip_prefix("make ").unwrap_or(gate);
        return vec!["make".to_string(), target.to_string()];
    }
    gate.split_whitespace().map(str::to_string).collect()
}

static COPY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Staged isolated copy: the original bytes plus a scratch file the tool
/// mutates. Dropping removes the scratch dir best-effort; the source is
/// only touched by explicit write-back or restore.
struct StagedCopy {
    work_dir: PathBuf,
    copy_path: PathBuf,
}

impl StagedCopy {
    fn stage(source: &Path, original: &[u8]) -> Result<Self, OffloadError> {
        let id = COPY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let work_dir =
            std::env::temp_dir().join(format!("guidance-offload-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&work_dir).map_err(common_core::error::IoError::from)?;
        let name = source.file_name().map_or("file".to_string(), |name| {
            name.to_string_lossy().into_owned()
        });
        let copy_path = work_dir.join(name);
        common_core::io::write_atomic(&copy_path, original)?;
        Ok(Self {
            work_dir,
            copy_path,
        })
    }

    fn reset(&self, original: &[u8]) -> Result<(), OffloadError> {
        common_core::io::write_atomic(&self.copy_path, original)?;
        Ok(())
    }
}

impl Drop for StagedCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}

/// Run `tool` on the staged copy; `Ok(true)` when bytes changed.
fn apply_tool(copy_path: &Path, tool: ApplyTool) -> Result<bool, OffloadError> {
    match tool {
        ApplyTool::Rustfmt => {
            let before = common_core::io::read_to_string_err(copy_path)
                .map_err(|e| OffloadError::Tool(e.to_string()))?;
            let argv = [
                "rustfmt".to_string(),
                copy_path.to_string_lossy().into_owned(),
            ];
            let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
            let output = common_core::shell::run_capture(&argv_ref)
                .map_err(|e| OffloadError::Tool(e.to_string()))?;
            if !output.success {
                return Err(OffloadError::Tool(format!(
                    "rustfmt failed: {}",
                    output.stderr.trim()
                )));
            }
            let after = common_core::io::read_to_string_err(copy_path)
                .map_err(|e| OffloadError::Tool(e.to_string()))?;
            if after == before {
                return Ok(false);
            }
            if !is_whitespace_only(&before, &after) {
                return Err(OffloadError::Tool(
                    "rustfmt changed non-whitespace bytes".to_string(),
                ));
            }
            Ok(true)
        }
    }
}

/// Stage, apply, verify, keep-or-restore one file. Green writes back via
/// `write_atomic`; red restores the exact original bytes. No model calls.
pub fn apply_verified(
    source: &Path,
    tool: ApplyTool,
    verify_command: &[String],
    max_attempts: u32,
) -> Result<ApplyReport, OffloadError> {
    if verify_command.is_empty() {
        return Err(OffloadError::EmptyCommand);
    }
    let original = common_core::io::read_to_string_err(source)?;
    let before_hash = common_core::hash::sha256_hex(original.as_bytes());
    let staged = StagedCopy::stage(source, original.as_bytes())?;
    let attempts = max_attempts.max(1);
    let argv: Vec<&str> = verify_command.iter().map(String::as_str).collect();
    let mut gate_output = String::new();
    for attempt in 1..=attempts {
        staged.reset(original.as_bytes())?;
        if !apply_tool(&staged.copy_path, tool)? {
            return Ok(ApplyReport {
                file: source.to_string_lossy().into_owned(),
                tool: tool.name(),
                changed: false,
                verdict: ApplyVerdict::Unchanged,
                attempts_used: attempt,
                gate_output,
                before_hash: before_hash.clone(),
                after_hash: before_hash.clone(),
            });
        }
        let patched = common_core::io::read_to_string_err(&staged.copy_path)?;
        common_core::io::write_atomic(source, patched.as_bytes())?;
        let (green, output) = capture_gate(&argv)?;
        gate_output = output;
        if green {
            return Ok(ApplyReport {
                file: source.to_string_lossy().into_owned(),
                tool: tool.name(),
                changed: true,
                verdict: ApplyVerdict::Kept,
                attempts_used: attempt,
                gate_output,
                before_hash: before_hash.clone(),
                after_hash: common_core::hash::sha256_hex(patched.as_bytes()),
            });
        }
        common_core::io::write_atomic(source, original.as_bytes())?;
    }
    Ok(ApplyReport {
        file: source.to_string_lossy().into_owned(),
        tool: tool.name(),
        changed: true,
        verdict: ApplyVerdict::Reverted,
        attempts_used: attempts,
        gate_output,
        before_hash: before_hash.clone(),
        after_hash: before_hash,
    })
}

/// Apply one generative draft under policy: the triage class must
/// auto-apply, the target must not be a test path, and the narrow gate
/// must go green — otherwise the original bytes stand. No test edits,
/// no commits: this function cannot invoke either. Retries re-apply the
/// same content (deterministic); fresh drafts arrive via fresh calls.
pub fn apply_draft(
    source: &Path,
    class: TriageClass,
    draft: &str,
    verify_command: &[String],
    max_attempts: u32,
) -> Result<ApplyReport, OffloadError> {
    if !matches!(class, TriageClass::Formatting | TriageClass::ClippyFix) {
        return Err(OffloadError::Policy(
            "triage abstained: drafts apply for auto-apply classes only".to_string(),
        ));
    }
    if is_test_path(source) {
        return Err(OffloadError::Policy(format!(
            "test target refused: {}",
            source.to_string_lossy()
        )));
    }
    if verify_command.is_empty() {
        return Err(OffloadError::EmptyCommand);
    }
    let original = common_core::io::read_to_string_err(source)?;
    if original == draft {
        let hash = common_core::hash::sha256_hex(original.as_bytes());
        return Ok(ApplyReport {
            file: source.to_string_lossy().into_owned(),
            tool: "draft",
            changed: false,
            verdict: ApplyVerdict::Unchanged,
            attempts_used: 1,
            gate_output: String::new(),
            before_hash: hash.clone(),
            after_hash: hash,
        });
    }
    let before_hash = common_core::hash::sha256_hex(original.as_bytes());
    let staged = StagedCopy::stage(source, original.as_bytes())?;
    let attempts = max_attempts.max(1);
    let argv: Vec<&str> = verify_command.iter().map(String::as_str).collect();
    let mut gate_output = String::new();
    for attempt in 1..=attempts {
        staged.reset(draft.as_bytes())?;
        common_core::io::write_atomic(source, draft.as_bytes())?;
        let (green, output) = capture_gate(&argv)?;
        gate_output = output;
        if green {
            return Ok(ApplyReport {
                file: source.to_string_lossy().into_owned(),
                tool: "draft",
                changed: true,
                verdict: ApplyVerdict::Kept,
                attempts_used: attempt,
                gate_output,
                before_hash: before_hash.clone(),
                after_hash: common_core::hash::sha256_hex(draft.as_bytes()),
            });
        }
        common_core::io::write_atomic(source, original.as_bytes())?;
    }
    Ok(ApplyReport {
        file: source.to_string_lossy().into_owned(),
        tool: "draft",
        changed: true,
        verdict: ApplyVerdict::Reverted,
        attempts_used: attempts,
        gate_output,
        before_hash: before_hash.clone(),
        after_hash: before_hash,
    })
}

#[cfg(test)]
#[path = "../tests/offload_apply.rs"]
mod offload_apply_tests;

#[cfg(test)]
#[path = "../tests/offload_triage.rs"]
mod offload_triage_tests;

/// Cap for gate text carried in packets: sized for an orchestrator,
/// never the full log (the log path travels separately in CI output).
pub const GATE_EXCERPT_LIMIT: usize = 2000;

/// One reviewed file: what lint ran, what it rewrote, how the gate voted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReviewEntry {
    /// Source file.
    pub file: String,
    /// Tool/lint name.
    pub tool: String,
    /// What changed, in tool terms.
    pub rewrite: String,
    /// Final disposition.
    pub verdict: ApplyVerdict,
    /// sha256 before the first attempt.
    pub before_hash: String,
    /// sha256 as left.
    pub after_hash: String,
    /// Captured gate output of the deciding run.
    pub gate_output: String,
}

/// Reviewable summary of applied changes: one entry per file.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ReviewSummary {
    /// Per-file entries, in report order.
    pub entries: Vec<ReviewEntry>,
}

impl ReviewSummary {
    /// Empty summary.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one file's report.
    pub fn push_report(&mut self, report: &ApplyReport) {
        self.entries.push(ReviewEntry {
            file: report.file.clone(),
            tool: report.tool.to_string(),
            rewrite: describe_rewrite(report.tool),
            verdict: report.verdict,
            before_hash: report.before_hash.clone(),
            after_hash: report.after_hash.clone(),
            gate_output: report.gate_output.clone(),
        });
    }

    /// Compact human-readable rendering, one block per entry.
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let kept = self
            .entries
            .iter()
            .filter(|entry| entry.verdict == ApplyVerdict::Kept)
            .count();
        let reverted = self
            .entries
            .iter()
            .filter(|entry| entry.verdict == ApplyVerdict::Reverted)
            .count();
        let mut text = format!("offload review: {kept} kept, {reverted} reverted\n");
        for entry in &self.entries {
            let _ = writeln!(
                text,
                "- {} [{}] {} ({})\n  gate: {}",
                entry.file,
                entry.tool,
                entry.verdict.as_str(),
                entry.rewrite,
                first_line(&entry.gate_output),
            );
        }
        text
    }
}

/// Escalation packet: everything needed to resume without re-running —
/// inputs, attempts, gate excerpt, hashes, and the next step.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EscalationPacket {
    /// Why a human or orchestrator must look.
    pub reason: String,
    /// Target file.
    pub file: String,
    /// Attempted tool.
    pub tool: String,
    /// Triage class (or `unknown`).
    pub class: String,
    /// Apply attempts performed.
    pub attempts_used: u32,
    /// Capped gate output.
    pub gate_excerpt: String,
    /// sha256 of the source as left (equals input on refusal).
    pub before_hash: String,
    /// sha256 of the source as left.
    pub after_hash: String,
    /// What to do next.
    pub resume: String,
}

impl EscalationPacket {
    /// Packet for a reverted report: bytes already restored, the gate
    /// excerpt names the failure, resume names the narrow gate rerun.
    #[must_use]
    pub fn from_report(report: &ApplyReport, reason: &str) -> Self {
        Self {
            reason: reason.to_string(),
            file: report.file.clone(),
            tool: report.tool.to_string(),
            class: report.tool.to_string(),
            attempts_used: report.attempts_used,
            gate_excerpt: excerpt(&report.gate_output),
            before_hash: report.before_hash.clone(),
            after_hash: report.after_hash.clone(),
            resume: "fix the cause named above, then re-run the narrow gate for this file"
                .to_string(),
        }
    }

    /// Packet for a policy refusal: nothing was staged, no bytes changed.
    #[must_use]
    pub fn from_policy(file: &str, tool: &str, class: &str, reason: &str) -> Self {
        Self {
            reason: reason.to_string(),
            file: file.to_string(),
            tool: tool.to_string(),
            class: class.to_string(),
            attempts_used: 0,
            gate_excerpt: String::new(),
            before_hash: String::new(),
            after_hash: String::new(),
            resume: "manual review required; no bytes were changed".to_string(),
        }
    }

    /// Compact human-readable rendering.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "offload escalation: {}\n  file: {}\n  tool: {} class: {}\n  attempts: {}\n  gate: {}\n  resume: {}\n",
            self.reason,
            self.file,
            self.tool,
            self.class,
            self.attempts_used,
            first_line(&self.gate_excerpt),
            self.resume,
        )
    }
}

/// Rewrite description in tool terms (the tool determines the class).
fn describe_rewrite(tool: &str) -> String {
    match tool {
        "rustfmt" => "format: whitespace-only".to_string(),
        "draft" => "draft content".to_string(),
        other => format!("{other} rewrite"),
    }
}

/// First line of output, or `(empty)`; keeps renders to one line per entry.
fn first_line(output: &str) -> String {
    output
        .lines()
        .next()
        .map_or("(empty)".to_string(), |line| line.trim_end().to_string())
}

/// Gate output capped for packet size.
fn excerpt(output: &str) -> String {
    if output.len() <= GATE_EXCERPT_LIMIT {
        return output.to_string();
    }
    format!("{}…[truncated]", &output[..GATE_EXCERPT_LIMIT])
}

#[cfg(test)]
#[path = "../tests/offload_draft.rs"]
mod offload_draft_tests;

#[cfg(test)]
#[path = "../tests/offload_summary.rs"]
mod offload_summary_tests;

/// Triage decision point id (shadow-only until a registry entry
/// authorizes it; no production caller runs the ladder yet).
pub const TRIAGE_POINT: &str = "triage";
/// Triage question version (bumped with the options, never silently).
pub const TRIAGE_VERSION: u64 = 1;

/// Triage Choice over the mechanical classes: the allowlist plus the
/// explicit abstain label. Caches stay cold until a calibration entry
/// authorizes the point.
#[must_use]
pub fn triage_spec() -> QuestionSpec {
    QuestionSpec::new()
        .id(TRIAGE_POINT.to_string())
        .version(TRIAGE_VERSION)
        .kind("choice".to_string())
        .prompt_template(
            "Name the mechanical class for this finding, or manual when it is not machine-applicable."
                .to_string(),
        )
        .options_csv("rustfmt,clippy-fix,manual".to_string())
        .act_threshold(0.9)
        .review_threshold(0.7)
        .timeout_ms(5000)
        .cacheable(false)
        .build()
}

/// Where a finding routes. Only allowlist classes auto-apply, and only
/// at Act confidence with the producing tool agreeing; everything else
/// is `Manual` (propose textually, never apply). The verdict is the
/// shared `decide_verdict` algebra — no second threshold logic — and the
/// output is a ranking signal only: the narrow gate certifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriageClass {
    /// `rustfmt` finding at Act: formatting applies.
    Formatting,
    /// `clippy` finding at Act: machine-applicable fix applies.
    ClippyFix,
    /// Abstain: propose textually, never auto-apply.
    Manual,
}

/// Route one finding. The tool that produced the finding must agree with
/// the class (a test failure labeled `clippy-fix` is still `Manual`);
/// the label must be allowlisted; the verdict must be `Act`.
#[must_use]
pub fn classify_finding(
    tool: &str,
    decision: Option<&Decision>,
    thresholds: &ThresholdSet,
) -> TriageClass {
    let Some(decision) = decision else {
        return TriageClass::Manual;
    };
    if decide_verdict(decision, thresholds) != Verdict::Act {
        return TriageClass::Manual;
    }
    match (tool, decision.label.as_str()) {
        ("rustfmt", "rustfmt") => TriageClass::Formatting,
        ("clippy", "clippy-fix") => TriageClass::ClippyFix,
        _ => TriageClass::Manual,
    }
}
