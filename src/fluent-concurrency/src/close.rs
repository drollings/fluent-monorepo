//! Shared close-wakes-waiters core for the bounded queues.
//!
//! `pool::Queue` (FIFO) and `queue::BoundedPriorityQueue` (priority-ordered)
//! share exactly one discipline: a closed flag plus two wait channels (items
//! available, space available) with register-`notified()`-before-lock
//! anti-missed-wakeup ordering. This type owns that discipline so the two
//! queues keep only their own storage (`VecDeque` vs. `PriorityQueue` +
//! capacity). Ordering, capacity errors, and wake semantics are unchanged.

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Mutex,
};
use tokio::sync::{futures::Notified, Notify};

use crate::sync::lock;

/// Which wait channel [`CloseWaker::wait_while`] parks on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitChannel {
    /// An item arrives (or the queue closes).
    Items,
    /// Space frees up (or the queue closes).
    Space,
}

/// The close/wake half of a bounded queue. Composed, never reimplemented.
pub struct CloseWaker {
    closed: AtomicBool,
    generation: AtomicUsize,
    item_notify: Notify,
    space_notify: Notify,
}

impl CloseWaker {
    pub fn new() -> Self {
        Self {
            closed: AtomicBool::new(false),
            generation: AtomicUsize::new(0),
            item_notify: Notify::new(),
            space_notify: Notify::new(),
        }
    }

    /// Whether `close` has run. Sequentially-consistent load, as before.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Wake-epoch counter, bumped on every wake and close. A waiter that
    /// observes a new epoch across its check-then-park window re-checks
    /// instead of parking on a stale read.
    pub fn generation(&self) -> usize {
        self.generation.load(Ordering::SeqCst)
    }

    /// Future resolving when an item arrives or the queue closes. Create it
    /// *before* taking the storage lock (anti-missed-wakeup).
    ///
    /// Low-level primitive for callers that already arm the wait; new code
    /// prefers [`CloseWaker::wait_while`].
    pub fn items_available(&self) -> Notified<'_> {
        self.item_notify.notified()
    }

    /// Future resolving when space frees up or the queue closes. Create it
    /// *before* taking the storage lock (anti-missed-wakeup).
    ///
    /// Low-level primitive for callers that already arm the wait; new code
    /// prefers [`CloseWaker::wait_while`].
    pub fn space_available(&self) -> Notified<'_> {
        self.space_notify.notified()
    }

    /// Park until `keep_waiting` reports done. The wait registers first,
    /// then the predicate re-checks under the caller's lock, so a wake
    /// landing in the window surfaces as fresh state or a new epoch —
    /// never a lost park. Returns for the caller to act under its lock.
    pub async fn wait_while<T, F>(
        &self,
        channel: WaitChannel,
        mutex: &Mutex<T>,
        mut keep_waiting: F,
    ) where
        F: FnMut(&mut T) -> bool,
    {
        loop {
            let epoch = self.generation.load(Ordering::SeqCst);
            let notified = match channel {
                WaitChannel::Items => self.item_notify.notified(),
                WaitChannel::Space => self.space_notify.notified(),
            };
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut guard = lock(mutex);
                if !keep_waiting(&mut guard) {
                    return;
                }
            }
            if self.generation.load(Ordering::SeqCst) != epoch {
                continue;
            }
            notified.await;
        }
    }

    /// Wake one item waiter (after a push).
    pub fn wake_item(&self) {
        self.bump_generation();
        self.item_notify.notify_one();
    }

    /// Wake one space waiter (after a pop).
    pub fn wake_space(&self) {
        self.bump_generation();
        self.space_notify.notify_one();
    }

    /// Close the queue, waking all waiters on both channels.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.bump_generation();
        self.item_notify.notify_waiters();
        self.space_notify.notify_waiters();
    }

    /// Advance the wake epoch, saturating instead of wrapping so an epoch
    /// never repeats while a waiter may still hold the old one.
    fn bump_generation(&self) {
        let _ = self
            .generation
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |epoch| {
                epoch.checked_add(1)
            });
    }
}

impl Default for CloseWaker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "../tests/close_core.rs"]
mod tests;
