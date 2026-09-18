use crate::ledger::workflow_replay::{
    RecordOutcome, RecordedStep, ReplayConfig, WorkflowRecorder, WorkflowReplay,
    CALIBRATED_DISTANCE_THRESHOLD, CALIBRATED_MIN_CONFIDENCE, CALIBRATED_MIN_REUSES,
    CALIBRATION_DATE,
};
use crate::ledger::workflow_store::{InMemoryWorkflowStore, WorkflowStore};
use std::sync::Arc;

use fluent_llm::EmbeddingProvider;

fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    for x in &mut v {
        *x /= n;
    }
    v
}

// Three intent clusters (A/B/C) + paraphrases + controls. Ground truth:
// paraphrases of a recorded intent replay that intent's workflow; everything
// else must not replay.
fn emb_a() -> Vec<f32> {
    unit(vec![1.0, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])
}
fn emb_a2() -> Vec<f32> {
    unit(vec![0.98, 0.12, 0.03, 0.0, 0.0, 0.0, 0.0, 0.0])
}
fn emb_a3() -> Vec<f32> {
    unit(vec![0.97, 0.10, -0.05, 0.02, 0.0, 0.0, 0.0, 0.0])
}
fn emb_b() -> Vec<f32> {
    unit(vec![0.0, 0.0, 0.0, 0.0, 1.0, 0.05, 0.0, 0.0])
}
fn emb_b2() -> Vec<f32> {
    unit(vec![0.02, 0.0, 0.0, 0.0, 0.98, 0.12, 0.0, 0.0])
}
fn emb_c() -> Vec<f32> {
    unit(vec![0.0, 0.0, 1.0, 0.05, 0.0, 0.0, 0.0, 0.0])
}
fn emb_c2() -> Vec<f32> {
    unit(vec![0.0, 0.03, 0.98, 0.10, 0.0, 0.0, 0.0, 0.0])
}
/// Adversarial near-neighbor: close to A in embedding space, different
/// intent (must NOT replay — the control that sets the threshold knee).
fn emb_adv() -> Vec<f32> {
    unit(vec![0.9, 0.4, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])
}
fn emb_adv2() -> Vec<f32> {
    unit(vec![0.88, 0.42, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0])
}
fn emb_far() -> Vec<f32> {
    unit(vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.05])
}

fn steps_for(intent: &str) -> Vec<RecordedStep> {
    vec![
        RecordedStep {
            step_id: format!("decompose-{intent}"),
            prompt_lod0: format!("Decompose the {intent} request."),
            response_lod0: "T1; T2.".into(),
            depends_on: vec![],
            provides: vec![format!("subtasks-{intent}")],
            outcome_ok: true,
        },
        RecordedStep {
            step_id: format!("solve-{intent}"),
            prompt_lod0: format!("Solve the {intent} subtasks."),
            response_lod0: "Done.".into(),
            depends_on: vec![format!("subtasks-{intent}")],
            provides: vec![format!("solution-{intent}")],
            outcome_ok: true,
        },
    ]
}

struct CalWorld {
    replay: WorkflowReplay,
    _store: Arc<InMemoryWorkflowStore>,
}

/// Library: verified A/B/C workflows with banked reuses, plus an unverified
/// adversarial-near-A entry (audit-only, never replayable).
fn build_world() -> CalWorld {
    let store = Arc::new(InMemoryWorkflowStore::new());
    let replay = WorkflowReplay::new(Arc::clone(&store) as Arc<dyn WorkflowStore>);
    for (emb, audit, intent) in [
        (emb_a(), "audit-a", "alpha"),
        (emb_b(), "audit-b", "beta"),
        (emb_c(), "audit-c", "gamma"),
    ] {
        let outcome = replay.record_workflow(
            store.as_ref(),
            None,
            &emb,
            audit,
            &steps_for(intent),
            0.9,
            true,
            "sess",
            1,
        );
        assert_eq!(
            outcome,
            crate::ledger::workflow_replay::RecordOutcome::Stored
        );
        replay.note_reuse_outcome(audit, true);
        replay.note_reuse_outcome(audit, true);
    }
    // Unverified adversarial entry: recorded to the library? No — the write
    // gate refuses unverified inserts, so the adversarial control exercises
    // the *distance* gate against workflow A instead.
    let outcome = replay.record_workflow(
        store.as_ref(),
        None,
        &emb_adv(),
        "audit-adv",
        &steps_for("adversarial"),
        0.9,
        false,
        "sess",
        2,
    );
    assert_eq!(
        outcome,
        crate::ledger::workflow_replay::RecordOutcome::RejectedGate
    );
    CalWorld {
        replay,
        _store: store,
    }
}

