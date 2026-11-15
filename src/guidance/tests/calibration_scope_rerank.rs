//! Scope + rerank calibration: human-labelled datasets gate
//! authorization, controls veto promotion, and nothing authorizes
//! without a passing registry entry. Until then every gate stays
//! shadow-only and serving stays rank-only.
use super::*;
use fluent_llm::decision::{gate_for_point_with, CalibrationRegistry, GateParams};

fn scope_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../doc/guidance/eval/scope.jsonl")
}

fn rerank_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../doc/guidance/eval/rerank.jsonl")
}

#[test]
fn scope_dataset_loads_with_must_not_fire_controls() {
    let rows = load_labeled_scopes(&scope_path()).expect("scope corpus loads");
    assert!(rows.len() >= 12, "{} rows", rows.len());
    let mut seen = std::collections::HashSet::new();
    for row in &rows {
        assert!(!row.query.trim().is_empty(), "{:?}", row.id);
        assert!(seen.insert(row.id.clone()), "duplicate {}", row.id);
    }
    let controls: Vec<_> = rows.iter().filter(|row| row.control).collect();
    assert!(controls.len() >= 3, "M1 control sets carried over");
    assert!(
        controls.iter().all(|row| row.expected_crate == "unknown"),
        "controls must hypothesize nothing: {controls:?}"
    );
    let support = rows.iter().filter(|row| !row.control).count();
    assert!(support >= 9, "recall support: {support}");
    assert!(
        rows.iter()
            .any(|row| row.id == "control-scope-nonexistent-crate"),
        "M1 controls referenced by id"
    );
}

#[test]
fn rerank_dataset_loads_with_must_keep_and_must_drop() {
    let rows = load_labeled_findings(&rerank_path()).expect("rerank corpus loads");
    assert!(rows.len() >= 10, "{} rows", rows.len());
    for id in [
        "rerank-keep-calibrate-size",
        "rerank-keep-stale-sync",
        "rerank-drop-manifest",
        "rerank-drop-fixture",
        "control-rerank-eval-decoy",
    ] {
        assert!(rows.iter().any(|row| row.id == id), "missing {id}");
    }
    let controls: Vec<_> = rows.iter().filter(|row| row.control).collect();
    assert!(!controls.is_empty(), "controls present");
    assert!(
        controls.iter().all(|row| !row.expected_relevant),
        "controls never relevant: {controls:?}"
    );
}

#[test]
fn scope_gate_passes_when_judge_agrees_and_holds_on_control_fire() {
    let rows = load_labeled_scopes(&scope_path()).expect("scope corpus loads");
    // Scripted-correct judge: right crate per row, unknown on controls.
    let verdict = measure_scope(&rows, |row| row.expected_crate.clone());
    assert!(verdict.passed, "correct judge passes: {verdict:?}");
    assert_eq!(verdict.control_fires, 0);
    let cases = score_scopes(&rows, |row| row.expected_crate.clone());
    assert!(
        gate_for_point_with(&cases, SCOPE_THRESHOLD, &GateParams::default()),
        "shared gate agrees with the task-value bars"
    );
    // Adversarial: a control hypothesizes a real crate.
    let held = measure_scope(&rows, |row| {
        if row.control {
            "guidance-core".to_string()
        } else {
            row.expected_crate.clone()
        }
    });
    assert!(!held.passed, "control fire vetoes promotion");
    assert_eq!(held.control_fires, 3);
}

#[test]
fn rerank_gate_passes_when_judge_agrees_and_holds_on_control_fire() {
    let rows = load_labeled_findings(&rerank_path()).expect("rerank corpus loads");
    // Scripted-correct judge: verdict follows the human label.
    let verdict = measure_rerank(&rows, |row| if row.expected_relevant { 1.0 } else { 0.0 });
    assert!(verdict.passed, "correct judge passes: {verdict:?}");
    assert_eq!(verdict.control_fires, 0);
    let cases = score_findings(&rows, |row| if row.expected_relevant { 1.0 } else { 0.0 });
    assert!(
        gate_for_point_with(&cases, RERANK_THRESHOLD, &GateParams::default()),
        "shared gate agrees with the task-value bars"
    );
    // Adversarial: a control fires.
    let held = measure_rerank(&rows, |row| f64::from(row.control || row.expected_relevant));
    assert!(!held.passed, "control fire vetoes promotion");
    assert_eq!(held.control_fires, 2);
    // Adversarial: recall below the per-point floor.
    let mut dropped_first = false;
    let weak = measure_rerank(&rows, |row| {
        if row.expected_relevant && !dropped_first {
            dropped_first = true;
            0.0
        } else if row.expected_relevant {
            1.0
        } else {
            0.0
        }
    });
    assert!(!weak.passed, "recall floor binds: {weak:?}");
}

#[test]
fn bars_come_from_default_floors_plus_recall_floors() {
    for bars in [SCOPE_BARS, RERANK_BARS] {
        assert!((bars.min_precision - 0.90).abs() < 1e-12, "{bars:?}");
        assert!((bars.max_fpr - 0.05).abs() < 1e-12, "{bars:?}");
        assert!(bars.min_recall >= 0.80, "{bars:?}");
    }
    let defaults = GateParams::default();
    assert!((defaults.min_precision - 0.90).abs() < 1e-12);
    assert_eq!(defaults.max_control_fires, 0);
}

