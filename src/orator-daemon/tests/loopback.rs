//! Loopback test: synthetic source through the durable feed to manifest.

use crate::run_loopback;

#[test]
fn one_second_loopback_yields_a_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = common_core::runtime::block_on_shared(run_loopback(dir.path(), 1));
    let manifest = manifest.expect("loopback succeeds");
    assert_eq!(manifest.segments.len(), 1);
    assert_eq!(manifest.segments[0].frames, 50);
    assert!(dir.path().join("manifest.json").exists());
    assert!(dir.path().join("clock-offsets.json").exists());
}
