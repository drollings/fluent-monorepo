//! P3 watcher tests (port of zvec-grep `test/watch-manager.test.mjs`):
//! debounce + overflow, scheduled probes, Linux per-directory strategy,
//! ignore filtering, storm compaction, reattach, resume drift, error
//! recovery, independent retries, and close-drains-inflight.

use crate::change_set::ChangeSetSnapshot;
use crate::scheduler::BoxFuture;
use crate::watcher::{
    FileEventKind, ManualBackend, WatchBackend, WatchOptions, WatchManager, WatchPlatform,
    WatchReason, WatchRoot,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn test_options(
    root: PathBuf,
    backend: ManualBackend,
    on_changes: Arc<dyn Fn(ChangeSetSnapshot, WatchReason) -> BoxFuture<()> + Send + Sync>,
) -> WatchOptions {
    WatchOptions {
        root,
        platform: WatchPlatform::MacOs,
        debounce_ms: 5,
        max_wait_ms: 20,
        reconcile_interval_ms: 0,
        resume_check_interval_ms: 0,
        resume_threshold_ms: 90_000,
        max_changed_paths: 1_000,
        backend: Arc::new(backend) as Arc<dyn WatchBackend>,
        root_paths: Vec::new(),
        on_changes,
        on_pending_change: None,
        on_activity: None,
    }
}

type BatchLog = Arc<std::sync::Mutex<Vec<(ChangeSetSnapshot, WatchReason)>>>;
type BatchSink = Arc<dyn Fn(ChangeSetSnapshot, WatchReason) -> BoxFuture<()> + Send + Sync>;

fn collect_sink() -> (BatchLog, BatchSink) {
    let batches = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_batches = Arc::clone(&batches);
    let sink: Arc<dyn Fn(ChangeSetSnapshot, WatchReason) -> BoxFuture<()> + Send + Sync> =
        Arc::new(move |changes, reason| {
            sink_batches.lock().unwrap().push((changes, reason));
            Box::pin(async {}) as BoxFuture<()>
        });
    (batches, sink)
}

async fn wait_for<F>(mut predicate: F)
where
    F: FnMut() -> bool,
{
    for _ in 0..500 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("condition was not reached");
}

fn temp_repo(name: &str) -> tempfile::TempDir {
    tempfile::Builder::new().prefix(name).tempdir().expect("tempdir")
}

#[tokio::test]
async fn debounces_file_changes_and_reports_overflow_reconciliation() {
    let temp = temp_repo("guidance-watch-");
    let root = temp.path().join("repo");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src").join("a.ts"), "export const a = 1;\n").unwrap();
    let backend = ManualBackend::new();
    let (batches, sink) = collect_sink();
    let manager = WatchManager::new(test_options(root.clone(), backend.clone(), sink));
    manager.start();
    backend.emit(&root, "src/a.ts", FileEventKind::Changed);
    backend.emit(&root, "src/a.ts", FileEventKind::Changed);
    wait_for(|| batches.lock().unwrap().len() == 1).await;
    assert_eq!(
        batches.lock().unwrap()[0].0.touched_files,
        vec![root.join("src").join("a.ts").to_string_lossy().into_owned()]
    );
    backend.emit_error(&root, "overflow");
    wait_for(|| batches.lock().unwrap().len() == 2).await;
    assert!(batches.lock().unwrap()[1].0.force_full_reconcile);
    manager.close().await;
}

#[tokio::test]
async fn scheduled_reconciliation_does_not_count_as_activity() {
    let temp = temp_repo("guidance-watch-activity-");
    let root = temp.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.ts"), "export const a = 1;\n").unwrap();
    let backend = ManualBackend::new();
    let activities = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reasons: Arc<std::sync::Mutex<Vec<WatchReason>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let activities_sink = Arc::clone(&activities);
    let reasons_sink = Arc::clone(&reasons);
    let manager = WatchManager::new(WatchOptions {
        reconcile_interval_ms: 10,
        on_activity: Some(Arc::new(move || {
            activities_sink.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })),
        on_changes: Arc::new(move |_, reason| {
            reasons_sink.lock().unwrap().push(reason);
            Box::pin(async {}) as BoxFuture<()>
        }),
        ..test_options(root.clone(), backend.clone(), Arc::new(|_, _| Box::pin(async {}) as BoxFuture<()>))
    });
    manager.start();
    wait_for(|| reasons.lock().unwrap().contains(&WatchReason::Reconcile)).await;
    assert_eq!(activities.load(std::sync::atomic::Ordering::SeqCst), 0);
    backend.emit(&root, "a.ts", FileEventKind::Changed);
    wait_for(|| activities.load(std::sync::atomic::Ordering::SeqCst) == 1).await;
    assert_eq!(activities.load(std::sync::atomic::Ordering::SeqCst), 1);
    manager.close().await;
}

#[tokio::test]
async fn uses_per_directory_watchers_on_linux() {
    let temp = temp_repo("guidance-watch-fallback-");
    let root = temp.path().join("repo");
    let nested = root.join("src");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("a.ts"), "export const a = 1;\n").unwrap();
    let backend = ManualBackend::fail_recursive();
    let (batches, sink) = collect_sink();
    let manager = WatchManager::new(WatchOptions {
        platform: WatchPlatform::Linux,
        ..test_options(root.clone(), backend.clone(), sink)
    });
    manager.start();
    wait_for(|| backend.is_watched(&nested)).await;
    assert!(!backend.used_recursive(), "recursive watcher must not be attempted on linux");
    backend.emit(&nested, "a.ts", FileEventKind::Changed);
    wait_for(|| batches.lock().unwrap().len() == 1).await;
    assert_eq!(
        batches.lock().unwrap()[0].0.touched_files,
        vec![nested.join("a.ts").to_string_lossy().into_owned()]
    );
    manager.close().await;
}

