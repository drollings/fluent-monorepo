//! P3 `change_set` tests (port of zvec-grep `test/change-set.test.mjs` verbatim
//! plus the absolute-path validation the Rust boundary adds).

use crate::change_set::{ChangeKind, ChangeSet, ChangeSetOptions};
use std::time::Instant;

fn join(root: &str, parts: &[&str]) -> String {
    let mut path = std::path::PathBuf::from(root);
    for part in parts {
        path.push(part);
    }
    path.to_string_lossy().into_owned()
}

#[test]
fn folds_child_paths_and_invalidates_gitignore_subtrees() {
    let root = join(&std::env::temp_dir().to_string_lossy(), &["change-set-repo"]);
    let mut changes = ChangeSet::new(ChangeSetOptions::default());
    changes.add(&join(&root, &["src", "a.ts"]), ChangeKind::Changed, false).unwrap();
    changes.add(&join(&root, &["src", "b.ts"]), ChangeKind::Changed, false).unwrap();
    changes.add(&join(&root, &["src"]), ChangeKind::Created, true).unwrap();
    changes.add(&join(&root, &["packages", ".gitignore"]), ChangeKind::Changed, false).unwrap();
    let snapshot = changes.snapshot();
    let mut expected_dirs = vec![join(&root, &["packages"]), join(&root, &["src"])];
    expected_dirs.sort();
    assert!(snapshot.touched_files.is_empty(), "{snapshot:?}");
    assert_eq!(snapshot.rescan_directories, expected_dirs);
    assert!(snapshot.deleted_prefixes.is_empty(), "{snapshot:?}");
    assert!(!snapshot.force_full_reconcile);
}

#[test]
fn collapses_deleted_prefixes() {
    let root = join(&std::env::temp_dir().to_string_lossy(), &["change-set-storm-repo"]);
    let mut changes = ChangeSet::new(ChangeSetOptions {
        root: Some(root.clone()),
        max_changed_paths: 10,
    });
    changes.add(&join(&root, &["src", "a.ts"]), ChangeKind::Deleted, false).unwrap();
    changes.add(&join(&root, &["src"]), ChangeKind::Deleted, true).unwrap();
    changes.add(&join(&root, &["other.ts"]), ChangeKind::Changed, false).unwrap();
    changes.add(&join(&root, &["third.ts"]), ChangeKind::Changed, false).unwrap();
    let snapshot = changes.snapshot();
    assert_eq!(snapshot.deleted_prefixes, vec![join(&root, &["src"])]);
    assert!(!snapshot.force_full_reconcile);
}

#[test]
fn batches_large_watcher_bursts_without_blocking() {
    let root = join(&std::env::temp_dir().to_string_lossy(), &["change-set-large-burst-repo"]);
    let mut changes = ChangeSet::new(ChangeSetOptions {
        root: None,
        max_changed_paths: 1_000,
    });
    let started = Instant::now();
    for index in 0..200 {
        let name = "removed.ts".to_string();
        let package = format!("package-{index}");
        changes
            .add(&join(&root, &[&package, &name]), ChangeKind::Deleted, false)
            .unwrap();
    }
    let snapshot = changes.snapshot();
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(snapshot.deleted_prefixes.len(), 200);
    assert!(elapsed_ms < 1_500.0, "processing 200 watcher paths took {elapsed_ms:.0}ms");
}

#[test]
fn compacts_exact_event_storms_without_full_reconciliation() {
    let root = join(&std::env::temp_dir().to_string_lossy(), &["change-set-exact-overflow-repo"]);
    let mut changes = ChangeSet::new(ChangeSetOptions {
        root: Some(root.clone()),
        max_changed_paths: 3,
    });
    changes.add(&join(&root, &["src", "a.ts"]), ChangeKind::Changed, false).unwrap();
    changes.add(&join(&root, &["src", "b.ts"]), ChangeKind::Changed, false).unwrap();
    changes.add(&join(&root, &["src", "c.ts"]), ChangeKind::Changed, false).unwrap();
    let snapshot = changes.snapshot();
    assert!(snapshot.touched_files.is_empty(), "{snapshot:?}");
    assert_eq!(snapshot.rescan_directories, vec![join(&root, &["src"])]);
    assert!(snapshot.deleted_prefixes.is_empty(), "{snapshot:?}");
    assert!(!snapshot.force_full_reconcile);
}

