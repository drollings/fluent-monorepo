use fluent_concurrency::scope::{scoped, Scope, ScopeCloseReport};
use fluent_wvr::{CapabilitySet, FsCapability};

#[tokio::test]
async fn denied_without_caps_allowed_with_ferry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("probe.txt");
    std::fs::write(&path, b"hello").expect("seed file");
    let fs = FsCapability::new();
    let denied = fs.read(&path).await;
    assert!(denied.is_err());
    let caps = CapabilitySet::new().with(FsCapability::new());
    let data = scoped(caps, async {
        FsCapability::new()
            .read(&path)
            .await
            .expect("ferried read allows")
    })
    .await;
    assert_eq!(data, b"hello");
}

#[tokio::test]
async fn output_passes_through_untouched() {
    let caps = CapabilitySet::new();
    let value = scoped(caps, async { 41 + 1 }).await;
    assert_eq!(value, 42);
}

/// A spawned panic surfaces in the close report instead of vanishing:
/// the report names the failure and carries its payload.
#[tokio::test]
async fn close_report_surfaces_spawned_panic() {
    let mut scope = Scope::new();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    scope.spawn(async move {
        let _ = tx.send(());
        panic!("boom-payload-marker");
    });
    // The send runs in the same synchronous segment as the panic, so a
    // received signal means the task already finished panicking — no
    // race with the abort below.
    rx.await.expect("task ran to its panic");
    let report: ScopeCloseReport = scope.close().await;
    assert_eq!(report.panicked.len(), 1);
    assert!(
        report.panicked[0].contains("boom-payload-marker"),
        "payload preserved, got: {:?}",
        report.panicked
    );
}
