//! Unit tests for the owned scope tree: build, drain, close.

use std::time::Duration;

use crate::RootScope;

#[test]
fn empty_tree_builds_and_drains() {
    let mut root = RootScope::build();
    assert!(root.is_empty());
    common_core::runtime::block_on_shared(async move {
        root.shutdown(Duration::from_secs(1)).await;
    });
}

#[test]
fn shutdown_is_idempotent() {
    let mut root = RootScope::default();
    common_core::runtime::block_on_shared(async move {
        root.shutdown(Duration::from_millis(100)).await;
        root.shutdown(Duration::from_millis(100)).await;
    });
}
