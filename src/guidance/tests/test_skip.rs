//! Test-path exclusion: unit tests and e2e tests are never indexed and
//! never described, at every funnel (selection, sidecar gen, staleness).

use super::*;
use fluent_wvr_testutil::tempdir;

#[test]
fn gen_refuses_test_paths() {
    let dir = tempdir();
    let source_dir = dir.path().join("src");
    std::fs::create_dir(&source_dir).expect("mkdir");
    let test_file = source_dir.join("tests").join("e2e_thing.zig");
    std::fs::create_dir_all(test_file.parent().expect("parent")).expect("mkdir");
    std::fs::write(&test_file, "pub fn foo() void {}").expect("write");
    let guidance_dir = dir.path().join(".guidance");
    let mut engine = SyncEngine::new(guidance_dir, source_dir);
    let error = engine.gen(&test_file).expect_err("test path refused");
    assert!(error.to_string().contains("test path skipped"), "{error}");
}

#[test]
fn gen_if_stale_treats_test_paths_as_up_to_date() {
    let dir = tempdir();
    let source_dir = dir.path().join("src");
    std::fs::create_dir(&source_dir).expect("mkdir");
    let test_file = source_dir.join("test_helpers.zig");
    std::fs::write(&test_file, "pub fn foo() void {}").expect("write");
    let guidance_dir = dir.path().join(".guidance");
    let mut engine = SyncEngine::new(guidance_dir, source_dir);
    assert!(!engine.gen_if_stale(&test_file).expect("skipped, not stale"));
}

#[test]
fn walk_source_files_skips_test_paths() {
    let dir = tempdir();
    let source_dir = dir.path().join("src");
    std::fs::create_dir_all(source_dir.join("tests")).expect("mkdir");
    std::fs::write(source_dir.join("lib.zig"), "pub fn a() void {}\n").expect("write");
    std::fs::write(source_dir.join("tests/e2e.zig"), "pub fn t() void {}\n").expect("write");
    std::fs::write(source_dir.join("lib_test.zig"), "pub fn u() void {}\n").expect("write");
    let guidance_dir = dir.path().join(".guidance");
    let engine = SyncEngine::new(guidance_dir, source_dir);
    let mut seen: Vec<std::path::PathBuf> = Vec::new();
    engine.walk_source_files(|path| seen.push(path.to_path_buf()));
    assert_eq!(seen.len(), 1, "only lib.zig walks: {seen:?}");
    assert!(seen[0].ends_with("lib.zig"));
}
