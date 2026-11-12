//! Loop safety: no overwrite of human edits, breaker stops, ledger lists
//! every applied change, test targets refused.

use std::sync::{Arc, Mutex};

use super::{
    run_to_fixpoint, run_to_fixpoint_with_breaker, serial_loop_tests, ActOutcome, BreakerConfig,
    LoopBreaker, LoopGuard, LoopReport, MaintenanceQueue, Tier,
};
use crate::offload::{
    apply_draft, apply_draft_expected, apply_verified_expected, ApplyTool, TriageClass,
};
use crate::offload::{ApplyReport, ApplyVerdict, OffloadError, DEFAULT_MAX_ATTEMPTS};
use crate::policy::MaintenancePolicy;
use fluent_concurrency::stream::StreamAbort;
use fluent_wvr_testutil::tempdir;

type Act = Arc<Mutex<dyn FnMut(&str, Tier) -> ActOutcome + Send>>;

const DRIFTED: &str = "pub fn messy( x : u32 ) -> u32 {\n    x   +   1\n}\n";

fn kept_report(file: &str) -> ApplyReport {
    ApplyReport {
        file: file.to_string(),
        tool: "rustfmt",
        changed: true,
        verdict: ApplyVerdict::Kept,
        attempts_used: 1,
        gate_output: "ok".to_string(),
        before_hash: "a".to_string(),
        after_hash: "b".to_string(),
    }
}

fn drifted_file() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempdir();
    let file = dir.path().join("drift.rs");
    std::fs::write(&file, DRIFTED).expect("write");
    (dir, file)
}

#[test]
fn stale_read_never_overwrites_human_edit() {
    let (_dir, file) = drifted_file();
    let stale = std::fs::read(&file).expect("read");
    let human = b"// human owns this file now\npub fn messy(x: u32) -> u32 {\n    x + 1\n}\n";
    std::fs::write(&file, human).expect("human edit");
    let result = apply_verified_expected(
        &file,
        ApplyTool::Rustfmt,
        &["true".to_string()],
        DEFAULT_MAX_ATTEMPTS,
        &stale,
    );
    assert!(
        matches!(result, Err(OffloadError::Changed(_))),
        "{result:?}"
    );
    assert_eq!(
        std::fs::read(&file).expect("read"),
        human,
        "human bytes stand"
    );
}

#[test]
fn revert_path_never_restores_over_human_edit() {
    let (_dir, file) = drifted_file();
    let stale = std::fs::read(&file).expect("read");
    let script = format!("echo '// human' >> '{}' && false", file.display());
    let gate = vec!["sh".to_string(), "-c".to_string(), script];
    let result = apply_verified_expected(&file, ApplyTool::Rustfmt, &gate, 1, &stale);
    assert!(
        matches!(result, Err(OffloadError::Changed(_))),
        "{result:?}"
    );
    let content = std::fs::read_to_string(&file).expect("read");
    assert!(content.contains("// human"), "human edit stands: {content}");
    assert!(
        !content.contains("x   +   1"),
        "stale bytes not restored: {content}"
    );
}

#[test]
fn green_gate_with_mid_gate_edit_is_not_certified() {
    let (_dir, file) = drifted_file();
    let script = format!("echo '// human' >> '{}'", file.display());
    let gate = vec!["sh".to_string(), "-c".to_string(), script];
    let result = apply_verified_expected(&file, ApplyTool::Rustfmt, &gate, 1, DRIFTED.as_bytes());
    assert!(
        matches!(result, Err(OffloadError::Changed(_))),
        "{result:?}"
    );
}

#[test]
fn breaker_stops_repeat_failure() {
    let mut breaker = LoopBreaker::new(BreakerConfig::strict());
    assert!(breaker.observe(false).is_none());
    assert!(breaker.observe(false).is_none());
    assert_eq!(breaker.observe(false).as_deref(), Some("repeat failure"));
}

#[test]
fn breaker_stops_revert_rate() {
    let config = BreakerConfig {
        max_consecutive_failures: usize::MAX,
        max_failure_rate: 0.25,
        rate_min_passes: 4,
        max_stagnant_passes: usize::MAX,
        max_systemic_items: usize::MAX,
    };
    let mut breaker = LoopBreaker::new(config);
    assert!(breaker.observe(true).is_none());
    assert!(breaker.observe(false).is_none());
    assert!(breaker.observe(false).is_none());
    assert_eq!(breaker.observe(false).as_deref(), Some("revert rate"));
}

#[test]
fn breaker_stops_no_progress() {
    let config = BreakerConfig {
        max_consecutive_failures: usize::MAX,
        max_failure_rate: 2.0,
        rate_min_passes: usize::MAX,
        max_stagnant_passes: 2,
        max_systemic_items: usize::MAX,
    };
    let mut breaker = LoopBreaker::new(config);
    assert!(breaker.observe(false).is_none());
    assert!(breaker.observe(false).is_none());
    assert_eq!(breaker.observe(false).as_deref(), Some("no progress"));
}

