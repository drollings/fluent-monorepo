//! Parallel-wave batch test: independent targets run concurrently in one
//! `SupervisedBatch`, rendezvousing through shell markers.
//!
//! Soak tier (`make soak` / `make test-heavy`, never `make test`): a
//! shell-rendezvous stress with sleeps — textbook slow-gate material.

use std::sync::Arc;

use bitvec::vec::BitVec;
use fluent_concurrency::batch::{SupervisedBatch, SupervisedBatchSummary};
use fluent_concurrency::tokio_runtime;
use fluent_dag::target::CapabilityRegistry;
use fluent_dag::target::Target;
use fluent_dag::target_work_unit::TargetWorkUnit;
use fluent_types::{ExecutorKind, TargetType};
use internment::ArcIntern;
use tempfile::tempdir;

/// Parallel-wave variant: independent targets run concurrently in one
/// `SupervisedBatch`. Each writes an `arrived` marker then waits for all three to
/// arrive (a rendezvous barrier) before writing `done` — so every `done`
/// marker existing proves the three ran concurrently, not serially.
#[ignore = "heavy (~5s): shell-rendezvous concurrency stress with sleeps; run via `make test-heavy`"]
#[tokio::test]
async fn test_target_work_unit_batch_parallel_wave() {
    let caps = CapabilityRegistry::new();
    let dir = tempdir().unwrap();
    let dir_str = dir.path().to_string_lossy().into_owned();
    let mut batch = SupervisedBatch::new(tokio_runtime(), fluent_wvr::CapabilitySet::new());

    for i in 0..3 {
        let target = Target::new()
            .id(i)
            .name(ArcIntern::from(format!("worker_{i}")))
            .target_type(TargetType::File)
            .executor(ExecutorKind::Native)
            .depends(BitVec::new())
            .provides(BitVec::new())
            .command(format!(
                "touch {dir_str}/arrived_{i}; \
                 i=0; while [ $i -lt 200 ]; do \
                 c=$(ls {dir_str}/arrived_* 2>/dev/null | wc -l); \
                 [ \"$c\" -ge 3 ] && break; \
                 sleep 0.01; i=$((i+1)); done; \
                 touch {dir_str}/done_{i}"
            ))
            .build();
        let unit = TargetWorkUnit::from_target(&target, &caps);
        batch.register(Arc::new(unit)).unwrap();
    }

    let summary: SupervisedBatchSummary = (&mut batch).await;
    assert_eq!(summary.completed.len(), 3, "all workers complete");
    assert_eq!(summary.failed.len(), 0);
    assert_eq!(summary.panicked.len(), 0);
    assert_eq!(summary.cancelled.len(), 0);
    for i in 0..3 {
        assert!(
            dir.path().join(format!("done_{i}")).exists(),
            "worker {i} must have passed the rendezvous barrier"
        );
    }
}
