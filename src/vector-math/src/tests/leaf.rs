//! Dependency-leaf invariant: this crate must stay below every consumer,
//! so its manifest must declare no dependencies (workspace-internal or
//! otherwise). Any `[dependencies]` section breaks the acyclicity argument
//! the whole workspace relies on.

#[test]
fn manifest_declares_no_dependencies() {
    let manifest = include_str!("../../Cargo.toml");
    assert!(
        !manifest.lines().any(|line| line.trim() == "[dependencies]"),
        "vector-math must stay dependency-free"
    );
    assert!(
        !manifest.contains("path ="),
        "vector-math must not depend on workspace crates"
    );
}