#[tokio::test]
async fn drops_ignored_file_events_before_creating_a_batch() {
    let temp = temp_repo("guidance-watch-ignore-");
    let root = temp.path().join("repo");
    let dependency = root.join("node_modules").join("pkg");
    std::fs::create_dir_all(&dependency).unwrap();
    std::fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
    std::fs::write(dependency.join("index.js"), "module.exports = true;\n").unwrap();
    let backend = ManualBackend::new();
    let (batches, sink) = collect_sink();
    let pending: Arc<std::sync::Mutex<Vec<bool>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let pending_sink = Arc::clone(&pending);
    let manager = WatchManager::new(WatchOptions {
        root_paths: vec![WatchRoot::recursive(root.clone())],
        on_pending_change: Some(Arc::new(move |value| {
            pending_sink.lock().unwrap().push(value);
        })),
        ..test_options(root.clone(), backend.clone(), sink)
    });
    manager.start();
    backend.emit(&root, "node_modules/pkg/index.js", FileEventKind::Changed);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(batches.lock().unwrap().is_empty());
    assert!(pending.lock().unwrap().is_empty());
    backend.emit(&root, ".gitignore", FileEventKind::Changed);
    wait_for(|| batches.lock().unwrap().len() == 1).await;
    assert_eq!(
        batches.lock().unwrap()[0].0.rescan_directories,
        vec![root.to_string_lossy().into_owned()]
    );
    manager.close().await;
}

#[tokio::test]
async fn fallback_prunes_ignored_directories_and_restores_included_ones() {
    let temp = temp_repo("guidance-watch-ignore-fallback-");
    let root = temp.path().join("repo");
    let source = root.join("src");
    let dependencies = root.join("node_modules");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(dependencies.join("pkg")).unwrap();
    std::fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
    let backend = ManualBackend::fail_recursive();
    let (_batches, sink) = collect_sink();
    let manager = WatchManager::new(WatchOptions {
        platform: WatchPlatform::Linux,
        root_paths: vec![WatchRoot::recursive(root.clone())],
        ..test_options(root.clone(), backend.clone(), sink)
    });
    manager.start();
    wait_for(|| backend.is_watched(&source)).await;
    assert!(!backend.is_watched(&dependencies));
    std::fs::write(root.join(".gitignore"), "!node_modules/\n").unwrap();
    backend.emit(&root, ".gitignore", FileEventKind::Changed);
    wait_for(|| backend.is_watched(&dependencies)).await;
    manager.close().await;
}

#[tokio::test]
async fn fallback_honors_no_ignore_when_selecting_directories() {
    let temp = temp_repo("guidance-watch-no-ignore-");
    let root = temp.path().join("repo");
    let dependencies = root.join("node_modules");
    std::fs::create_dir_all(&dependencies).unwrap();
    std::fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
    let backend = ManualBackend::fail_recursive();
    let (_batches, sink) = collect_sink();
    let manager = WatchManager::new(WatchOptions {
        platform: WatchPlatform::Linux,
        root_paths: vec![WatchRoot {
            path: root.clone(),
            recursive: true,
            no_ignore: true,
            hidden: false,
            include: Vec::new(),
        }],
        ..test_options(root.clone(), backend.clone(), sink)
    });
    manager.start();
    wait_for(|| backend.is_watched(&dependencies)).await;
    manager.close().await;
}

