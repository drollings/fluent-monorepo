//! No runtime bridge on the async venue path: the async submission
//! protocol must never park on a per-call runtime, or a pooled connection
//! handed back by work the runtime drops strands the next caller with a
//! dead waker.
//!
//! The check is source-textual by necessity: Rust cannot name the runtime
//! a call bridges on at runtime, so the test walks the two crates' `src/`
//! trees (never `tests/`, which may drive doubles directly) and asserts
//! where `block_on` may appear. The sync dry-run bridge stays — sync
//! callers and tests need it — but nothing async may touch it; the
//! terminal seam uses the shared bridge, which outlives the call and runs
//! concurrent callers in parallel.

use std::path::{Path, PathBuf};

/// Production roots the invariant covers: the venue owns the protocol and
/// the routing; the server owns the decision drivers and the terminal seam.
fn roots() -> Vec<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    vec![
        manifest.join("src"),
        manifest.join("../protopyne-server/src"),
    ]
}

/// Every `.rs` file under `root`, in a stable order.
fn sources(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).expect("src tree reads");
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// The `fn` whose body a line number falls in, by indentation: a
/// `    fn name` line opens a body until the next one.
fn enclosing_fn(lines: &[&str], index: usize) -> String {
    let mut current = String::new();
    for line in lines.iter().take(index + 1) {
        let trimmed = line.strip_prefix("    fn ").or_else(|| {
            line.strip_prefix("    pub fn ")
                .or_else(|| line.strip_prefix("    pub async fn "))
                .or_else(|| line.strip_prefix("    async fn "))
        });
        if let Some(rest) = trimmed {
            current = rest.split(['(', '<', ' ']).next().unwrap_or("").to_owned();
        }
    }
    current
}

#[test]
fn async_venue_path_bridges_no_runtime() {
    let mut offenders = Vec::new();
    for root in roots() {
        for file in sources(&root) {
            let name = file
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let text = std::fs::read_to_string(&file).expect("source reads");
            let lines: Vec<&str> = text.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                // The shared bridge outlives the call; only the bare
                // per-call form strands pooled continuations.
                if !code.contains("block_on(") || code.contains("block_on_shared(") {
                    continue;
                }
                let display = || {
                    format!(
                        "{}:{}: {}",
                        file.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                            .unwrap_or(&file)
                            .display(),
                        index + 1,
                        code.trim()
                    )
                };
                // The sync dry-run bridge serves sync callers and tests;
                // the async override answers natively beside it.
                if name == "live.rs" && enclosing_fn(&lines, index) == "dry_run" {
                    continue;
                }
                offenders.push(display());
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "bare runtime bridge outside the sync dry run:\n{}",
        offenders.join("\n")
    );
}
