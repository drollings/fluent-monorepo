//! P3 file watcher (port of zvec-grep `src/daemon/watch-manager.ts`):
//! debounced event batches drain into `ChangeSet`s, storms compact to
//! directory scopes, errors and resume drift escalate to full
//! reconciliation, and an hourly probe keeps dishonest watchers honest.
//! The OS seam is the `WatchBackend` trait: `NotifyBackend` serves
//! production (`notify`), `ManualBackend` scripts events in tests (the
//! `watchFactory` role).
//!
//! M11.4: lifecycle stays on the bespoke loop + pending-counter scheme;
//! it does not migrate onto `Scope` or `CreditFlow`. `Scope::spawn`
//! needs `&mut` (the manager is `Clone`-shared) and `Scope` panics on
//! drop-if-unclosed, while `close()` here waits for the in-flight drain
//! (`Scope::close` aborts instead — a semantic change pinned against by
//! `close_waits_for_an_in_flight_async_change_callback`). There is no
//! bounded producer/consumer backlog for `CreditFlow` to gate —
//! debounce/max-wait timers are the backpressure — so a credit pair
//! would be speculative. `spawn_tracked` tasks perform no
//! capability-gated I/O and are handle-tracked (pending count + idle
//! `Notify` awaited by `close`/`flush_pending`), which is the ownership
//! discipline. Production closes the chain on Ctrl-C: `manager.close`,
//! then `coordinator.close`, which calls `scheduler.close`. Stays —
//! pinned by the
//! watcher suite (debounce, max-wait, storm, reattach, resume, error
//! escalation, independent retries, close-drain) and the `WATCH_*`
//! constant pins.

use crate::change_set::{ChangeKind, ChangeSet, ChangeSetOptions, ChangeSetSnapshot};
use crate::scheduler::BoxFuture;
use notify::Watcher as _;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// File event kind (maps notify event kinds + test script lines).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEventKind {
    /// Created / renamed-to.
    Created,
    /// Content changed.
    Changed,
    /// Deleted / renamed-away.
    Deleted,
    /// Renamed (stat decides created vs deleted).
    Renamed,
}

/// Backend event delivered to the manager.
#[derive(Debug, Clone)]
pub enum WatcherEvent {
    /// A file event (path already joined/absolute).
    File {
        /// Absolute path.
        path: PathBuf,
        /// Event kind.
        kind: FileEventKind,
    },
    /// Watcher failure (overflow, teardown) with the failing watch key.
    Error {
        /// Watch key (watched directory).
        key: String,
        /// Human message.
        message: String,
    },
}

/// Sink receiving backend events (the `watchFactory` callback role).
pub type WatchEventSink = Arc<dyn Fn(WatcherEvent) + Send + Sync>;

/// Close handle for one backend watch.
pub trait WatchHandle: Send {
    /// Stop watching.
    fn close(&mut self);
}

/// OS watch seam (production `NotifyBackend`, test `ManualBackend`).
pub trait WatchBackend: Send + Sync {
    /// Watch a directory; `recursive` selects the platform strategy.
    /// Failures (unsupported recursion) fall back to a directory tree.
    fn watch(
        &self,
        dir: &Path,
        recursive: bool,
        sink: WatchEventSink,
    ) -> Result<Box<dyn WatchHandle>, String>;
}

/// Batch drain reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchReason {
    /// Watcher events.
    Watch,
    /// Scheduled probe, resume drift, or error escalation.
    Reconcile,
}

/// Platform watch strategy (zvec: Linux always uses per-directory
/// watchers — Node's recursive watcher ignores exclusions and exhausts
/// the inotify quota).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WatchPlatform {
    /// Per-directory non-recursive watches.
    Linux,
    /// Single recursive watch.
    MacOs,
    /// Single recursive watch.
    Windows,
    /// Resolve from the host at `start`.
    #[default]
    Current,
}

impl WatchPlatform {
    fn effective(self) -> Self {
        match self {
            Self::Current => {
                if std::env::consts::OS == "linux" {
                    Self::Linux
                } else if std::env::consts::OS == "macos" {
                    Self::MacOs
                } else {
                    Self::Windows
                }
            }
            platform => platform,
        }
    }
}

/// A configured watch root (mirrors zvec `RootPath` selection flags).
#[derive(Debug, Clone)]
pub struct WatchRoot {
    /// Absolute root path.
    pub path: PathBuf,
    /// Descend into subdirectories.
    pub recursive: bool,
    /// Skip `.gitignore` filtering.
    pub no_ignore: bool,
    /// Watch hidden (dot) directories.
    pub hidden: bool,
    /// Include patterns that override ignores (gitignore-style, relative).
    pub include: Vec<String>,
}

impl WatchRoot {
    /// Recursive root with default selection.
    #[must_use]
    pub fn recursive(path: PathBuf) -> Self {
        Self {
            path,
            recursive: true,
            no_ignore: false,
            hidden: false,
            include: Vec::new(),
        }
    }
}

