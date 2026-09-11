//! Shared hermetic scaffold for the binary e2e targets: built-binary
//! path, temp-workspace seeding, process spawn, stdout decoding, and the
//! index-then-sync flow. One copy — e2e targets compose this instead of
//! redefining it. Wired per target via `#[path = "common.rs"] mod common;`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the built `guidance` binary under test.
#[must_use]
pub fn guidance_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_guidance"))
}

/// Spawn the built binary with `args` in `dir`.
pub fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(guidance_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn guidance")
}

/// Decode a spawned process's stdout.
#[must_use]
pub fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Hermetic temp workspace seeded with one anchored Rust file.
pub fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("lib.rs"),
        "/// Adds one.\npub fn e2e_anchor_fn(x: u64) -> u64 {\n    x + 1\n}\n",
    )
    .expect("write");
    dir
}

/// Index-then-sync flow on `dir`, ingesting into `db_name` (workspace-local
/// file name, e.g. `".sync.db"`); `verbose` adds `--verbose` to the sync
/// invocation. Returns the sync stdout.
pub fn sync(dir: &Path, db_name: &str, verbose: bool) -> String {
    let root = dir.to_str().unwrap().to_string();
    let json_dir = dir.join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let out = run(dir, &["index", &root, "--workspace", &root, "--json-dir", &json_dir]);
    assert!(out.status.success(), "index failed: {out:?}");
    let db = dir.join(db_name);
    let db = db.to_str().unwrap().to_string();
    let mut args = vec![
        "sync".to_string(),
        "--workspace".to_string(),
        root,
        "--json-dir".to_string(),
        json_dir,
        "--db".to_string(),
        db,
    ];
    if verbose {
        args.push("--verbose".to_string());
    }
    let out = run(dir, &args.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(out.status.success(), "sync failed: {out:?}");
    stdout(&out)
}
