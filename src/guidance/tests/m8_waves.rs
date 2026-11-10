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

fn ctx(
    journal: PathBuf,
    wal: PathBuf,
    clippy: ClippyRunner,
    protected: &[String],
) -> WaveCtx<'_> {
    WaveCtx {
        journal_dir: journal,
        wal_dir: wal,
        protected_globs: protected,
        allow_pub_change: false,
        allow_cfg_change: false,
        max_bytes: 1_000_000,
        max_changed_lines: 500,
        max_waves: 5,
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
        [(p1.clone(), vec![diag("a.rs"), diag("b.rs")]), (p2.clone(), Vec::new())]
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
        &ctx(journal, wal, stub_runner(Arc::clone(&calls), diags, false), &[]),
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
        &ctx(journal, wal, stub_runner(Arc::clone(&calls), diags, true), &[]),
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