/// Drain callback: batch + reason (may await; close waits for it).
pub type ChangesSink = Arc<dyn Fn(ChangeSetSnapshot, WatchReason) -> BoxFuture<()> + Send + Sync>;

/// Watch manager options.
#[derive(Clone)]
pub struct WatchOptions {
    /// Workspace root.
    pub root: PathBuf,
    /// Platform strategy.
    pub platform: WatchPlatform,
    /// Debounce delay.
    pub debounce_ms: u64,
    /// Max-wait forced flush.
    pub max_wait_ms: u64,
    /// Full-reconcile probe interval (0 disables).
    pub reconcile_interval_ms: u64,
    /// Resume-drift check interval (0 disables).
    pub resume_check_interval_ms: u64,
    /// Resume-drift threshold.
    pub resume_threshold_ms: u64,
    /// ChangeSet path budget.
    pub max_changed_paths: usize,
    /// OS watch seam.
    pub backend: Arc<dyn WatchBackend>,
    /// Configured roots (empty = track everything under root).
    pub root_paths: Vec<WatchRoot>,
    /// Batch drain.
    pub on_changes: ChangesSink,
    /// Pending-state observations.
    pub on_pending_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    /// Watcher-activity observations (probes excluded).
    pub on_activity: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Per-watch error-recovery state (zvec `WatchRecoveryState`).
#[derive(Debug, Clone, Default)]
struct RecoveryState {
    consecutive_errors: u32,
    reconciliation_pending: bool,
    retry_scheduled: bool,
    stable_scheduled: bool,
}

enum LoopMsg {
    Start,
    Backend(WatcherEvent),
    WatchRegistered {
        key: String,
        handle: Box<dyn WatchHandle>,
    },
    Statted {
        path: PathBuf,
        hint: FileEventKind,
        exists_dir: Option<bool>,
    },
    RefreshTree {
        dir: PathBuf,
        done: Option<oneshot::Sender<()>>,
    },
    FlushNow {
        done: Option<oneshot::Sender<()>>,
    },
    ResumeCheck {
        now: Instant,
    },
    RetryWatch {
        key: String,
    },
    WatchStable {
        key: String,
    },
    FlushDone,
    Close,
}

struct LoopState {
    options: WatchOptions,
    platform: WatchPlatform,
    filters: Arc<FilterState>,
    changes: ChangeSet,
    debounce_at: Option<Instant>,
    maxwait_at: Option<Instant>,
    reconcile_requested: bool,
    last_resume_check: Instant,
    pending_flag: bool,
    lifecycle: Lifecycle,
    handles: HashMap<String, Box<dyn WatchHandle>>,
    watched_dirs: HashSet<PathBuf>,
    recovery: HashMap<String, RecoveryState>,
    reconcile_at: Option<Instant>,
    resume_at: Option<Instant>,
}

/// Loop lifecycle (replaces separate started/closed flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Lifecycle {
    /// Constructed, not yet started.
    #[default]
    New,
    /// Watching.
    Running,
    /// Draining after close.
    Closed,
}

/// Compiled per-root selection filters.
struct FilterState {
    roots: Vec<RootFilter>,
}

struct RootFilter {
    path: PathBuf,
    ignore: ignore::gitignore::Gitignore,
    includes: ignore::gitignore::Gitignore,
    include_prefixes: Vec<String>,
    no_ignore: bool,
    hidden: bool,
}

fn build_filters(root: &Path, root_paths: &[WatchRoot]) -> FilterState {
    let mut roots = Vec::new();
    if root_paths.is_empty() {
        roots.push(build_root_filter(&WatchRoot::recursive(root.to_path_buf())));
    } else {
        for configured in root_paths {
            roots.push(build_root_filter(configured));
        }
    }
    FilterState { roots }
}

fn build_root_filter(configured: &WatchRoot) -> RootFilter {
    let mut ignore_builder = ignore::gitignore::GitignoreBuilder::new(&configured.path);
    let gitignore = configured.path.join(".gitignore");
    if gitignore.is_file() {
        let _ = ignore_builder.add(&gitignore);
    }
    let ignore = ignore_builder.build().unwrap_or_else(|_| {
        ignore::gitignore::GitignoreBuilder::new(&configured.path)
            .build()
            .expect("empty gitignore builder builds")
    });
    let mut include_builder = ignore::gitignore::GitignoreBuilder::new(&configured.path);
    for pattern in &configured.include {
        let _ = include_builder.add_line(None, pattern);
    }
    let includes = include_builder.build().expect("include builder builds");
    let include_prefixes = configured
        .include
        .iter()
        .map(|pattern| pattern.trim_start_matches('/').to_string())
        .collect();
    RootFilter {
        path: configured.path.clone(),
        ignore,
        includes,
        include_prefixes,
        no_ignore: configured.no_ignore,
        hidden: configured.hidden,
    }
}

