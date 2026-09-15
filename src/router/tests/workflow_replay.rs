use super::*;
use crate::ledger::workflow_store::{InMemoryWorkflowStore, WorkflowStore};
use std::sync::Arc;

fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    for x in &mut v {
        *x /= n;
    }
    v
}

/// Intent cluster A (paraphrases share the first axis); B is orthogonal.
/// Adversarial shares most of A but answers a different intent.
fn emb_a() -> Vec<f32> {
    unit(vec![1.0, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])
}
fn emb_a_para() -> Vec<f32> {
    unit(vec![0.98, 0.12, 0.03, 0.0, 0.0, 0.0, 0.0, 0.0])
}
fn emb_b() -> Vec<f32> {
    unit(vec![0.0, 0.0, 0.0, 0.0, 1.0, 0.05, 0.0, 0.0])
}
fn emb_adversarial() -> Vec<f32> {
    unit(vec![0.9, 0.4, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])
}
fn emb_far() -> Vec<f32> {
    unit(vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.05])
}

fn steps_ok() -> Vec<RecordedStep> {
    vec![
        RecordedStep {
            step_id: "decompose".into(),
            prompt_lod0: "Break the request into subtasks.".into(),
            response_lod0: "T1; T2.".into(),
            depends_on: vec![],
            provides: vec!["subtasks".into()],
            outcome_ok: true,
        },
        RecordedStep {
            step_id: "solve".into(),
            prompt_lod0: "Solve each subtask.".into(),
            response_lod0: "Done.".into(),
            depends_on: vec!["subtasks".into()],
            provides: vec!["solution".into()],
            outcome_ok: true,
        },
    ]
}

fn replay_with_store() -> (WorkflowReplay, Arc<InMemoryWorkflowStore>) {
    let store = Arc::new(InMemoryWorkflowStore::new());
    let replay = WorkflowReplay::new(Arc::clone(&store) as Arc<dyn WorkflowStore>);
    (replay, store)
}

fn record_a(replay: &WorkflowReplay, store: &InMemoryWorkflowStore) {
    let outcome = replay.record_workflow(
        store,
        None,
        &emb_a(),
        "audit-a",
        &steps_ok(),
        0.9,
        true,
        "sess",
        7,
    );
    assert_eq!(outcome, RecordOutcome::Stored, "record must store");
    // Two validated reuses reach the calibrated N below in later tests via
    // `note_reuse_outcome`; the gate default is asserted in the cal file.
    replay.note_reuse_outcome("audit-a", true);
    replay.note_reuse_outcome("audit-a", true);
}

#[test]
fn record_builds_target_dag_and_retrieves() {
    let (replay, store) = replay_with_store();
    record_a(&replay, &store);
    let hits = store.nearest_verified(&emb_a_para(), 3, 0.5);
    assert_eq!(hits.len(), 1, "paraphrase finds the workflow");
    assert_eq!(hits[0].0.audit_id, "audit-a");
    assert_eq!(hits[0].0.dag.len(), 2);
    // Dependency order: decompose provides what solve consumes.
    let names: Vec<String> = hits[0].0.dag.iter().map(|t| t.name.to_string()).collect();
    assert_eq!(names, vec!["decompose".to_string(), "solve".to_string()]);
}

#[test]
fn record_rejects_cycle() {
    let (replay, store) = replay_with_store();
    let outcome = replay.record_workflow(
        store.as_ref(),
        None,
        &emb_a(),
        "audit-cycle",
        &[
            RecordedStep {
                step_id: "x".into(),
                prompt_lod0: "x".into(),
                response_lod0: "x".into(),
                depends_on: vec!["b1".into()],
                provides: vec!["a1".into()],
                outcome_ok: true,
            },
            RecordedStep {
                step_id: "y".into(),
                prompt_lod0: "y".into(),
                response_lod0: "y".into(),
                depends_on: vec!["a1".into()],
                provides: vec!["b1".into()],
                outcome_ok: true,
            },
        ],
        0.9,
        true,
        "sess",
        7,
    );
    assert_eq!(outcome, RecordOutcome::RejectedCycle);
    assert!(store.nearest(&emb_a(), 3).is_empty(), "cyclic DAG never stored");
}

