//! Shared close-wakes-waiters core for the bounded queues.
//!
//! `pool::Queue` (FIFO) and `queue::BoundedPriorityQueue` (priority-ordered)
//! share exactly one discipline: a closed flag plus two wait channels (items
//! available, space available) with register-`notified()`-before-lock
//! anti-missed-wakeup ordering. This type owns that discipline so the two
//! queues keep only their own storage (`VecDeque` vs. `PriorityQueue` +
//! capacity). Ordering, capacity errors, and wake semantics are unchanged.

use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{futures::Notified, Notify};

/// The close/wake half of a bounded queue. Composed, never reimplemented.
pub struct CloseWaker {
    closed: AtomicBool,
    item_notify: Notify,
    space_notify: Notify,
}

impl CloseWaker {
    pub fn new() -> Self {
        Self {
            closed: AtomicBool::new(false),
            item_notify: Notify::new(),
            space_notify: Notify::new(),
        }
    }

    /// Whether `close` has run. Sequentially-consistent load, as before.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Future resolving when an item arrives or the queue closes. Create it
    /// *before* taking the storage lock (anti-missed-wakeup).
    pub fn items_available(&self) -> Notified<'_> {
        self.item_notify.notified()
    }

    /// Future resolving when space frees up or the queue closes. Create it
    /// *before* taking the storage lock (anti-missed-wakeup).
    pub fn space_available(&self) -> Notified<'_> {
        self.space_notify.notified()
    }

    /// Wake one item waiter (after a push).
    pub fn wake_item(&self) {
        self.item_notify.notify_one();
    }

    /// Wake one space waiter (after a pop).
    pub fn wake_space(&self) {
        self.space_notify.notify_one();
    }

    /// Close the queue, waking all waiters on both channels.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.item_notify.notify_waiters();
        self.space_notify.notify_waiters();
    }
}

impl Default for CloseWaker {
    fn default() -> Self {
        Self::new()
    }
}