impl FilterState {
    /// Whether a watched path can affect the index (zvec
    /// `pathCanAffectIndex`; matchers are prebuilt so filtering never
    /// fails — an unreadable rule file yields an empty matcher, which
    /// matches nothing and fails open).
    fn path_tracked(&self, path: &Path, is_dir: bool) -> bool {
        if has_system_segment(path) {
            return false;
        }
        if file_name_is(path, ".gitignore") {
            return true;
        }
        for root in &self.roots {
            if path_starts_with(path, &root.path) {
                return root.path_tracked(path, is_dir);
            }
        }
        false
    }

    /// Whether a directory should be watched during tree walks.
    fn dir_selected(&self, dir: &Path) -> bool {
        if has_system_segment(dir) {
            return false;
        }
        for root in &self.roots {
            if path_starts_with(dir, &root.path) {
                return root.dir_selected(dir);
            }
        }
        false
    }
}

impl RootFilter {
    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.path)
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default()
    }

    fn include_whitelisted(&self, path: &Path, is_dir: bool) -> bool {
        if self.includes.matched(path, is_dir).is_whitelist() {
            return true;
        }
        // Ancestor-of-include: watching the directory is required to
        // observe the included path beneath it.
        let relative = self.relative(path);
        if relative.is_empty() {
            return true;
        }
        let prefix = format!("{relative}/");
        self.include_prefixes.iter().any(|pattern| {
            pattern.starts_with(&prefix) || pattern.trim_end_matches("/**") == relative
        })
    }

    fn path_tracked(&self, path: &Path, is_dir: bool) -> bool {
        if self.no_ignore {
            return true;
        }
        if self.include_whitelisted(path, is_dir) {
            return true;
        }
        if is_hidden_segment(path) && !self.hidden {
            return false;
        }
        !is_ignored_under(&self.ignore, path, is_dir, &self.path)
    }

    fn dir_selected(&self, dir: &Path) -> bool {
        if self.no_ignore {
            return true;
        }
        if self.include_whitelisted(dir, true) {
            return true;
        }
        if is_hidden_segment(dir) && !self.hidden {
            return false;
        }
        !is_ignored_under(&self.ignore, dir, true, &self.path)
    }
}

/// Git ignore semantics for one matcher: an excluded ancestor hides
/// everything beneath it (dir-only patterns never match the file path
/// itself, and a file-level whitelist cannot resurrect a file under an
/// excluded directory). The walk stops at the filter root the matcher is
/// anchored to.
fn is_ignored_under(
    matcher: &ignore::gitignore::Gitignore,
    path: &Path,
    is_dir: bool,
    stop_at: &Path,
) -> bool {
    let mut ancestor = path.parent();
    while let Some(dir) = ancestor {
        if !dir.starts_with(stop_at) {
            break;
        }
        if matcher.matched(dir, true).is_ignore() {
            return true;
        }
        if dir == stop_at {
            break;
        }
        ancestor = dir.parent();
    }
    matcher.matched(path, is_dir).is_ignore()
}

fn has_system_segment(path: &Path) -> bool {    path.components().any(|component| {
        matches!(component.as_os_str().to_str(), Some(".git" | ".guidance"))
    })
}

fn is_hidden_segment(path: &Path) -> bool {
    path.components().any(|component| {
        component.as_os_str().to_str().is_some_and(|segment| {
            segment.starts_with('.') && segment != "." && segment != ".."
        })
    })
}

fn file_name_is(path: &Path, name: &str) -> bool {
    path.file_name().and_then(|file| file.to_str()) == Some(name)
}

