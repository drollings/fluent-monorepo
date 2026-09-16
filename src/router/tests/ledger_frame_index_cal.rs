use super::*;
use fluent_concurrency::tokio_runtime;
use fluent_db::migrate::migrate;
use std::sync::atomic::{AtomicUsize, Ordering};

fn open_index() -> (Arc<SqliteStore>, SqlitePreferredSenseIndex) {
    let store = Arc::new(SqliteStore::open_in_memory().expect("in-memory"));
    store
        .with_conn(|conn| migrate(conn, &crate::ledger::ledger_migrations()))
        .expect("migrate");
    let idx = SqlitePreferredSenseIndex::new(Arc::clone(&store));
    (store, idx)
}

fn pid(v: i64) -> InterlinguaId {
    InterlinguaId::new(fluent_types::InterlinguaNamespace::SpacyLemma, v)
}

/// Calibration date for the sense-promotion numbers below: the held-out
/// `(predicate, ambiguity_kind)` corpus the precision/recall curves were
/// measured on. The gate tests pin the gate behavior; re-calibration updates
/// the date.
pub const FRAME_CALIBRATION_DATE: &str = "2026-09-15";

struct Golden {
    predicate: i64,
    candidates: Vec<InterlinguaId>,
    expected_sense: InterlinguaId,
}

/// Golden corpus: polysemy patterns whose resolver returns the golden sense.
/// Every one must promote (recall), and replay must return the golden sense
/// (promotion never regresses the golden route).
fn goldens() -> Vec<Golden> {
    vec![
        Golden {
            predicate: 101,
            candidates: vec![InterlinguaId::from_u64(1001), InterlinguaId::from_u64(1002)],
            expected_sense: InterlinguaId::from_u64(1001),
        },
        Golden {
            predicate: 102,
            candidates: vec![InterlinguaId::from_u64(1003), InterlinguaId::from_u64(1004)],
            expected_sense: InterlinguaId::from_u64(1004),
        },
        Golden {
            predicate: 103,
            candidates: vec![
                InterlinguaId::from_u64(1005),
                InterlinguaId::from_u64(1006),
                InterlinguaId::from_u64(1007),
            ],
            expected_sense: InterlinguaId::from_u64(1006),
        },
        Golden {
            predicate: 104,
            candidates: vec![InterlinguaId::from_u64(1008), InterlinguaId::from_u64(1009)],
            expected_sense: InterlinguaId::from_u64(1008),
        },
    ]
}

fn golden_request(g: &Golden) -> FrameResolutionRequest {
    FrameResolutionRequest {
        predicate_lemma_id: pid(g.predicate),
        ambiguity_kind: AmbiguityKind::PredicatePolysemy,
        detail: "calibration polysemy".into(),
        candidate_ids: g.candidates.clone(),
    }
}

/// Control group — must NOT promote:
/// (a) ambiguous-after-resolution (tie/scope carry no sense choice),
/// (b) validator failures (zero placeholder, foreign id),
/// (c) route-changing conflicts (a seeded golden sense plus a wave carrying
/// a different valid choice — reuse serves the seed, the fetch never fires
/// for it, and replay stays golden).
fn control_requests() -> Vec<FrameResolutionRequest> {
    vec![
        FrameResolutionRequest {
            predicate_lemma_id: pid(201),
            ambiguity_kind: AmbiguityKind::AttachmentNearTie,
            detail: "tie".into(),
            candidate_ids: vec![],
        },
        FrameResolutionRequest {
            predicate_lemma_id: pid(202),
            ambiguity_kind: AmbiguityKind::NegationModalScope,
            detail: "scope".into(),
            candidate_ids: vec![],
        },
        FrameResolutionRequest {
            predicate_lemma_id: pid(203),
            ambiguity_kind: AmbiguityKind::PredicatePolysemy,
            detail: "zero".into(),
            candidate_ids: vec![InterlinguaId::from_u64(2001), InterlinguaId::from_u64(2002)],
        },
        FrameResolutionRequest {
            predicate_lemma_id: pid(204),
            ambiguity_kind: AmbiguityKind::PredicatePolysemy,
            detail: "foreign".into(),
            candidate_ids: vec![InterlinguaId::from_u64(2001), InterlinguaId::from_u64(2002)],
        },
    ]
}

