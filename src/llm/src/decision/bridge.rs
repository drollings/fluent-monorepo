//! Detached shadow bridge: bounded queue, per-call timeout, panic-proof
//! driver, bounded shutdown — zero caller impact when the shadow path
//! errors, hangs, or panics.
//!
//! Construct where a runtime lives; `submit` is then sync-callable from
//! anywhere (it never blocks — a full queue drops with a visible counter).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Deferred shadow work: identifiers for logs plus the async closure.
pub struct ShadowJob {
    /// Decision point, for logs.
    pub point: String,
    /// Question id, for logs (identifiers only — never raw state).
    pub question_id: String,
    pub question_version: u64,
    work: ShadowWork,
}

/// `ShadowWork`: boxed one-shot shadow closure.
pub type ShadowWork =
    Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send>;

impl ShadowJob {
    /// One job: log identifiers plus the work to run detached.
    #[must_use]
    pub fn new(
        point: String,
        question_id: String,
        question_version: u64,
        work: impl FnOnce() -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + 'static,
    ) -> Self {
        Self {
            point,
            question_id,
            question_version,
            work: Box::new(work),
        }
    }
}

impl std::fmt::Debug for ShadowJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShadowJob")
            .field("point", &self.point)
            .field("question_id", &self.question_id)
            .field("question_version", &self.question_version)
            .field("work", &"<shadow closure>")
            .finish()
    }
}

#[derive(Default)]
struct BridgeCounters {
    dropped: AtomicU64,
    completed: AtomicU64,
    errors: AtomicU64,
    timeouts: AtomicU64,
    panics: AtomicU64,
}

/// Bounded detached executor for shadow work. Drop/abandon safety: dropping
/// the bridge aborts the driver (no zombie); prefer `shutdown` for a
/// bounded flush on short-lived processes.
///
/// ```rust,no_run
/// use fluent_llm::decision::{ShadowBridge, ShadowJob};
/// use fluent_llm::client::block_on;
///
/// let mut bridge = ShadowBridge::new(64, 1_000);
/// assert!(bridge.submit(ShadowJob::new(
///     "intent".to_string(),
///     "q1".to_string(),
///     1,
///     || Box::pin(async { Ok(()) }),
/// )));
/// assert!(block_on(bridge.shutdown(5_000)));
/// assert_eq!(bridge.completed_count(), 1);
/// ```
pub struct ShadowBridge {
    tx: Option<tokio::sync::mpsc::Sender<ShadowJob>>,
    driver: Option<tokio::task::JoinHandle<()>>,
    counters: Arc<BridgeCounters>,
}

impl ShadowBridge {
    /// Spawn the driver on the current runtime: `capacity` queued jobs,
    /// `timeout_ms` per job. Panics without an active runtime.
    #[must_use]
    pub fn new(capacity: usize, timeout_ms: u64) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ShadowJob>(capacity.max(1));
        let counters = Arc::new(BridgeCounters::default());
        let timeout = Duration::from_millis(timeout_ms);
        let worker_counters = Arc::clone(&counters);
        let driver = tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let outcome = tokio::time::timeout(timeout, async {
                    futures_util::FutureExt::catch_unwind(
                        std::panic::AssertUnwindSafe((job.work)()),
                    )
                    .await
                })
                .await;
                match outcome {
                    // Completed.
                    Ok(Ok(Ok(()))) => {
                        worker_counters.completed.fetch_add(1, Ordering::Relaxed);
                    }
                    // Errored.
                    Ok(Ok(Err(_))) => {
                        worker_counters.errors.fetch_add(1, Ordering::Relaxed);
                    }
                    // Panicked (driver survives).
                    Ok(Err(_)) => {
                        worker_counters.panics.fetch_add(1, Ordering::Relaxed);
                    }
                    // Hung past the per-call timeout.
                    Err(_) => {
                        worker_counters.timeouts.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });
        Self {
            tx: Some(tx),
            driver: Some(driver),
            counters,
        }
    }

    /// Submit without blocking: `false` (and a drop count) when full.
    #[must_use]
    pub fn submit(&self, job: ShadowJob) -> bool {
        if let Some(tx) = self.tx.as_ref() {
            if tx.try_send(job).is_ok() {
                return true;
            }
        }
        self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        false
    }

    /// Flush with a bound: closes the queue, awaits the driver up to
    /// `timeout_ms`. `false` leaves the driver to finish detached.
    pub async fn shutdown(&mut self, timeout_ms: u64) -> bool {
        self.tx.take();
        match self.driver.take() {
            None => true,
            Some(driver) => tokio::time::timeout(Duration::from_millis(timeout_ms), driver)
                .await
                .is_ok(),
        }
    }

    /// Sync `shutdown` for non-async exits via the canonical bridge.
    #[must_use]
    pub fn shutdown_blocking(&mut self, timeout_ms: u64) -> bool {
        common_core::runtime::block_on(self.shutdown(timeout_ms))
    }

    /// Jobs dropped on a full (or closed) queue.
    #[must_use]
    pub fn dropped_count(&self) -> u64 {
        self.counters.dropped.load(Ordering::Relaxed)
    }

    /// Jobs completed cleanly.
    #[must_use]
    pub fn completed_count(&self) -> u64 {
        self.counters.completed.load(Ordering::Relaxed)
    }

    /// Jobs that returned `Err`.
    #[must_use]
    pub fn error_count(&self) -> u64 {
        self.counters.errors.load(Ordering::Relaxed)
    }

    /// Jobs that exceeded the per-call timeout.
    #[must_use]
    pub fn timeout_count(&self) -> u64 {
        self.counters.timeouts.load(Ordering::Relaxed)
    }

    /// Jobs that panicked (driver survived).
    #[must_use]
    pub fn panic_count(&self) -> u64 {
        self.counters.panics.load(Ordering::Relaxed)
    }
}

