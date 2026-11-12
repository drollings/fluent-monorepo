//! Wave-loop pins: per-package invocation counts, ladder convergence,
//! and escalation fingerprints.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::clippy::{ClippyDiag, ClippyReport};
use crate::fix::Suggestion;
use crate::waves::{run_fix_waves, ClippyRunner, WaveCtx};

const NEEDLESS: &str = "fn a() {\n    let x = &5;\n    println!(\"{x}\");\n}\n";

fn package_with(dir: &Path, name: &str, files: &[&str]) -> PathBuf {
    let root = dir.join(name);
    std::fs::create_dir_all(&root).expect("mkdir");
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");
    for file in files {
        std::fs::write(root.join(file), NEEDLESS).expect("write");
    }
    root
}

/// Persistent diagnostics for one package root: the diagnostic never
/// clears, so the loop must hold or escalate it.
fn code_diags(root: &Path, code: &str) -> Arc<Mutex<HashMap<PathBuf, Vec<ClippyDiag>>>> {
    Arc::new(Mutex::new(
        [(
            root.to_path_buf(),
            vec![ClippyDiag {
                file: Some("a.rs".to_string()),
                code: Some(code.to_string()),
                message: "this diagnostic never clears".to_string(),
                level: "warning".to_string(),
                suggestions: vec![Suggestion {
                    start: 11,
                    end: 12,
                    replacement: String::new(),
                }],
            }],
        )]
        .into_iter()
        .collect(),
    ))
}

/// Persistent needless-borrow diagnostics for one package root.
fn needless_diags(root: &Path) -> Arc<Mutex<HashMap<PathBuf, Vec<ClippyDiag>>>> {
    code_diags(root, "needless_borrow")
}

fn stub_runner(
    calls: Arc<AtomicUsize>,
    diags: Arc<Mutex<HashMap<PathBuf, Vec<ClippyDiag>>>>,
    persist: bool,
) -> ClippyRunner {
    // Take-once models a wave that fixes everything it flags; persistent
    // models a diagnostic that never clears.
    Arc::new(move |package: &Path| {
        calls.fetch_add(1, Ordering::SeqCst);
        let mut report = ClippyReport::default();
        let entries = diags
            .lock()
            .ok()
            .and_then(|mut guard| {
                if persist {
                    guard.get(package).cloned()
                } else {
                    guard.remove(package)
                }
            })
            .unwrap_or_default();
        report.diagnostics = entries;
        Ok(report)
    })
}

fn diag(file: &str) -> ClippyDiag {
    ClippyDiag {
        file: Some(file.to_string()),
        code: Some("needless_borrow".to_string()),
        message: "needless borrow".to_string(),
        level: "warning".to_string(),
        suggestions: vec![Suggestion {
            start: 21,
            end: 22,
            replacement: String::new(),
        }],
    }
}

fn ctx(journal: PathBuf, wal: PathBuf, clippy: ClippyRunner, protected: &[String]) -> WaveCtx<'_> {
    let mechanical: Vec<String> = vec!["rustfmt".to_string(), "clippy-fix".to_string()];
    let approval: Vec<String> = Vec::new();
    // Test-scoped policy: both deterministic rungs allowed, nothing
    // held. Decision endpoint stays disabled (static-table-only), so
    // no test dials a model. Leaked on purpose: the loop borrows the
    // slices for the whole run.
    let mechanical: &'static [String] = Box::leak(mechanical.into_boxed_slice());
    let approval: &'static [String] = Box::leak(approval.into_boxed_slice());
    WaveCtx {
        journal_dir: journal,
        wal_dir: wal,
        protected_globs: protected,
        allow_pub_change: false,
        allow_cfg_change: false,
        max_bytes: 1_000_000,
        max_changed_lines: 500,
        max_waves: 5,
        max_attempts: 3,
        decision: crate::waves::DecisionEndpoint::default(),
        calibration: fluent_llm::decision::CalibrationRegistry::empty(),
        mechanical_classes: mechanical,
        requires_approval: approval,
        clippy,
    }
}

