//! Credit-based backpressure flow control.
//! Mirrors RabbitMQ's `credit_flow` semantics: a sender has a credit budget,
//! the receiver periodically sends bumps when its counter reaches `more_after`.

use std::future::Future;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

use tokio::sync::{mpsc, Mutex};

/// Configuration for a credit flow pair.
#[derive(Debug, Clone)]
pub struct CreditSpec {
    pub initial: usize,
    pub more_after: usize,
}

/// Sends work items, consuming one credit per send.
/// Blocks when credit is exhausted until the receiver sends a bump.
pub struct CreditSender {
    credit: AtomicIsize,
    bump_rx: Mutex<mpsc::UnboundedReceiver<usize>>,
}

impl CreditSender {
    pub async fn send<F, Fut, T>(&self, op: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        loop {
            let current = self.credit.load(Ordering::SeqCst);
            if current > 0 {
                if self
                    .credit
                    .compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    return op().await;
                }
            } else {
                let mut rx = self.bump_rx.lock().await;
                if let Some(amount) = rx.recv().await {
                    self.credit.fetch_add(amount as isize, Ordering::SeqCst);
                }
            }
        }
    }
}

/// Receives work notifications and sends credit bumps upstream.
pub struct CreditReceiver {
    spec: CreditSpec,
    counter: AtomicUsize,
    bump_tx: mpsc::UnboundedSender<usize>,
}

impl CreditReceiver {
    pub fn recv(&self) {
        let prev = self.counter.fetch_add(1, Ordering::SeqCst);
        if prev + 1 >= self.spec.more_after {
            self.counter.store(0, Ordering::SeqCst);
            let _ = self.bump_tx.send(self.spec.more_after);
        }
    }
}

/// Creates a new credit flow pair from a `CreditSpec`.
/// Returns `(sender, receiver)`.
pub fn new(spec: CreditSpec) -> (CreditSender, CreditReceiver) {
    let (bump_tx, bump_rx) = mpsc::unbounded_channel();
    (
        CreditSender {
            credit: AtomicIsize::new(spec.initial as isize),
            bump_rx: Mutex::new(bump_rx),
        },
        CreditReceiver {
            spec,
            counter: AtomicUsize::new(0),
            bump_tx,
        },
    )
}
