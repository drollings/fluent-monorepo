//! Sync→async runtime bridge: run a future to completion from a possibly-sync
//! context.
//!
//! This is the workspace's canonical home for sync→async bridging. `fluent-db` and
//! `fluent-llm` delegate their own `block_on` entry points here (module paths
//! preserved for callers), and `fluent-concurrency` composes [`block_on_shared`]
//! rather than building its own fallback runtime, so the bridging semantics live
//! in exactly one zero-domain place instead of duplicated copies that diverge.
//!
//! Zero-domain rationale: bridging between synchronous and asynchronous
//! execution is generic tokio plumbing — it knows nothing about nodes, LLMs,
//! or databases — so it belongs in `common-core`, the workspace's only
//! zero-domain crate.
//!
//! Two bridges live here because they differ in one property that callers must
//! choose deliberately:
//!
//! | | [`block_on`] | [`block_on_shared`] |
//! |---|---|---|
//! | concurrent sync callers | serialize (one drives at a time) | run in parallel |
//! | survives past the call | yes | yes |
//!
//! Reach for [`block_on`] when one future is being run and nothing else is
//! competing for the thread; reach for [`block_on_shared`] when several
//! threads bridge at once (a limiter, a worker pool, a CLI fanning out).

use std::sync::OnceLock;

/// Run a future to completion synchronously from a possibly-sync context.
///
/// - Inside a **multi-threaded** tokio runtime worker: drive via
///   `tokio::task::block_in_place(|| handle.block_on(fut))` so the worker is
///   not starved and a bare `handle.block_on` never panics.
/// - Inside a **current-thread** runtime (where `block_in_place` panics):
///   `handle.block_on(fut)`.
/// - With **no active runtime** (e.g. a `spawn_blocking` thread, a scoped OS
///   thread, or a non-tokio thread): the process-wide fallback runtime.
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread {
                handle.block_on(fut)
            } else {
                tokio::task::block_in_place(move || handle.block_on(fut))
            }
        }
        Err(_) => fallback_runtime().block_on(fut),
    }
}

/// Run a future to completion from a thread that has no active runtime, on a
/// process-wide runtime that supports **concurrent** sync callers.
///
/// Use this — not [`block_on`] — when several threads may bridge at once (a
/// limiter's sync entry point, a worker pool handler, a CLI fanning out).
/// [`block_on`]'s fallback runtime is single-threaded, so concurrent callers
/// serialize on it, which defeats the point of whatever is limiting them.
///
/// Two properties this buys, both load-bearing:
///
/// - **The runtime outlives the call.** Never bridge on a runtime built for a
///   single call: dropping a per-call runtime also drops everything the bridged
///   future spawned onto it. For a pooled HTTP connection that continuation is
///   the handoff that returns the connection to its client's pool — drop it
///   mid-flight and the connection is stranded with a waker whose runtime is
///   gone, so the next caller that checks it out waits forever. That is a
///   lost wakeup with no error and no recovery, and it is invisible until the
///   affected caller hits its timeout.
/// - **Callers do not serialize.** A multi-thread runtime's `Handle::block_on`
///   runs each caller's future on that caller's own thread while one shared
///   worker pool services everything they spawn, so N concurrent bridges make
///   progress together instead of queueing.
///
/// The worker pool is sized for servicing spawned continuations, not for CPU
/// parallelism — concurrency of the work itself is the caller's limiter's job.
pub fn block_on_shared<F: std::future::Future>(fut: F) -> F::Output {
    shared_runtime().handle().block_on(fut)
}

/// Process-wide fallback runtime, used only when no tokio runtime is active on
/// the calling thread (plain `fn main()`, scoped OS threads, blocking threads).
fn fallback_runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build common-core fallback runtime")
    })
}

/// Process-wide runtime backing [`block_on_shared`]. Built once and kept for
/// the life of the process so it outlives every bridge performed on it.
fn shared_runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("common-shared")
            .enable_all()
            .build()
            .expect("build common-core shared runtime")
    })
}