fn path_starts_with(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

/// Watch manager handle (cheap clone; the loop task owns the state).
#[derive(Clone)]
pub struct WatchManager {
    shared: Arc<Shared>,
}

struct Shared {
    options: WatchOptions,
    tx: mpsc::UnboundedSender<LoopMsg>,
    pending: AtomicUsize,
    idle: tokio::sync::Notify,
    started: AtomicBool,
}

impl WatchManager {
    /// Create a manager (call `start` to begin watching).
    #[must_use]
    pub fn new(options: WatchOptions) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            options,
            tx,
            pending: AtomicUsize::new(0),
            idle: tokio::sync::Notify::new(),
            started: AtomicBool::new(false),
        });
        let worker = Arc::clone(&shared);
        tokio::spawn(async move { run_loop(worker, rx).await });
        Self { shared }
    }

    /// Begin watching (idempotent; noop after close). Watch installation
    /// is synchronous — events emitted immediately after `start` are
    /// observed, matching the `watchFactory` contract the ported tests
    /// rely on. Only timers arm through the loop.
    pub fn start(&self) {
        if self.shared.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let platform = self.shared.options.platform.effective();
        if platform == WatchPlatform::Linux {
            spawn_walk_tree(&self.shared, self.shared.options.root.clone(), None);
        } else {
            let root = self.shared.options.root.clone();
            let tx = self.shared.tx.clone();
            let sink: WatchEventSink = Arc::new(move |event| {
                let _ = tx.send(LoopMsg::Backend(event));
            });
            match self.shared.options.backend.watch(&root, true, sink) {
                Ok(handle) => {
                    let _ = self.shared.tx.send(LoopMsg::WatchRegistered {
                        key: dir_key(&root),
                        handle,
                    });
                }
                Err(_) => {
                    // Recursive unsupported: fall back to a directory tree.
                    spawn_walk_tree(&self.shared, root, None);
                }
            }
        }
        let _ = self.shared.tx.send(LoopMsg::Start);
    }

    /// Drain in-flight work and stop watching.
    pub async fn close(&self) {
        let _ = self.shared.tx.send(LoopMsg::Close);
        while self.shared.pending.load(Ordering::SeqCst) > 0 {
            self.shared.idle.notified().await;
        }
    }

    /// Flush pending changes now (awaits in-flight work first).
    pub async fn flush_pending(&self) {
        while self.shared.pending.load(Ordering::SeqCst) > 0 {
            self.shared.idle.notified().await;
        }
        let (done_tx, done_rx) = oneshot::channel();
        if self
            .shared
            .tx
            .send(LoopMsg::FlushNow { done: Some(done_tx) })
            .is_err()
        {
            return;
        }
        let _ = done_rx.await;
        while self.shared.pending.load(Ordering::SeqCst) > 0 {
            self.shared.idle.notified().await;
        }
    }

    /// Re-run the directory tree walk (public; `.gitignore` changes
    /// trigger it internally).
    pub async fn refresh_paths(&self) {
        let (done_tx, done_rx) = oneshot::channel();
        if self
            .shared
            .tx
            .send(LoopMsg::RefreshTree {
                dir: self.shared.options.root.clone(),
                done: Some(done_tx),
            })
            .is_err()
        {
            return;
        }
        let _ = done_rx.await;
    }

    /// Resume-drift probe entry point (interval-driven internally;
    /// `now` injectable for deterministic tests).
    pub fn check_for_resume(&self, now: Instant) {
        let _ = self.shared.tx.send(LoopMsg::ResumeCheck { now });
    }
}

fn spawn_tracked(
    shared: &Arc<Shared>,
    future: impl std::future::Future<Output = ()> + Send + 'static,
) {
    shared.pending.fetch_add(1, Ordering::SeqCst);
    let shared = Arc::clone(shared);
    tokio::spawn(async move {
        future.await;
        if shared.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
            shared.idle.notify_waiters();
        }
    });
}

fn fresh_change_set(options: &WatchOptions) -> ChangeSet {
    ChangeSet::new(ChangeSetOptions {
        root: Some(options.root.to_string_lossy().replace('\\', "/")),
        max_changed_paths: options.max_changed_paths,
    })
}

async fn run_loop(shared: Arc<Shared>, mut rx: mpsc::UnboundedReceiver<LoopMsg>) {
    let options = shared.options.clone();
    let mut state = LoopState {
        filters: Arc::new(build_filters(&options.root, &options.root_paths)),
        changes: fresh_change_set(&options),
        debounce_at: None,
        maxwait_at: None,
        reconcile_requested: false,
        last_resume_check: Instant::now(),
        pending_flag: false,
        lifecycle: Lifecycle::New,
        handles: HashMap::new(),
        watched_dirs: HashSet::new(),
        recovery: HashMap::new(),
        reconcile_at: None,
        resume_at: None,
        platform: options.platform.effective(),
        options,
    };
    loop {
        let debounce_sleep = state.debounce_at.map(|at| tokio::time::sleep_until(at.into()));
        let maxwait_sleep = state.maxwait_at.map(|at| tokio::time::sleep_until(at.into()));
        let reconcile_sleep = state.reconcile_at.map(|at| tokio::time::sleep_until(at.into()));
        let resume_sleep = state.resume_at.map(|at| tokio::time::sleep_until(at.into()));
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { return };
                handle_msg(&shared, &mut state, msg);
                if state.lifecycle == Lifecycle::Closed {
                    return;
                }
            }
            () = async_or_pending(debounce_sleep) => {
                state.debounce_at = None;
                start_flush(&shared, &mut state, None);
            }
            () = async_or_pending(maxwait_sleep) => {
                state.maxwait_at = None;
                state.debounce_at = None;
                start_flush(&shared, &mut state, None);
            }
            () = async_or_pending(reconcile_sleep) => {
                if state.options.reconcile_interval_ms > 0 {
                    state.reconcile_at =
                        Some(Instant::now() + Duration::from_millis(state.options.reconcile_interval_ms));
                } else {
                    state.reconcile_at = None;
                }
                queue_full_reconcile(&shared, &mut state);
            }
            () = async_or_pending(resume_sleep) => {
                if state.options.resume_check_interval_ms > 0 {
                    state.resume_at =
                        Some(Instant::now() + Duration::from_millis(state.options.resume_check_interval_ms));
                } else {
                    state.resume_at = None;
                }
                resume_check(&mut state, Instant::now());
                schedule_if_dirty(&shared, &mut state);
            }
        }
    }
}

