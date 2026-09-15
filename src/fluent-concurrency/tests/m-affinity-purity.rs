//! `effective_priority` purity + priority-math goldens.
//!
//! `effective_priority` is compute-only: repeated/junk calls must leave the
//! LRU exactly as they found it (no `put`), so a twin scheduler that never
//! calls it behaves identically. The affinity bonus measures
//! session-history value (stickiness reduces context switching), not parse
//! confidence — the math below pins the bonus as an additive term on top
//! of the recorded base, never as a correctness signal.

use super::*;
use crate::tokio_runtime;

fn pool() -> Arc<PriorityResultPool<String, String, String>> {
    let runtime = tokio_runtime();
    Arc::new(PriorityResultPool::new(
        Arc::clone(&runtime),
        1,
        move |identity: String| async move { Ok::<String, String>(identity) },
    ))
}

fn task_at(identity: &str, age_secs: u64, runtime: &Arc<dyn fluent_wvr::Runtime>) -> ScheduledTask<String> {
    ScheduledTask {
        identity: identity.into(),
        task: identity.into(),
        enqueued_at: runtime
            .now()
            .checked_sub(std::time::Duration::from_secs(age_secs))
            .expect("test clock supports aged tasks"),
    }
}

#[tokio::test]
async fn effective_priority_leaves_lru_untouched() {
    let sched_a = AffinityScheduler::new(pool());
    let sched_b = AffinityScheduler::new(pool());
    sched_a.set_affinity(Some("s".into()));
    sched_b.set_affinity(Some("s".into()));

    let rt = tokio_runtime();
    let t1 = task_at("s", 0, &rt);
    assert_eq!(sched_a.effective_priority(&t1, 5), 15);
    assert_eq!(sched_b.effective_priority(&t1, 5), 15);

    // Junk queries on B must not perturb any observable state.
    for _ in 0..10 {
        let junk = task_at("junk", 0, &rt);
        let _ = sched_b.effective_priority(&junk, 99);
    }
    assert_eq!(sched_b.base_priorities_len(), sched_a.base_priorities_len());
    assert_eq!(sched_b.base_priorities_len(), 0);

    // Both schedulers still agree afterwards (twin behavior identical).
    let t2 = task_at("other", 0, &rt);
    assert_eq!(
        sched_a.effective_priority(&t2, 7),
        sched_b.effective_priority(&t2, 7)
    );
}

#[tokio::test]
async fn priority_math_goldens() {
    // Default AgingConfig: bonus 10, rate 2 (age cap), max 100.
    let sched = AffinityScheduler::new(pool());
    sched.set_affinity(Some("s".into()));
    let rt = tokio_runtime();

    // Affine, fresh: base 5 recorded, 5 + bonus 10 returned.
    assert_eq!(sched.effective_priority(&task_at("s", 0, &rt), 5), 15);
    // Non-affine, fresh: no bonus.
    assert_eq!(sched.effective_priority(&task_at("o", 0, &rt), 5), 5);
    // Aged 20s non-affine: age bonus min(20, rate 2) = 2 → 5 + 2.
    assert_eq!(sched.effective_priority(&task_at("o", 20, &rt), 5), 7);
    // Aged affine: (5 + 2) + 10.
    assert_eq!(sched.effective_priority(&task_at("s", 20, &rt), 5), 17);
    // Pure reads only — nothing was recorded by any of the above.
    assert_eq!(sched.base_priorities_len(), 0);
}

#[tokio::test]
async fn submit_records_base_without_bonus() {
    // The recorded base excludes the affinity bonus: a later identical
    // submit computes from the recorded base, with the bonus re-applied
    // on top (values identical to the pre-change implementation).
    let sched = AffinityScheduler::new(pool());
    sched.set_affinity(Some("s".into()));
    let rt = tokio_runtime();
    sched.submit(task_at("s", 0, &rt), 5).await.unwrap();
    assert_eq!(sched.base_priorities_len(), 1);
    // Recorded 5 (no bonus baked in): recompute agrees with a fresh read.
    assert_eq!(sched.effective_priority(&task_at("s", 0, &rt), 5), 15);
}
