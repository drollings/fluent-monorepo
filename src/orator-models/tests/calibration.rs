//! Model-verification calibration: the four-group fixture behind the
//! pinned gate. "Pinned" is producer confidence (a catalog flag);
//! "hash matches" is task-value (bytes on disk). The report must
//! never call an unpinned or mismatched file verified.

use crate::model::{ModelEntry, ModelKind};
use crate::ops::verify_entry;
use crate::ops::ENV_LOCK;

// Process env is shared across test threads: HOME is redirected
// below, so these serialize on the ops-owned lock.

/// Redirect HOME at a temp dir (restored by the caller). Managed
/// paths resolve under it without touching the real installation.
fn redirect_home() -> (tempfile::TempDir, Option<std::ffi::OsString>) {
    let prior = std::env::var_os("HOME");
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", home.path());
    (home, prior)
}

fn restore_home(prior: Option<std::ffi::OsString>) {
    match prior {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
}

fn entry(name: &str, file: &str, bytes: Option<&[u8]>) -> ModelEntry {
    let (size, sha256) = bytes.map_or((None, None), |content| {
        (
            Some(content.len() as u64),
            Some(common_core::hash::sha256_hex(content)),
        )
    });
    ModelEntry {
        name: name.to_string(),
        kind: ModelKind::Whisper,
        repo: "local/calibration".to_string(),
        file: file.to_string(),
        size,
        sha256,
    }
}

fn write_managed(file: &str, bytes: &[u8]) {
    let dir = orator_core::models_dir().expect("fixture home resolves");
    std::fs::create_dir_all(&dir).expect("managed dir");
    std::fs::write(dir.join(file), bytes).expect("fixture writes");
}

#[test]
fn pinned_matching_verifies_ok() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let (_home, prior) = redirect_home();
    write_managed("cal-ok.bin", b"exact bytes");
    let entry = entry("pinned-ok", "cal-ok.bin", Some(b"exact bytes"));
    assert!(
        verify_entry(&entry).expect("verification runs"),
        "pinned + matching hash is ok"
    );
    restore_home(prior);
}

#[test]
fn pinned_mismatch_verifies_mismatch() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let (_home, prior) = redirect_home();
    write_managed("cal-bad.bin", b"wrong bytes");
    let entry = entry("pinned-bad", "cal-bad.bin", Some(b"exact bytes"));
    assert!(
        !verify_entry(&entry).expect("verification runs"),
        "same size, different bytes is MISMATCH"
    );
    restore_home(prior);
}

#[test]
fn unpinned_entries_never_verify_ok() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let (_home, prior) = redirect_home();
    write_managed("cal-unpinned.bin", b"whatever");
    for (name, file) in [
        ("unpinned-here", "cal-unpinned.bin"),
        ("unpinned-gone", "cal-missing.bin"),
    ] {
        let entry = entry(name, file, None);
        let error = verify_entry(&entry).expect_err("unpinned never verifies");
        assert!(
            error.to_string().contains("unpinned"),
            "names the reason: {error}"
        );
    }
    restore_home(prior);
}

#[test]
fn false_ok_and_false_fail_rates_are_zero() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let (_home, prior) = redirect_home();
    write_managed("cal-ok.bin", b"exact bytes");
    write_managed("cal-bad.bin", b"wrong bytes");
    write_managed("cal-unpinned.bin", b"whatever");
    // (entry, must_verify_ok): the four calibration groups.
    let cases = [
        (entry("pinned-ok", "cal-ok.bin", Some(b"exact bytes")), true),
        (
            entry("pinned-bad", "cal-bad.bin", Some(b"exact bytes")),
            false,
        ),
        (entry("unpinned-here", "cal-unpinned.bin", None), false),
        (entry("unpinned-gone", "cal-missing.bin", None), false),
    ];
    let mut false_ok = 0;
    let mut false_fail = 0;
    for (entry, must_verify_ok) in &cases {
        let ok = matches!(verify_entry(entry), Ok(true));
        if ok && !must_verify_ok {
            false_ok += 1;
        }
        if !ok && *must_verify_ok {
            false_fail += 1;
        }
    }
    assert_eq!((false_ok, false_fail), (0, 0));
    restore_home(prior);
}