impl Drop for ShadowBridge {
    fn drop(&mut self) {
        // Abandoned bridge: abort the driver rather than leak a zombie.
        // Prefer `shutdown` for a bounded flush.
        if let Some(driver) = self.driver.take() {
            driver.abort();
        }
    }
}

/// One offline replay item: identifiers plus the spec to judge with. Raw
/// state arrives via the provider callback, never via logs.
#[derive(Debug, Clone)]
/// One offline replay item: identifiers plus the spec to judge with. Raw
/// state arrives via the provider callback, never via logs.
pub struct ReplayItem {
    pub point: String,
    pub question_id: String,
    pub spec: crate::decision::QuestionSpec,
    pub question: String,
    pub expected_label: Option<String>,
}

/// One replay outcome (`ReplayOutcome`): identifiers plus the verdict against expectation.
/// Carries no state bytes by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One replay outcome (`ReplayOutcome`): identifiers plus the verdict against expectation.
/// Carries no state bytes by construction.
pub struct ReplayOutcome {
    pub question_id: String,
    pub label: Option<String>,
    pub matched: Option<bool>,
}

/// Replay logged identifiers offline: fetch each state through `provider`
/// (missing state skips without sending), judge through `client` (backend
/// aware, redaction applied), and compare against expected labels.
///
/// ```rust
/// use fluent_llm::client::{block_on, LlmClient};
/// use fluent_llm::decision::{replay_shadow_log, ReplayItem, QuestionSpec};
///
/// # let client = LlmClient::new("http://127.0.0.1:9", "stub");
/// # let spec = QuestionSpec::new()
/// #     .id("q".to_string()).version(1).kind("noul".to_string())
/// #     .prompt_template("True?".to_string()).options_csv("Yes,No".to_string())
/// #     .act_threshold(0.9).review_threshold(0.7)
/// #     .timeout_ms(1000).cacheable(false).build();
/// let items = vec![ReplayItem {
///     point: "sufficiency".to_string(),
///     question_id: "q1".to_string(),
///     spec,
///     question: "enough?".to_string(),
///     expected_label: None,
/// }];
/// let outcomes = block_on(replay_shadow_log(&client, items, &|_| None));
/// assert_eq!(outcomes.len(), 1);
/// ```
pub async fn replay_shadow_log(
    client: &crate::client::LlmClient,
    items: Vec<ReplayItem>,
    provide_state: &dyn Fn(&str) -> Option<String>,
) -> Vec<ReplayOutcome> {
    let mut outcomes = Vec::with_capacity(items.len());
    for item in items {
        let Some(state) = provide_state(&item.question_id) else {
            outcomes.push(ReplayOutcome {
                question_id: item.question_id,
                label: None,
                matched: None,
            });
            continue;
        };
        let question = crate::decision::Question {
            spec: &item.spec,
            state: &state,
            question: &item.question,
        };
        match client.decide_async(&question).await {
            Ok(decision) => outcomes.push(ReplayOutcome {
                question_id: item.question_id,
                matched: item.expected_label.as_ref().map(|e| e == &decision.label),
                label: Some(decision.label),
            }),
            Err(_) => outcomes.push(ReplayOutcome {
                question_id: item.question_id,
                label: None,
                matched: None,
            }),
        }
    }
    outcomes
}