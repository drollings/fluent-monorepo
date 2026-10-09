//! Supervised reconnect-with-backfill worker shape.
//!
//! A drop in a sequenced stream must never surface to consumers as a gap:
//! the worker counts the drop, sleeps the shared [`capped_backoff_ms`]
//! schedule, and on resubscribe enqueues the backfill *before* live items
//! resume, deduping by sequence number so a double-drop during backoff
//! cannot duplicate. Delivery itself is owned by the composed
//! [`CreditedFeedWorker`] (credit gate + bounded feed + concurrency bound);
//! supervision of the driver task is the caller's [`Scope`], and this worker
//! only serializes its event handling so backfill-before-resume ordering
//! holds under concurrent drivers.
//!
//! The backfill gap policy is a parameter, not a fork:
//! - [`GapPolicy::Sparse`] — the series may skip sequence numbers (sparse
//!   bars, filtered ticks); anything newer than the last accepted item is
//!   accepted, no contiguity check.
//! - [`GapPolicy::Dense`] — the series must be contiguous; a backfill whose
//!   first new item jumps past the last accepted item reports the uncovered
//!   `(from, to)` range so the caller widens its refetch.
//!
//! [`capped_backoff_ms`]: common_core::retry::capped_backoff_ms
//! [`Scope`]: crate::scope::Scope

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common_core::retry::capped_backoff_ms;
use common_core::sync::lock;
use fluent_wvr::Runtime;
use tokio::task::JoinHandle;

use crate::feed_worker::{FeedConfig, CreditedFeedWorker};

/// Whether a backfilled series must be contiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GapPolicy {
    /// Non-contiguous series: accept anything newer, never report a gap.
    #[default]
    Sparse,
    /// Contiguous series: report the uncovered range when the backfill jumps.
    Dense,
}

/// Configuration for a [`ReconnectWorker`].
#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    /// Feed/credit/concurrency shape of the composed delivery worker.
    pub feed: FeedConfig,
    /// Base of the [`capped_backoff_ms`] reconnect schedule.
    pub base_ms: u64,
    /// Ceiling multiplier of the reconnect schedule.
    pub cap: u32,
    /// Backfill gap policy (parameter, not a fork).
    pub gap: GapPolicy,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            feed: FeedConfig::default(),
            base_ms: 1_000,
            cap: 12,
            gap: GapPolicy::Sparse,
        }
    }
}

impl ReconnectConfig {
    /// Reconnect delay after `drops` consecutive drops — exactly the shared
    /// `capped_backoff_ms` schedule, so there is one backoff math to audit.
    #[must_use]
    pub fn delay_for(&self, drops: u32) -> u64 {
        capped_backoff_ms(self.base_ms, drops, self.cap)
    }
}

/// One sequenced payload: the sequence number is the dedupe and gap key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sequenced<T> {
    /// Monotone source sequence number.
    pub seq: u64,
    /// Payload.
    pub item: T,
}

impl<T> Sequenced<T> {
    /// Pair a sequence number with its payload.
    #[must_use]
    pub fn new(seq: u64, item: T) -> Self {
        Self { seq, item }
    }
}

/// Source-side events driving a [`ReconnectWorker`].
#[derive(Debug)]
pub enum SourceEvent<T> {
    /// One live item from the subscription.
    Live(Sequenced<T>),
    /// The subscription dropped; back off before resubscribing.
    Dropped,
    /// The subscription is back; `backfill` covers what was missed while
    /// down and is enqueued before live items resume.
    Resubscribed {
        /// Items missed while down (may overlap already-seen seqs).
        backfill: Vec<Sequenced<T>>,
    },
}

/// What one [`ReconnectWorker::handle`] call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleReport {
    /// A live item was accepted (`true`) or dropped as a duplicate (`false`).
    Live {
        /// Whether the item was enqueued.
        accepted: bool,
    },
    /// A drop was counted; the driver should sleep `delay_ms` before
    /// resubscribing. Test and calibration callers assert on the delay.
    Dropped {
        /// Consecutive drops including this one.
        drops: u32,
        /// `delay_for(drops)` in milliseconds.
        delay_ms: u64,
    },
    /// A resubscribe completed: backfill enqueued, drop count reset.
    Resumed(BackfillReport),
}

/// Outcome of one backfill enqueue.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BackfillReport {
    /// Items enqueued for delivery.
    pub accepted: usize,
    /// Items skipped as already-seen duplicates.
    pub skipped_duplicate: usize,
    /// Under [`GapPolicy::Dense`], the uncovered `(from, to)` range when the
    /// backfill jumps past the last accepted item; always `None` under
    /// [`GapPolicy::Sparse`].
    pub gap_remaining: Option<(u64, u64)>,
}

