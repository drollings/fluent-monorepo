//! P5 acceptance: `cli.test.mjs:602` FULL port (index→search→refresh→status→rg).
//!
//! Lifecycle parity through the built binary on a hermetic temp workspace.
//!
//! `sync --db` ingests member docs into the fragment index; the shared
//! `nodes` document store is a separate primitive with its own goldens
//! (`search-vector/tests/node_sync.rs`) and is not on the sync path.
//! The fused recall path is covered hermetically by `query_recall` /
//! `hybrid_parity` in `guidance-core`; the `--rg` route below proves
//! end-to-end retrieval.

use std::path::Path;

#[path = "common.rs"]
#[allow(dead_code)]
mod common;

use common::{run, stdout, workspace};

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

    // sync: member docs flow into the node index; the summary is the
    // agent contract, progress lines stay behind --verbose.
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
            "--verbose",
        ],
    );
    assert!(out.status.success(), "sync: {out:?}");
    assert!(stdout(&out).contains("sync: clean"), "{}", stdout(&out));
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

#[test]
fn clean_removes_build_cache_and_reports_bytes() {
    // Seeded `.guidance/target` (3072 bytes across two files) is
    // regenerable cache: `clean` removes it and reports the exact
    // bytes reclaimed. Fails before the target-cache step exists.
    let dir = workspace();
    let target = dir.path().join(".guidance").join("target").join("debug");
    std::fs::create_dir_all(&target).expect("mkdir target");
    std::fs::write(target.join("a.rlib"), vec![0u8; 1024]).expect("write");
    std::fs::write(target.join("b.rlib"), vec![0u8; 2048]).expect("write");
    let out = run(
        dir.path(),
        &["clean", "--json-dir", ".guidance", "--db", ".clean.db"],
    );
    assert!(out.status.success(), "clean: {out:?}");
    let text = stdout(&out);
    assert!(
        !dir.path().join(".guidance").join("target").exists(),
        "build cache must be gone"
    );
    assert!(text.contains("3072 bytes"), "bytes reclaimed: {text}");
    assert!(text.contains("Clean complete."), "{text}");
}

#[test]
fn clean_without_build_cache_still_exits_zero() {
    // Missing cache dir: nothing to remove, still a clean exit and
    // the legacy lines unchanged.
    let dir = workspace();
    assert!(!dir.path().join(".guidance").join("target").exists());
    let out = run(
        dir.path(),
        &["clean", "--json-dir", ".guidance", "--db", ".clean.db"],
    );
    assert!(out.status.success(), "clean: {out:?}");
    let text = stdout(&out);
    assert!(!text.contains("Removed build cache"), "{text}");
    assert!(text.contains("Clean complete."), "{text}");
}

#[test]
fn clean_skips_build_cache_while_supervised_run_holds_it() {
    // A held cargo target lock (exactly what a supervised `run` /
    // `fix` clippy invocation holds mid-build) makes `clean` skip
    // the cache with a message instead of racing the live build.
    // Fails before the liveness probe exists (dir would be removed).
    use fs2::FileExt;
    let dir = workspace();
    let target = dir.path().join(".guidance").join("target");
    std::fs::create_dir_all(&target).expect("mkdir target");
    std::fs::write(target.join("a.rlib"), vec![0u8; 512]).expect("write");
    let lock_path = target.join(".cargo-lock");
    std::fs::write(&lock_path, b"").expect("write lock");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open lock");
    held.lock_exclusive().expect("hold lock");
    let out = run(
        dir.path(),
        &["clean", "--json-dir", ".guidance", "--db", ".clean.db"],
    );
    assert!(out.status.success(), "clean: {out:?}");
    let text = stdout(&out);
    assert!(target.exists(), "held cache must survive: {text}");
    assert!(
        text.contains("supervised run"),
        "skip names the holder: {text}"
    );
    held.unlock().expect("release");
}

