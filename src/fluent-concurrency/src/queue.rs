//! A priority queue with a fast path for zero-priority items.

use std::collections::{BTreeMap, VecDeque};

/// A priority queue combining O(1) fast path for priority-0 items
/// with a `BTreeMap`-backed ordered queue for non-zero priorities.
pub struct PriorityQueue<T> {
    simple: VecDeque<T>,
    prioritized: BTreeMap<i32, VecDeque<T>>,
}

impl<T> PriorityQueue<T> {
    /// Creates a new empty priority queue.
    pub fn new() -> Self {
        Self {
            simple: VecDeque::new(),
            prioritized: BTreeMap::new(),
        }
    }

    /// Pushes an item with the given priority.
    /// Priority 0 items are stored in a FIFO fast-path queue.
    /// Non-zero priorities are stored in a `BTreeMap` ordered by priority.
    pub fn push(&mut self, item: T, priority: i32) {
        if priority == 0 {
            self.simple.push_back(item);
        } else {
            self.prioritized
                .entry(priority)
                .or_default()
                .push_back(item);
        }
    }

    /// Pops the highest-priority item, maintaining FIFO within each priority level.
    /// Higher positive priorities are popped first; priority-0 items are next;
    /// negative priorities are popped last.
    pub fn pop(&mut self) -> Option<T> {
        // If highest priority > 0, pop from prioritized (higher than simple)
        if let Some((&key, _)) = self.prioritized.last_key_value() {
            if key > 0 {
                if let Some(items) = self.prioritized.get_mut(&key) {
                    let item = items.pop_front();
                    if items.is_empty() {
                        self.prioritized.remove(&key);
                    }
                    return item;
                }
            }
        }
        // Pop from simple (priority 0) first
        if let Some(item) = self.simple.pop_front() {
            return Some(item);
        }
        // Simple is empty, drain any remaining prioritized items (negative priorities)
        if let Some((&key, _)) = self.prioritized.last_key_value() {
            if let Some(items) = self.prioritized.get_mut(&key) {
                let item = items.pop_front();
                if items.is_empty() {
                    self.prioritized.remove(&key);
                }
                return item;
            }
        }
        None
    }
}

impl<T> Default for PriorityQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}