#[tokio::test]
async fn fallback_mirrors_scanner_hidden_directory_selection() {
    let temp = temp_repo("guidance-watch-hidden-");
    let root = temp.path().join("repo");
    let source = root.join("src");
    let idea = root.join(".idea");
    let vscode = root.join(".vscode");
    let git = root.join(".git");
    let metadata = root.join(".guidance");
    for directory in [&source, &idea, &vscode, &git, &metadata] {
        std::fs::create_dir_all(directory).unwrap();
    }

    async fn run(root: &std::path::Path, root_path: WatchRoot) -> Vec<PathBuf> {
        let backend = ManualBackend::fail_recursive();
        let (_batches, sink) = collect_sink();
        let manager = WatchManager::new(WatchOptions {
            platform: WatchPlatform::Linux,
            root_paths: vec![root_path],
            ..test_options(root.to_path_buf(), backend.clone(), sink)
        });
        manager.start();
        wait_for(|| backend.is_watched(&root.join("src"))).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        let watched = backend.watched_dirs();
        manager.close().await;
        watched
    }

    let defaults = run(&root, WatchRoot::recursive(root.clone())).await;
    assert!(!defaults.contains(&idea));
    assert!(!defaults.contains(&vscode));

    let with_hidden = run(
        &root,
        WatchRoot {
            hidden: true,
            ..WatchRoot::recursive(root.clone())
        },
    )
    .await;
    assert!(with_hidden.contains(&idea));
    assert!(with_hidden.contains(&vscode));
    assert!(!with_hidden.contains(&git));
    assert!(!with_hidden.contains(&metadata));

    let with_include = run(
        &root,
        WatchRoot {
            include: vec![".vscode/settings.json".to_string()],
            ..WatchRoot::recursive(root.clone())
        },
    )
    .await;
    assert!(!with_include.contains(&idea));
    assert!(with_include.contains(&vscode));
}

#[tokio::test]
async fn compacts_an_exact_event_storm_into_one_directory_scan() {
    let temp = temp_repo("guidance-watch-storm-");
    let root = temp.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    for index in 0..4 {
        std::fs::write(root.join(format!("{index}.ts")), format!("export const value{index} = {index};\n")).unwrap();
    }
    let backend = ManualBackend::new();
    let (batches, sink) = collect_sink();
    let manager = WatchManager::new(WatchOptions {
        debounce_ms: 10,
        max_wait_ms: 30,
        max_changed_paths: 3,
        ..test_options(root.clone(), backend.clone(), sink)
    });
    manager.start();
    for index in 0..4 {
        backend.emit(&root, &format!("{index}.ts"), FileEventKind::Changed);
    }
    wait_for(|| batches.lock().unwrap().len() == 1).await;
    let snapshot = batches.lock().unwrap()[0].0.clone();
    assert!(snapshot.touched_files.is_empty(), "{snapshot:?}");
    assert_eq!(snapshot.rescan_directories, vec![root.to_string_lossy().into_owned()]);
    assert!(snapshot.deleted_prefixes.is_empty(), "{snapshot:?}");
    assert!(!snapshot.force_full_reconcile);
    assert_eq!(batches.lock().unwrap().len(), 1);
    manager.close().await;
}

#[tokio::test]
async fn fallback_reattaches_after_directory_deleted_and_recreated() {
    let temp = temp_repo("guidance-watch-recreate-");
    let root = temp.path().join("repo");
    let nested = root.join("src");
    std::fs::create_dir_all(&nested).unwrap();
    let backend = ManualBackend::fail_recursive();
    let (_batches, sink) = collect_sink();
    let manager = WatchManager::new(test_options(root.clone(), backend.clone(), sink));
    manager.start();
    wait_for(|| backend.watch_count(&nested) == 1).await;
    std::fs::remove_dir_all(&nested).unwrap();
    backend.emit(&root, "src", FileEventKind::Renamed);
    tokio::time::sleep(Duration::from_millis(10)).await;
    std::fs::create_dir_all(&nested).unwrap();
    backend.emit(&root, "src", FileEventKind::Renamed);
    wait_for(|| backend.watch_count(&nested) == 2).await;
    manager.close().await;
}