#[test]
fn one_clippy_per_package_per_wave() {
    let dir = fluent_wvr_testutil::tempdir();
    let p1 = package_with(dir.path(), "p1", &["a.rs", "b.rs"]);
    let p2 = package_with(dir.path(), "p2", &["c.rs", "d.rs"]);
    let files: Vec<PathBuf> = ["a.rs", "b.rs"]
        .iter()
        .map(|f| p1.join(f))
        .chain(["c.rs", "d.rs"].iter().map(|f| p2.join(f)))
        .collect();
    let calls = Arc::new(AtomicUsize::new(0));
    let diags: Arc<Mutex<HashMap<PathBuf, Vec<ClippyDiag>>>> = Arc::new(Mutex::new(
        [
            (p1.clone(), vec![diag("a.rs"), diag("b.rs")]),
            (p2.clone(), Vec::new()),
        ]
        .into_iter()
        .collect(),
    ));
    // First wave fixes p1 (p2 reports clean and converges); the second
    // wave diagnoses only the still-dirty package as its witness, then
    // the loop finds nothing dirty. Two packages, two diagnose waves
    // but only three invocations — never per file.
    let journal = dir.path().join("journal");
    let wal = dir.path().join("wal");
    let report = run_fix_waves(
        &files,
        &ctx(
            journal,
            wal,
            stub_runner(Arc::clone(&calls), diags, false),
            &[],
        ),
    );
    assert_eq!(report.fixed, 2, "{report:?}");
    assert_eq!(report.clippy_invocations, 3, "{report:?}");
    assert!(report.stopped.is_none(), "{report:?}");
    assert!(report.escalated.is_empty(), "{report:?}");
}

#[test]
fn ladder_converges_and_fingerprints_escalations() {
    let dir = fluent_wvr_testutil::tempdir();
    let root = package_with(dir.path(), "pkg", &["a.rs"]);
    let files = vec![root.join("a.rs")];
    let calls = Arc::new(AtomicUsize::new(0));
    // Unshaped diagnostic forever: the ladder exhausts attempts and
    // escalates with a stable fingerprint instead of spinning.
    let diags: Arc<Mutex<HashMap<PathBuf, Vec<ClippyDiag>>>> = Arc::new(Mutex::new(
        [(
            root.clone(),
            vec![ClippyDiag {
                file: Some("a.rs".to_string()),
                code: Some("mystery_lint".to_string()),
                message: "mystery".to_string(),
                level: "warning".to_string(),
                suggestions: Vec::new(),
            }],
        )]
        .into_iter()
        .collect(),
    ));
    let journal = dir.path().join("journal");
    let wal = dir.path().join("wal");
    let report = run_fix_waves(
        &files,
        &ctx(
            journal,
            wal,
            stub_runner(Arc::clone(&calls), diags, true),
            &[],
        ),
    );
    assert_eq!(report.escalated.len(), 1, "{report:?}");
    assert_eq!(report.fixed, 0, "{report:?}");
    let fresh: Arc<Mutex<HashMap<PathBuf, Vec<ClippyDiag>>>> = Arc::new(Mutex::new(
        [(
            root.clone(),
            vec![ClippyDiag {
                file: Some("a.rs".to_string()),
                code: Some("mystery_lint".to_string()),
                message: "mystery".to_string(),
                level: "warning".to_string(),
                suggestions: Vec::new(),
            }],
        )]
        .into_iter()
        .collect(),
    ));
    let again = run_fix_waves(
        &files,
        &ctx(
            dir.path().join("j2"),
            dir.path().join("w2"),
            stub_runner(Arc::new(AtomicUsize::new(0)), fresh, true),
            &[],
        ),
    );
    assert_eq!(
        again.escalated[0].fingerprint, report.escalated[0].fingerprint,
        "fingerprints are stable for requeue"
    );
}

