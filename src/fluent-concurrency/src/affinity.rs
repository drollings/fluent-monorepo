//! Affinity-aware priority scheduler.
//!
//! Wraps `PriorityResultPool` and adds:
//! - **Affinity bonus** — tasks for the currently-active session get a priority
//!   boost, preventing excessive context switching.
//! - **Aging** — starved tasks periodically increase in priority to prevent
//!   indefinite starvation of other sessions.
//!
//! The scheduler is not a `WorkUnit` — it is a separate component that sits
//! between the pipeline and dispatch. It is designed to be owned by the
//! orchestrator and called directly in the async dispatch path.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lru::LruCache;

use crate::pool::{PriorityResultPool, ResultPoolError};
use crate::sync as sync_lock;

/// Configuration for aging behaviour.
#[derive(Debug, Clone)]
/// Configuration for aging behaviour.
pub struct AgingConfig {
    /// Priority bonus added when task is for the currently-affine session.
    pub affinity_bonus: i32,
    /// How often aging runs (wall-clock).
    pub aging_interval: Duration,
    /// Priority increase per aging tick for starved tasks.
    pub aging_rate: i32,
    /// Maximum priority a starved task can reach through aging.
    pub max_priority: i32,
}

impl Default for AgingConfig {
    fn default() -> Self {
        Self {
            affinity_bonus: 10,
            aging_interval: Duration::from_secs(5),
            aging_rate: 2,
            max_priority: 100,
        }
    }
}

/// A task enqueued in the scheduler with its metadata.
#[derive(Debug, Clone)]
/// A task enqueued in the scheduler with its metadata.
pub struct ScheduledTask<T> {
    pub identity: String,
    pub task: T,
    pub enqueued_at: Instant,
}

/// Affinity-aware scheduler that wraps a `PriorityResultPool`.
///
/// Tracks which session is currently "affine" and gives tasks for that
/// session a priority bonus. Starved tasks are periodically aged (their
/// base priority increases) to prevent indefinite starvation.
///
/// The scheduler is not a `WorkUnit` — it is a separate component that
/// sits between the pipeline and agent dispatch.
pub struct AffinityScheduler<T: Send + 'static, R: Send + 'static, E: Send + 'static> {
    pool: Arc<PriorityResultPool<T, R, E>>,
    aging: AgingConfig,
    /// The whole scheduler state behind one lock: affinity session,
    /// per-identity base priorities (bounded LRU), and the last aging
    /// timestamp. One guard per submit — compute reads, aging mutates, and
    /// the record step writes, all under the same critical section, so no
    /// lock ordering exists to get wrong.
    state: Mutex<State>,
    runtime: Arc<dyn fluent_wvr::Runtime>,
}

struct State {
    affinity: Option<String>,
    base: LruCache<String, i32>,
    last_aging: Instant,
}

const DEFAULT_MAX_IDENTITIES: usize = 1024;