/// Await a deadline sleep, or pend forever when no deadline is armed.
async fn async_or_pending(sleep: Option<tokio::time::Sleep>) {
    match sleep {
        Some(sleep) => sleep.await,
        None => std::future::pending().await,
    }
}

fn handle_msg(shared: &Arc<Shared>, state: &mut LoopState, msg: LoopMsg) {
    match msg {
        LoopMsg::Start => {
            if state.lifecycle != Lifecycle::New {
                return;
            }
            state.lifecycle = Lifecycle::Running;
            state.last_resume_check = Instant::now();
            if state.options.reconcile_interval_ms > 0 {
                state.reconcile_at =
                    Some(Instant::now() + Duration::from_millis(state.options.reconcile_interval_ms));
            }
            if state.options.resume_check_interval_ms > 0 {
                state.resume_at = Some(
                    Instant::now() + Duration::from_millis(state.options.resume_check_interval_ms),
                );
            }
        }
        LoopMsg::Backend(event) => handle_backend(shared, state, event),
        LoopMsg::WatchRegistered { key, handle } => {
            if state.lifecycle == Lifecycle::Closed {
                return;
            }
            state.handles.insert(key.clone(), handle);
            state.watched_dirs.insert(PathBuf::from(key));
        }
        LoopMsg::Statted { path, hint, exists_dir } => {
            handle_statted(shared, state, &path, hint, exists_dir);
        }
        LoopMsg::RefreshTree { dir, done } => {
            refresh_tree(shared, state, dir, done);
        }
        LoopMsg::FlushNow { done } => {
            start_flush(shared, state, done);
        }
        LoopMsg::ResumeCheck { now } => {
            resume_check(state, now);
            schedule_if_dirty(shared, state);
        }
        LoopMsg::RetryWatch { key } => {
            retry_watch(shared, state, &key);
        }
        LoopMsg::WatchStable { key } => {
            if let Some(recovery) = state.recovery.get_mut(&key) {
                recovery.consecutive_errors = 0;
                recovery.reconciliation_pending = false;
                recovery.stable_scheduled = false;
                if !recovery.retry_scheduled {
                    state.recovery.remove(&key);
                }
            }
        }
        LoopMsg::FlushDone => {
            set_pending(shared, state, false);
        }
        LoopMsg::Close => {
            state.lifecycle = Lifecycle::Closed;
            state.handles.clear();
            state.watched_dirs.clear();
            state.debounce_at = None;
            state.maxwait_at = None;
            emit_pending(shared, state, false);
        }
    }
}

fn handle_backend(shared: &Arc<Shared>, state: &mut LoopState, event: WatcherEvent) {
    match event {
        WatcherEvent::File { path, kind } => {
            if state.lifecycle == Lifecycle::Closed {
                return;
            }
            let tx = shared.tx.clone();
            spawn_tracked(shared, async move {
                let exists_dir = tokio::fs::symlink_metadata(&path)
                    .await
                    .ok()
                    .map(|metadata| metadata.is_dir());
                let _ = tx.send(LoopMsg::Statted { path, hint: kind, exists_dir });
            });
        }
        WatcherEvent::Error { key, message } => {
            handle_watch_error(shared, state, &key, &message);
        }
    }
}

fn handle_statted(
    shared: &Arc<Shared>,
    state: &mut LoopState,
    path: &Path,
    hint: FileEventKind,
    exists_dir: Option<bool>,
) {
    if state.lifecycle == Lifecycle::Closed {
        return;
    }
    let kind = match (exists_dir, hint) {
        (None, _) => {
            if hint == FileEventKind::Renamed {
                prune_dir_watchers(state, path);
            }
            ChangeKind::Deleted
        }
        (Some(true), FileEventKind::Renamed) => {
            spawn_walk_tree(shared, path.to_path_buf(), None);
            ChangeKind::Created
        }
        (Some(_), FileEventKind::Renamed) => ChangeKind::Created,
        (Some(_), _) => ChangeKind::Changed,
    };
    let is_dir = exists_dir.unwrap_or(false);
    if !state.filters.path_tracked(path, is_dir) {
        return;
    }
    if let Some(activity) = &state.options.on_activity {
        activity();
    }
    let path_string = path.to_string_lossy().into_owned();
    if state.changes.add(&path_string, kind, is_dir).is_err() {
        tracing::warn!(path = %path_string, "watcher dropped relative path");
        return;
    }
    if file_name_is(path, ".gitignore") {
        if let Some(parent) = path.parent().map(Path::to_path_buf) {
            refresh_tree(shared, state, parent, None);
        }
    }
    schedule_flush(shared, state);
}

