use super::*;
use fluent_concept::InMemoryConceptStore;
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

fn res(id: i64) -> Resolution {
    Resolution {
        chosen_candidate_id: pid(id),
        detail: "chose sense".into(),
    }
}

#[test]
fn record_and_query_roundtrip_per_ambiguity_kind() {
    let (_store, idx) = open_index();
    let p = pid(7);
    assert!(idx
        .preferred_sense(p, AmbiguityKind::PredicatePolysemy)
        .is_none());
    idx.record_preferred_sense(p, AmbiguityKind::PredicatePolysemy, res(1))
        .expect("record");
    let back = idx
        .preferred_sense(p, AmbiguityKind::PredicatePolysemy)
        .expect("query");
    assert_eq!(back, res(1));
    // A different ambiguity kind is a distinct pattern.
    assert!(idx
        .preferred_sense(p, AmbiguityKind::AttachmentNearTie)
        .is_none());
    // A different predicate is a distinct pattern.
    assert!(idx
        .preferred_sense(pid(8), AmbiguityKind::PredicatePolysemy)
        .is_none());
}

#[test]
fn sense_rows_do_not_collide_with_correction_rows() {
    let (store, idx) = open_index();
    let p = pid(9);
    idx.record_preferred_sense(p, AmbiguityKind::PredicatePolysemy, res(2))
        .expect("sense");

    // A correction-cache row for the same predicate uses role='correction'
    // and must not be visible as a sense resolution.
    use crate::ledger::correction_index::upsert_correction_row;
    use crate::ledger::correction_index::CorrectionRow;
    store
        .with_conn(|conn| {
            upsert_correction_row(
                conn,
                &CorrectionRow {
                    lemma_id: p.as_i64(),
                    entity_id: 0,
                    corrections_json: "[]".into(),
                },
            )
        })
        .expect("correction row");

    assert!(idx
        .preferred_sense(p, AmbiguityKind::PredicatePolysemy)
        .is_some());
    // The role columns are distinct — no accidental collision.
    let count: i64 = store
        .query_row(
            "SELECT COUNT(*) FROM interlingua_index WHERE node_id = ?1 AND interlingua_id = ?2",
            params![PATTERN_NODE, p.as_i64()],
            |r| r.get(0),
        )
        .expect("count")
        .expect("row");
    assert_eq!(count, 2, "one sense row + one correction row");
}

#[test]
fn record_upserts_instead_of_duplicating() {
    let (store, idx) = open_index();
    let p = pid(3);
    idx.record_preferred_sense(p, AmbiguityKind::NegationModalScope, res(4))
        .expect("first");
    idx.record_preferred_sense(p, AmbiguityKind::NegationModalScope, res(5))
        .expect("second");
    let count: i64 = store
        .query_row(
            "SELECT COUNT(*) FROM interlingua_index WHERE node_id = ?1 AND role = ?2",
            params![PATTERN_NODE, SENSE_ROLE],
            |r| r.get(0),
        )
        .expect("count")
        .expect("row");
    assert_eq!(count, 1, "upsert never duplicates");
}

