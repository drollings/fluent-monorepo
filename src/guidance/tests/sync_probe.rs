//! Incremental sync pins: the stat sweep spends no hash on fresh
//! files, one hash revalidates a touched-identical file, an edited
//! file regenerates, two syncs converge, and L0 writes no source.
//! Included via `#[path]` forwarder from `sync_engine.rs`, per repo
//! convention. Hermetic: tempdirs, no model, no network.

use super::{CountingProbe, SyncEngine};
use fluent_wvr_testutil::tempdir;

fn fixture(names: &[&str]) -> (tempfile::TempDir, SyncEngine, Vec<std::path::PathBuf>) {
    let dir = tempdir();
    let source_dir = dir.path().join("src");
    std::fs::create_dir(&source_dir).expect("mkdir");
    let guidance_dir = dir.path().join(".guidance");
    let mut files = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let file = source_dir.join(name);
        std::fs::write(
            &file,
            format!("pub fn member{index:02}() -> u64 {{ {index} }}\n"),
        )
        .expect("write");
        files.push(file);
    }
    let engine = SyncEngine::new(guidance_dir, source_dir);
    (dir, engine, files)
}

fn settle() {
    // Idle past the ambiguity window so sidecars are decisively
    // newer: the no-op sweep below must prove zero hashes.
    std::thread::sleep(std::time::Duration::from_millis(1200));
}

#[test]
fn stat_first_probe_counts_hashes() {
    // One sweep per phase on an idle tree: no-op spends zero hashes,
    // a touch revalidates with exactly one, an edit spends one and
    // restales exactly its file.
    let (_dir, engine, files) = fixture(&["a.rs", "b.rs", "c.rs"]);
    let mut engine = engine;
    // Idle past the ambiguity window before the first stamp: a
    // converged tree then proves freshness from timestamps alone.
    settle();
    for file in &files {
        assert!(engine.gen_if_stale(file).expect("gate"), "first gen runs");
    }
    let probe = CountingProbe::default();
    let status = engine.status_with(&probe).expect("status");
    assert_eq!(status.stale_files, 0, "converged tree is clean");
    assert_eq!(probe.calls(), 0, "no hash on a no-op sweep");
    assert_eq!(status.fresh_files, files.len());
    // Same bytes, new mtime: one hash revalidates, nothing restales.
    let bytes = std::fs::read(&files[0]).expect("read");
    std::fs::write(&files[0], &bytes).expect("touch");
    let probe = CountingProbe::default();
    let status = engine.status_with(&probe).expect("status");
    assert_eq!(status.stale_files, 0, "identical bytes never reprocess");
    assert_eq!(probe.calls(), 1, "exactly one hash decides");
    assert_eq!(status.revalidated_files, 1);
    // Changed bytes: the touched file revalidates again (its stamp
    // is still within the window) while the edited file restales.
    std::fs::write(&files[1], "pub fn member01() -> u64 { 999 }\n").expect("edit");
    let probe = CountingProbe::default();
    let status = engine.status_with(&probe).expect("status");
    assert_eq!(status.stale_files, 1, "edited file restales");
    assert_eq!(probe.calls(), 2, "touched plus edited each spend one hash");
    assert!(!engine.gen_if_stale(&files[0]).expect("gate"));
    assert!(
        engine.gen_if_stale(&files[1]).expect("gate"),
        "edit regenerates"
    );
    assert!(!engine.gen_if_stale(&files[2]).expect("gate"));
    let clean = engine.status().expect("status");
    assert_eq!(clean.stale_files, 0, "regen reconverges");
}

#[test]
fn sync_twice_converges_to_zero_stale() {
    // Fix-before-index-and-stamp order converges: a second pass over
    // a synced tree finds nothing stale and regenerates nothing.
    let (_dir, engine, files) = fixture(&["a.rs", "b.rs", "c.rs"]);
    let mut engine = engine;
    for file in &files {
        assert!(
            engine.gen_if_stale(file).expect("gate"),
            "first pass generates"
        );
    }
    let status = engine.status().expect("status");
    assert_eq!(status.stale_files, 0, "synced tree reports zero stale");
    assert_eq!(status.total_files, files.len());
    for file in &files {
        assert!(
            !engine.gen_if_stale(file).expect("gate"),
            "second pass is a no-op: {file:?}"
        );
    }
    let again = engine.status().expect("status");
    assert_eq!(again.stale_files, 0);
    assert!(engine.stale_files().is_empty());
}

#[test]
fn l0_writes_no_source() {
    // Index-only work never rewrites sources: even a badly formatted
    // file keeps byte-identical bytes through generation.
    let (_dir, engine, files) = fixture(&["messy.rs"]);
    let mut engine = engine;
    std::fs::write(&files[0], "pub fn   messy  (x: u32) -> u32{x+1}\n").expect("write messy");
    let before = std::fs::read(&files[0]).expect("read");
    assert!(engine.gen_if_stale(&files[0]).expect("gate"));
    let after = std::fs::read(&files[0]).expect("read");
    assert_eq!(before, after, "L0 generation writes no source bytes");
}

#[test]
fn probe_counts_hashes_without_changing_verdicts() {
    // The counting probe observes without deciding: same verdicts as
    // the production probe on one mixed tree.
    let (_dir, engine, files) = fixture(&["a.rs", "b.rs"]);
    let mut engine = engine;
    for file in &files {
        assert!(engine.gen_if_stale(file).expect("gate"));
    }
    std::fs::write(&files[0], "pub fn member00() -> u64 { 100 }\n").expect("edit");
    let counting = CountingProbe::default();
    let counted = engine.status_with(&counting).expect("counted");
    let plain = engine.status().expect("plain");
    assert_eq!(counted.stale_files, plain.stale_files);
    assert_eq!(counted.total_files, plain.total_files);
    assert!(counting.calls() >= 1, "mixed tree spends at least one hash");
}
