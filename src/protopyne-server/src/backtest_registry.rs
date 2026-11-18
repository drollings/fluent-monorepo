//! Stored backtest runs: the registry beside the replay one.
//!
//! A run is projected once (metrics, equity curve, trades, markers, final
//! status) and stored under a deterministic run id, so the terminal reads
//! the same bytes on every state call without re-running the tape. The
//! registry mirrors the replay registry's lock idiom: short synchronous
//! borrows, never an `await` across the lock.

use common_core::registry::KeyedRegistry;
use protopyne_view::BacktestRunView;

/// One stored run: the id it is filed under and its projected view.
#[derive(Clone)]
pub struct BacktestEntry {
    /// Registry id (deterministic over the request).
    pub run_id: String,
    /// The projected run: metrics, curve, trades, markers, final status.
    pub view: BacktestRunView,
}

/// Stored backtest runs by run id.
#[derive(Default)]
pub struct BacktestRegistry {
    runs: KeyedRegistry<String, BacktestEntry>,
}

impl BacktestRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Store (or replace) the entry in `run_id`'s slot.
    pub fn publish(&mut self, entry: BacktestEntry) {
        self.runs.insert(entry.run_id.clone(), entry);
    }

    /// Borrow a stored entry, if its slot is occupied.
    #[must_use]
    pub fn get(&self, run_id: &str) -> Option<&BacktestEntry> {
        self.runs.get(run_id)
    }

    /// Release a slot; answers whether one was occupied.
    pub fn remove(&mut self, run_id: &str) -> bool {
        self.runs.remove(run_id).is_some()
    }

    /// Slot ids in sorted order.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.runs.keys().cloned().collect()
    }

    /// Number of occupied slots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.runs.len()
    }

    /// Whether no slot is occupied.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
}

#[cfg(test)]
#[path = "../tests/backtest_registry.rs"]
mod tests;