#[tokio::test]
async fn wave_fires_one_fetch_and_promotes_valid_sense_only() {
    let (_store, idx) = open_index();
    let index: Arc<dyn PreferredSenseIndex> = Arc::new(idx);
    let fetched = Arc::new(AtomicUsize::new(0));
    let f = Arc::clone(&fetched);
    let fetch: FrameResolutionFetch = Arc::new(move |_reqs| {
        f.fetch_add(1, Ordering::SeqCst);
        Ok(r#"[
            {"chosen_candidate_id": 281474976710656, "detail": "a"},
            {"chosen_candidate_id": 281474976710657, "detail": "b"}
        ]"#
        .into())
    });

    let gate = FrameResolutionGate::open();
    let worker = Arc::new(FrameResolutionWorker::new(
        &index,
        &fetch,
        "frame-model".into(),
        8,
        4,
        tokio_runtime(),
        &gate,
    ));

    let p1 = pid(1);
    let p2 = pid(2);
    let c1 = InterlinguaId::from_u64(281474976710656);
    let c2 = InterlinguaId::from_u64(281474976710657);
    let job = FrameResolutionJob {
        requests: vec![
            FrameResolutionRequest {
                predicate_lemma_id: p1,
                ambiguity_kind: AmbiguityKind::PredicatePolysemy,
                detail: "p1 polysemy".into(),
                candidate_ids: vec![c1, c2],
            },
            FrameResolutionRequest {
                predicate_lemma_id: p2,
                ambiguity_kind: AmbiguityKind::AttachmentNearTie,
                detail: "p2 tie".into(),
                candidate_ids: vec![],
            },
        ],
    };
    worker.enqueue(job).await.expect("enqueue");
    // Give the worker a moment to process the (single, batched) job.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    worker.drain().await;

    assert_eq!(fetched.load(Ordering::SeqCst), 1, "N frames → one fetch");
    assert_eq!(
        index.preferred_sense(p1, AmbiguityKind::PredicatePolysemy),
        Some(Resolution {
            chosen_candidate_id: c1,
            detail: "a".into(),
        }),
        "valid sense choice promotes p1"
    );
    assert!(
        index
            .preferred_sense(p2, AmbiguityKind::AttachmentNearTie)
            .is_none(),
        "tie carries no sense choice — p2 never promotes"
    );
}

#[tokio::test]
async fn fully_resolved_wave_skips_the_fetch() {
    let (_store, idx) = open_index();
    let index: Arc<dyn PreferredSenseIndex> = Arc::new(idx);
    let p = pid(1);
    index
        .record_preferred_sense(p, AmbiguityKind::PredicatePolysemy, res(9))
        .expect("seed");

    let fetched = Arc::new(AtomicUsize::new(0));
    let f = Arc::clone(&fetched);
    let fetch: FrameResolutionFetch = Arc::new(move |_r| {
        f.fetch_add(1, Ordering::SeqCst);
        Err("should not be called".into())
    });
    let gate = FrameResolutionGate::open();
    let worker = Arc::new(FrameResolutionWorker::new(
        &index,
        &fetch,
        "frame-model".into(),
        8,
        4,
        tokio_runtime(),
        &gate,
    ));

    worker
        .enqueue(FrameResolutionJob {
            requests: vec![FrameResolutionRequest {
                predicate_lemma_id: p,
                ambiguity_kind: AmbiguityKind::PredicatePolysemy,
                detail: "already resolved".into(),
                candidate_ids: vec![],
            }],
        })
        .await
        .expect("enqueue");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    worker.drain().await;
    assert_eq!(
        fetched.load(Ordering::SeqCst),
        0,
        "a fully-resolved wave never fires the fetch"
    );
}

