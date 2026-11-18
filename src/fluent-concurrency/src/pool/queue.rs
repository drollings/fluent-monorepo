//! Bounded close-wakes-waiters queue (`Queue`), owned by the `pool` module
//! tree and re-exported through the `pool` facade.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use common_core::capacity::nonzero_capacity;

use crate::sync as sync_lock;

use super::traits::PoolError;

struct QueueInner<T> {
    items: Mutex<VecDeque<T>>,
    capacity: usize,
    waker: crate::close::CloseWaker,
}

/// A bounded, concurrent, multi-producer/multi-consumer queue with
/// close-wakes-waiters semantics.
///
/// The queue's backing store is a `std::sync::Mutex` — never held across an
/// `.await` (every guard is scoped so it drops before the future yields), so
/// the async mutex's waker bookkeeping would be pure overhead. Poisoned
/// mutexes are recovered via `crate::sync::lock` rather than panicking
/// the caller. Contrast `CreditSender` in `flow.rs`, whose mutex guards the
/// bump receiver across `recv().await` and therefore must stay a
/// `tokio::sync::Mutex`.
///
/// # Examples
///
/// ```no_run
/// use fluent_concurrency::pool::Queue;
///
/// # async fn example() {
/// let q = Queue::new(10).expect("example capacity is nonzero");
/// q.push(42).unwrap();
/// q.push(99).unwrap();
/// q.close();
///
/// assert_eq!(q.pop().await, Some(42));
/// assert_eq!(q.pop().await, Some(99));
/// assert_eq!(q.pop().await, None); // closed + empty
/// # }
/// ```
pub struct Queue<T> {
    inner: Arc<QueueInner<T>>,
}

impl<T: Send + 'static> Queue<T> {
    /// Creates a new bounded queue with the given capacity.
    ///
    /// Refuses zero capacity: a zero-slot queue could never hand an item
    /// from a blocked `push_wait` to a `pop`, hanging both sides.
    pub fn new(capacity: usize) -> Result<Self, String> {
        let capacity = nonzero_capacity(capacity, "queue")?;
        Ok(Self {
            inner: Arc::new(QueueInner {
                items: Mutex::new(VecDeque::with_capacity(capacity.get())),
                capacity: capacity.get(),
                waker: crate::close::CloseWaker::new(),
            }),
        })
    }

    /// Pushes an item into the queue. Returns `Err(Full)` if at capacity.
    ///
    /// The fast path is synchronous (never blocks).
    pub fn push(&self, item: T) -> Result<(), PoolError> {
        if self.inner.waker.is_closed() {
            return Err(PoolError::Closed);
        }
        let mut items = sync_lock::lock(&self.inner.items);
        if items.len() >= self.inner.capacity {
            return Err(PoolError::Full);
        }
        items.push_back(item);
        self.inner.waker.wake_item();
        Ok(())
    }

    /// Pushes an item, waiting if the queue is at capacity.
    /// Returns `Err(Closed)` if the queue is closed before space becomes available.
    pub async fn push_wait(&self, item: T) -> Result<(), PoolError> {
        let mut item = Some(item);
        loop {
            if self.inner.waker.is_closed() {
                return Err(PoolError::Closed);
            }
            {
                let mut items = sync_lock::lock(&self.inner.items);
                if items.len() < self.inner.capacity {
                    items.push_back(item.take().unwrap());
                    self.inner.waker.wake_item();
                    return Ok(());
                }
            }
            // Same full/closed verdicts, parked on the shared helper so a
            // space wake in the check-then-park window still releases us.
            let capacity = self.inner.capacity;
            self.inner
                .waker
                .wait_while(crate::close::WaitChannel::Space, &self.inner.items, |items| {
                    items.len() >= capacity && !self.inner.waker.is_closed()
                })
                .await;
        }
    }

    /// Pops an item from the queue, awaiting if empty. Returns `None` when closed.
    pub async fn pop(&self) -> Option<T> {
        loop {
            {
                let mut items = sync_lock::lock(&self.inner.items);
                if let Some(item) = items.pop_front() {
                    self.inner.waker.wake_space();
                    return Some(item);
                }
                if self.inner.waker.is_closed() {
                    return None;
                }
            }
            // Same empty/closed verdicts, parked on the shared helper so an
            // item wake or close in the check-then-park window still wakes us.
            self.inner
                .waker
                .wait_while(crate::close::WaitChannel::Items, &self.inner.items, |items| {
                    items.is_empty() && !self.inner.waker.is_closed()
                })
                .await;
        }
    }

    /// Closes the queue, waking all waiters. Subsequent `pop`s return `None`.
    pub fn close(&self) {
        self.inner.waker.close();
    }
}
