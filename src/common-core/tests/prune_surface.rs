//! Deleted-surface persistence: pruned shims and scaffolds stay deleted.
//!
//! Each entry names a removed file or item; any regrowth fails this test.
//! Name checks are scoped to the defining crate's tree so unrelated
//! same-named items elsewhere (e.g. protopyne's own command unit) do
//! not trip the guard.

use std::path::{Path, PathBuf};

/// Removed files: regrowing the file fails the test.
const DELETED_FILES: &[&str] = &[
    "src/common-core/src/git.rs",
    "src/dag/src/middleware.rs",
    "src/dag/src/work_unit.rs",
    "src/dag/src/target_work_unit.rs",
    "src/db/src/wvr.rs",
];

/// (defining crate tree, removed item name): a reintroduced name fails.
const DELETED_ITEMS: &[(&str, &str)] = &[
    ("src/common-core/src", "common_core::git"),
    ("src/dag/src", "TimingMiddleware"),
    ("src/dag/src", "CommandUnit"),
    ("src/dag/src", "TargetWorkUnit"),
    ("src/db/src", "DbWorkUnit"),
    ("src/db/src", "store_unit"),
    ("src/llm/src", "prefix_cascade"),
    ("src/types/src", "SyncResult"),
    ("src/types/src", "FileMatch"),
    ("src/types/src", "ASTAnalysis"),
    ("src/types/src", "GuidanceInfo"),
    ("src/types/src", "QueryResult"),
];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("read_dir");
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn check_tree(root: &Path, names: &[&str]) -> Vec<String> {
    let mut files = Vec::new();
    rs_files(root, &mut files);
    let mut hits = Vec::new();
    for f in files {
        let Ok(content) = std::fs::read_to_string(&f) else {
            continue;
        };
        for (i, line) in content.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            for name in names {
                if code.contains(name) {
                    hits.push(format!("{}:{}: revived `{name}`", f.display(), i + 1));
                }
            }
        }
    }
    hits
}

#[test]
fn deleted_names_stay_deleted() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    for rel in DELETED_FILES {
        assert!(
            !root.join(rel).exists(),
            "{rel} regrew — deleted surface must stay deleted"
        );
    }
    let mut hits = Vec::new();
    let mut by_dir: Vec<(&str, Vec<&str>)> = Vec::new();
    for (dir, name) in DELETED_ITEMS {
        match by_dir.iter_mut().find(|(d, _)| *d == *dir) {
            Some((_, names)) => names.push(name),
            None => by_dir.push((dir, vec![name])),
        }
    }
    for (dir, names) in by_dir {
        hits.extend(check_tree(&root.join(dir), &names));
    }
    assert!(
        hits.is_empty(),
        "deleted surface regrew:\n{}",
        hits.join("\n")
    );
}
