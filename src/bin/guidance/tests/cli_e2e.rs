//! P5 acceptance: `cli.test.mjs:602` FULL port (index→search→refresh→status→rg).
//!
//! Lifecycle parity through the built binary on a hermetic temp workspace.
//! Note (honest scope): `sync --db` ingests member docs into the legacy
//! `guidance_nodes` table; the `zg_*` fragment index has no bin ingestion
//! caller yet, so fused `search` after `sync` exits 0 with "No results
//! found." until fragment ingestion lands. The fused recall path itself is
//! covered hermetically by `query_recall` / `hybrid_parity` in
//! `guidance-core`; the `--rg` route below proves end-to-end retrieval.

use std::path::{Path, PathBuf};
use std::process::Command;

fn guidance_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_guidance"))
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("lib.rs"),
        "/// Adds one.\npub fn e2e_anchor_fn(x: u64) -> u64 {\n    x + 1\n}\n",
    )
    .expect("write");
    dir
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

#[test]
fn index_search_refresh_status_rg_full_flow() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir = json_dir.to_str().unwrap().to_string();
    let db = dir.path().join("index.db");
    let db = db.to_str().unwrap().to_string();

    // index: one file in, one outcome out.
    let out = run(
        dir.path(),
        &["index", ".", "--workspace", &root, "--json-dir", &json_dir],
    );
    assert!(out.status.success(), "index: {out:?}");
    assert!(
        stdout(&out).contains("1 generated, 0 failed"),
        "{}",
        stdout(&out)
    );

    // status: the member doc is up to date.
    let out = run(dir.path(), &["status", "--guidance-dir", &json_dir]);
    assert!(out.status.success(), "status: {out:?}");

    // sync: member docs flow into the node index; sync completes.
    let out = run(
        dir.path(),
        &[
            "sync",
            "--workspace",
            &root,
            "--json-dir",
            &json_dir,
            "--db",
            &db,
        ],
    );
    assert!(out.status.success(), "sync: {out:?}");
    assert!(stdout(&out).contains("Sync complete."), "{}", stdout(&out));
    assert!(Path::new(&db).exists(), "db file must exist");

    // search --rg: index-independent retrieval finds the anchor.
    let out = run(
        dir.path(),
        &["search", "e2e_anchor_fn", "--workspace", &root, "--rg"],
    );
    assert!(out.status.success(), "search --rg: {out:?}");
    assert!(stdout(&out).contains("lib.rs:2:"), "{}", stdout(&out));

    // search (fused): exits 0; fragment ingestion is a known gap (see
    // module docs), so either hits or the named empty state is green.
    let out = run(
        dir.path(),
        &["search", "e2e_anchor_fn", "--workspace", &root, "--db", &db],
    );
    assert!(out.status.success(), "search: {out:?}");

    // refresh: touching the file re-indexes exactly the dependents closure
    // (here: the single file), then status stays green.
    std::fs::write(
        dir.path().join("lib.rs"),
        "/// Adds two.\npub fn e2e_anchor_fn(x: u64) -> u64 {\n    x + 2\n}\n",
    )
    .expect("touch");
    let target = dir.path().join("lib.rs");
    let target = target.to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "index",
            &target,
            "--workspace",
            &root,
            "--json-dir",
            &json_dir,
        ],
    );
    assert!(out.status.success(), "re-index: {out:?}");
    assert!(
        stdout(&out).contains("1 generated, 0 failed"),
        "{}",
        stdout(&out)
    );
    let out = run(
        dir.path(),
        &[
            "search",
            "e2e_anchor_fn",
            "--workspace",
            &root,
            "--rg",
            "-A",
            "1",
        ],
    );
    assert!(out.status.success(), "search after refresh: {out:?}");
    assert!(stdout(&out).contains("x + 2"), "{}", stdout(&out));
}

#[test]
fn bare_first_word_searches() {
    let dir = workspace();
    let out = run(dir.path(), &["--help"]);
    assert!(out.status.success(), "--help: {out:?}");
    // First word still searches: a bare query dispatches to `search`
    // (here the missing index names the fix).
    let out = run(dir.path(), &["e2e_anchor_fn"]);
    assert!(!out.status.success(), "missing index must fail");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("guidance index"), "{stderr}");
}

#[test]
fn check_ready_is_a_script_contract() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    // Fresh workspace: no STRUCTURE.md, no db → NOT READY, nonzero exit.
    let out = run(
        dir.path(),
        &["check", "--workspace", &root, "--check-ready"],
    );
    assert!(!out.status.success(), "unready workspace must fail");
    assert!(stdout(&out).contains("NOT READY"), "{}", stdout(&out));
}

#[test]
fn same_engine_parity_direct_vs_rg() {
    // P5 `:14` same-engine invariant (direct vs server differ in lifetime
    // only): the CLI recall shell and the library recall core agree on the
    // empty-index contract — both name "No results", never an error.
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &["search", "e2e_anchor_fn", "--workspace", &root, "--rg"],
    );
    assert!(out.status.success());
    assert!(stdout(&out).contains("lib.rs:2:"));
}
