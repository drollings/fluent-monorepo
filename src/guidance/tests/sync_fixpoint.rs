use super::absolute_or;
use fluent_wvr_testutil::tempdir;

// A Python `#` comment is attached by the parser but missed by the
// `///`-only skip, so the comment step backfills `///` into source after
// the sidecar save. The stored-hash entry must follow the post-comment
// bytes, or the immediate second pass restales on bytes gen already
// processed — and every later sync with it.
#[test]
fn comment_write_refreshes_stored_hash_no_second_pass() {
    let dir = tempdir();
    let source_dir = dir.path().join("src");
    std::fs::create_dir(&source_dir).expect("mkdir");
    let file = source_dir.join("fix.py");
    let v1 = "# Human note.\ndef fix():\n    return 1\n";
    std::fs::write(&file, v1).expect("write");
    let guidance_dir = dir.path().join(".guidance");

    // Stored evidence as last ingest recorded it: the pre-comment bytes.
    let absolute = absolute_or(file.clone()).to_string_lossy().into_owned();
    let hashes =
        std::collections::HashMap::from([(absolute, common_core::hash::sha256_hex(v1.as_bytes()))]);
    let mut gated =
        super::SyncEngine::new(guidance_dir.clone(), source_dir.clone()).with_source_hashes(hashes);

    assert!(
        gated.gen_if_stale(&file).expect("gate"),
        "first gen must run"
    );
    let after = std::fs::read_to_string(&file).expect("read");
    assert!(
        after.contains("/// Human note."),
        "comment write must fire or the test is vacuous"
    );

    let json_path = guidance_dir.join("src").join("fix.py.json");
    let sidecar_mtime = std::fs::metadata(&json_path)
        .expect("stat")
        .modified()
        .expect("mtime");
    assert!(
        !gated.gen_if_stale(&file).expect("gate"),
        "second pass over comment-written bytes must skip"
    );
    assert_eq!(
        std::fs::metadata(&json_path)
            .expect("stat")
            .modified()
            .expect("mtime"),
        sidecar_mtime,
        "skipped pass leaves sidecar mtime untouched"
    );
}