#[test]
fn record_rejects_ungated_writes() {
    let (replay, store) = replay_with_store();
    // Low confidence.
    assert_eq!(
        replay.record_workflow(store.as_ref(), None, &emb_a(), "a1", &steps_ok(), 0.5, true, "s", 7),
        RecordOutcome::RejectedGate
    );
    // Unverified.
    assert_eq!(
        replay.record_workflow(store.as_ref(), None, &emb_a(), "a2", &steps_ok(), 0.9, false, "s", 7),
        RecordOutcome::RejectedGate
    );
    assert!(store.nearest(&emb_a(), 5).is_empty());
}

#[test]
fn replay_disabled_is_none() {
    let (replay, store) = replay_with_store();
    record_a(&replay, &store);
    let cfg = ReplayConfig::default();
    assert!(!cfg.enabled);
    let hit = replay.try_replay("plain query", &emb_a_para(), 0.99, &cfg);
    assert!(hit.is_none(), "flag off: never replay");
}

#[test]
fn replay_hit_returns_lod0_steps_capped_at_local_model() {
    let (replay, store) = replay_with_store();
    record_a(&replay, &store);
    let cfg = ReplayConfig {
        enabled: true,
        ..ReplayConfig::calibrated()
    };
    let hit = replay
        .try_replay("plain paraphrase query", &emb_a_para(), 0.99, &cfg)
        .expect("paraphrase must replay");
    assert_eq!(hit.audit_id, "audit-a");
    assert_eq!(hit.steps.len(), 2);
    assert_eq!(hit.steps[0].step_id, "decompose");
    assert!(!hit.steps[0].prompt_lod0.is_empty(), "LOD0 text travels");
    assert_eq!(hit.provenance_cap, fluent_types::ProvenanceTier::LocalModel);
    assert!(hit.score >= cfg.distance_threshold);
}

#[test]
fn replay_misses_without_reuses_confidence_or_proximity() {
    let (replay, store) = replay_with_store();
    // Record but never validate a reuse: reuses (0) < N.
    let outcome = replay.record_workflow(store.as_ref(), None, &emb_a(), "audit-n", &steps_ok(), 0.9, true, "s", 7);
    assert_eq!(outcome, RecordOutcome::Stored);
    let cfg = ReplayConfig {
        enabled: true,
        ..ReplayConfig::calibrated()
    };
    assert!(
        replay.try_replay("q", &emb_a_para(), 0.99, &cfg).is_none(),
        "no validated reuses yet"
    );
    replay.note_reuse_outcome("audit-n", true);
    replay.note_reuse_outcome("audit-n", true);
    // Low current confidence blocks even a high-success workflow (self-doubt
    // axis never stands in for task value, and vice versa).
    assert!(
        replay.try_replay("q", &emb_a_para(), 0.10, &cfg).is_none(),
        "low-confidence plan must not replay"
    );
    // Far embedding misses on distance (task-value axis).
    assert!(
        replay.try_replay("q", &emb_far(), 0.99, &cfg).is_none(),
        "far query must not replay"
    );
}

#[test]
fn replay_refuses_flagged_queries() {
    let (replay, store) = replay_with_store();
    record_a(&replay, &store);
    let cfg = ReplayConfig {
        enabled: true,
        ..ReplayConfig::calibrated()
    };
    // The write-path scrubber flags this input (PII pattern), so the
    // must-NOT-replay control group holds even at cosine 1.0.
    let flagged = "contact me at ssn 123-45-6789 tomorrow";
    assert!(
        crate::ledger_guard::scrub_for_ledger(flagged).flagged,
        "probe assumes the scrubber flags this input"
    );
    assert!(
        replay.try_replay(flagged, &emb_a(), 0.99, &cfg).is_none(),
        "flagged queries never replay"
    );
}

#[test]
fn reuse_failures_demote() {
    let (replay, store) = replay_with_store();
    record_a(&replay, &store);
    let cfg = ReplayConfig {
        enabled: true,
        ..ReplayConfig::calibrated()
    };
    assert!(replay.try_replay("q", &emb_a_para(), 0.99, &cfg).is_some());
    replay.note_reuse_outcome("audit-a", false);
    replay.note_reuse_outcome("audit-a", false);
    assert!(
        replay.try_replay("q", &emb_a_para(), 0.99, &cfg).is_none(),
        "failures demote below N"
    );
}