#[test]
fn systemic_fingerprint_halts_with_cause_named() {
    let dir = fluent_wvr_testutil::tempdir();
    let root = package_with(dir.path(), "pkg", &["a.rs", "b.rs", "c.rs"]);
    let files: Vec<PathBuf> = ["a.rs", "b.rs", "c.rs"]
        .iter()
        .map(|f| root.join(f))
        .collect();
    // One unshaped diagnostic persistent on every file: no recipe ever
    // converges it, so every file terminally fails with the same cause.
    let diags: Arc<Mutex<HashMap<PathBuf, Vec<ClippyDiag>>>> = Arc::new(Mutex::new(
        [(
            root.clone(),
            ["a.rs", "b.rs", "c.rs"]
                .iter()
                .map(|file| ClippyDiag {
                    file: Some(file.to_string()),
                    code: Some("mystery_lint".to_string()),
                    message: "mystery".to_string(),
                    level: "warning".to_string(),
                    suggestions: Vec::new(),
                })
                .collect(),
        )]
        .into_iter()
        .collect(),
    ));
    let report = run_fix_waves(
        &files,
        &ctx(
            dir.path().join("journal"),
            dir.path().join("wal"),
            stub_runner(Arc::new(AtomicUsize::new(0)), diags, true),
            &[],
        ),
    );
    assert_eq!(report.escalated.len(), 3, "{report:?}");
    let stopped = report.stopped.expect("systemic run must halt named");
    assert!(
        stopped.starts_with("systemic fingerprint"),
        "halt names the cause: {stopped}"
    );
    assert!(stopped.contains("mystery_lint"), "{stopped}");
}

#[test]
fn exit_code_contract_prioritizes_halt_over_stuck() {
    use crate::seal::ExitCode;
    use crate::waves::{Escalation, WaveReport};
    let clean = WaveReport {
        waves_run: 2,
        witness_clean: true,
        ..WaveReport::default()
    };
    assert_eq!(clean.exit_code(), ExitCode::Done);
    let stuck = WaveReport {
        escalated: vec![Escalation {
            file: "a.rs".to_string(),
            fingerprint: "f".to_string(),
            reason: "attempts exhausted".to_string(),
        }],
        witness_clean: false,
        ..WaveReport::default()
    };
    assert_eq!(stuck.exit_code(), ExitCode::Stuck);
    let halted = WaveReport {
        stopped: Some("systemic fingerprint x".to_string()),
        escalated: vec![Escalation {
            file: "a.rs".to_string(),
            fingerprint: "f".to_string(),
            reason: "x".to_string(),
        }],
        ..WaveReport::default()
    };
    assert_eq!(halted.exit_code(), ExitCode::Halted);
    let remaining = WaveReport {
        stopped: Some("max waves reached".to_string()),
        ..WaveReport::default()
    };
    assert_eq!(remaining.exit_code(), ExitCode::WorkRemaining);
    let no_witness = WaveReport {
        waves_run: 1,
        ..WaveReport::default()
    };
    assert_eq!(no_witness.exit_code(), ExitCode::WorkRemaining);
}