fn handle_watch_error(shared: &Arc<Shared>, state: &mut LoopState, key: &str, _message: &str) {
    if let Some(mut handle) = state.handles.remove(key) {
        handle.close();
    }
    state.watched_dirs.retain(|dir| dir_key(dir) != key);
    let needs_reconcile = {
        let recovery = state.recovery.entry(key.to_string()).or_default();
        recovery.consecutive_errors += 1;
        let escalate = !recovery.reconciliation_pending;
        if escalate {
            recovery.reconciliation_pending = true;
        }
        escalate
    };
    if needs_reconcile {
        queue_full_reconcile(shared, state);
    }
    let (retry_due, stable_due) = {
        let recovery = state.recovery.entry(key.to_string()).or_default();
        let retry_due = state.lifecycle != Lifecycle::Closed && !recovery.retry_scheduled;
        if retry_due {
            recovery.retry_scheduled = true;
        }
        let stable_due = !recovery.stable_scheduled;
        if stable_due {
            recovery.stable_scheduled = true;
        }
        (retry_due, stable_due)
    };
    if retry_due {
        let consecutive = state
            .recovery
            .get(key)
            .map_or(1, |recovery| recovery.consecutive_errors);
        let delay_ms = 100u64.saturating_mul(2u64.pow(consecutive.saturating_sub(1).min(8))).min(5_000);
        let tx = shared.tx.clone();
        let key = key.to_string();
        spawn_tracked(shared, async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            let _ = tx.send(LoopMsg::RetryWatch { key });
        });
    }
    if stable_due {
        let tx = shared.tx.clone();
        let key = key.to_string();
        spawn_tracked(shared, async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let _ = tx.send(LoopMsg::WatchStable { key });
        });
    }
}

fn retry_watch(shared: &Arc<Shared>, state: &mut LoopState, key: &str) {
    if let Some(recovery) = state.recovery.get_mut(key) {
        recovery.retry_scheduled = false;
    }
    if state.lifecycle == Lifecycle::Closed {
        return;
    }
    if Path::new(key) == state.options.root && state.platform != WatchPlatform::Linux {
        // Primary recursive watch (non-Linux strategy).
        if state.lifecycle == Lifecycle::Closed {
            return;
        }
        let root = state.options.root.clone();
        let tx = shared.tx.clone();
        let sink: WatchEventSink = Arc::new(move |event| {
            let _ = tx.send(LoopMsg::Backend(event));
        });
        match state.options.backend.watch(&root, true, sink) {
            Ok(handle) => {
                state.handles.insert(dir_key(&root), handle);
                state.watched_dirs.insert(root);
            }
            Err(_) => {
                spawn_walk_tree(shared, state.options.root.clone(), None);
            }
        }
    } else {
        spawn_walk_tree(shared, PathBuf::from(key), None);
    }
}

/// Spawn an async directory-tree walk reporting registrations to the loop.
fn spawn_walk_tree(shared: &Arc<Shared>, dir: PathBuf, done: Option<oneshot::Sender<()>>) {
    let backend = Arc::clone(&shared.options.backend);
    let filters = Arc::new(build_filters(&shared.options.root, &shared.options.root_paths));
    let tx = shared.tx.clone();
    spawn_tracked(shared, async move {
        walk_dir(&backend, &dir, &tx, &filters).await;
        if let Some(done) = done {
            let _ = done.send(());
        }
    });
}

async fn walk_dir(
    backend: &Arc<dyn WatchBackend>,
    dir: &Path,
    tx: &mpsc::UnboundedSender<LoopMsg>,
    filters: &FilterState,
) {
    if !filters.dir_selected(dir) {
        return;
    }
    let sink_tx = tx.clone();
    let sink: WatchEventSink = Arc::new(move |event| {
        let _ = sink_tx.send(LoopMsg::Backend(event));
    });
    if let Ok(handle) = backend.watch(dir, false, sink) {
        let _ = tx.send(LoopMsg::WatchRegistered {
            key: dir_key(dir),
            handle,
        });
    } else {
        let _ = tx.send(LoopMsg::Backend(WatcherEvent::Error {
            key: dir_key(dir),
            message: "watch failed".to_string(),
        }));
        return;
    }
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    let mut subdirs = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let is_dir = entry.file_type().await.is_ok_and(|kind| kind.is_dir());
        if is_dir && filters.dir_selected(&path) {
            subdirs.push(path);
        }
    }
    for subdir in subdirs {
        Box::pin(walk_dir(backend, &subdir, tx, filters)).await;
    }
}