#[tokio::test]
async fn resume_drift_requests_reconciliation_and_pending_spans_debounce() {
    let temp = temp_repo("guidance-watch-resume-");
    let root = temp.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.ts"), "export const a = 1;\n").unwrap();
    let backend = ManualBackend::new();
    let pending: Arc<std::sync::Mutex<Vec<bool>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let reasons: Arc<std::sync::Mutex<Vec<WatchReason>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let pending_sink = Arc::clone(&pending);
    let reasons_sink = Arc::clone(&reasons);
    let manager = WatchManager::new(WatchOptions {
        debounce_ms: 20,
        max_wait_ms: 40,
        resume_threshold_ms: 100,
        on_pending_change: Some(Arc::new(move |value| {
            pending_sink.lock().unwrap().push(value);
        })),
        on_changes: Arc::new(move |_, reason| {
            reasons_sink.lock().unwrap().push(reason);
            Box::pin(async {}) as BoxFuture<()>
        }),
        ..test_options(root.clone(), backend.clone(), Arc::new(|_, _| Box::pin(async {}) as BoxFuture<()>))
    });
    manager.start();
    backend.emit(&root, "a.ts", FileEventKind::Changed);
    wait_for(|| pending.lock().unwrap().contains(&true)).await;
    assert!(pending.lock().unwrap().last().copied().unwrap_or(false));
    wait_for(|| reasons.lock().unwrap().len() == 1).await;
    assert!(!pending.lock().unwrap().last().copied().unwrap_or(true));
    manager.check_for_resume(std::time::Instant::now() + Duration::from_secs(1_000));
    wait_for(|| reasons.lock().unwrap().len() == 2).await;
    assert_eq!(reasons.lock().unwrap()[1], WatchReason::Reconcile);
    manager.close().await;
}

#[tokio::test]
async fn errors_trigger_reconciliation_and_replace_the_failed_watcher() {
    let temp = temp_repo("guidance-watch-error-");
    let root = temp.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    let backend = ManualBackend::new();
    let reasons: Arc<std::sync::Mutex<Vec<WatchReason>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let reasons_sink = Arc::clone(&reasons);
    let sink: Arc<dyn Fn(ChangeSetSnapshot, WatchReason) -> BoxFuture<()> + Send + Sync> =
        Arc::new(move |_, reason| {
            reasons_sink.lock().unwrap().push(reason);
            Box::pin(async {}) as BoxFuture<()>
        });
    let manager = WatchManager::new(test_options(root.clone(), backend.clone(), sink));
    manager.start();
    wait_for(|| backend.total_watches() == 1).await;
    backend.emit_error(&root, "watch failed");
    wait_for(|| reasons.lock().unwrap().len() == 1).await;
    wait_for(|| backend.total_watches() == 2).await;
    backend.emit_error(&root, "watch still failed");
    wait_for(|| backend.total_watches() == 3).await;
    assert_eq!(reasons.lock().unwrap()[0], WatchReason::Reconcile);
    assert_eq!(reasons.lock().unwrap().len(), 1);
    manager.close().await;
}

#[tokio::test]
async fn directory_watcher_retries_are_independent() {
    let temp = temp_repo("guidance-watch-multi-error-");
    let root = temp.path().join("repo");
    let first = root.join("first");
    let second = root.join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let backend = ManualBackend::fail_recursive();
    let (_batches, sink) = collect_sink();
    let manager = WatchManager::new(WatchOptions {
        platform: WatchPlatform::Linux,
        ..test_options(root.clone(), backend.clone(), sink)
    });
    manager.start();
    wait_for(|| backend.watch_count(&first) == 1 && backend.watch_count(&second) == 1).await;
    backend.emit_error(&first, "first failed");
    backend.emit_error(&second, "second failed");
    wait_for(|| backend.watch_count(&first) == 2 && backend.watch_count(&second) == 2).await;
    manager.close().await;
}

#[tokio::test]
async fn close_waits_for_an_in_flight_async_change_callback() {
    let temp = temp_repo("guidance-watch-close-");
    let root = temp.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.ts"), "export const a = 1;\n").unwrap();
    let backend = ManualBackend::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let started_sink = Arc::clone(&started);
    let release_sink = Arc::clone(&release);
    let sink: Arc<dyn Fn(ChangeSetSnapshot, WatchReason) -> BoxFuture<()> + Send + Sync> =
        Arc::new(move |_, _| {
            started_sink.notify_one();
            let release_sink = Arc::clone(&release_sink);
            Box::pin(async move {
                release_sink.notified().await;
            }) as BoxFuture<()>
        });
    let manager = WatchManager::new(test_options(root.clone(), backend.clone(), sink));
    manager.start();
    backend.emit(&root, "a.ts", FileEventKind::Changed);
    started.notified().await;
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let closed_task = Arc::clone(&closed);
    let manager_task = manager;
    let closing = tokio::spawn(async move {
        manager_task.close().await;
        closed_task.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!closed.load(std::sync::atomic::Ordering::SeqCst));
    release.notify_one();
    closing.await.expect("close");
    assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
}
