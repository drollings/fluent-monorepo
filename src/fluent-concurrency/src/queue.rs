//! A priority queue with a fast path for zero-priority items.

use std::collections::{BTreeMap, VecDeque};

/// A priority queue combining O(1) fast path for priority-0 items
/// with a `BTreeMap`-backed ordered queue for non-zero priorities.
pub struct PriorityQueue<T> {
    simple: VecDeque<T>,
    prioritized: BTreeMap<i32, VecDeque<T>>,
    count: usize,
}

impl<T> PriorityQueue<T> {
    /// Creates a new empty priority queue.
    pub fn new() -> Self {
        Self {
            simple: VecDeque::new(),
            prioritized: BTreeMap::new(),
            count: 0,
        }
    }

    /// Pushes an item with the given priority.
    /// Priority 0 items are stored in a FIFO fast-path queue.
    /// Non-zero priorities are stored in a `BTreeMap` ordered by priority.
    pub fn push(&mut self, item: T, priority: i32) {
        self.count += 1;
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
                    if item.is_some() {
                        self.count -= 1;
                    }
                    return item;
                }
            }
        }
        // Pop from simple (priority 0) first
        if let Some(item) = self.simple.pop_front() {
            self.count -= 1;
            return Some(item);
        }
        // Simple is empty, drain any remaining prioritized items (negative priorities)
        if let Some((&key, _)) = self.prioritized.last_key_value() {
            if let Some(items) = self.prioritized.get_mut(&key) {
                let item = items.pop_front();
                if items.is_empty() {
                    self.prioritized.remove(&key);
                }
                if item.is_some() {
                    self.count -= 1;
                }
                return item;
            }
        }
        None
    }
}

impl<T> PriorityQueue<T> {
    /// Returns the total number of items across all priority levels in O(1).
    pub fn len(&self) -> usize {
        self.count
    }

    /// Returns true if the queue contains no items.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns a reference to the highest-priority item without removing it.
    pub fn peek(&self) -> Option<(&T, i32)> {
        if let Some((&key, queue)) = self.prioritized.last_key_value() {
            if key > 0 {
                return queue.front().map(|item| (item, key));
            }
        }
        if let Some(item) = self.simple.front() {
            return Some((item, 0));
        }
        if let Some((&key, queue)) = self.prioritized.last_key_value() {
            return queue.front().map(|item| (item, key));
        }
        None
    }

    /// Converts the queue into an iterator, consuming it.
    /// Items are yielded in priority order (highest first).
    pub fn into_iter(self) -> impl Iterator<Item = (i32, T)> {
        let mut items: Vec<(i32, T)> = Vec::new();
        for (prio, queue) in self.prioritized {
            for item in queue {
                items.push((prio, item));
            }
        }
        for item in self.simple {
            items.push((0, item));
        }
        items.sort_by_key(|b| std::cmp::Reverse(b.0));
        items.into_iter()
    }

    /// Removes all items from the queue, returning them in priority order.
    pub fn drain(&mut self) -> impl Iterator<Item = (i32, T)> + '_ {
        self.count = 0;
        let mut items: Vec<(i32, T)> = Vec::new();
        let prioritized = std::mem::take(&mut self.prioritized);
        for (prio, queue) in prioritized {
            for item in queue {
                items.push((prio, item));
            }
        }
        let simple = std::mem::take(&mut self.simple);
        for item in simple {
            items.push((0, item));
        }
        items.sort_by_key(|b| std::cmp::Reverse(b.0));
        items.into_iter()
    }
}

impl<T> Default for PriorityQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}