#[test]
fn policy_gates_rungs_and_holds_without_burning() {
    use crate::waves::WaveReport;
    // D1 needs "rustfmt": without it the dirty file is never touched.
    let dir = fluent_wvr_testutil::tempdir();
    let file = dir.path().join("messy.rs");
    std::fs::write(&file, "fn messy(x:u32)->u32{x+1}\n").expect("write");
    let mechanical: Vec<String> = vec![];
    let approval: Vec<String> = vec![];
    let report = run_fix_waves(
        std::slice::from_ref(&file),
        &WaveCtx {
            journal_dir: dir.path().join("journal"),
            wal_dir: dir.path().join("wal"),
            protected_globs: &[],
            allow_pub_change: false,
            allow_cfg_change: false,
            max_bytes: 1_000_000,
            max_changed_lines: 500,
            max_waves: 2,
            max_attempts: 3,
            decision: crate::waves::DecisionEndpoint::default(),
            calibration: fluent_llm::decision::CalibrationRegistry::empty(),
            mechanical_classes: &mechanical,
            requires_approval: &approval,
            clippy: stub_runner(
                Arc::new(AtomicUsize::new(0)),
                Arc::new(Mutex::new(HashMap::new())),
                false,
            ),
        },
    );
    assert_eq!(report.fixed, 0, "D1 gated off by policy: {report:?}");
    assert_eq!(
        std::fs::read_to_string(&file).expect("read"),
        "fn messy(x:u32)->u32{x+1}\n"
    );
    // D2 needs "clippy-fix": a needless-borrow diagnostic with the tool
    // class disallowed is held immediately, never applied.
    let dir = fluent_wvr_testutil::tempdir();
    let root = package_with(dir.path(), "p", &["a.rs"]);
    let target = root.join("a.rs");
    let diags = needless_diags(&root);
    let report = run_fix_waves(
        std::slice::from_ref(&target),
        &WaveCtx {
            journal_dir: dir.path().join("journal"),
            wal_dir: dir.path().join("wal"),
            protected_globs: &[],
            allow_pub_change: false,
            allow_cfg_change: false,
            max_bytes: 1_000_000,
            max_changed_lines: 500,
            max_waves: 3,
            max_attempts: 3,
            decision: crate::waves::DecisionEndpoint::default(),
            calibration: fluent_llm::decision::CalibrationRegistry::empty(),
            mechanical_classes: &mechanical,
            requires_approval: &approval,
            clippy: stub_runner(Arc::new(AtomicUsize::new(0)), Arc::clone(&diags), true),
        },
    );
    assert_eq!(report.fixed, 0, "D2 gated off by policy: {report:?}");
    assert!(
        report
            .escalated
            .iter()
            .any(|esc| esc.file == target.to_string_lossy()),
        "unfixable-under-policy escalates: {report:?}"
    );
    // requires_approval holds even when the tool class is allowed: the
    // class is named in the reason and nothing is applied.
    let dir = fluent_wvr_testutil::tempdir();
    let root = package_with(dir.path(), "p", &["a.rs"]);
    let target = root.join("a.rs");
    let mechanical: Vec<String> = vec!["rustfmt".to_string(), "clippy-fix".to_string()];
    let approval: Vec<String> = vec!["needless_borrow".to_string()];
    let diags = needless_diags(&root);
    let report = run_fix_waves(
        std::slice::from_ref(&target),
        &WaveCtx {
            journal_dir: dir.path().join("journal"),
            wal_dir: dir.path().join("wal"),
            protected_globs: &[],
            allow_pub_change: false,
            allow_cfg_change: false,
            max_bytes: 1_000_000,
            max_changed_lines: 500,
            max_waves: 3,
            max_attempts: 3,
            decision: crate::waves::DecisionEndpoint::default(),
            calibration: fluent_llm::decision::CalibrationRegistry::empty(),
            mechanical_classes: &mechanical,
            requires_approval: &approval,
            clippy: stub_runner(Arc::new(AtomicUsize::new(0)), Arc::clone(&diags), true),
        },
    );
    assert_eq!(report.fixed, 0, "held classes never apply: {report:?}");
    let esc = report
        .escalated
        .iter()
        .find(|esc| esc.file == target.to_string_lossy())
        .expect("held item escalates");
    assert!(
        esc.reason.contains("approval"),
        "names the hold: {}",
        esc.reason
    );
    let _ = WaveReport::default();
}

#[test]
fn triage_rung_names_unavailable_without_burning() {
    use crate::triage::{TRIAGE_POINT, TRIAGE_PROMPT_VERSION};
    use crate::waves::DecisionEndpoint;
    use fluent_llm::decision::{
        CalibrationRegistry, CorpusProvenance, GateReport, RegistryEntry, ThresholdSet,
    };
    // Calibrated point, unreachable endpoint (port 1 refuses instantly):
    // the residual holds as an owned `model: unavailable` wait, never
    // an attempt burn or a silent hold.
    let dir = fluent_wvr_testutil::tempdir();
    let root = package_with(dir.path(), "p", &["a.rs"]);
    let target = root.join("a.rs");
    let mut registry = CalibrationRegistry::empty();
    registry.upsert(RegistryEntry {
        point: TRIAGE_POINT.to_string(),
        model_fingerprint: "test-model".to_string(),
        question_version: TRIAGE_PROMPT_VERSION,
        thresholds: ThresholdSet {
            act: 1.0,
            review: 1.0,
        },
        gate: GateReport {
            threshold: 1.0,
            precision: 1.0,
            recall: 1.0,
            fpr: 0.0,
            support: 2,
            control_fires: 0,
            passed: true,
            date: "2026-11-10".to_string(),
        },
        provenance: CorpusProvenance {
            source: "test".to_string(),
            labeller: "test".to_string(),
            date: "2026-11-10".to_string(),
            dataset: "abc123".to_string(),
        },
        dataset: "abc123".to_string(),
    });
    let report = run_fix_waves(
        std::slice::from_ref(&target),
        &WaveCtx {
            journal_dir: dir.path().join("journal"),
            wal_dir: dir.path().join("wal"),
            protected_globs: &[],
            allow_pub_change: false,
            allow_cfg_change: false,
            max_bytes: 1_000_000,
            max_changed_lines: 500,
            max_waves: 1,
            max_attempts: 3,
            decision: DecisionEndpoint {
                base_url: "http://127.0.0.1:1".to_string(),
                model: "test-model".to_string(),
                timeout_ms: 2_000,
                enabled: true,
            },
            calibration: registry,
            mechanical_classes: &["rustfmt".to_string(), "clippy-fix".to_string()],
            requires_approval: &[],
            clippy: stub_runner(
                Arc::new(AtomicUsize::new(0)),
                code_diags(&root, "mystery_lint"),
                true,
            ),
        },
    );
    assert_eq!(report.fixed, 0, "nothing applies: {report:?}");
    let esc = report
        .escalated
        .iter()
        .find(|esc| esc.file == target.to_string_lossy())
        .expect("unavailable holds the item");
    assert!(
        esc.reason.contains("model: unavailable"),
        "names the wait: {}",
        esc.reason
    );
    assert!(
        report.stopped.is_none(),
        "a wait never trips the breaker: {report:?}"
    );
}