#[derive(Debug, Default)]
struct ReconnectState {
    last_seen: u64,
    drops: u32,
}

/// A reconnect-with-backfill worker over sequenced items.
///
/// Composes a [`CreditedFeedWorker`] for delivery (the consumer's handler
/// runs per item under its credit/concurrency bounds), [`capped_backoff_ms`]
/// for the reconnect schedule, and an internal gate so concurrent drivers
/// cannot interleave a live item between a resubscribe and its backfill.
pub struct ReconnectWorker<T: Send + Sync + 'static> {
    feed: Arc<CreditedFeedWorker<Sequenced<T>>>,
    config: ReconnectConfig,
    state: Mutex<ReconnectState>,
    gate: tokio::sync::Mutex<()>,
}

impl<T: Send + Sync + 'static> ReconnectWorker<T> {
    /// Build a worker delivering to `handler` per sequenced item.
    pub fn new<F, Fut>(runtime: Arc<dyn Runtime>, config: ReconnectConfig, handler: F) -> Self
    where
        F: Fn(Sequenced<T>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let feed = Arc::new(CreditedFeedWorker::new(runtime, config.feed.clone(), handler));
        Self {
            feed,
            config,
            state: Mutex::new(ReconnectState::default()),
            gate: tokio::sync::Mutex::new(()),
        }
    }

    /// Start the background delivery loop (the caller holds the handle, e.g.
    /// under a [`Scope`](crate::scope::Scope), for the worker's lifetime).
    pub fn start(self: &Arc<Self>) -> JoinHandle<()> {
        self.feed.start()
    }

    /// Stop accepting items; queued items still deliver.
    pub fn drain(&self) {
        self.feed.drain();
    }

    /// Consecutive drops counted since the last resubscribe.
    #[must_use]
    pub fn drops(&self) -> u32 {
        lock(&self.state).drops
    }

    /// Highest sequence number accepted so far.
    #[must_use]
    pub fn last_seen(&self) -> u64 {
        lock(&self.state).last_seen
    }

    /// Handle one source event: live items dedupe by seq, drops count and
    /// sleep the backoff schedule, resubscribes reset the count and enqueue
    /// the backfill before anything live that follows. Serialized, so a
    /// backfill can never interleave with live items from another driver.
    pub async fn handle(&self, event: SourceEvent<T>) -> HandleReport {
        let _guard = self.gate.lock().await;
        match event {
            SourceEvent::Live(item) => {
                let mut state = lock(&self.state);
                if item.seq <= state.last_seen {
                    return HandleReport::Live { accepted: false };
                }
                state.last_seen = item.seq;
                drop(state);
                self.feed.enqueue(item);
                HandleReport::Live { accepted: true }
            }
            SourceEvent::Dropped => {
                let delay = {
                    let mut state = lock(&self.state);
                    state.drops = state.drops.saturating_add(1);
                    self.config.delay_for(state.drops)
                };
                tokio::time::sleep(Duration::from_millis(delay)).await;
                HandleReport::Dropped {
                    drops: self.drops(),
                    delay_ms: delay,
                }
            }
            SourceEvent::Resubscribed { backfill } => {
                let report = self.enqueue_backfill(backfill);
                lock(&self.state).drops = 0;
                HandleReport::Resumed(report)
            }
        }
    }

    /// Enqueue a backfill window per the configured gap policy. Items at or
    /// below `last_seen` are duplicates and skipped; under `Dense` the first
    /// accepted jump reports the uncovered range.
    fn enqueue_backfill(&self, backfill: Vec<Sequenced<T>>) -> BackfillReport {
        let mut items = backfill;
        items.sort_by_key(|item| item.seq);
        let mut report = BackfillReport::default();
        let mut state = lock(&self.state);
        for item in items {
            if item.seq <= state.last_seen {
                report.skipped_duplicate += 1;
                continue;
            }
            if self.config.gap == GapPolicy::Dense
                && report.accepted == 0
                && report.gap_remaining.is_none()
                && item.seq > state.last_seen + 1
            {
                report.gap_remaining = Some((state.last_seen + 1, item.seq - 1));
            }
            state.last_seen = item.seq;
            report.accepted += 1;
            drop(state);
            self.feed.enqueue(item);
            state = lock(&self.state);
        }
        report
    }
}

#[cfg(test)]
#[path = "../tests/reconnect.rs"]
mod tests;