#[test]
fn bounds_twelve_thousand_exact_paths_to_directory_scopes() {
    let root = join(&std::env::temp_dir().to_string_lossy(), &["change-set-twelve-thousand-repo"]);
    let mut changes = ChangeSet::new(ChangeSetOptions {
        root: Some(root.clone()),
        max_changed_paths: 1_000,
    });
    let started = Instant::now();
    for index in 0..12_000 {
        let package = format!("package-{}", index / 500);
        let name = format!("{index}.ts");
        changes
            .add(&join(&root, &[&package, &name]), ChangeKind::Changed, false)
            .unwrap();
    }
    let snapshot = changes.snapshot();
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(!snapshot.force_full_reconcile);
    assert!(snapshot.touched_files.is_empty(), "{snapshot:?}");
    assert_eq!(snapshot.rescan_directories.len(), 24, "{snapshot:?}");
    assert!(elapsed_ms < 1_500.0, "processing 12000 paths took {elapsed_ms:.0}ms");
}

#[test]
fn retains_a_path_after_explicit_full_reconciliation_request() {
    let root = join(&std::env::temp_dir().to_string_lossy(), &["change-set-reconciliation-repo"]);
    let mut changes = ChangeSet::new(ChangeSetOptions::default());
    changes.require_full_reconcile();
    changes.add(&join(&root, &["changed.ts"]), ChangeKind::Changed, false).unwrap();
    let snapshot = changes.snapshot();
    assert_eq!(snapshot.touched_files, vec![join(&root, &["changed.ts"])]);
    assert!(snapshot.rescan_directories.is_empty(), "{snapshot:?}");
    assert!(snapshot.deleted_prefixes.is_empty(), "{snapshot:?}");
    assert!(snapshot.force_full_reconcile);
}

#[test]
fn rejects_relative_paths_at_the_boundary() {
    let mut changes = ChangeSet::new(ChangeSetOptions::default());
    let error = changes.add("relative/path.ts", ChangeKind::Changed, false).unwrap_err();
    assert!(matches!(error, crate::change_set::ChangeSetError::NotAbsolute(_)));
}

// M1.1 characterization: lexical path helpers pinned verbatim before the
// `common_core::path` extraction. These call the real implementations
// (now `pub(crate)`) so any semantic drift fails the gate.

#[test]
fn m1_normalize_path_matrix() {
    use crate::change_set::normalize_path;
    // Roadmap cases first.
    assert_eq!(normalize_path("a//b/./c/../d"), "a/b/d");
    assert_eq!(normalize_path("C:\\a\\b"), "C:/a/b");
    assert_eq!(normalize_path("C:/x"), "C:/x");
    // Dotdot never climbs above root `/`.
    assert_eq!(normalize_path("/../a"), "/a");
    assert_eq!(normalize_path("/.."), "/");
    assert_eq!(normalize_path("/a/../../b"), "/b");
    // Relative dotdot that would climb above cwd collapses to empty/remainder.
    assert_eq!(normalize_path(".."), "");
    assert_eq!(normalize_path("../a"), "a");
    assert_eq!(normalize_path("a/../.."), "");
    // Trailing slashes strip except root.
    assert_eq!(normalize_path("/a/b/"), "/a/b");
    assert_eq!(normalize_path("a/b/"), "a/b");
    assert_eq!(normalize_path("/"), "/");
    // Empty and dot-only inputs.
    assert_eq!(normalize_path(""), "");
    assert_eq!(normalize_path("."), "");
    assert_eq!(normalize_path("./a"), "a");
    // Absolute basics and separator runs.
    assert_eq!(normalize_path("/x"), "/x");
    assert_eq!(normalize_path("/a//b"), "/a/b");
    assert_eq!(normalize_path("/a/./b"), "/a/b");
    assert_eq!(normalize_path("a/b"), "a/b");
}

