//! Ancestor-walk baseline: the fix and clippy walkers share shape.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::clippy::package_root;
use crate::fix::find_rustfmt_config;

static COUNTER: AtomicU64 = AtomicU64::new(0);

// Scratch tree under the system temp dir (never the repo).
fn scratch_root() -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let root =
        std::env::temp_dir().join(format!("split-m0-ancestor-{}-{}", std::process::id(), id));
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

fn write_file(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parents");
    }
    std::fs::write(path, "x").expect("write");
}

#[test]
fn ancestor_walk_agreement() {
    let root = scratch_root();
    write_file(&root.join("rustfmt.toml"));
    write_file(&root.join("Cargo.toml"));
    let deep = root.join("a").join("b").join("input.rs");
    write_file(&deep);

    // Both walkers find their probe file from the same depth.
    assert_eq!(find_rustfmt_config(&deep), Some(root.join("rustfmt.toml")));
    assert_eq!(package_root(&deep), Some(root.clone()));

    // A nonexistent start still resolves upward: only the probes matter.
    let missing = root.join("nope").join("input.rs");
    assert_eq!(
        find_rustfmt_config(&missing),
        Some(root.join("rustfmt.toml"))
    );

    // With no probe anywhere up the chain, both terminate with None.
    let bare = scratch_root();
    let orphan = bare.join("nope").join("input.rs");
    assert_eq!(find_rustfmt_config(&orphan), None);
    assert_eq!(package_root(&orphan), None);

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&bare).ok();
}

#[test]
fn ancestor_walk_cap_edge() {
    let root = scratch_root();
    write_file(&root.join("rustfmt.toml"));
    write_file(&root.join("Cargo.toml"));

    // Seven levels below the probe: the eighth check still lands on it.
    let mut dir = root.clone();
    for part in ["d1", "d2", "d3", "d4", "d5", "d6", "d7"] {
        dir = dir.join(part);
    }
    let edge = dir.join("input.rs");
    write_file(&edge);
    assert_eq!(find_rustfmt_config(&edge), Some(root.join("rustfmt.toml")));

    // Eight levels below the probe: the root falls off the 8-check cap.
    let deeper = dir.join("d8").join("input.rs");
    write_file(&deeper);
    assert_eq!(find_rustfmt_config(&deeper), None);

    std::fs::remove_dir_all(&root).ok();
}