#[test]
fn materialize_stamps_local_model_and_lod0_refs() {
    let (replay, store) = replay_with_store();
    record_a(&replay, &store);
    let cfg = ReplayConfig {
        enabled: true,
        ..ReplayConfig::calibrated()
    };
    let hit = replay.try_replay("q", &emb_a_para(), 0.99, &cfg).expect("hit");
    let nodes = replay.materialize(&hit, "sess-1", 4242);
    assert_eq!(nodes.len(), 2);
    for node in &nodes {
        assert!(!node.lod.is_empty() && !node.lod[0].is_empty(), "LOD0 travels");
        let meta = node.metadata.as_ref().expect("replay metadata");
        assert_eq!(meta.get("origin").and_then(|v| v.as_str()), Some("workflow_replay"));
        assert_eq!(meta.get("audit_id").and_then(|v| v.as_str()), Some("audit-a"));
        assert_eq!(
            meta.get("query_content_hash").and_then(serde_json::Value::as_u64),
            Some(4242),
            "replay references LOD0, never a summary"
        );
        let claims: Vec<fluent_types::AnnotationClaim> =
            serde_json::from_value(meta.get("replay_claims").cloned().unwrap_or_default())
                .expect("claims decode");
        assert!(!claims.is_empty());
        for claim in claims {
            assert!(
                claim.tier <= fluent_types::ProvenanceTier::LocalModel,
                "replayed claims never outrank LocalModel, got {:?}",
                claim.tier
            );
            assert_eq!(claim.produced_by, "workflow_replay");
        }
    }
}

#[tokio::test]
async fn resolve_prefers_replay_then_walks_rungs_in_order() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (replay, store) = replay_with_store();
    record_a(&replay, &store);
    let cfg = ReplayConfig {
        enabled: true,
        ..ReplayConfig::calibrated()
    };
    // Replay hit: fresh rungs never run (replay is cheaper than any rung).
    let calls = Arc::new(AtomicUsize::new(0));
    let out: Result<Resolution<String>, String> = replay
        .resolve("q", &emb_a_para(), 0.99, &cfg, ["fresh-a", "fresh-b"], {
            let calls = Arc::clone(&calls);
            move |rung: &str| {
                calls.fetch_add(1, Ordering::SeqCst);
                let rung = rung.to_string();
                async move { Ok(Some(format!("fresh:{rung}"))) }
            }
        }, |_: &String| false)
        .await;
    assert!(
        matches!(&out, Ok(Resolution::Replayed(h)) if h.audit_id == "audit-a"),
        "replay wins: {out:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no fresh rung ran");

    // Replay miss (far query): the fresh walk runs in order, first Ok wins —
    // the canonical combinator, never a hand-rolled loop.
    let out: Result<Resolution<String>, String> = replay
        .resolve("q", &emb_far(), 0.99, &cfg, ["fresh-a", "fresh-b"], |rung: &str| {
            let rung = rung.to_string();
            async move {
                if rung == "fresh-a" {
                    Ok(None)
                } else {
                    Ok(Some("fresh:b".to_string()))
                }
            }
        }, |_: &String| false)
        .await;
    assert!(
        matches!(&out, Ok(Resolution::Fresh(Some(s))) if s == "fresh:b"),
        "ladder order: {out:?}"
    );
}

#[test]
fn similarity_bands_hold_for_fixture() {
    use search_vector::math::cosine_similarity_f32;
    let para = cosine_similarity_f32(&emb_a(), &emb_a_para());
    let adv = cosine_similarity_f32(&emb_a(), &emb_adversarial());
    let far = cosine_similarity_f32(&emb_a(), &emb_far());
    let cross = cosine_similarity_f32(&emb_a(), &emb_b());
    assert!(para > 0.97, "paraphrase near-identical: {para}");
    assert!(adv > 0.85 && adv < para, "adversarial near but below paraphrase: {adv}");
    assert!(far < 0.5, "far is distant: {far}");
    assert!(cross < 0.5, "other intent is distant: {cross}");
}

fn recorder_with(
    enabled: bool,
    embedder: Option<std::sync::Arc<dyn fluent_llm::EmbeddingProvider>>,
) -> (WorkflowRecorder, std::sync::Arc<InMemoryWorkflowStore>) {
    let store = std::sync::Arc::new(InMemoryWorkflowStore::new());
    let cfg = ReplayConfig {
        enabled,
        ..ReplayConfig::default()
    };
    let recorder = WorkflowRecorder::new(
        std::sync::Arc::clone(&store) as std::sync::Arc<dyn WorkflowStore>,
        embedder,
        cfg,
    );
    (recorder, store)
}