#[test]
fn m1_parent_dir_matrix() {
    use crate::change_set::parent_dir;
    assert_eq!(parent_dir("/"), None);
    assert_eq!(parent_dir("/a"), Some("/".to_string()));
    assert_eq!(parent_dir("/a/b"), Some("/a".to_string()));
    assert_eq!(parent_dir("/a/b/"), Some("/a".to_string()));
    assert_eq!(parent_dir("a"), None);
    assert_eq!(parent_dir("a/b"), Some("a".to_string()));
}

#[test]
fn m1_file_name_matrix() {
    use crate::change_set::file_name;
    assert_eq!(file_name("/a/b.ts"), Some("b.ts"));
    assert_eq!(file_name("/a/b/"), Some("b"));
    assert_eq!(file_name("a"), Some("a"));
    assert_eq!(file_name("/"), Some(""));
    assert_eq!(file_name(".gitignore"), Some(".gitignore"));
    assert_eq!(file_name("/x/.gitignore"), Some(".gitignore"));
}

#[test]
fn m1_is_absolute_path_matrix() {
    use crate::change_set::is_absolute_path;
    assert!(is_absolute_path("/x"));
    assert!(is_absolute_path("/"));
    assert!(is_absolute_path("C:/x"));
    assert!(is_absolute_path("C:\\x"));
    assert!(!is_absolute_path("relative/path.ts"));
    assert!(!is_absolute_path(""));
    assert!(!is_absolute_path("C:x"));
}

#[test]
fn m1_add_observes_normalization() {
    // Normalization is observable through the public `add`/`snapshot` path:
    // separators collapse and `.`/`..` resolve before storage.
    let root = join(&std::env::temp_dir().to_string_lossy(), &["m1-normalize-repo"]);
    let mut changes = ChangeSet::new(ChangeSetOptions::default());
    changes
        .add(&format!("{root}//src/./a.ts"), ChangeKind::Changed, false)
        .unwrap();
    changes
        .add(&format!("{root}/src/../src/b.ts"), ChangeKind::Changed, false)
        .unwrap();
    let snapshot = changes.snapshot();
    assert_eq!(
        snapshot.touched_files,
        vec![format!("{root}/src/a.ts"), format!("{root}/src/b.ts")]
    );
}

// M2.1 characterization: snapshot/collapse ordering pinned verbatim before
// the `common_core::sort` extraction. Snapshots sort byte-wise; collapse
// prunes by ascending path length (shortest scope wins).

#[test]
fn m2_snapshot_sorts_reverse_insertions_and_dedups() {
    let root = join(&std::env::temp_dir().to_string_lossy(), &["m2-order-repo"]);
    let mut changes = ChangeSet::new(ChangeSetOptions::default());
    for name in ["c.ts", "a.ts", "b.ts", "a.ts"] {
        changes
            .add(&join(&root, &[name]), ChangeKind::Changed, false)
            .unwrap();
    }
    let snapshot = changes.snapshot();
    assert_eq!(
        snapshot.touched_files,
        vec![
            join(&root, &["a.ts"]),
            join(&root, &["b.ts"]),
            join(&root, &["c.ts"]),
        ]
    );
}

#[test]
fn m2_collapse_prunes_nested_prefixes_shortest_first() {
    let root = join(&std::env::temp_dir().to_string_lossy(), &["m2-collapse-repo"]);
    let mut changes = ChangeSet::new(ChangeSetOptions {
        root: Some(root.clone()),
        max_changed_paths: 100,
    });
    changes
        .add(&join(&root, &["src", "nested", "deep.ts"]), ChangeKind::Deleted, false)
        .unwrap();
    changes
        .add(&join(&root, &["src", "nested"]), ChangeKind::Deleted, true)
        .unwrap();
    changes.add(&join(&root, &["src"]), ChangeKind::Deleted, true).unwrap();
    let snapshot = changes.snapshot();
    assert_eq!(snapshot.deleted_prefixes, vec![join(&root, &["src"])]);
}