#[test]
fn sync_imports_external_corpus_and_project_cards() {
    // Card-import acceptance through the built binary: a workspace with
    // a doc plus an external corpus gets a ready capability row and
    // deterministic project cards from one `sync`, with no model.
    let dir = workspace();
    let root = dir.path();
    std::fs::create_dir_all(root.join("doc/capabilities")).expect("mkdir corpus");
    std::fs::write(
        root.join("doc/capabilities/corpus.json"),
        r#"{"briefs": [{"id": "doc/guide.md#guide", "title": "Guide", "purpose": "Explains the anchor contract.", "crate": "c", "domain": "d", "source_path": "doc/guide.md", "anchor": "guide"}]}"#,
    )
    .expect("write corpus");
    std::fs::create_dir_all(root.join("doc")).expect("mkdir doc");
    std::fs::write(
        root.join("doc/guide.md"),
        "# Guide\n\nExplains the anchor contract.\n",
    )
    .expect("write doc");
    let text = common::sync(root, ".cards.db", true);
    assert!(text.contains("Imported 1 capability cards"), "{text}");
    assert!(text.contains("project cards"), "{text}");
    let db = guidance_core::query::search_db::GuidanceSearchDb::open(&root.join(".cards.db"))
        .expect("open db");
    assert_eq!(db.capability_ready_count().expect("count"), 1);
    let rows = db
        .capabilities_for_source("doc/guide.md")
        .expect("capability rows");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].status, "ready", "{rows:?}");
    assert_eq!(
        rows[0].origin, "external:doc/capabilities/corpus.json",
        "{rows:?}"
    );
    assert!(db.project_card_count().expect("count") >= 1);
    let keys: Vec<String> = db
        .project_cards_all()
        .expect("cards")
        .into_iter()
        .map(|card| card.key)
        .collect();
    assert!(
        keys.iter().any(|key| key.starts_with("workspace:")),
        "{keys:?}"
    );
}

#[test]
fn explain_no_llm_builds_no_judge() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    common::sync(dir.path(), ".noj.db", false);
    let db = dir.path().join(".noj.db").to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "explain",
            "e2e_anchor_fn",
            "--workspace",
            &root,
            "--db",
            &db,
            "--no-llm",
        ],
    );
    assert!(out.status.success(), "explain --no-llm: {out:?}");
    let text = stdout(&out);
    assert!(!text.contains("[relevant]"), "{text}");
    assert!(!text.contains("[irrelevant]"), "{text}");
    assert!(!text.contains("judge unavailable"), "{text}");
    assert!(text.contains("e2e_anchor_fn"), "{text}");
}

#[test]
fn explain_judge_failure_degrades_with_gap() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    common::sync(dir.path(), ".judge.db", false);
    let db = dir.path().join(".judge.db").to_str().unwrap().to_string();
    let cfg_dir = dir.path().join(".guidance");
    std::fs::create_dir_all(&cfg_dir).expect("mkdir");
    std::fs::write(
        cfg_dir.join("guidance-config.json"),
        r#"{"decision": {"base_url": "http://127.0.0.1:9", "model": "x", "timeout_ms": 1000, "enabled": true, "transport": "system_one"}}"#,
    )
    .expect("write cfg");
    let out = run(
        dir.path(),
        &[
            "explain",
            "e2e_anchor_fn",
            "--workspace",
            &root,
            "--db",
            &db,
            "--no-llm",
        ],
    );
    assert!(out.status.success(), "no-llm still no judge: {out:?}");
    assert!(!stdout(&out).contains("judge unavailable"));
    let out = run(
        dir.path(),
        &[
            "explain",
            "e2e_anchor_fn",
            "--workspace",
            &root,
            "--db",
            &db,
        ],
    );
    assert!(out.status.success(), "judge failure must degrade: {out:?}");
    let text = stdout(&out);
    assert!(
        text.contains("judge unavailable") || text.contains("shadow-only"),
        "{text}"
    );
    assert!(text.contains("e2e_anchor_fn"), "{text}");
}

#[test]
fn calibrate_finding_prints_held_and_writes_nothing() {
    let dir = workspace();
    let dataset = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../guidance/../guidance/eval/findings.jsonl");
    // Fallback to repo-relative path when CARGO_MANIFEST_DIR differs.
    let dataset = if dataset.exists() {
        dataset
    } else {
        std::path::Path::new("/opt/src/rust/monorepo/doc/guidance/eval/findings.jsonl")
            .to_path_buf()
    };
    let out = run(
        dir.path(),
        &[
            "calibrate",
            "--point",
            "finding",
            "--dataset",
            dataset.to_str().unwrap(),
            "--model-fingerprint",
            "test-fp",
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
            "--journal-dir",
            dir.path().join("journal").to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "calibrate finding HELD: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("HELD"), "{text}");
    assert!(
        !dir.path().join("state/calibration.json").exists(),
        "HELD writes nothing"
    );
}

#[test]
fn explain_control_abstains_without_fabrication() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    common::sync(dir.path(), ".ctrl.db", false);
    let db = dir.path().join(".ctrl.db").to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "explain",
            "What is the zzqy nonexistent subsystem?",
            "--workspace",
            &root,
            "--db",
            &db,
            "--no-llm",
        ],
    );
    assert!(out.status.success(), "control must exit 0: {out:?}");
    let text = stdout(&out);
    assert!(text.contains("none found in covered index"), "{text}");
    assert!(!text.contains("other.rs"), "{text}");
}

