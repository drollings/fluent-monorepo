//! Production `Runtime` implementation backed by `tokio::spawn` / `tokio::time::sleep`.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use fluent_wvr::Runtime;
use tokio::task::JoinHandle;

/// Production runtime that delegates to `tokio::spawn`, `tokio::time::sleep`, and `Instant::now()`.
#[derive(Clone)]
pub struct TokioRuntime;

impl Runtime for TokioRuntime {
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) -> JoinHandle<()> {
        tokio::spawn(future)
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(duration))
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}
