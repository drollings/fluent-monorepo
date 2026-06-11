//! Supervision zone with async retry, dependency cancellation, and timeout.
//! A `Zone` manages a group of `WorkUnit` tasks and propagates cancellation
//! across dependent tasks when a prerequisite fails.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use fluent_wvr::{CapabilitySet, ConcurrencyError, Runtime, WorkContext, WorkOutput, WorkUnit};
use internment::ArcIntern;
use tokio::sync::mpsc;

/// Events emitted by tasks running inside a `Zone`.
/// Events emitted by tasks running inside a `Zone`.
#[derive(Debug, Clone)]
pub enum ZoneEvent {
    Completed {
        name: ArcIntern<str>,
        output: WorkOutput,
    },
    Panicked {
        name: ArcIntern<str>,
        info: String,
    },
    Cancelled {
        name: ArcIntern<str>,
        reason: CancelReason,
    },
}

/// Reasons why a zone task was cancelled.
#[derive(Debug, Clone)]
pub enum CancelReason {
    Timeout,
    DependencyFailed,
    Aborted,
}

/// Summary of a zone's execution result.
#[derive(Debug, Default)]
pub struct ZoneSummary {
    pub completed: Vec<ZoneEvent>,
    pub panicked: Vec<ZoneEvent>,
    pub cancelled: Vec<ZoneEvent>,
}

/// A supervision zone that manages a group of `WorkUnit` tasks with retry, timeout,
/// and dependency-based cancellation. Implements `Future` to drive task completion.
pub struct Zone {
    runtime: Arc<dyn Runtime>,
    caps: CapabilitySet,
    deps: HashMap<ArcIntern<str>, Vec<ArcIntern<str>>>,
    task_provides: HashMap<ArcIntern<str>, Vec<ArcIntern<str>>>,
    task_handles: HashMap<ArcIntern<str>, tokio::task::AbortHandle>,
    event_tx: mpsc::UnboundedSender<ZoneEvent>,
    event_rx: mpsc::UnboundedReceiver<ZoneEvent>,
    active_count: usize,
    summary: ZoneSummary,
    done: bool,
}

impl Zone {
    pub fn new(runtime: Arc<dyn Runtime>, caps: CapabilitySet) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        Self {
            runtime,
            caps,
            deps: HashMap::new(),
            task_provides: HashMap::new(),
            task_handles: HashMap::new(),
            event_tx,
            event_rx,
            active_count: 0,
            summary: ZoneSummary::default(),
            done: false,
        }
    }

    pub fn register(&mut self, unit: Arc<dyn WorkUnit>) {
        let name: ArcIntern<str> = ArcIntern::from(unit.name());
        let depends: Vec<ArcIntern<str>> = unit.depends().to_vec();
        let provides: Vec<ArcIntern<str>> = unit.provides().to_vec();

        if !depends.is_empty() {
            self.deps.insert(name.clone(), depends);
        }
        if !provides.is_empty() {
            self.task_provides.insert(name.clone(), provides);
        }

        let ctx = WorkContext {
            rt: Arc::clone(&self.runtime),
            caps: self.caps.clone(),
            ..WorkContext::default()
        };

        self.spawn_unit(unit, ctx);
    }

    fn spawn_unit(&mut self, unit: Arc<dyn WorkUnit>, ctx: WorkContext) {
        let name: ArcIntern<str> = ArcIntern::from(unit.name());
        let name_for_task = name.clone();
        let event_tx = self.event_tx.clone();
        let max_retries = ctx.max_retries;
        let timeout_ms = ctx.timeout_ms;

        let handle = tokio::spawn(async move {
            let result = execute_with_timeout_and_retry(unit, ctx, max_retries, timeout_ms).await;
            let event = match result {
                Ok(output) => ZoneEvent::Completed {
                    name: name_for_task.clone(),
                    output,
                },
                Err(e) => ZoneEvent::Panicked {
                    name: name_for_task.clone(),
                    info: e.to_string(),
                },
            };
            let _ = event_tx.send(event);
        });

        self.task_handles.insert(name, handle.abort_handle());
        self.active_count += 1;
    }

    fn cancel_dependents(&self, name: &ArcIntern<str>) {
        let Some(provides) = self.task_provides.get(name) else {
            return;
        };
        for provided in provides {
            for (dep_name, deps) in &self.deps {
                if deps.contains(provided) {
                    if let Some(handle) = self.task_handles.get(dep_name) {
                        handle.abort();
                    }
                }
            }
        }
    }
}

impl Future for Zone {
    type Output = ZoneSummary;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(ZoneSummary {
                completed: std::mem::take(&mut this.summary.completed),
                panicked: std::mem::take(&mut this.summary.panicked),
                cancelled: std::mem::take(&mut this.summary.cancelled),
            });
        }

        loop {
            match this.event_rx.poll_recv(cx) {
                Poll::Ready(Some(event)) => {
                    match event {
                        ZoneEvent::Completed { .. } => {
                            this.summary.completed.push(event);
                            this.active_count -= 1;
                        }
                        ZoneEvent::Panicked { ref name, .. } => {
                            this.cancel_dependents(name);
                            this.summary.panicked.push(event);
                            this.active_count -= 1;
                        }
                        ZoneEvent::Cancelled { .. } => {
                            this.summary.cancelled.push(event);
                            this.active_count -= 1;
                        }
                    }
                    if this.active_count == 0 {
                        this.done = true;
                        return Poll::Ready(ZoneSummary {
                            completed: std::mem::take(&mut this.summary.completed),
                            panicked: std::mem::take(&mut this.summary.panicked),
                            cancelled: std::mem::take(&mut this.summary.cancelled),
                        });
                    }
                }
                Poll::Ready(None) => {
                    this.done = true;
                    return Poll::Ready(ZoneSummary {
                        completed: std::mem::take(&mut this.summary.completed),
                        panicked: std::mem::take(&mut this.summary.panicked),
                        cancelled: std::mem::take(&mut this.summary.cancelled),
                    });
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl Drop for Zone {
    fn drop(&mut self) {
        for handle in self.task_handles.values() {
            handle.abort();
        }
    }
}

async fn execute_with_timeout_and_retry(
    unit: Arc<dyn WorkUnit>,
    ctx: WorkContext,
    max_retries: u32,
    timeout_ms: u64,
) -> Result<WorkOutput, ConcurrencyError> {
    let fut = async {
        let mut attempts = 0u32;
        loop {
            attempts += 1;
            match unit.execute(&ctx) {
                Ok(output) => return Ok(output),
                Err(e) => {
                    if attempts > max_retries {
                        return Err(ConcurrencyError::Io(std::io::Error::other(
                            e.to_string(),
                        )));
                    }
                    tokio::time::sleep(Duration::from_millis(100 * u64::from(attempts))).await;
                }
            }
        }
    };

    if timeout_ms > 0 {
        match tokio::time::timeout(Duration::from_millis(timeout_ms), fut).await {
            Ok(result) => result,
            Err(_) => Err(ConcurrencyError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "zone task timed out",
            ))),
        }
    } else {
        fut.await
    }
}
