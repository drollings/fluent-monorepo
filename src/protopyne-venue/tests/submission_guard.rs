//! No token-free order path: every production call into a venue's `submit`
//! runs through the safe protocol, and the deleted grant-free session entry
//! stays deleted.
//!
//! The check is source-textual by necessity: Rust cannot introspect call
//! sites at runtime, so the test walks the two crates' `src/` trees (never
//! `tests/`, which may drive doubles directly) and asserts both halves of
//! the invariant. A second submission entry reintroduced anywhere in
//! production code fails this test before it can bypass the risk token or
//! the grant ledger.

use std::path::{Path, PathBuf};

/// Production roots the invariant covers: the venue owns the protocol and
/// the routing; the server owns the decision drivers.
fn roots() -> Vec<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    vec![
        manifest.join("src"),
        manifest.join("../protopyne-server/src"),
    ]
}

/// Every `.rs` file under `root`, excluding nothing: `src/` holds no test
/// doubles by convention (unit tests live in `tests/` behind `#[path]`
/// forwarders, never inline).
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

#[test]
fn no_grant_free_submission_entry_returns() {
    let mut offenders = Vec::new();
    for root in roots() {
        for file in sources(&root) {
            let text = std::fs::read_to_string(&file).expect("source reads");
            for marker in ["submit_order", "SubmitJob"] {
                if text.contains(marker) {
                    offenders.push(format!(
                        "{} reintroduces {marker}",
                        file.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                            .unwrap_or(&file)
                            .display()
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "grant-free session entry must stay deleted:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn raw_venue_submit_reaches_only_through_protocol_or_routing() {
    // Venue `submit` takes the intent first; pool submits take jobs, so
    // requiring `intent` on the call line separates the order path from the
    // backtest sweep fan-out without an allowlist of every pool call site.
    // The protocol journals before the first venue call; the registry and
    // the versioned backtest venue only select which handle a protocol call
    // reaches. All three are the one order path, not a second one.
    let allowed = ["submit.rs", "registry.rs", "backtest.rs"];
    let mut offenders = Vec::new();
    for root in roots() {
        for file in sources(&root) {
            let name = file
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if allowed.contains(&name) {
                continue;
            }
            let text = std::fs::read_to_string(&file).expect("source reads");
            for (index, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                if code.contains(".submit(") && code.contains("intent") {
                    offenders.push(format!(
                        "{}:{}: {}",
                        file.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                            .unwrap_or(&file)
                            .display(),
                        index + 1,
                        code.trim()
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "raw venue submit outside the protocol or routing:\n{}",
        offenders.join("\n")
    );
}