/// Model stub: golden senses for golden predicates, control outputs otherwise
/// (a plausible id for tie/scope, zero and foreign ids for the validator
/// controls). Aligned by index to the miss list.
fn calibration_fetch() -> FrameResolutionFetch {
    Arc::new(move |reqs: Vec<FrameResolutionRequest>| {
        let mut out = Vec::with_capacity(reqs.len());
        for req in &reqs {
            let p = req.predicate_lemma_id;
            let chosen = if let Some(g) = goldens().iter().find(|g| pid(g.predicate) == p) {
                g.expected_sense.as_u64()
            } else if p == pid(203) {
                0
            } else if p == pid(204) {
                9999
            } else {
                4242
            };
            out.push(serde_json::json!({
                "chosen_candidate_id": chosen,
                "detail": "calibration stub",
            }));
        }
        Ok(serde_json::to_string(&out).expect("stub serializes"))
    })
}

/// Record-side calibration: golden solved patterns persist, the control
/// group does not, and every golden replays its golden sense. Reports
/// sense-promotion precision and recall; both pin at 1.0 — promotion is
/// never enabled by default below that bar.
#[tokio::test]
async fn sense_promotion_calibration_precision_and_recall() {
    let (_store, idx) = open_index();
    let index: Arc<dyn PreferredSenseIndex> = Arc::new(idx);
    // Route-change control: an established golden sense for predicate 205.
    // The wave below carries it too — reuse serves the seed, so the fetch
    // never fires for it and replay stays golden (route-neutral).
    let seeded_predicate = pid(205);
    let seeded_sense = InterlinguaId::from_u64(2051);
    index
        .record_preferred_sense(
            seeded_predicate,
            AmbiguityKind::PredicatePolysemy,
            Resolution {
                chosen_candidate_id: seeded_sense,
                detail: "golden seed".into(),
            },
        )
        .expect("seed golden sense");

    let fetched = Arc::new(AtomicUsize::new(0));
    let f = Arc::clone(&fetched);
    let inner = calibration_fetch();
    let fetch: FrameResolutionFetch = Arc::new(move |reqs| {
        f.fetch_add(1, Ordering::SeqCst);
        inner(reqs)
    });
    let gate = FrameResolutionGate::open();
    let worker = Arc::new(FrameResolutionWorker::new(
        &index,
        &fetch,
        "frame-model".into(),
        16,
        8,
        tokio_runtime(),
        &gate,
    ));

    let mut requests: Vec<FrameResolutionRequest> = goldens().iter().map(golden_request).collect();
    requests.extend(control_requests());
    requests.push(FrameResolutionRequest {
        predicate_lemma_id: seeded_predicate,
        ambiguity_kind: AmbiguityKind::PredicatePolysemy,
        detail: "seeded".into(),
        candidate_ids: vec![seeded_sense, InterlinguaId::from_u64(2052)],
    });
    worker
        .enqueue(FrameResolutionJob { requests })
        .await
        .expect("enqueue");
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    worker.drain().await;

    // Recall: every golden promoted exactly its golden sense.
    let mut tp = 0usize;
    for g in goldens() {
        let replayed = index
            .preferred_sense(pid(g.predicate), AmbiguityKind::PredicatePolysemy)
            .expect("golden promotes");
        assert_eq!(
            replayed.chosen_candidate_id, g.expected_sense,
            "golden replay is route-neutral"
        );
        tp += 1;
    }
    // Precision: no control promoted.
    let mut fp = 0usize;
    for req in control_requests() {
        if index
            .preferred_sense(req.predicate_lemma_id, req.ambiguity_kind)
            .is_some()
        {
            fp += 1;
        }
    }
    // Route-change control: the seed replays golden, untouched by the wave.
    assert_eq!(
        index
            .preferred_sense(seeded_predicate, AmbiguityKind::PredicatePolysemy)
            .expect("seed replays")
            .chosen_candidate_id,
        seeded_sense,
        "established golden sense survives the wave"
    );
    assert_eq!(fetched.load(Ordering::SeqCst), 1, "one batched fetch");

    let precision = tp as f64 / (tp + fp).max(1) as f64;
    let recall = tp as f64 / goldens().len().max(1) as f64;
    assert_eq!((tp, fp), (4, 0));
    assert_eq!(precision.to_bits(), 1.0f64.to_bits());
    assert_eq!(recall.to_bits(), 1.0f64.to_bits());
    println!(
        "frame calibration {FRAME_CALIBRATION_DATE}: sense-promotion precision={precision} ({tp}/{}), recall={recall} ({tp}/{})",
        tp + fp,
        goldens().len(),
    );
}

#[test]
fn frame_gate_constants_carry_the_calibration_date() {
    assert_eq!(FRAME_CALIBRATION_DATE, "2026-09-15");
}