impl<T, R, E> AffinityScheduler<T, R, E>
where
    T: Send + Sync + 'static,
    R: Send + 'static,
    E: Send + std::fmt::Debug + 'static,
{
    /// Create a new affinity scheduler wrapping the given pool.
    pub fn new(pool: Arc<PriorityResultPool<T, R, E>>) -> Self {
        Self::new_with_runtime(pool, crate::tokio_runtime())
    }

    /// Create a new affinity scheduler with an explicit runtime (for virtual-time tests).
    pub fn new_with_runtime(
        pool: Arc<PriorityResultPool<T, R, E>>,
        runtime: Arc<dyn fluent_wvr::Runtime>,
    ) -> Self {
        let cap = NonZeroUsize::new(DEFAULT_MAX_IDENTITIES).unwrap();
        let now = runtime.now();
        Self {
            pool,
            aging: AgingConfig::default(),
            state: Mutex::new(State {
                affinity: None,
                base: LruCache::new(cap),
                last_aging: now,
            }),
            runtime,
        }
    }

    /// Create with explicit LRU cap (for testing eviction).
    pub fn new_with_cap(
        pool: Arc<PriorityResultPool<T, R, E>>,
        runtime: Arc<dyn fluent_wvr::Runtime>,
        cap: usize,
        aging: AgingConfig,
    ) -> Self {
        let cap = NonZeroUsize::new(cap.max(1)).unwrap();
        let now = runtime.now();
        Self {
            pool,
            aging,
            state: Mutex::new(State {
                affinity: None,
                base: LruCache::new(cap),
                last_aging: now,
            }),
            runtime,
        }
    }

    /// Set the aging configuration.
    #[must_use]
    pub fn with_aging(mut self, aging: AgingConfig) -> Self {
        self.aging = aging;
        self
    }

    /// Submit a task with a base priority. If the task's identity matches
    /// the current affinity session, an affinity bonus is added.
    ///
    /// Also runs aging if the configured interval has elapsed — all under
    /// one guard, so the aging check costs no extra lock on any submit.
    ///
    /// [task-value]: the affinity bonus measures session-history value
    /// (stickiness reduces context switching), not parse confidence. A
    /// confident-but-wrong-session task still earns the bonus, and a
    /// correct task in a cold session earns none — never read the bonus
    /// as a correctness signal.
    pub async fn submit(
        &self,
        task: ScheduledTask<T>,
        base_priority: i32,
    ) -> Result<R, ResultPoolError<E>> {
        let priority = {
            let mut state = sync_lock::lock(&self.state);
            let State {
                affinity,
                base,
                last_aging,
            } = &mut *state;
            let now = self.runtime.now();
            if now.duration_since(*last_aging) >= self.aging.aging_interval {
                age_locked(base, affinity.as_deref(), &self.aging);
                *last_aging = now;
            }
            let cached = base.peek(&task.identity).copied();
            let (record, priority) = Self::compute_priority(
                affinity.as_deref(),
                cached,
                &task,
                base_priority,
                now,
                &self.aging,
            );
            base.put(task.identity.clone(), record);
            priority
        };
        self.pool.submit(task.task, priority).await
    }

    /// Pure priority math over a state snapshot: cached base (or the call
    /// argument), per-task age bonus capped at `aging_rate`, then the
    /// affinity bonus for the affine session. Returns `(record, return)`:
    /// the value to store in the LRU (base + age, bonus excluded) and the
    /// priority to submit with. No locks, no mutation, no clones of the
    /// affinity string (compared by reference).
    fn compute_priority(
        affinity: Option<&str>,
        cached_base: Option<i32>,
        task: &ScheduledTask<T>,
        base_priority: i32,
        now: Instant,
        aging: &AgingConfig,
    ) -> (i32, i32) {
        let effective_base = cached_base.unwrap_or(base_priority);
        let age_secs = now
            .checked_duration_since(task.enqueued_at)
            .unwrap_or(Duration::ZERO)
            .as_secs() as i32;
        let age_bonus = age_secs.min(aging.aging_rate).max(0);
        let with_age = effective_base.saturating_add(age_bonus);
        let out = if affinity == Some(task.identity.as_str()) {
            with_age.saturating_add(aging.affinity_bonus)
        } else {
            with_age
        };
        (with_age, out)
    }

    /// Public helper for tests: compute effective priority without submitting.
    ///
    /// Compute-only and LRU-clean: reads through `peek` (no recency churn)
    /// and never `put`s, so repeated calls leave the scheduler exactly as
    /// found. (The pre-change helper recorded into the LRU as a side
    /// effect; only the returned values are preserved.)
    pub fn effective_priority(&self, task: &ScheduledTask<T>, base_priority: i32) -> i32 {
        let state = sync_lock::lock(&self.state);
        let now = self.runtime.now();
        let cached = state.base.peek(&task.identity).copied();
        let (_, priority) = Self::compute_priority(
            state.affinity.as_deref(),
            cached,
            task,
            base_priority,
            now,
            &self.aging,
        );
        priority
    }

    /// Test-only introspection: the recorded base for one identity (bonus
    /// excluded), without touching recency.
    #[cfg(test)]
    fn recorded_base(&self, identity: &str) -> Option<i32> {
        sync_lock::lock(&self.state).base.peek(identity).copied()
    }

    /// Set the current affinity session.
    pub fn set_affinity(&self, identity: Option<String>) {
        sync_lock::lock(&self.state).affinity = identity;
    }

    /// Get the current affinity session.
    pub fn current_affinity(&self) -> Option<String> {
        sync_lock::lock(&self.state).affinity.clone()
    }

    /// Remove an identity from the bounded map (session close).
    pub fn remove_identity(&self, identity: &str) {
        sync_lock::lock(&self.state).base.pop(identity);
    }

    /// For testing: current map length.
    pub fn base_priorities_len(&self) -> usize {
        sync_lock::lock(&self.state).base.len()
    }

    /// Run aging immediately (useful for testing).
    pub fn age_now(&self) {
        let mut state = sync_lock::lock(&self.state);
        let State {
            affinity,
            base,
            last_aging,
        } = &mut *state;
        age_locked(base, affinity.as_deref(), &self.aging);
        *last_aging = self.runtime.now();
    }

    /// Access the underlying pool.
    pub fn pool(&self) -> &Arc<PriorityResultPool<T, R, E>> {
        &self.pool
    }
}

/// One aging tick over the base map: every non-affine identity gains
/// `aging_rate`, capped at `max_priority`. Pure over explicit borrows —
/// the caller holds the single state guard.
fn age_locked(base: &mut LruCache<String, i32>, affinity: Option<&str>, aging: &AgingConfig) {
    for (identity, prio) in base.iter_mut() {
        if Some(identity.as_str()) != affinity {
            *prio = (*prio + aging.aging_rate).min(aging.max_priority);
        }
    }
}

#[cfg(test)]
#[path = "../tests/affinity.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/m-affinity-purity.rs"]
mod m_affinity_purity_tests;