#[test]
fn breaker_arms_a_live_run() {
    let _serial = serial_loop_tests();
    let guard = LoopGuard::acquire().expect("guard");
    let mut queue = MaintenanceQueue::new();
    queue
        .enqueue("a", "src", "rustfmt", &[])
        .expect("enqueue a");
    queue
        .enqueue("b", "src", "rustfmt", &[])
        .expect("enqueue b");
    let act: Act = Arc::new(Mutex::new(|_id: &str, _tier: Tier| {
        ActOutcome::Retryable("red".to_string())
    }));
    let abort = StreamAbort::new();
    let mut breaker = LoopBreaker::new(BreakerConfig::strict());
    let report = run_to_fixpoint_with_breaker(
        &guard,
        &mut queue,
        &MaintenancePolicy {
            attempt_limit: 100,
            ..MaintenancePolicy::default()
        },
        50,
        &abort,
        Some(&mut breaker),
        &act,
    );
    assert_eq!(report.stopped.as_deref(), Some("repeat failure"));
    assert!(report.passes < 50);
}

#[test]
fn ledger_round_trips_every_applied_change() {
    let _serial = serial_loop_tests();
    let guard = LoopGuard::acquire().expect("guard");
    let mut queue = MaintenanceQueue::new();
    queue
        .enqueue("a", "src", "rustfmt", &[])
        .expect("enqueue a");
    queue
        .enqueue("b", "src", "rustfmt", &[])
        .expect("enqueue b");
    let act: Act = Arc::new(Mutex::new(|id: &str, _tier: Tier| {
        ActOutcome::Verified(kept_report(id))
    }));
    let abort = StreamAbort::new();
    let report = run_to_fixpoint(
        &guard,
        &mut queue,
        &MaintenancePolicy::default(),
        10,
        &abort,
        &act,
    );
    assert!(report.stopped.is_none());
    let dir = tempdir();
    let ledger = dir.path().join("ledger.json");
    report.save_ledger(&ledger).expect("save");
    let loaded = LoopReport::load_ledger(&ledger).expect("load");
    assert_eq!(loaded.verified, report.verified);
    assert_eq!(loaded.summary.entries.len(), 2);
    for id in &loaded.verified {
        assert!(loaded.summary.entries.iter().any(|entry| &entry.file == id));
    }
    assert!(loaded.render().contains("fixpoint"));
}

#[test]
fn report_carries_efficiency_sections_with_ledger_compat() {
    // New efficiency fields render in their own sections (evidence
    // routing apart from authority modes), and pre-enrichment ledgers
    // still load with zeroed defaults.
    let mut report = LoopReport {
        passes: 3,
        reprocessed_clean: 2,
        model_calls_avoided: 5,
        ..LoopReport::default()
    };
    report.record_modes(&["dp.triage_residual=shadow".to_string()]);
    let text = report.render();
    assert!(text.contains("reprocessed clean: 2"), "{text}");
    assert!(text.contains("model calls avoided: 5"), "{text}");
    assert!(text.contains("dp.triage_residual=shadow"), "{text}");
    let legacy = serde_json::json!({
        "passes": 3,
        "verified": [],
        "escalated": [],
        "summary": crate::offload::ReviewSummary::new(),
        "packets": [],
        "stopped": null,
    });
    let dir = tempdir();
    let path = dir.path().join("ledger.json");
    std::fs::write(&path, serde_json::to_string(&legacy).unwrap()).expect("write");
    let loaded = LoopReport::load_ledger(&path).expect("old ledgers load");
    assert_eq!(loaded.reprocessed_clean, 0);
    assert_eq!(loaded.model_calls_avoided, 0);
    assert!(loaded.decision_modes.is_empty());
    assert!(loaded.render().contains("decision modes: unmeasured"));
}

#[test]
fn report_merges_index_and_decision_counters() {
    use crate::index_pipeline::IndexStats;
    use crate::query::decision_points::{DecisionMetrics, LadderOutcome};
    let mut report = LoopReport::default();
    let stats = IndexStats {
        reprocessed_clean: 3,
        ..IndexStats::default()
    };
    report.merge_index_stats(&stats);
    report.merge_index_stats(&stats);
    assert_eq!(report.reprocessed_clean, 6);
    let metrics = DecisionMetrics::new();
    metrics.record(LadderOutcome::Rule);
    metrics.record(LadderOutcome::CacheHit);
    metrics.record(LadderOutcome::Model);
    report.merge_decision_snapshot(&metrics.snapshot());
    assert_eq!(report.model_calls_avoided, 2, "rule wins + cache hits only");
}

#[test]
fn draft_never_touches_test_targets() {
    let dir = tempdir();
    let tests = dir.path().join("tests");
    std::fs::create_dir_all(&tests).expect("mkdir");
    let target = tests.join("draft_me.rs");
    std::fs::write(&target, "pub fn f() {}\n").expect("write");
    let before = std::fs::read(&target).expect("read");
    let result = apply_draft(
        &target,
        TriageClass::Formatting,
        "pub fn f() {}\n// draft\n",
        &["true".to_string()],
        1,
    );
    assert!(matches!(result, Err(OffloadError::Policy(_))), "{result:?}");
    let stale = apply_draft_expected(
        &target,
        TriageClass::Formatting,
        "pub fn f() {}\n// draft\n",
        &["true".to_string()],
        1,
        &before,
    );
    assert!(matches!(stale, Err(OffloadError::Policy(_))), "{stale:?}");
    assert_eq!(std::fs::read(&target).expect("read"), before);
}