fn refresh_tree(
    shared: &Arc<Shared>,
    state: &mut LoopState,
    dir: PathBuf,
    done: Option<oneshot::Sender<()>>,
) {
    if state.platform != WatchPlatform::Linux {
        // Recursive strategy covers everything with the primary watch.
        if let Some(done) = done {
            let _ = done.send(());
        }
        return;
    }
    // Rebuild ignore filters (a `.gitignore` change may have arrived),
    // prune the subtree, and re-walk it.
    state.filters = Arc::new(build_filters(&state.options.root, &state.options.root_paths));
    prune_dir_watchers(state, &dir);
    spawn_walk_tree(shared, dir, done);
}

fn prune_dir_watchers(state: &mut LoopState, path: &Path) {
    let stale: Vec<PathBuf> = state
        .watched_dirs
        .iter()
        .filter(|dir| *dir == path || dir.starts_with(path))
        .cloned()
        .collect();
    for dir in stale {
        state.watched_dirs.remove(&dir);
        if let Some(mut handle) = state.handles.remove(&dir_key(&dir)) {
            handle.close();
        }
    }
}

fn queue_full_reconcile(shared: &Arc<Shared>, state: &mut LoopState) {
    state.changes.require_full_reconcile();
    state.reconcile_requested = true;
    schedule_flush(shared, state);
}

fn resume_check(state: &mut LoopState, now: Instant) {
    if now.saturating_duration_since(state.last_resume_check).as_millis() as u64
        >= state.options.resume_threshold_ms
    {
        state.changes.require_full_reconcile();
        state.reconcile_requested = true;
    }
    state.last_resume_check = now;
}

fn schedule_if_dirty(shared: &Arc<Shared>, state: &mut LoopState) {
    if !state.changes.is_empty() {
        schedule_flush(shared, state);
    }
}

fn schedule_flush(shared: &Arc<Shared>, state: &mut LoopState) {
    if state.lifecycle == Lifecycle::Closed {
        return;
    }
    set_pending(shared, state, true);
    let now = Instant::now();
    state.debounce_at = Some(now + Duration::from_millis(state.options.debounce_ms));
    if state.maxwait_at.is_none() {
        state.maxwait_at = Some(now + Duration::from_millis(state.options.max_wait_ms));
    }
}

fn set_pending(shared: &Arc<Shared>, state: &mut LoopState, value: bool) {
    if state.pending_flag == value {
        return;
    }
    state.pending_flag = value;
    emit_pending(shared, state, value);
}

fn emit_pending(shared: &Arc<Shared>, state: &LoopState, value: bool) {
    if let Some(notify) = &state.options.on_pending_change {
        notify(value);
    }
    let _ = shared;
}

fn start_flush(
    shared: &Arc<Shared>,
    state: &mut LoopState,
    done: Option<oneshot::Sender<()>>,
) {
    if state.lifecycle == Lifecycle::Closed || state.changes.is_empty() {
        if let Some(done) = done {
            let _ = done.send(());
        }
        return;
    }
    state.debounce_at = None;
    state.maxwait_at = None;
    let snapshot = state.changes.snapshot();
    let reason = if state.reconcile_requested {
        WatchReason::Reconcile
    } else {
        WatchReason::Watch
    };
    state.changes = fresh_change_set(&state.options);
    state.reconcile_requested = false;
    let sink = Arc::clone(&state.options.on_changes);
    let tx = shared.tx.clone();
    spawn_tracked(shared, async move {
        sink(snapshot, reason).await;
        let _ = tx.send(LoopMsg::FlushDone);
        if let Some(done) = done {
            let _ = done.send(());
        }
    });
}

fn dir_key(dir: &Path) -> String {
    dir.to_string_lossy().into_owned()
}

/// Production backend over the `notify` crate.
pub struct NotifyBackend;

impl NotifyBackend {
    /// Create the production backend.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for NotifyBackend {
    fn default() -> Self {
        Self::new()
    }
}

struct NotifyHandle {
    _watcher: notify::RecommendedWatcher,
}

impl WatchHandle for NotifyHandle {
    fn close(&mut self) {}
}

struct NotifyDispatch {
    sink: WatchEventSink,
    key: String,
}

impl notify::EventHandler for NotifyDispatch {
    fn handle_event(&mut self, event: Result<notify::Event, notify::Error>) {
        match event {
            Ok(event) => {
                for path in event.paths {
                    let kind = match event.kind {
                        notify::EventKind::Create(_) => FileEventKind::Created,
                        notify::EventKind::Modify(_) | notify::EventKind::Any => {
                            FileEventKind::Changed
                        }
                        notify::EventKind::Remove(_) => FileEventKind::Deleted,
                        _ => continue,
                    };
                    (self.sink)(WatcherEvent::File { path, kind });
                }
            }
            Err(error) => (self.sink)(WatcherEvent::Error {
                key: self.key.clone(),
                message: error.to_string(),
            }),
        }
    }
}

