//! Test `Runtime` implementation with paused time support.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use fluent_wvr::Runtime;
use tokio::task::JoinHandle;

/// Test runtime that delegates to a `tokio::runtime::Handle` for deterministic time control.
pub struct TestRuntime {
    handle: tokio::runtime::Handle,
    _seed: u64,
}

impl TestRuntime {
    pub fn new(handle: tokio::runtime::Handle, seed: u64) -> Self {
        Self { handle, _seed: seed }
    }
}

impl Runtime for TestRuntime {
    fn spawn(
        &self,
        future: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> JoinHandle<()> {
        self.handle.spawn(future)
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(duration))
    }

    fn now(&self) -> Instant {
        tokio::time::Instant::now().into_std()
    }
}
