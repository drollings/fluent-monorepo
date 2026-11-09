use super::SyncEngine;
use fluent_wvr_testutil::tempdir;

fn write_source(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let source_dir = dir.join("src");
    std::fs::create_dir_all(&source_dir).expect("mkdir");
    let file = source_dir.join(name);
    std::fs::write(&file, body).expect("write");
    file
}

#[test]
fn second_pass_over_clean_file_is_skipped() {
    // Stale-only cycles: a clean file is not re-processed.
    let dir = tempdir();
    let file = write_source(dir.path(), "a.zig", "pub fn a() void {}\n");
    let guidance_dir = dir.path().join(".guidance");
    let mut engine = SyncEngine::new(guidance_dir.clone(), dir.path().join("src"));
    assert!(engine.gen_if_stale(&file).expect("first gen"));
    assert!(
        !engine.gen_if_stale(&file).expect("second pass"),
        "clean file must be skipped"
    );
}

#[test]
fn skipped_pass_leaves_sidecar_mtime_untouched() {
    // Skipped means skipped: no byte or mtime disturbance downstream.
    let dir = tempdir();
    let file = write_source(dir.path(), "b.zig", "pub fn b() void {}\n");
    let guidance_dir = dir.path().join(".guidance");
    let mut engine = SyncEngine::new(guidance_dir.clone(), dir.path().join("src"));
    assert!(engine.gen_if_stale(&file).expect("first gen"));
    let json = guidance_dir.join("src").join("b.zig.json");
    let mtime = std::fs::metadata(&json)
        .expect("stat")
        .modified()
        .expect("mtime");
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(!engine.gen_if_stale(&file).expect("second pass"));
    assert_eq!(
        std::fs::metadata(&json)
            .expect("stat")
            .modified()
            .expect("mtime"),
        mtime,
        "skipped pass must not touch the sidecar"
    );
}
