use common_core::walk::{
    SkipReason, WalkFilter, sniff_is_binary, walk_files, walk_files_filtered,
};
use std::collections::HashSet;
use tempfile::TempDir;

fn tree() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    for d in ["src", "target", ".git", "node_modules"] {
        std::fs::create_dir_all(root.join(d)).expect("mkdir");
    }
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("write");
    std::fs::write(root.join("src/big.rs"), vec![b'x'; 5000]).expect("write");
    std::fs::write(root.join("target/cached.rs"), "cached").expect("write");
    std::fs::write(root.join(".git/HEAD"), "ref").expect("write");
    std::fs::write(root.join("node_modules/dep.js"), "dep").expect("write");
    // Binary: >30% control chars in the first 8KB.
    let mut blob = vec![0x00u8; 3000];
    blob.extend(vec![b'a'; 7000]);
    std::fs::write(root.join("src/blob.o"), blob).expect("write");
    dir
}

#[test]
fn legacy_walk_files_unchanged() {
    let dir = tree();
    let mut seen = Vec::new();
    walk_files(dir.path(), &["rs"], |path| {
        seen.push(path.to_path_buf());
    });
    // Legacy behavior: hidden dirs skipped, target skipped via DEFAULT_SKIP.
    assert!(seen.iter().any(|p| p.ends_with("src/main.rs")));
}

#[test]
fn filtered_walk_applies_size_and_binary_caps() {
    let dir = tree();
    let filter = WalkFilter {
        extensions: HashSet::from(["rs".to_string()]),
        max_bytes: Some(1024),
        skip_binary: true,
        extra_skip_dirs: HashSet::from(["node_modules".to_string()]),
        honor_gitignore: false,
    };
    let mut seen = Vec::new();
    let mut skipped = Vec::new();
    walk_files_filtered(dir.path(), &filter, &mut |entry| seen.push(entry), &mut |path, reason| {
        skipped.push((path.to_path_buf(), reason));
    });
    assert_eq!(seen.len(), 1, "only src/main.rs survives: {seen:?}");
    assert!(seen[0].path.ends_with("src/main.rs"));
    assert_eq!(seen[0].size_bytes, 13);
    let reasons: Vec<SkipReason> = skipped.iter().map(|(_, r)| *r).collect();
    assert!(reasons.contains(&SkipReason::TooLarge), "big.rs: {skipped:?}");
}

#[test]
fn binary_sniff_uses_control_char_ratio() {
    assert!(!sniff_is_binary(b"fn main() {}\n"));
    let mut blob = vec![0x00u8; 3000];
    blob.extend(vec![b'a'; 7000]);
    assert!(sniff_is_binary(&blob));
    assert!(!sniff_is_binary(b"\t\n\r normal text with tabs"));
}

#[test]
fn filtered_walk_honors_gitignore() {
    let dir = tree();
    std::fs::write(dir.path().join(".gitignore"), "src/ignored.rs\n").expect("write");
    std::fs::write(dir.path().join("src/ignored.rs"), "x").expect("write");
    let filter = WalkFilter {
        extensions: HashSet::from(["rs".to_string()]),
        max_bytes: None,
        skip_binary: false,
        extra_skip_dirs: HashSet::new(),
        honor_gitignore: true,
    };
    let mut seen = Vec::new();
    let mut skipped = Vec::new();
    walk_files_filtered(dir.path(), &filter, &mut |entry| seen.push(entry), &mut |path, reason| {
        skipped.push((path.to_path_buf(), reason));
    });
    assert!(seen.iter().any(|p| p.path.ends_with("src/main.rs")));
    assert!(!seen.iter().any(|p| p.path.ends_with("src/ignored.rs")), "{seen:?}");
    assert!(
        skipped.iter().any(|(_, r)| *r == SkipReason::Ignored),
        "{skipped:?}"
    );
}