#[test]
fn parse_naive_handles_single_object_and_garbage() {
    let single = parse_naive(r#"{"chosen_candidate_id":5,"detail":"x"}"#, 3);
    assert_eq!(single.len(), 3);
    assert_eq!(single[0].chosen_candidate_id.as_i64(), 5);
    let garbage = parse_naive("nonsense", 2);
    assert_eq!(garbage.len(), 2);
}

#[test]
fn trait_is_implementable_over_hermetic_store() {
    // Compile-check: the router impl satisfies the ledger-owner trait
    // (M2a: moved from spacy-rs to `ledger::frame_index`).
    let (_store, idx) = open_index();
    let _: &dyn PreferredSenseIndex = &idx;
    // And a hermetic ConceptStore still works (the trait's error type).
    let _mem = InMemoryConceptStore::new();
}

fn polysemy_request(predicate: i64, candidates: Vec<InterlinguaId>) -> FrameResolutionRequest {
    FrameResolutionRequest {
        predicate_lemma_id: pid(predicate),
        ambiguity_kind: AmbiguityKind::PredicatePolysemy,
        detail: "polysemy".into(),
        candidate_ids: candidates,
    }
}

/// Validator-failure controls never promote: the zero placeholder, a choice
/// outside the offered candidates, and an unparseable model reply all leave
/// the index untouched (the fetch still fires exactly once — the wave
/// batching is unchanged, only promotion is gated).
#[tokio::test]
async fn validator_failures_never_promote() {
    let (_store, idx) = open_index();
    let index: Arc<dyn PreferredSenseIndex> = Arc::new(idx);
    let fetched = Arc::new(AtomicUsize::new(0));
    let f = Arc::clone(&fetched);
    // One zero placeholder, one foreign id, one garbage reply shape handled
    // by lanes below: here the batch returns a zero id and a foreign id.
    let fetch: FrameResolutionFetch = Arc::new(move |_reqs| {
        f.fetch_add(1, Ordering::SeqCst);
        Ok(r#"[
            {"chosen_candidate_id": 0, "detail": "placeholder"},
            {"chosen_candidate_id": 999, "detail": "hallucinated"}
        ]"#
        .into())
    });
    let gate = FrameResolutionGate::open();
    let worker = Arc::new(FrameResolutionWorker::new(
        &index,
        &fetch,
        "frame-model".into(),
        8,
        4,
        tokio_runtime(),
        &gate,
    ));

    let c1 = InterlinguaId::from_u64(101);
    let c2 = InterlinguaId::from_u64(102);
    worker
        .enqueue(FrameResolutionJob {
            requests: vec![
                polysemy_request(11, vec![c1, c2]),
                polysemy_request(12, vec![c1, c2]),
            ],
        })
        .await
        .expect("enqueue");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    worker.drain().await;

    assert_eq!(
        fetched.load(Ordering::SeqCst),
        1,
        "wave still batches one fetch"
    );
    assert!(
        index
            .preferred_sense(pid(11), AmbiguityKind::PredicatePolysemy)
            .is_none(),
        "zero placeholder never promotes"
    );
    assert!(
        index
            .preferred_sense(pid(12), AmbiguityKind::PredicatePolysemy)
            .is_none(),
        "foreign id never promotes"
    );
}

/// An unparseable model reply degrades to placeholder resolutions, which the
/// promotion gate refuses — garbage never persists.
#[tokio::test]
async fn unparseable_reply_never_promotes() {
    let (_store, idx) = open_index();
    let index: Arc<dyn PreferredSenseIndex> = Arc::new(idx);
    let fetch: FrameResolutionFetch = Arc::new(|_reqs| Ok("nonsense".into()));
    let gate = FrameResolutionGate::open();
    let worker = Arc::new(FrameResolutionWorker::new(
        &index,
        &fetch,
        "frame-model".into(),
        8,
        4,
        tokio_runtime(),
        &gate,
    ));

    worker
        .enqueue(FrameResolutionJob {
            requests: vec![polysemy_request(
                21,
                vec![InterlinguaId::from_u64(201), InterlinguaId::from_u64(202)],
            )],
        })
        .await
        .expect("enqueue");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    worker.drain().await;

    assert!(
        index
            .preferred_sense(pid(21), AmbiguityKind::PredicatePolysemy)
            .is_none(),
        "placeholder resolutions from garbage output never promote"
    );
}