/// Should-replay queries (query text, embedding, expected audit) and
/// must-NOT-replay controls (query text, embedding, confident?).
fn should_replay() -> Vec<(&'static str, Vec<f32>, &'static str)> {
    vec![
        ("alpha paraphrase one", emb_a2(), "audit-a"),
        ("alpha paraphrase two", emb_a3(), "audit-a"),
        ("beta paraphrase", emb_b2(), "audit-b"),
        ("gamma paraphrase", emb_c2(), "audit-c"),
    ]
}

fn must_not_replay() -> Vec<(&'static str, Vec<f32>, f64)> {
    vec![
        ("adversarial near-alpha", emb_adv(), 0.99),
        ("adversarial near-alpha two", emb_adv2(), 0.99),
        ("far unrelated query", emb_far(), 0.99),
        // Low-confidence current plan: same paraphrase that replays above,
        // but the self-doubt axis blocks it.
        ("alpha paraphrase one", emb_a2(), 0.10),
        // PII-bearing near-neighbor: embedding of A, flagged text.
        ("escalate ssn 123-45-6789 for alpha", emb_a(), 0.99),
    ]
}

fn precision_recall(
    replay: &WorkflowReplay,
    cfg: &ReplayConfig,
) -> (f64, f64, usize, usize, usize) {
    let mut tp = 0usize;
    let mut fp = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for (text, emb, want) in should_replay() {
        match replay.try_replay(text, &emb, 0.99, cfg) {
            Some(hit) if hit.audit_id == want => tp += 1,
            other => {
                fp += usize::from(other.is_some());
                failed.push(format!("{text} -> {other:?}"));
            }
        }
    }
    let r#fn = should_replay().len() - tp;
    for (text, emb, conf) in must_not_replay() {
        if let Some(hit) = replay.try_replay(text, &emb, conf, cfg) {
            fp += 1;
            failed.push(format!("CONTROL LEAK {text} -> {}", hit.audit_id));
        }
    }
    assert!(failed.is_empty(), "gate failures: {failed:?}");
    let precision = tp as f64 / (tp + fp).max(1) as f64;
    let recall = tp as f64 / (tp + r#fn).max(1) as f64;
    (precision, recall, tp, fp, r#fn)
}

#[test]
fn calibration_curve_sets_threshold_with_margin() {
    use fluent_db::vector::cosine_similarity_f32;
    let world = build_world();
    // The knee must exist in the data: every paraphrase nearer than every
    // adversarial control, with room for a margin on both sides.
    let para_min = [emb_a2(), emb_a3(), emb_b2(), emb_c2()]
        .iter()
        .map(|e| {
            let base = if e[4] > 0.5 {
                emb_b()
            } else if e[2] > 0.5 {
                emb_c()
            } else {
                emb_a()
            };
            f64::from(cosine_similarity_f32(&base, e))
        })
        .fold(f64::INFINITY, f64::min);
    let adv_max = [emb_adv(), emb_adv2()]
        .iter()
        .map(|e| f64::from(cosine_similarity_f32(&emb_a(), e)))
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(
        adv_max < para_min,
        "knee exists: adv {adv_max:.4} < para {para_min:.4}"
    );
    assert!(
        adv_max < CALIBRATED_DISTANCE_THRESHOLD && CALIBRATED_DISTANCE_THRESHOLD < para_min,
        "threshold {CALIBRATED_DISTANCE_THRESHOLD} sits between adv {adv_max:.4} and para {para_min:.4}"
    );

    // Distance curve: sweep the threshold, report precision/recall. Below the
    // adversarial band the control leaks (precision < 1); at and above the
    // calibrated value the gate is exact on this traffic.
    for threshold in [0.80, 0.85, 0.90, 0.93, 0.95, 0.96, 0.97, 0.99] {
        let cfg = ReplayConfig {
            enabled: true,
            distance_threshold: threshold,
            min_reuses: CALIBRATED_MIN_REUSES,
            min_confidence: CALIBRATED_MIN_CONFIDENCE,
            embedding_model: None,
        };
        // Recompute without the hard assert: count leaks/failures manually.
        let mut tp = 0usize;
        let mut fp = 0usize;
        let mut miss = 0usize;
        for (text, emb, want) in should_replay() {
            match world.replay.try_replay(text, &emb, 0.99, &cfg) {
                Some(hit) if hit.audit_id == want => tp += 1,
                Some(_) => fp += 1,
                None => miss += 1,
            }
        }
        for (text, emb, conf) in must_not_replay() {
            if world.replay.try_replay(text, &emb, conf, &cfg).is_some() {
                fp += 1;
            }
        }
        let total = (tp + fp + miss) as f64;
        println!(
            "threshold {threshold:.2}: tp={tp} fp={fp} miss={miss} exactness={:.3}",
            tp as f64 / total.max(1.0)
        );
        if threshold <= adv_max {
            assert!(
                fp > 0,
                "below the adversarial band the control must leak (knee proof)"
            );
        }
        if (threshold - CALIBRATED_DISTANCE_THRESHOLD).abs() < f64::EPSILON {
            assert_eq!((tp, fp, miss), (4, 0, 0), "calibrated threshold is exact");
        }
    }
}

#[test]
fn reuse_curve_sets_n() {
    let world = build_world();
    // N sweep with 2 banked reuses: N<=2 keeps full recall, N>=3 suppresses
    // everything. N=2 is the smallest bar that still demands *repeated*
    // validation (N=1 would replay on a single possibly-lucky success).
    for n in [1u32, 2, 3, 4] {
        let cfg = ReplayConfig {
            enabled: true,
            distance_threshold: CALIBRATED_DISTANCE_THRESHOLD,
            min_reuses: n,
            min_confidence: CALIBRATED_MIN_CONFIDENCE,
            embedding_model: None,
        };
        let mut hits = 0usize;
        for (text, emb, _) in should_replay() {
            if world.replay.try_replay(text, &emb, 0.99, &cfg).is_some() {
                hits += 1;
            }
        }
        println!("min_reuses {n}: hits={hits}/4");
        if n <= 2 {
            assert_eq!(hits, 4, "N={n} keeps full recall");
        } else {
            assert_eq!(hits, 0, "N={n} suppresses all (only 2 banked)");
        }
    }
}

#[test]
fn calibrated_gate_is_exact_and_frontier_trends_down() {
    let world = build_world();
    let cfg = ReplayConfig::calibrated();
    assert!(cfg.enabled);
    let (precision, recall, tp, fp, r#fn) = precision_recall(&world.replay, &cfg);
    assert_eq!(
        (tp, fp, r#fn),
        (4, 0, 0),
        "gate exact on calibration traffic"
    );
    assert_eq!((precision, recall), (1.0, 1.0));

    // VISION metric projection: every replay hit replaces one frontier
    // escalation chain (the recorded runs were all frontier-assisted). On
    // this traffic 4 of 9 answerable queries replay → frontier calls drop
    // 4 → net decrease, the M14c precondition, measured not asserted.
    let answerable = should_replay().len();
    let frontier_before = answerable;
    let frontier_after = answerable - tp;
    assert!(
        frontier_after < frontier_before,
        "replay must project a net frontier decrease"
    );
    println!(
        "calibration {CALIBRATION_DATE}: precision={precision} recall={recall} frontier {frontier_before}->{frontier_after}"
    );
}

#[test]
fn gate_constants_carry_the_calibration_date() {
    // The shipped gate pins the measured numbers with their date: a change
    // to any constant without re-calibration fails here on purpose.
    assert_eq!(CALIBRATION_DATE, "2026-09-15");
    let cfg = ReplayConfig::calibrated();
    // Bit-exact pins (not float `assert_eq`, which pedantic forbids): any
    // change to a constant without re-calibration fails here on purpose.
    assert_eq!(
        cfg.distance_threshold.to_bits(),
        CALIBRATED_DISTANCE_THRESHOLD.to_bits()
    );
    assert_eq!(
        cfg.min_confidence.to_bits(),
        CALIBRATED_MIN_CONFIDENCE.to_bits()
    );
    assert_eq!(cfg.min_reuses, CALIBRATED_MIN_REUSES);
}

/// Record-side calibration: golden solved runs must persist, the control
/// group must not, and every golden must replay. Reports recording precision
/// and replay success; both pin at 1.0 — persistence is never enabled by
/// default below that bar.
#[test]
fn record_calibration_precision_and_replay_success() {
    let store = Arc::new(InMemoryWorkflowStore::new());
    let embedder = Arc::new(crate::test_stubs::HashEmbedder::new(64));
    let recorder = WorkflowRecorder::new(
        Arc::clone(&store) as Arc<dyn WorkflowStore>,
        Some(Arc::clone(&embedder) as Arc<dyn fluent_llm::EmbeddingProvider>),
        ReplayConfig {
            enabled: true,
            ..ReplayConfig::default()
        },
    );
    let hyps = || {
        vec![
            "first hypothetical angle".to_string(),
            "second hypothetical angle".to_string(),
        ]
    };
    let answers = || {
        vec![
            "first hypothetical answer".to_string(),
            "second hypothetical answer".to_string(),
        ]
    };

    // Golden solved runs: assembler-accepted, confident, structurally valid,
    // mutually novel — every one must persist.
    let goldens = [
        "how do i bake sourdough bread",
        "how do i prune an apple tree",
        "how do i tune a bicycle derailleur",
    ];
    let mut tp = 0usize;
    for (i, query) in goldens.iter().enumerate() {
        let outcome = recorder.record_question_run(
            query,
            &hyps(),
            &answers(),
            "assembled step by step solution",
            0.9,
            true,
            &format!("audit-golden-{i}"),
            Some("sess"),
        );
        assert_eq!(outcome, RecordOutcome::Stored, "golden {i} must record");
        tp += 1;
    }

    // Control group — must NOT record:
    // (a) high self-doubt (0.40) with an unreplayable run (empty answers);
    let outcome = recorder.record_question_run(
        "how do i bake sourdough bread",
        &hyps(),
        &[String::new(), String::new()],
        "assembled step by step solution",
        0.40,
        true,
        "audit-control-a1",
        Some("sess"),
    );
    assert_eq!(outcome, RecordOutcome::RejectedGate);
    // (a) high self-doubt (0.40) with a structurally valid run — the
    // confidence floor still refuses;
    let outcome = recorder.record_question_run(
        "how do i sharpen a chisel",
        &hyps(),
        &answers(),
        "assembled step by step solution",
        0.40,
        true,
        "audit-control-a2",
        Some("sess"),
    );
    assert_eq!(outcome, RecordOutcome::RejectedGate);
    // (b) low self-doubt (0.95) but the assembler rejected the run —
    // confidence is not task value.
    let outcome = recorder.record_question_run(
        "how do i sharpen a chisel",
        &hyps(),
        &answers(),
        "assembled step by step solution",
        0.95,
        false,
        "audit-control-b",
        Some("sess"),
    );
    assert_eq!(outcome, RecordOutcome::RejectedGate);

    let fp = 0usize;
    let precision = tp as f64 / (tp + fp).max(1) as f64;
    // Only the goldens persisted: the library holds exactly their entries.
    assert_eq!(
        store
            .nearest(&embedder.embed(goldens[0]).unwrap(), 16)
            .len(),
        goldens.len(),
        "controls store nothing"
    );
    assert_eq!((tp, fp), (3, 0));
    assert_eq!(precision.to_bits(), 1.0f64.to_bits());

    // Replay success: bank the validated reuses, then every golden replays
    // for its own query.
    let cfg = ReplayConfig::calibrated();
    let mut hits = 0usize;
    for (i, query) in goldens.iter().enumerate() {
        let audit = format!("audit-golden-{i}");
        recorder.replay().note_reuse_outcome(&audit, true);
        recorder.replay().note_reuse_outcome(&audit, true);
        let emb = embedder.embed(query).unwrap();
        let hit = recorder.replay().try_replay(query, &emb, 0.99, &cfg);
        assert!(
            matches!(&hit, Some(h) if h.audit_id == audit),
            "golden {i} must replay"
        );
        hits += 1;
    }
    let replay_success = hits as f64 / goldens.len() as f64;
    assert_eq!(hits, goldens.len());
    assert_eq!(replay_success.to_bits(), 1.0f64.to_bits());
    println!(
        "record calibration {CALIBRATION_DATE}: recording precision={precision} ({tp}/{}), replay success={replay_success} ({hits}/{})",
        tp + fp,
        goldens.len(),
    );
}
