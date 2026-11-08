use fluent_concurrency::scope::scoped;
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