#[test]
fn triage_rung_holds_without_model_when_disabled() {
    use crate::waves::DecisionEndpoint;
    // Static table decides nothing for the unknown code and no model
    // rung exists: the item holds for review, classified, unburned.
    let dir = fluent_wvr_testutil::tempdir();
    let root = package_with(dir.path(), "p", &["a.rs"]);
    let target = root.join("a.rs");
    let report = run_fix_waves(
        std::slice::from_ref(&target),
        &WaveCtx {
            journal_dir: dir.path().join("journal"),
            wal_dir: dir.path().join("wal"),
            protected_globs: &[],
            allow_pub_change: false,
            allow_cfg_change: false,
            max_bytes: 1_000_000,
            max_changed_lines: 500,
            max_waves: 1,
            max_attempts: 3,
            decision: DecisionEndpoint::default(),
            calibration: fluent_llm::decision::CalibrationRegistry::empty(),
            mechanical_classes: &["rustfmt".to_string(), "clippy-fix".to_string()],
            requires_approval: &[],
            clippy: stub_runner(
                Arc::new(AtomicUsize::new(0)),
                code_diags(&root, "mystery_lint"),
                true,
            ),
        },
    );
    let esc = report
        .escalated
        .iter()
        .find(|esc| esc.file == target.to_string_lossy())
        .expect("static-only holds the item");
    assert!(
        esc.reason.contains("needs human judgment"),
        "classified hold: {}",
        esc.reason
    );
}

#[test]
fn model_draft_attempts_pinned_at_three() {
    // The local code-model rung gets a bounded budget: mechanical rungs
    // run under the policy limit, model drafts under their own cap.
    assert_eq!(crate::waves::MODEL_DRAFT_ATTEMPTS, 3);
}

#[test]
fn disabled_endpoint_skips_draft_rung_without_network() {
    // A disabled generative role never dials: every file skips, nothing
    // is kept, no call is attempted.
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("a.rs");
    std::fs::write(&target, "fn a() -> u32 {\n    1\n}\n").expect("write");
    let endpoint = crate::waves::GenerativeEndpoint {
        base_url: "http://127.0.0.1:1".to_string(),
        model: "code".to_string(),
        timeout_ms: 1000,
        enabled: false,
    };
    let report = crate::waves::run_model_draft_rung(
        &[(target, "attempts exhausted".to_string())],
        &endpoint,
    );
    assert_eq!(report.skipped, 1, "{report:?}");
    assert_eq!(report.kept, 0, "{report:?}");
    assert_eq!(report.attempted, 0, "{report:?}");
}

#[test]
fn draft_prompt_fences_file_and_parses_response_fence() {
    // The model sees the whole file fenced as inert data and must answer
    // with the whole fixed file in one rust fence.
    let source = "fn a() -> u32 {\n    1\n}\n";
    let prompt = crate::waves::draft_prompt(source, "clippy::needless_borrow");
    assert!(prompt.contains("```rust"), "{prompt}");
    assert!(prompt.contains(source), "{prompt}");
    assert!(prompt.contains("inert"), "{prompt}");
    let response = "fixed:\n```rust\nfn a() -> u32 {\n    1\n}\n```\n";
    assert_eq!(
        crate::waves::parse_draft_fence(response).as_deref(),
        Some("fn a() -> u32 {\n    1\n}")
    );
    assert!(crate::waves::parse_draft_fence("no fence here").is_none());
}
