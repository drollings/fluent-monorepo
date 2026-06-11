//! Structured concurrency via `Scope` — all spawned tasks are joined or aborted on drop.
//! Capabilities are propagated to child tasks through a task-local.

use std::future::Future;

use fluent_wvr::CapabilitySet;
use tokio::task::{AbortHandle, JoinSet};

tokio::task_local! {
    pub static CURRENT_CAPS: CapabilitySet;
}

#[must_use = "Scopes must be explicitly closed with .close().await"]
pub struct Scope {
    tasks: JoinSet<()>,
    closed: bool,
}

impl Scope {
    pub fn new() -> Self {
        Self {
            tasks: JoinSet::new(),
            closed: false,
        }
    }

    pub fn spawn<F>(&mut self, future: F) -> AbortHandle
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let caps = CURRENT_CAPS.try_with(Clone::clone).unwrap_or_default();
        self.tasks.spawn(async move {
            CURRENT_CAPS.scope(caps, future).await;
        })
    }

    pub async fn close(&mut self) {
        self.closed = true;
        self.tasks.abort_all();
        while self.tasks.join_next().await.is_some() {}
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if !self.closed {
            self.tasks.abort_all();
            if std::thread::panicking() {
                // During panic unwind a secondary panic would abort the process.
                // Log the violation and let the original panic propagate.
                tracing::error!(
                    "Scope dropped without calling .close().await during panic unwind; \
                     all tasks were aborted"
                );
            } else {
                panic!(
                    "Scope dropped without calling .close().await — \
                     all tasks were aborted. This is a structured concurrency violation. \
                     Call scope.close().await before dropping."
                );
            }
        }
    }
}
