//! Supervision zone with async retry, dependency cancellation, and timeout.
//! A `Zone` manages a group of `WorkUnit` tasks and propagates cancellation
//! across dependent tasks when a prerequisite fails.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use fluent_wvr::{CapabilitySet, ConcurrencyError, Runtime, WorkContext, WorkOutput, WorkUnit};
use internment::ArcIntern;
use tokio::task::JoinSet;

const POLL_BUDGET: usize = 64;

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
#[must_use = "Zone must be awaited to completion to get a ZoneSummary"]
pub struct Zone {
    runtime: Arc<dyn Runtime>,
    caps: CapabilitySet,
    deps: HashMap<ArcIntern<str>, Vec<ArcIntern<str>>>,
    task_provides: HashMap<ArcIntern<str>, Vec<ArcIntern<str>>>,
    task_names: HashMap<tokio::task::Id, ArcIntern<str>>,
    abort_handles: HashMap<ArcIntern<str>, tokio::task::AbortHandle>,
    cancelled_tasks: HashSet<ArcIntern<str>>,
    join_set: JoinSet<Result<WorkOutput, ConcurrencyError>>,
    active_count: usize,
    summary: ZoneSummary,
    done: bool,
}

impl Zone {
    /// Creates a new zone with the given runtime and capabilities.
    pub fn new(runtime: Arc<dyn Runtime>, caps: CapabilitySet) -> Self {
        Self {
            runtime,
            caps,
            deps: HashMap::new(),
            task_provides: HashMap::new(),
            task_names: HashMap::new(),
            abort_handles: HashMap::new(),
            cancelled_tasks: HashSet::new(),
            join_set: JoinSet::new(),
            active_count: 0,
            summary: ZoneSummary::default(),
            done: false,
        }
    }

    /// Registers a `WorkUnit` in the zone. Returns `&mut Self` for builder chaining.
    pub fn register(&mut self, unit: Arc<dyn WorkUnit>) -> &mut Self {
        let ctx = WorkContext {
            rt: Arc::clone(&self.runtime),
            caps: self.caps.clone(),
            ..WorkContext::default()
        };
        self.register_with_context(unit, ctx)
    }

    /// Registers a `WorkUnit` with a custom `WorkContext`.
    pub fn register_with_context(&mut self, unit: Arc<dyn WorkUnit>, ctx: WorkContext) -> &mut Self {
        let name: ArcIntern<str> = ArcIntern::from(unit.name());
        let depends: Vec<ArcIntern<str>> = unit.depends().to_vec();
        let provides: Vec<ArcIntern<str>> = unit.provides().to_vec();

        if !depends.is_empty() {
            self.deps.insert(name.clone(), depends);
        }
        if !provides.is_empty() {
            self.task_provides.insert(name.clone(), provides);
        }

        self.spawn_unit(unit, ctx);
        self
    }

    fn spawn_unit(&mut self, unit: Arc<dyn WorkUnit>, ctx: WorkContext) {
        let name: ArcIntern<str> = ArcIntern::from(unit.name());
        let max_retries = ctx.max_retries;
        let timeout_ms = ctx.timeout_ms;

        let abort = self.join_set.spawn(async move {
            execute_with_timeout_and_retry(unit, ctx, max_retries, timeout_ms).await
        });

        let id = abort.id();
        self.task_names.insert(id, name.clone());
        self.abort_handles.insert(name, abort);
        self.active_count += 1;
    }

    fn cancel_dependents_of(&mut self, name: &ArcIntern<str>) {
        let mut to_cancel: Vec<ArcIntern<str>> = Vec::new();
        let mut visited: HashSet<ArcIntern<str>> = HashSet::new();
        let mut stack = vec![name.clone()];

        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(provides) = self.task_provides.get(&current) {
                for provided in provides {
                    for (dep_name, deps) in &self.deps {
                        if deps.contains(provided) && !visited.contains(dep_name) {
                            to_cancel.push(dep_name.clone());
                            stack.push(dep_name.clone());
                        }
                    }
                }
            }
        }

        for task_name in &to_cancel {
            if let Some(handle) = self.abort_handles.get(task_name) {
                if !handle.is_finished() {
                    handle.abort();
                    self.cancelled_tasks.insert(task_name.clone());
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
            return Poll::Ready(std::mem::take(&mut this.summary));
        }

        let mut budget = POLL_BUDGET;
        loop {
            let mut join_set = std::pin::Pin::new(&mut this.join_set);
            match join_set.as_mut().poll_join_next_with_id(cx) {
                Poll::Ready(Some(Ok((id, Ok(output))))) => {
                    let name = this
                        .task_names
                        .remove(&id)
                        .unwrap_or_else(|| ArcIntern::from("unknown"));
                    this.summary.completed.push(ZoneEvent::Completed {
                        name,
                        output,
                    });
                    this.active_count -= 1;
                    budget -= 1;
                }
                Poll::Ready(Some(Ok((id, Err(e))))) => {
                    let name = this
                        .task_names
                        .remove(&id)
                        .unwrap_or_else(|| ArcIntern::from("unknown"));
                    this.cancel_dependents_of(&name);
                    this.summary.panicked.push(ZoneEvent::Panicked {
                        name,
                        info: e.to_string(),
                    });
                    this.active_count -= 1;
                    budget -= 1;
                }
                Poll::Ready(Some(Err(e))) => {
                    let name = this
                        .task_names
                        .remove(&e.id())
                        .unwrap_or_else(|| ArcIntern::from("unknown"));
                    if e.is_cancelled() {
                        let reason = if this.cancelled_tasks.remove(&name) {
                            CancelReason::DependencyFailed
                        } else {
                            CancelReason::Aborted
                        };
                        this.summary.cancelled.push(ZoneEvent::Cancelled {
                            name,
                            reason,
                        });
                    } else {
                        this.cancel_dependents_of(&name);
                        this.summary.panicked.push(ZoneEvent::Panicked {
                            name,
                            info: "task panicked".into(),
                        });
                    }
                    this.active_count -= 1;
                    budget -= 1;
                }
                Poll::Ready(None) => {
                    this.done = true;
                    return Poll::Ready(std::mem::take(&mut this.summary));
                }
                Poll::Pending => {
                    if this.active_count == 0 {
                        this.done = true;
                        return Poll::Ready(std::mem::take(&mut this.summary));
                    }
                    return Poll::Pending;
                }
            }

            if budget == 0 {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }

            if this.active_count == 0 {
                this.done = true;
                return Poll::Ready(std::mem::take(&mut this.summary));
            }
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
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                unit.execute(&ctx)
            })) {
                Ok(Ok(output)) => return Ok(output),
                Ok(Err(e)) => {
                    if attempts > max_retries {
                        return Err(ConcurrencyError::Io(std::io::Error::other(
                            e.to_string(),
                        )));
                    }
                    tokio::time::sleep(Duration::from_millis(100 * u64::from(attempts))).await;
                }
                Err(_) => {
                    if attempts > max_retries {
                        return Err(ConcurrencyError::Io(std::io::Error::other(
                            "task panicked",
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
