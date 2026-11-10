//! Embedding tests: order-preserving batches through the seam.

use std::sync::Arc;

use crate::diary::embed::{batch_embed, EmbedSeam, ScriptSeam, TEST_DIMS};

#[test]
fn batches_return_in_index_order() {
    let seam = Arc::new(ScriptSeam {
        script: vec![vec![1.0; TEST_DIMS], vec![0.0; TEST_DIMS]],
    });
    let segments = vec![vec![0.5; 100]; 6];
    let vectors = batch_embed(seam, &segments, 2).expect("pool embeds");
    assert_eq!(vectors.len(), 6);
    assert_eq!(vectors[0], vec![1.0; TEST_DIMS]);
    assert_eq!(vectors[1], vec![0.0; TEST_DIMS]);
    assert_eq!(vectors[2], vec![1.0; TEST_DIMS]);
}

#[test]
fn empty_segments_embed_to_empty() {
    let seam = Arc::new(ScriptSeam {
        script: vec![vec![1.0; TEST_DIMS]],
    });
    let vectors = batch_embed(seam, &[], 2).expect("empty batch succeeds");
    assert!(vectors.is_empty());
}

#[test]
fn zero_parallel_clamps_to_one_worker() {
    let seam = Arc::new(ScriptSeam {
        script: vec![vec![1.0; TEST_DIMS], vec![0.0; TEST_DIMS]],
    });
    let segments = vec![vec![0.5; 100]; 4];
    let vectors = batch_embed(seam, &segments, 0).expect("zero parallel clamps");
    assert_eq!(vectors.len(), 4);
    assert_eq!(vectors[0], vec![1.0; TEST_DIMS]);
    assert_eq!(vectors[1], vec![0.0; TEST_DIMS]);
    assert_eq!(vectors[2], vec![1.0; TEST_DIMS]);
    assert_eq!(vectors[3], vec![0.0; TEST_DIMS]);
}

#[test]
fn scripted_vectors_cycle_by_input_index() {
    let seam = Arc::new(ScriptSeam {
        script: vec![vec![1.0; TEST_DIMS], vec![2.0; TEST_DIMS], vec![3.0; TEST_DIMS]],
    });
    let segments = vec![vec![0.0; 8]; 7];
    let vectors = batch_embed(seam, &segments, 3).expect("pool embeds");
    for (index, vector) in vectors.iter().enumerate() {
        assert_eq!(*vector, vec![index as f32 % 3.0 + 1.0; TEST_DIMS]);
    }
}

/// A segment-0 embed that waits for segment 1 forces out-of-order
/// completion; results must still return in input order. The rendezvous
/// is latch-based: whichever job runs first, segment 1 always finishes
/// first, with no timing assumption.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delayed_completion_still_returns_index_order() {
    use std::sync::{Condvar, Mutex};
    struct LatchSeam {
        pair: Arc<(Mutex<bool>, Condvar)>,
        returned: Arc<Mutex<Vec<usize>>>,
    }
    impl EmbedSeam for LatchSeam {
        fn embed(&self, index: usize, _samples: &[f32]) -> Vec<f32> {
            let (flag, cvar) = &*self.pair;
            if index == 0 {
                let mut done = flag.lock().expect("latch");
                while !*done {
                    done = cvar.wait(done).expect("latch");
                }
            } else {
                *flag.lock().expect("latch") = true;
                cvar.notify_all();
            }
            self.returned.lock().expect("order").push(index);
            vec![index as f32; TEST_DIMS]
        }
    }
    let seam = Arc::new(LatchSeam {
        pair: Arc::new((Mutex::new(false), Condvar::new())),
        returned: Arc::new(Mutex::new(Vec::new())),
    });
    let segments = vec![vec![0.0; 8]; 2];
    let vectors = batch_embed(Arc::clone(&seam) as Arc<dyn EmbedSeam>, &segments, 2)
        .expect("latched batch succeeds");
    assert_eq!(vectors, vec![vec![0.0; TEST_DIMS], vec![1.0; TEST_DIMS]]);
    assert_eq!(
        *seam.returned.lock().expect("order"),
        vec![1, 0],
        "segment 1 completes before segment 0, results stay indexed"
    );
}

/// Barrier rendezvous across more segments than workers: every wave must
/// overlap (proving concurrency above one) while the atomic high-water
/// mark proves the pool never exceeds its cap. The latch carries a
/// timeout guard so a serialization regression fails instead of hanging;
/// the timeout never fires on the passing path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_workers_overlap_without_exceeding_the_cap() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};
    struct Latch {
        state: Mutex<(usize, u64)>,
        cvar: Condvar,
        size: usize,
    }
    impl Latch {
        /// Wait for the wave to fill. `true` on rendezvous, `false` when
        /// the guard expires (a serialization regression, never the pass path).
        fn wait(&self) -> bool {
            let mut guard = self.state.lock().expect("latch");
            let generation = guard.1;
            guard.0 += 1;
            if guard.0 == self.size {
                guard.0 = 0;
                guard.1 += 1;
                self.cvar.notify_all();
                return true;
            }
            loop {
                let (next, result) = self
                    .cvar
                    .wait_timeout(guard, std::time::Duration::from_secs(30))
                    .expect("latch");
                guard = next;
                if guard.1 != generation {
                    return true;
                }
                if result.timed_out() {
                    return false;
                }
            }
        }
    }
    struct RendezvousSeam {
        latch: Arc<Latch>,
        active: AtomicUsize,
        max: AtomicUsize,
        met: AtomicBool,
    }
    impl EmbedSeam for RendezvousSeam {
        fn embed(&self, index: usize, _samples: &[f32]) -> Vec<f32> {
            let current = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max.fetch_max(current, Ordering::SeqCst);
            if !self.latch.wait() {
                self.met.store(false, Ordering::SeqCst);
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            vec![index as f32; TEST_DIMS]
        }
    }
    let seam = Arc::new(RendezvousSeam {
        latch: Arc::new(Latch {
            state: Mutex::new((0, 0)),
            cvar: Condvar::new(),
            size: 2,
        }),
        active: AtomicUsize::new(0),
        max: AtomicUsize::new(0),
        met: AtomicBool::new(true),
    });
    let segments = vec![vec![0.0; 8]; 8];
    let vectors = batch_embed(Arc::clone(&seam) as Arc<dyn EmbedSeam>, &segments, 2)
        .expect("rendezvous batch succeeds");
    assert_eq!(vectors.len(), 8);
    for (index, vector) in vectors.iter().enumerate() {
        assert_eq!(*vector, vec![index as f32; TEST_DIMS]);
    }
    assert!(
        seam.met.load(Ordering::SeqCst),
        "every wave rendezvoused: workers overlap"
    );
    assert_eq!(
        seam.max.load(Ordering::SeqCst),
        2,
        "both workers overlap, never more than the cap"
    );
}

/// An empty script panics inside the worker (`% 0`); the pool must report
/// the worker-loss outcome instead of propagating the panic.
#[test]
fn empty_script_reports_worker_loss_without_panicking() {
    let seam = Arc::new(ScriptSeam { script: Vec::new() });
    let segments = vec![vec![0.5; 100]; 2];
    let error = batch_embed(seam, &segments, 2).expect_err("empty script must fail");
    assert_eq!(error, "orator: embed worker left a segment empty");
}
