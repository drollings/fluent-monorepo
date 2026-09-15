//! A priority queue with a fast path for zero-priority items.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use crate::pool::PoolError;
use crate::sync::lock;

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
        self.pop_entry().map(|(item, _)| item)
    }

    /// Shared ordering helper: which bucket holds the next item.
    fn pick_bucket(&self) -> Pick {
        let top = self.prioritized.last_key_value().map(|(&key, _)| key);
        match top {
            Some(key) if key > 0 => Pick::Positive(key),
            _ => {
                if !self.simple.is_empty() {
                    return Pick::Simple;
                }
                match top {
                    Some(key) => Pick::Negative(key),
                    None => Pick::Empty,
                }
            }
        }
    }

    #[allow(clippy::redundant_closure_for_method_calls)]
    fn pop_entry(&mut self) -> Option<(T, i32)> {
        match self.pick_bucket() {
            Pick::Positive(key) | Pick::Negative(key) => {
                let item = match self.prioritized.entry(key) {
                    std::collections::btree_map::Entry::Occupied(mut slot) => {
                        let item = slot.get_mut().pop_front();
                        if slot.get().is_empty() {
                            slot.remove();
                        }
                        item
                    }
                    // Unreachable: `key` came from this map with no
                    // mutation in between. Graceful `None` preserves the
                    // old defensive behavior rather than panicking.
                    std::collections::btree_map::Entry::Vacant(_) => None,
                };
                if item.is_some() {
                    self.count -= 1;
                }
                item.map(|v| (v, key))
            }
            Pick::Simple => {
                let item = self.simple.pop_front();
                if item.is_some() {
                    self.count -= 1;
                }
                item.map(|v| (v, 0))
            }
            Pick::Empty => None,
        }
    }

    fn peek_entry(&self) -> Option<(&T, i32)> {
        match self.pick_bucket() {
            Pick::Positive(key) | Pick::Negative(key) => self
                .prioritized
                .get(&key)
                .and_then(|q| q.front().map(|v| (v, key))),
            Pick::Simple => self.simple.front().map(|v| (v, 0)),
            Pick::Empty => None,
        }
    }
}

enum Pick {
    Positive(i32),
    Simple,
    Negative(i32),
    Empty,
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
        self.peek_entry()
    }

    /// Converts the queue into an iterator, consuming it.
    /// Items are yielded in priority order (highest first): repeated `pop`,
    /// so the sequence is exactly the pop order (no separate sort pass and
    /// no extra allocation beyond the output).
    pub fn into_iter(mut self) -> impl Iterator<Item = (i32, T)> {
        let mut items = Vec::with_capacity(self.count);
        while let Some((item, prio)) = self.pop_entry() {
            items.push((prio, item));
        }
        items.into_iter()
    }

    /// Removes all items from the queue, returning them in priority order
    /// (repeated `pop`: exactly the pop order, no separate sort pass).
    pub fn drain(&mut self) -> impl Iterator<Item = (i32, T)> + '_ {
        let mut items = Vec::with_capacity(self.count);
        while let Some((item, prio)) = self.pop_entry() {
            items.push((prio, item));
        }
        items.into_iter()
    }
}

impl<T> Default for PriorityQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// A bounded, concurrent priority queue with close-wakes-waiters semantics.
///
/// Composes the shared [`crate::close::CloseWaker`] core for the close/wake
/// discipline (the same core as `crate::pool::Queue`): same
/// close-wakes-waiters behavior, same anti-missed-wakeup pattern (register
/// `notified()` *before* taking the lock), same `std::sync::Mutex` backing
/// (never held across an await — the async mutex's waker bookkeeping would be
/// pure overhead). The only queue-specific state here is the ordering:
/// items are popped by [`PriorityQueue`]'s priority order (descending, FIFO
/// within a priority, priority-0 fast path).
///
/// Ordering (LSP): identical to `PriorityQueue` — priority desc, FIFO within
/// priority, priority-0 fast path.
pub struct BoundedPriorityQueue<T> {
    inner: Mutex<PriorityQueue<T>>,
    capacity: usize,
    waker: crate::close::CloseWaker,
}

impl<T: Send + 'static> BoundedPriorityQueue<T> {
    /// Creates a new bounded priority queue with the given capacity.
    pub fn new(capacity: usize) -> Self {
        debug_assert!(capacity > 0, "queue capacity must be >0");
        Self {
            inner: Mutex::new(PriorityQueue::new()),
            capacity,
            waker: crate::close::CloseWaker::new(),
        }
    }

    /// Pushes an item with the given priority. Returns `Err(Full)` if at
    /// capacity, `Err(Closed)` if closed. Synchronous fast path.
    pub fn push(&self, item: T, priority: i32) -> Result<(), PoolError> {
        if self.waker.is_closed() {
            return Err(PoolError::Closed);
        }
        let mut inner = lock(&self.inner);
        if inner.len() >= self.capacity {
            return Err(PoolError::Full);
        }
        inner.push(item, priority);
        self.waker.wake_item();
        Ok(())
    }

    /// Pushes an item, waiting if the queue is at capacity.
    /// Returns `Err(Closed)` if the queue is closed before space becomes available.
    pub async fn push_wait(&self, item: T, priority: i32) -> Result<(), PoolError> {
        let mut item = Some(item);
        loop {
            if self.waker.is_closed() {
                return Err(PoolError::Closed);
            }
            let waited = self.waker.space_available();
            {
                let mut inner = lock(&self.inner);
                if inner.len() < self.capacity {
                    inner.push(item.take().unwrap(), priority);
                    self.waker.wake_item();
                    return Ok(());
                }
            }
            waited.await;
        }
    }

    /// Pops the highest-priority item, awaiting if empty. Returns `None`
    /// when closed and empty. Frees a blocked `push_wait` submitter on pop
    /// (the same shape as `Queue::pop`).
    pub async fn pop(&self) -> Option<T> {
        loop {
            let notified = self.waker.items_available();
            {
                let mut inner = lock(&self.inner);
                if let Some(item) = inner.pop() {
                    self.waker.wake_space();
                    return Some(item);
                }
                if self.waker.is_closed() {
                    return None;
                }
            }
            notified.await;
        }
    }

    /// Closes the queue, waking all waiters. Subsequent `pop`s return `None`.
    pub fn close(&self) {
        self.waker.close();
    }

    /// Returns the number of items currently queued.
    pub fn len(&self) -> usize {
        lock(&self.inner).len()
    }

    /// Returns true if the queue holds no items.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
