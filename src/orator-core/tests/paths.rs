//! `paths` resolution-helper tests: explicit-override classification,
//! gate filtering, and single-resolve precedence — no server, no weights.

use std::path::PathBuf;

use crate::paths::{
    current_uid, filter_validated_binaries, resolve_validated_binary, ExplicitBinary,
};

/// Writable fixture base outside `/tmp`: the ownership gate refuses
/// anything under `/tmp`, so fixtures live under the test binary's
/// own directory instead.
fn fixture_dir() -> tempfile::TempDir {
    let base = std::env::current_exe()
        .expect("exe path")
        .parent()
        .expect("exe parent")
        .to_path_buf();
    tempfile::tempdir_in(base).expect("fixture dir")
}

#[cfg(unix)]
fn write_stub(dir: &std::path::Path, name: &str, mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let candidate = dir.join(name);
    std::fs::write(&candidate, "#!/bin/sh\nexit 0\n").expect("stub writes");
    std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(mode))
        .expect("stub chmod");
    candidate
}

#[test]
fn explicit_slot_reads_the_env_var() {
    let _guard = common_core::sync::lock(&crate::ENV_LOCK);
    let var = "ORATOR_TEST_EXPLICIT_SLOT";
    std::env::remove_var(var);
    assert_eq!(
        ExplicitBinary::from_env(var),
        ExplicitBinary {
            env_var: var,
            path: None,
        }
    );
    std::env::set_var(var, "/opt/audio/bin/server");
    assert_eq!(
        ExplicitBinary::from_env(var).path,
        Some(PathBuf::from("/opt/audio/bin/server"))
    );
    std::env::remove_var(var);
}

#[test]
fn absolute_override_wins_untouched() {
    // Bypass: even a nonexistent `/tmp` path serves — the caller owns it.
    let explicit = ExplicitBinary {
        env_var: "ORATOR_TEST_EXPLICIT_ABS",
        path: Some(PathBuf::from("/tmp/definitely-not-a-server")),
    };
    assert_eq!(
        resolve_validated_binary(&explicit, vec![PathBuf::from("/bin/true")], current_uid()),
        Some(PathBuf::from("/tmp/definitely-not-a-server"))
    );
}

#[test]
fn relative_override_refuses_without_search() {
    let explicit = ExplicitBinary {
        env_var: "ORATOR_TEST_EXPLICIT_REL",
        path: Some(PathBuf::from("relative/server")),
    };
    assert_eq!(
        resolve_validated_binary(&explicit, vec![PathBuf::from("/bin/true")], current_uid()),
        None,
        "relative explicit never falls through to the table"
    );
}

#[test]
#[cfg(unix)]
fn unset_falls_through_to_the_first_validated_hit() {
    let dir = fixture_dir();
    let good = write_stub(dir.path(), "good-server", 0o755);
    let bad = write_stub(dir.path(), "bad-server", 0o777);
    let explicit = ExplicitBinary {
        env_var: "ORATOR_TEST_EXPLICIT_UNSET",
        path: None,
    };
    // Order preserved: bad first still serves good (bad is filtered).
    assert_eq!(
        resolve_validated_binary(&explicit, vec![bad.clone(), good.clone()], current_uid()),
        Some(good.clone())
    );
    // Nothing trusted: refused.
    assert_eq!(
        resolve_validated_binary(&explicit, vec![bad.clone()], current_uid()),
        None
    );
    // Wrong uid: refused with no serve.
    assert_eq!(
        resolve_validated_binary(&explicit, vec![good.clone()], current_uid().wrapping_add(1)),
        None
    );
}

#[test]
#[cfg(unix)]
fn filter_keeps_order_dedups_and_drops_untrusted() {
    let dir = fixture_dir();
    let good = write_stub(dir.path(), "good-server", 0o755);
    let bad = write_stub(dir.path(), "bad-server", 0o777);
    let filtered =
        filter_validated_binaries(vec![bad.clone(), good.clone(), good.clone()], current_uid());
    assert_eq!(filtered, vec![good], "untrusted dropped, duplicate deduped");
    assert!(
        filter_validated_binaries(vec![bad], current_uid()).is_empty(),
        "all-untrusted filters to empty"
    );
}

/// Fast containment check: lexical `.`/`..` and separator handling plus
/// hop-capped symlink resolution. Missing paths and escapes fail closed.
#[test]
#[cfg(unix)]
fn within_root_normalizes_lexical_components() {
    use crate::paths::within_root;
    let dir = fixture_dir();
    let root = dir.path().join("root");
    std::fs::create_dir(&root).expect("root");
    std::fs::create_dir(root.join("a")).expect("a");
    std::fs::write(root.join("b"), b"inside").expect("b");

    assert!(within_root(&root, &root.join("a/../b")));
    assert!(within_root(&root, &root.join("./b")));
    assert!(within_root(&root, &PathBuf::from(format!("{}//b", root.display()))));
    assert!(within_root(&root, &root), "root contains itself");
}

#[test]
#[cfg(unix)]
fn within_root_refuses_escapes_and_symlink_chains() {
    use crate::paths::within_root;
    let dir = fixture_dir();
    let root = dir.path().join("root");
    let outside = dir.path().join("outside");
    std::fs::create_dir(&root).expect("root");
    std::fs::create_dir(&outside).expect("outside");
    std::fs::create_dir(root.join("a")).expect("a");
    std::fs::write(outside.join("secret"), b"outside").expect("secret");

    // `..` past the root, and a plain absolute escape.
    assert!(!within_root(&root, &root.join("a/../../outside/secret")));
    assert!(!within_root(&root, &PathBuf::from("/etc/passwd")));

    // Chained relative/absolute symlinks landing outside the root.
    std::os::unix::fs::symlink(&outside, root.join("l2")).expect("l2");
    std::os::unix::fs::symlink("l2", root.join("l1")).expect("l1");
    assert!(!within_root(&root, &root.join("l1")));
}

/// M12-S1 characterization: symlink-parent detection. A path under a
/// symlinked directory reports true (swapped parents refuse); plain
/// and missing paths report false. Callers (`store`, `socket`)
/// compose this with `within_root`, which stays the fast check.
#[test]
#[cfg(unix)]
fn symlink_parent_detection() {
    use crate::paths::has_symlink_parent;
    let dir = fixture_dir();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).expect("real dir");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("link dir");
    assert!(
        has_symlink_parent(&link.join("child.txt")),
        "swapped parent detected through the child"
    );
    assert!(has_symlink_parent(&link), "the link itself detects");
    assert!(
        !has_symlink_parent(&real.join("child.txt")),
        "plain paths stay clean"
    );
    assert!(
        !has_symlink_parent(&dir.path().join("no-such-file.txt")),
        "missing paths are not symlink parents"
    );
}
