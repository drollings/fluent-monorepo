//! A priority queue with a fast path for zero-priority items.

use std::collections::{BTreeMap, VecDeque};

/// A priority queue combining O(1) fast path for priority-0 items
/// with a `BTreeMap`-backed ordered queue for non-zero priorities.
pub struct PriorityQueue<T> {
    simple: VecDeque<T>,
    prioritized: BTreeMap<i32, VecDeque<T>>,
    has_prioritized: bool,
}

impl<T> PriorityQueue<T> {
    pub fn new() -> Self {
        Self {
            simple: VecDeque::new(),
            prioritized: BTreeMap::new(),
            has_prioritized: false,
        }
    }

    pub fn push(&mut self, item: T, priority: i32) {
        if priority == 0 && !self.has_prioritized {
            self.simple.push_back(item);
        } else {
            if priority != 0 {
                self.has_prioritized = true;
            }
            self.prioritized
                .entry(priority)
                .or_default()
                .push_back(item);
        }
    }

    pub fn pop(&mut self) -> Option<T> {
        if !self.has_prioritized {
            return self.simple.pop_front();
        }
        if let Some((&key, items)) = self.prioritized.iter_mut().next_back() {
            if let Some(item) = items.pop_front() {
                if items.is_empty() {
                    self.prioritized.remove(&key);
                    self.has_prioritized = !self.prioritized.is_empty();
                }
                return Some(item);
            }
        }
        self.simple.pop_front()
    }
}

impl<T> Default for PriorityQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}