#[test]
fn no_gate_authorizes_without_a_passing_registry_entry() {
    use fluent_llm::decision::CalibrationStatus;
    let empty = CalibrationRegistry::empty();
    for (point, version) in [
        (FINDING_POINT, FINDING_QUESTION_VERSION),
        (SUPPORT_POINT, SUPPORT_QUESTION_VERSION),
        (
            crate::query::decision_points::SCOPE_POINT,
            crate::query::decision_points::SCOPE_VERSION,
        ),
        (
            crate::query::decision_points::RERANK_POINT,
            crate::query::decision_points::RERANK_VERSION,
        ),
    ] {
        let status = crate::query::decision_points::authorize_point_with(
            &empty,
            point,
            "test-fingerprint",
            version,
        );
        assert!(
            matches!(status, CalibrationStatus::ShadowOnly { .. }),
            "{point} fails closed without an entry"
        );
    }
    for status in [
        crate::query::decision_points::rerank_authorized(&empty, "test-fingerprint"),
        crate::query::decision_points::scope_authorized(&empty, "test-fingerprint"),
    ] {
        assert!(matches!(status, CalibrationStatus::ShadowOnly { .. }));
    }
}

#[test]
fn canary_pins_fingerprint_before_any_run() {
    use fluent_llm::decision::{canary_signature, canary_suite, verify_canary, Decision};
    let suite = canary_suite();
    assert_eq!(suite.len(), 2);
    let mk = |a: &str, ma: f64, b: &str| {
        Decision::from_distribution(
            a,
            vec![(a.to_string(), ma), (b.to_string(), 1.0 - ma)],
            "m".to_string(),
            1,
            0,
            false,
        )
        .expect("decision")
    };
    // Measured masses matching the suite verify; the signature is stable.
    let measured: Vec<Decision> = vec![mk("A", 0.97, "B"), mk("Yes", 0.97, "No")];
    for (decision, case) in measured.iter().zip(suite.iter()) {
        assert!(verify_canary(decision, case).is_ok(), "{}", case.id);
    }
    let pinned = canary_signature(&measured);
    assert_eq!(pinned, canary_signature(&measured));
    // Drifted weights fail verification and move the signature.
    let drifted = vec![mk("A", 0.60, "B"), mk("Yes", 0.97, "No")];
    assert!(verify_canary(&drifted[0], &suite[0]).is_err());
    assert_ne!(pinned, canary_signature(&drifted));
}

#[test]
fn shadow_only_serves_ranked_unfiltered_without_filtering() {
    use crate::read::engine::{ExplainContext, ExplainRequest, JudgeCtx, ReadEngine};
    use fluent_llm::decision::{Decision, ScriptedDecideBackend};
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir");
    std::fs::write(
        root.join("src/a.rs"),
        "pub fn alpha_fn() -> u64 {\n    1\n}\n",
    )
    .expect("write");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("manifest");
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    db.upsert_fragments(
        &search_vector::db::FileRecord {
            id: "/repo/src/a.rs".to_string(),
            absolute_path: "/repo/src/a.rs".to_string(),
            relative_path: "src/a.rs".to_string(),
            root_path: "/repo".to_string(),
            size_bytes: 64,
            last_modified_time: 100,
            kind: Some("code".to_string()),
            format: "rust".to_string(),
            content_hash: None,
            index_status: Some("indexed".to_string()),
            fail_count: 0,
            last_error: None,
        },
        &[search_vector::db::FragmentRecord {
            id: "fn".to_string(),
            group: None,
            file_id: "/repo/src/a.rs".to_string(),
            range_json: "{\"kind\":\"text\",\"start_line\":1,\"end_line\":3,\"start_offset\":0,\"end_offset\":10}".to_string(),
            content_kind: "text".to_string(),
            content_text: Some("pub fn alpha_fn() {}\nAdds one.".to_string()),
            cjk_text: String::new(),
            symbol_type: Some("function".to_string()),
            symbol_name: Some("alpha_fn".to_string()),
            scope: None,
            signature: None,
            doc: None,
            modifiers: String::new(),
            heading: None,
            heading_level: None,
            embedding: None,
        }],
        &[],
    )
    .expect("seed");
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: root,
        db: &db,
        nlp: None,
        coverage: None,
    };
    let request = ExplainRequest {
        query: "alpha_fn".to_string(),
        limit: 5,
        budget_override: None,
        no_llm: true,
        include_fixtures: false,
    };
    let plain = engine.explain(&request, &ctx, None);
    for canned in ["relevant", "irrelevant"] {
        let backend = ScriptedDecideBackend::canned(
            Decision::from_distribution(
                canned,
                vec![(canned.to_string(), 0.9)],
                "d".to_string(),
                1,
                0,
                false,
            )
            .expect("decision"),
        );
        let judge = JudgeCtx {
            backend: &backend,
            model: "d".to_string(),
            timeout_ms: 2000,
            fingerprint: None,
        };
        let judged = engine.explain_with_judge(&request, &ctx, None, Some(&judge));
        let mut plain_cites: Vec<String> = plain
            .citations
            .iter()
            .map(crate::source_ref::Citation::text)
            .collect();
        let mut judged_cites: Vec<String> = judged
            .citations
            .iter()
            .map(crate::source_ref::Citation::text)
            .collect();
        plain_cites.sort();
        judged_cites.sort();
        assert_eq!(
            plain_cites, judged_cites,
            "shadow-only never filters, even all-{canned}"
        );
    }
}
