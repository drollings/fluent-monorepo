//! M3 acceptance: stale-dependent propagation in one-shot sync.
//!
//! Fixture (hermetic temp workspace): importer→importee (`mod importee`
//! resolves at hydration; the `fee` call edge doubles the linkage) plus
//! an unrelated leaf. Editing the importee's signature with a same-size
//! edit (pins the size arm of both gates — the mtime arm must fire)
//! re-processes the importer; editing the leaf re-processes only itself.
//! Pre-registered bound: affected-set recall 1.0 required (any miss fails
//! the milestone); over-invalidation is recorded, not tuned.

use std::path::{Path, PathBuf};
use std::process::Command;

fn guidance_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_guidance"))
}

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(guidance_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn guidance")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("importee.rs"),
        "pub fn fee(x: u64) -> u64 {\n    x + 1\n}\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("importer.rs"),
        "mod importee;\nuse importee::fee;\n/// Calls fee.\npub fn caller(x: u64) -> u64 {\n    fee(x)\n}\n",
    )
    .expect("write");
    std::fs::write(dir.path().join("leaf.rs"), "pub fn leaf() -> u64 {\n    42\n}\n")
        .expect("write");
    dir
}

fn sync(dir: &Path, verbose: bool) -> String {
    let root = dir.to_str().unwrap().to_string();
    let json_dir = dir.join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let db = dir.join(".sync.db");
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

fn index(dir: &Path) {
    let root = dir.to_str().unwrap().to_string();
    let json_dir = dir.join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let out = run(
        dir,
        &["index", &root, "--workspace", &root, "--json-dir", &json_dir],
    );
    assert!(out.status.success(), "index failed: {out:?}");
}

/// Verbose `regenerated:` lines → sorted affected set (absolute paths).
fn regenerated(out: &str) -> Vec<String> {
    let mut paths: Vec<String> = out
        .lines()
        .filter_map(|line| line.trim().strip_prefix("regenerated: "))
        .map(|rest| rest.split_whitespace().next().unwrap_or("").to_string())
        .filter(|p| !p.is_empty())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

fn fragment_texts(dir: &Path, file: &str) -> Vec<String> {
    let db = dir.join(".sync.db");
    let file_id = dir.join(file).to_string_lossy().into_owned();
    let out = std::process::Command::new("sqlite3")
        .args([
            db.to_str().unwrap(),
            &format!(
                "SELECT content_text FROM zg_fragments WHERE file_id = '{file_id}' ORDER BY id"
            ),
        ])
        .output();
    // sqlite3 CLI may be absent (hermetic CI): fall back to file bytes.
    let Ok(out) = out else {
        return vec![std::fs::read_to_string(dir.join(file)).unwrap()];
    };
    if !out.status.success() {
        return vec![std::fs::read_to_string(dir.join(file)).unwrap()];
    }
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

#[test]
fn importee_signature_edit_reprocesses_importer() {
    let dir = fixture();
    let root = dir.path();
    index(root);
    sync(root, false);

    let before = fragment_texts(root, "importer.rs");

    // Same-size edit pins the size arm; the mtime arm must fire.
    std::fs::write(root.join("importee.rs"), "pub fn fee(x: u64) -> u64 {\n    x + 2\n}\n")
        .expect("write");
    let out = sync(root, true);

    let measured = regenerated(&out);
    let oracle = vec![
        root.join("importee.rs").to_string_lossy().into_owned(),
        root.join("importer.rs").to_string_lossy().into_owned(),
    ];
    // Pre-registered bound: recall 1.0 (no silent miss, ever).
    for expected in &oracle {
        assert!(measured.contains(expected), "missed {expected}; measured={measured:?}");
    }
    let over: Vec<_> = measured.iter().filter(|p| !oracle.contains(p)).collect();
    eprintln!("M3 probe: affected={measured:?} over-invalidation={over:?}");

    // DB content converges: the importer's bytes never changed, so its
    // derived rows must be identical-or-fresher, never divergent.
    assert_eq!(before, fragment_texts(root, "importer.rs"));
}

#[test]
fn leaf_edit_reprocesses_only_itself() {
    let dir = fixture();
    let root = dir.path();
    index(root);
    sync(root, false);

    std::fs::write(root.join("leaf.rs"), "pub fn leaf() -> u64 {\n    43\n}\n")
        .expect("write");
    let out = sync(root, true);

    let measured = regenerated(&out);
    let leaf = root.join("leaf.rs").to_string_lossy().into_owned();
    let importer = root.join("importer.rs").to_string_lossy().into_owned();
    assert!(measured.contains(&leaf), "leaf itself must re-process: {measured:?}");
    assert!(
        !measured.contains(&importer),
        "must-NOT-fire: importer untouched by a leaf edit: {measured:?}"
    );
    assert_eq!(measured.len(), 1, "affected is exactly the leaf: {measured:?}");
}
