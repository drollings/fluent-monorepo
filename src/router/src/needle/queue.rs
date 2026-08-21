//! `NeedleQueue` — the single-worker serialization seam for the Needle FFI.
//!
//! Needle is a single global engine with sticky weights; exactly one
//! completion can run at a time. This queue owns that invariant by running
//! the inner [`NeedleBackend`] on **one dedicated worker thread**, fed by a
//! bounded channel, with a per-request `oneshot` reply and a wall-clock
//! timeout.
//!
//! Why a thread, not an async pool: the router drives stages through the
//! **synchronous** `WorkUnit::execute` / `StageDecisionProducer::evaluate`
//! contract (`fluent-wvr` purity: no blocking I/O, no `block_on`), so an async
//! `ResultPool`/`LlmRequestQueue` cannot be awaited there. A worker thread
//! gives the same queue + backpressure + timeout the async pool would, while
//! preserving the sync [`NeedleBackend`] trait so the pre-filter rung, the
//! chart adjudicator, and the tree's `backend: "needle"` nodes all keep
//! working unchanged. If stage evaluation ever becomes async, this thread can
//! be swapped for a cap-1 `ResultPool` worker with no trait or seam change.
//!
//! The worker owns the engine, so the process-wide `ENGINE_LOCK` in
//! `engine.rs` becomes redundant on this path and is not taken here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common_core::sync::lock;

use super::backend::NeedleBackend;
use super::envelope::NeedleEnvelope;
use super::NeedleError;

/// The default per-completion wall-clock budget (ms) when `timeout_ms` is 0.
pub const DEFAULT_TIMEOUT_MS: u64 = 2000;

/// One unit of work for the worker thread.
enum NeedleWork {
    /// Run one completion and send the result back on `reply`.
    Complete {
        text: String,
        tools_json: String,
        max_new_tokens: i32,
        reply: SyncSender<Result<NeedleEnvelope, NeedleError>>,
    },
    /// Reset the engine's session state and acknowledge.
    Reset { reply: SyncSender<()> },
}

/// A [`NeedleBackend`] that serializes every call through one worker thread.
///
/// Construction spawns the worker; dropping the queue closes the channel and
/// the worker exits. `is_available` reports `false` once the worker is gone or
/// the inner backend reports unavailable, so the pipeline skips cleanly.
pub struct NeedleQueue {
    tx: Mutex<SyncSender<NeedleWork>>,
    timeout: Duration,
    available: Arc<AtomicBool>,
}

impl NeedleQueue {
    /// Build a queue over `inner` with a bounded channel of `queue_capacity`
    /// jobs and a per-call `timeout_ms` budget (0 ⇒ [`DEFAULT_TIMEOUT_MS`]).
    pub fn new(
        inner: Arc<dyn NeedleBackend>,
        queue_capacity: usize,
        timeout_ms: u64,
    ) -> Self {
        let (tx, rx) = mpsc::sync_channel(queue_capacity.max(1));
        let available = Arc::new(AtomicBool::new(inner.is_available()));
        let worker_available = Arc::clone(&available);
        std::thread::Builder::new()
            .name("needle-worker".into())
            .spawn(move || run_worker(inner, rx, worker_available))
            .expect("spawn needle worker thread");
        Self {
            tx: Mutex::new(tx),
            timeout: Duration::from_millis(if timeout_ms == 0 {
                DEFAULT_TIMEOUT_MS
            } else {
                timeout_ms
            }),
            available,
        }
    }

    /// Submit a completion and wait for the worker's reply, bounded by
    /// `timeout`. A full queue (backpressure) or a dead worker reports
    /// [`NeedleError::Unavailable`]; a timeout reports [`NeedleError::Complete`].
    fn dispatch(
        &self,
        work: NeedleWork,
        reply_rx: &mpsc::Receiver<Result<NeedleEnvelope, NeedleError>>,
    ) -> Result<NeedleEnvelope, NeedleError> {
        let tx = lock(&self.tx);
        match tx.try_send(work) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(NeedleError::Unavailable);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.available.store(false, Ordering::SeqCst);
                return Err(NeedleError::Unavailable);
            }
        }
        drop(tx);
        match reply_rx.recv_timeout(self.timeout) {
            Ok(result) => result,
            Err(_) => Err(NeedleError::Complete {
                detail: "needle worker did not reply in time".into(),
            }),
        }
    }
}

impl NeedleBackend for NeedleQueue {
    fn complete(
        &self,
        text: &str,
        tools_json: &str,
        max_new_tokens: i32,
    ) -> Result<NeedleEnvelope, NeedleError> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let work = NeedleWork::Complete {
            text: text.to_string(),
            tools_json: tools_json.to_string(),
            max_new_tokens,
            reply: reply_tx,
        };
        self.dispatch(work, &reply_rx)
    }

    fn is_available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let tx = lock(&self.tx);
        let _ = tx.send(NeedleWork::Reset { reply: reply_tx });
        drop(tx);
        let _ = reply_rx.recv_timeout(self.timeout);
    }
}