fn hash_embedder() -> std::sync::Arc<dyn fluent_llm::EmbeddingProvider> {
    std::sync::Arc::new(crate::test_stubs::HashEmbedder::new(64))
}

fn question_run() -> (Vec<String>, Vec<String>, String) {
    (
        vec!["hypothetical one".into(), "hypothetical two".into()],
        vec!["first answer".into(), "second answer".into()],
        "assembled solution".into(),
    )
}

#[test]
fn recorder_stores_successful_frontier_run_once() {
    let (recorder, store) = recorder_with(true, Some(hash_embedder()));
    let (hyps, answers, assembled) = question_run();
    let outcome = recorder.record_question_run(
        "how do i bake bread",
        &hyps,
        &answers,
        &assembled,
        0.9,
        true,
        "audit-q",
        Some("sess"),
    );
    assert_eq!(outcome, RecordOutcome::Stored);
    assert_eq!(store.nearest(&hash_embedder().embed("how do i bake bread").unwrap(), 3).len(), 1);
}

#[test]
fn recorder_is_noop_when_disabled() {
    let (recorder, store) = recorder_with(false, Some(hash_embedder()));
    let (hyps, answers, assembled) = question_run();
    let outcome = recorder.record_question_run(
        "how do i bake bread",
        &hyps,
        &answers,
        &assembled,
        0.9,
        true,
        "audit-q",
        Some("sess"),
    );
    assert_eq!(outcome, RecordOutcome::RejectedGate);
    assert!(store.nearest(&emb_a(), 3).is_empty());
}

#[test]
fn recorder_is_noop_when_write_gate_rejects() {
    // Accepted by the scorer (verified rung) but below the calibrated [A]
    // floor — the write gate refuses.
    let (recorder, store) = recorder_with(true, Some(hash_embedder()));
    let (hyps, answers, assembled) = question_run();
    let outcome = recorder.record_question_run(
        "how do i bake bread",
        &hyps,
        &answers,
        &assembled,
        0.75,
        true,
        "audit-q",
        Some("sess"),
    );
    assert_eq!(outcome, RecordOutcome::RejectedGate);
    assert!(store.nearest(&emb_a(), 3).is_empty());
}

#[test]
fn recorder_is_noop_without_embedder() {
    let (recorder, store) = recorder_with(true, None);
    let (hyps, answers, assembled) = question_run();
    let outcome = recorder.record_question_run(
        "how do i bake bread",
        &hyps,
        &answers,
        &assembled,
        0.9,
        true,
        "audit-q",
        Some("sess"),
    );
    assert_eq!(outcome, RecordOutcome::RejectedGate);
    assert!(store.nearest(&emb_a(), 3).is_empty());
}

#[test]
fn recorded_run_replays_for_near_neighbor_not_dissimilar() {
    let (recorder, _) = recorder_with(true, Some(hash_embedder()));
    let (hyps, answers, assembled) = question_run();
    let outcome = recorder.record_question_run(
        "how do i bake bread",
        &hyps,
        &answers,
        &assembled,
        0.9,
        true,
        "audit-q",
        Some("sess"),
    );
    assert_eq!(outcome, RecordOutcome::Stored);
    recorder.replay().note_reuse_outcome("audit-q", true);
    recorder.replay().note_reuse_outcome("audit-q", true);

    let cfg = ReplayConfig::calibrated();
    let embedder = hash_embedder();
    // Near-neighbor (identical text → identical embedding) replays.
    let near_emb = embedder.embed("how do i bake bread").unwrap();
    let hit = recorder
        .replay()
        .try_replay("how do i bake bread", &near_emb, 0.99, &cfg)
        .expect("near-neighbor replays");
    assert_eq!(hit.audit_id, "audit-q");
    // Dissimilar query does not.
    let far_emb = embedder.embed("quantum chromodynamics lecture notes").unwrap();
    assert!(
        recorder
            .replay()
            .try_replay("quantum chromodynamics lecture notes", &far_emb, 0.99, &cfg)
            .is_none(),
        "dissimilar query must not replay"
    );
}