impl WatchBackend for NotifyBackend {
    fn watch(
        &self,
        dir: &Path,
        recursive: bool,
        sink: WatchEventSink,
    ) -> Result<Box<dyn WatchHandle>, String> {
        let mode = if recursive {
            notify::RecursiveMode::Recursive
        } else {
            notify::RecursiveMode::NonRecursive
        };
        let key = dir_key(dir);
        let mut watcher =
            notify::RecommendedWatcher::new(NotifyDispatch { sink, key }, notify::Config::default())
                .map_err(|error| error.to_string())?;
        watcher.watch(dir, mode).map_err(|error| error.to_string())?;
        Ok(Box::new(NotifyHandle { _watcher: watcher }))
    }
}

/// Scripted backend for tests (the `watchFactory` role): records watches,
/// replays events and errors on demand.
#[cfg(test)]
#[derive(Clone)]
pub struct ManualBackend {
    inner: Arc<std::sync::Mutex<ManualInner>>,
}

#[cfg(test)]
struct ManualInner {
    watches: HashMap<String, ManualWatch>,
    watch_log: Vec<(PathBuf, bool)>,
    fail_recursive: bool,
}

#[cfg(test)]
struct ManualWatch {
    #[allow(dead_code)]
    recursive: bool,
    sink: WatchEventSink,
}

#[cfg(test)]
impl Default for ManualBackend {
    fn default() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(ManualInner {
                watches: HashMap::new(),
                watch_log: Vec::new(),
                fail_recursive: false,
            })),
        }
    }
}

#[cfg(test)]
impl ManualBackend {
    /// Create a backend that accepts every watch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a backend rejecting recursive watches (Linux strategy tests).
    pub fn fail_recursive() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(ManualInner {
                watches: HashMap::new(),
                watch_log: Vec::new(),
                fail_recursive: true,
            })),
        }
    }

    /// Emit a file event through the sink registered for `dir`.
    pub fn emit(&self, dir: &Path, name: &str, kind: FileEventKind) {
        let sink = common_core::sync::lock(&self.inner)
            .watches
            .get(&dir_key(dir))
            .map(|watch| Arc::clone(&watch.sink));
        if let Some(sink) = sink {
            sink(WatcherEvent::File {
                path: dir.join(name),
                kind,
            });
        }
    }

    /// Emit a watcher error for `dir` (overflow / teardown).
    pub fn emit_error(&self, dir: &Path, message: &str) {
        let sink = common_core::sync::lock(&self.inner)
            .watches
            .get(&dir_key(dir))
            .map(|watch| Arc::clone(&watch.sink));
        if let Some(sink) = sink {
            sink(WatcherEvent::Error {
                key: dir_key(dir),
                message: message.to_string(),
            });
        }
    }

    /// Whether `dir` currently has a live watch.
    #[must_use]
    pub fn is_watched(&self, dir: &Path) -> bool {
        common_core::sync::lock(&self.inner)
            .watches
            .contains_key(&dir_key(dir))
    }

    /// Currently watched directories.
    #[must_use]
    pub fn watched_dirs(&self) -> Vec<PathBuf> {
        common_core::sync::lock(&self.inner)
            .watches
            .keys()
            .map(PathBuf::from)
            .collect()
    }

    /// Total watch calls (including replaced/closed).
    #[must_use]
    pub fn total_watches(&self) -> usize {
        common_core::sync::lock(&self.inner).watch_log.len()
    }

    /// Watch calls for one directory (reattach counting).
    #[must_use]
    pub fn watch_count(&self, dir: &Path) -> usize {
        common_core::sync::lock(&self.inner)
            .watch_log
            .iter()
            .filter(|(watched, _)| watched == dir)
            .count()
    }

    /// Whether any recursive watch was attempted.
    #[must_use]
    pub fn used_recursive(&self) -> bool {
        common_core::sync::lock(&self.inner)
            .watch_log
            .iter()
            .any(|(_, recursive)| *recursive)
    }
}

#[cfg(test)]
struct ManualHandle {
    backend: ManualBackend,
    key: String,
}

#[cfg(test)]
impl WatchHandle for ManualHandle {
    fn close(&mut self) {
        common_core::sync::lock(&self.backend.inner)
            .watches
            .remove(&self.key);
    }
}

#[cfg(test)]
impl WatchBackend for ManualBackend {
    fn watch(
        &self,
        dir: &Path,
        recursive: bool,
        sink: WatchEventSink,
    ) -> Result<Box<dyn WatchHandle>, String> {
        let mut inner = common_core::sync::lock(&self.inner);
        inner.watch_log.push((dir.to_path_buf(), recursive));
        if recursive && inner.fail_recursive {
            return Err("recursive unsupported".to_string());
        }
        inner.watches.insert(
            dir_key(dir),
            ManualWatch {
                recursive,
                sink,
            },
        );
        Ok(Box::new(ManualHandle {
            backend: self.clone(),
            key: dir_key(dir),
        }))
    }
}

#[cfg(test)]
#[path = "../tests/watcher.rs"]
mod tests;