/// A repeated ambiguity pattern makes zero model calls on its second
/// occurrence: the first wave promotes, the second is served from the index.
#[tokio::test]
async fn repeated_pattern_makes_zero_model_calls_on_second_occurrence() {
    let (_store, idx) = open_index();
    let index: Arc<dyn PreferredSenseIndex> = Arc::new(idx);
    let fetched = Arc::new(AtomicUsize::new(0));
    let f = Arc::clone(&fetched);
    let fetch: FrameResolutionFetch = Arc::new(move |_reqs| {
        f.fetch_add(1, Ordering::SeqCst);
        Ok(r#"[{"chosen_candidate_id": 301, "detail": "first resolution"}]"#.into())
    });
    let gate = FrameResolutionGate::open();
    let worker = Arc::new(FrameResolutionWorker::new(
        &index,
        &fetch,
        "frame-model".into(),
        8,
        4,
        tokio_runtime(),
        &gate,
    ));

    let candidates = vec![InterlinguaId::from_u64(301), InterlinguaId::from_u64(302)];
    let job = || FrameResolutionJob {
        requests: vec![polysemy_request(31, candidates.clone())],
    };
    worker.enqueue(job()).await.expect("first enqueue");
    // Wait for the first wave to promote (deadline-poll: the second wave
    // must observe the established sense).
    let mut promoted = false;
    for _ in 0..200 {
        if index
            .preferred_sense(pid(31), AmbiguityKind::PredicatePolysemy)
            .is_some()
        {
            promoted = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(promoted, "first wave promotes");
    assert_eq!(fetched.load(Ordering::SeqCst), 1);

    worker.enqueue(job()).await.expect("second enqueue");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    worker.drain().await;

    assert_eq!(
        fetched.load(Ordering::SeqCst),
        1,
        "second occurrence is served from the index — zero model calls"
    );
    assert_eq!(
        index
            .preferred_sense(pid(31), AmbiguityKind::PredicatePolysemy)
            .expect("replay")
            .chosen_candidate_id,
        InterlinguaId::from_u64(301)
    );
}

/// A closed runtime gate drops jobs before any index read, fetch, or
/// promotion write — observationally identical to no worker.
#[tokio::test]
async fn closed_gate_drops_jobs_before_fetch_or_promote() {
    let (_store, idx) = open_index();
    let index: Arc<dyn PreferredSenseIndex> = Arc::new(idx);
    let fetched = Arc::new(AtomicUsize::new(0));
    let f = Arc::clone(&fetched);
    let fetch: FrameResolutionFetch = Arc::new(move |_reqs| {
        f.fetch_add(1, Ordering::SeqCst);
        Ok(r#"[{"chosen_candidate_id": 401, "detail": "x"}]"#.into())
    });
    let gate = FrameResolutionGate::open();
    gate.set_open(false);
    assert!(!gate.is_open());
    let worker = Arc::new(FrameResolutionWorker::new(
        &index,
        &fetch,
        "frame-model".into(),
        8,
        4,
        tokio_runtime(),
        &gate,
    ));

    worker
        .enqueue(FrameResolutionJob {
            requests: vec![polysemy_request(41, vec![InterlinguaId::from_u64(401)])],
        })
        .await
        .expect("enqueue still accepted");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    worker.drain().await;

    assert_eq!(fetched.load(Ordering::SeqCst), 0, "closed gate: no fetch");
    assert!(
        index
            .preferred_sense(pid(41), AmbiguityKind::PredicatePolysemy)
            .is_none(),
        "closed gate: no promotion"
    );
}

/// Model output never overwrites an already established sense: when a
/// concurrent wave (or a golden seed) establishes sense 501 mid-flight, this
/// wave's conflicting choice (502) is refused at promotion time, so the
/// replayed sense — and the routing decision downstream of it — is stable.
#[tokio::test]
async fn established_sense_survives_conflicting_model_output() {
    let (_store, idx) = open_index();
    let index: Arc<dyn PreferredSenseIndex> = Arc::new(idx);
    let concurrent = Arc::clone(&index);
    let fetch: FrameResolutionFetch = Arc::new(move |_reqs| {
        // Simulate the concurrent wave winning the race: it establishes 501
        // while this wave is still awaiting its own model reply.
        concurrent
            .record_preferred_sense(pid(51), AmbiguityKind::PredicatePolysemy, res(501))
            .expect("concurrent promote");
        Ok(r#"[{"chosen_candidate_id": 502, "detail": "conflicting"}]"#.into())
    });
    let gate = FrameResolutionGate::open();
    let worker = Arc::new(FrameResolutionWorker::new(
        &index,
        &fetch,
        "frame-model".into(),
        8,
        4,
        tokio_runtime(),
        &gate,
    ));

    worker
        .enqueue(FrameResolutionJob {
            requests: vec![polysemy_request(
                51,
                vec![InterlinguaId::from_u64(501), InterlinguaId::from_u64(502)],
            )],
        })
        .await
        .expect("enqueue");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    worker.drain().await;

    assert_eq!(
        index
            .preferred_sense(pid(51), AmbiguityKind::PredicatePolysemy)
            .expect("established sense replays")
            .chosen_candidate_id,
        pid(501),
        "the concurrent wave's sense wins; the conflicting choice is refused"
    );
}