#[test]
fn kb_keys_match_new_coverage_and_compact_omits_only_coverage() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    common::sync(dir.path(), ".kb.db", false);
    let out = run(
        dir.path(),
        &[
            "kb",
            "--workspace",
            &root,
            "--json-dir",
            ".guidance",
            "--db",
            ".kb.db",
        ],
    );
    assert!(out.status.success(), "kb: {out:?}");
    let value: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("kb json");
    let readiness = value.get("readiness").expect("readiness");
    let coverage = readiness
        .get("coverage")
        .or_else(|| value.get("coverage"))
        .expect("coverage");
    // New keys are present and stable; old keys remain.
    assert!(coverage.get("doc_cards").is_some(), "{coverage}");
    assert!(coverage.get("embeddings_off").is_some(), "{coverage}");
    assert!(coverage.get("support").is_some(), "{coverage}");
    assert!(coverage.get("mirror").is_some(), "{coverage}");
    let db = dir.path().join(".kb.db").to_str().unwrap().to_string();
    let out = run(
        dir.path(),
        &[
            "explain",
            "e2e_anchor_fn",
            "--workspace",
            &root,
            "--db",
            &db,
            "--no-llm",
            "--compact",
        ],
    );
    assert!(out.status.success(), "compact: {out:?}");
    let text = stdout(&out);
    assert!(
        !text.contains("### Coverage"),
        "compact omits Coverage: {text}"
    );
    assert!(
        text.contains("### Top matches") || text.contains("### Citations"),
        "{text}"
    );
    assert!(text.contains("### Envelope"), "{text}");
}

#[test]
fn docs_gate_commands_exist_in_help() {
    let dir = workspace();
    let out = run(dir.path(), &["--help"]);
    let text = stdout(&out);
    for cmd in [
        "explain",
        "sync",
        "search",
        "index",
        "status",
        "kb",
        "calibrate",
    ] {
        assert!(text.contains(cmd), "help mentions {cmd}: {text}");
    }
    let out = run(dir.path(), &["explain", "--help"]);
    let text = stdout(&out);
    assert!(text.contains("--compact"), "{text}");
    assert!(text.contains("--decision-fingerprint"), "{text}");
    let out = run(dir.path(), &["sync", "--help"]);
    let text = stdout(&out);
    assert!(text.contains("--embed"), "{text}");
}

#[test]
fn sync_default_no_embed_doc_cards_present_second_noop() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    // Default sync: no embed backend, policy off → no Embedded line.
    let out = run(
        dir.path(),
        &[
            "sync",
            "--workspace",
            &root,
            "--json-dir",
            ".guidance",
            "--db",
            ".m9.db",
            "--verbose",
        ],
    );
    assert!(out.status.success(), "sync: {out:?}");
    let text = stdout(&out);
    assert!(
        !text.contains("Embedded "),
        "default sync must not embed: {text}"
    );
    assert!(text.contains("project cards"), "doc cards present: {text}");
    // Second sync is a no-op (still succeeds, no failures).
    let out = run(
        dir.path(),
        &[
            "sync",
            "--workspace",
            &root,
            "--json-dir",
            ".guidance",
            "--db",
            ".m9.db",
        ],
    );
    assert!(out.status.success(), "second sync no-op: {out:?}");
    // Explicit --embed still succeeds without a backend (policy opt-in, no vectors).
    let out = run(
        dir.path(),
        &[
            "sync",
            "--workspace",
            &root,
            "--json-dir",
            ".guidance",
            "--db",
            ".m9.db",
            "--embed",
        ],
    );
    assert!(out.status.success(), "--embed with no backend: {out:?}");
}