/// The worker loop: pull jobs off the channel and run them against `inner`.
/// One thread owns the engine, so calls are inherently serialized.
#[allow(clippy::needless_pass_by_value)] // thread entry: takes owned 'static handles
fn run_worker(
    inner: Arc<dyn NeedleBackend>,
    rx: mpsc::Receiver<NeedleWork>,
    available: Arc<AtomicBool>,
) {
    for work in rx {
        match work {
            NeedleWork::Complete {
                text,
                tools_json,
                max_new_tokens,
                reply,
            } => {
                let result = inner.complete(&text, &tools_json, max_new_tokens);
                let _ = reply.send(result);
            }
            NeedleWork::Reset { reply } => {
                inner.reset();
                let _ = reply.send(());
            }
        }
    }
    available.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::needle::backend::MockNeedleBackend;
    use crate::needle::envelope::{NeedleEnvelopeType, NeedleFunctionCall};
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Instant;

    fn call_envelope(tool: &str) -> NeedleEnvelope {
        NeedleEnvelope {
            r#type: NeedleEnvelopeType::Call,
            success: None,
            error: None,
            error_code: None,
            function_calls: vec![NeedleFunctionCall {
                name: tool.into(),
                arguments: json!({}),
            }],
            reasoning: None,
            confidence: Some(0.9),
            results: None,
        }
    }

    #[test]
    fn returns_inner_result() {
        let inner = Arc::new(MockNeedleBackend::always(call_envelope("route_a")));
        let queue = NeedleQueue::new(inner, 4, 1000);
        let env = queue.complete("q", "[]", 32).expect("completion");
        assert_eq!(env.single_tool(), Some("route_a"));
    }

    #[test]
    fn propagates_inner_error() {
        let inner = Arc::new(MockNeedleBackend::failing());
        let queue = NeedleQueue::new(inner, 4, 1000);
        let err = queue.complete("q", "[]", 32).expect_err("fails");
        assert!(matches!(err, NeedleError::Unavailable));
    }

    #[test]
    fn serializes_calls_on_one_worker() {
        // Track peak concurrency of the inner backend: a single worker must
        // never see more than one in-flight completion.
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Tracked(Arc<AtomicUsize>, Arc<AtomicUsize>);
        impl NeedleBackend for Tracked {
            fn complete(
                &self,
                _: &str,
                _: &str,
                _: i32,
            ) -> Result<NeedleEnvelope, NeedleError> {
                let cur = self.0.fetch_add(1, Ordering::SeqCst) + 1;
                self.1.fetch_max(cur, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(5));
                self.0.fetch_sub(1, Ordering::SeqCst);
                Ok(call_envelope("r"))
            }
            fn is_available(&self) -> bool {
                true
            }
            fn reset(&self) {}
        }
        let cur = Arc::new(AtomicUsize::new(0));
        let max = Arc::new(AtomicUsize::new(0));
        let inner: Arc<dyn NeedleBackend> =
            Arc::new(Tracked(Arc::clone(&cur), Arc::clone(&max)));
        let queue = Arc::new(NeedleQueue::new(inner, 8, 1000));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let q = Arc::clone(&queue);
                std::thread::spawn(move || q.complete("q", "[]", 32).expect("ok"))
            })
            .collect();
        for h in handles {
            h.join().expect("join");
        }
        assert_eq!(max.load(Ordering::SeqCst), 1, "single worker ⇒ peak concurrency 1");
    }

    #[test]
    fn times_out_when_worker_blocks() {
        struct Blocking;
        impl NeedleBackend for Blocking {
            fn complete(
                &self,
                _: &str,
                _: &str,
                _: i32,
            ) -> Result<NeedleEnvelope, NeedleError> {
                std::thread::sleep(Duration::from_secs(60));
                unreachable!()
            }
            fn is_available(&self) -> bool {
                true
            }
            fn reset(&self) {}
        }
        let queue = NeedleQueue::new(Arc::new(Blocking), 1, 50);
        let start = Instant::now();
        let err = queue.complete("q", "[]", 32).expect_err("timeout");
        assert!(matches!(err, NeedleError::Complete { .. }));
        assert!(start.elapsed() < Duration::from_secs(5), "timeout must fire fast");
    }

    #[test]
    fn reset_forwards_to_worker() {
        let inner = Arc::new(MockNeedleBackend::always(call_envelope("r")));
        let queue = NeedleQueue::new(inner, 4, 1000);
        queue.reset();
        // A reset does not error; the worker remains usable afterwards.
        assert!(queue.complete("q", "[]", 32).is_ok());
    }
}
