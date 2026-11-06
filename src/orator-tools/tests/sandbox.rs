//! Sandbox tests: timeout kills the group (no orphan writers),
//! audit rows land, the path guard holds, the env scrubs.

use std::sync::Arc;

use crate::sandbox::{kill_program, shell_escape, Guarded, GuardedConfig, WithinError};

fn guarded(timeout_ms: u64) -> (Guarded, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let guard = Guarded::new(GuardedConfig {
        root: dir.path().to_path_buf(),
        timeout_ms,
        max_address_kb: 0,
        ..GuardedConfig::default()
    })
    .expect("jail builds");
    (guard, dir)
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

#[test]
fn echo_runs_and_audits() {
    let (guard, _dir) = guarded(5000);
    let result = common_core::runtime::block_on(guard.run("echo", &argv(&["echo", "hi"])))
        .expect("echo runs");
    assert!(!result.timed_out);
    assert!(result.output.contains("hi"));
    let rows = guard.audit_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "tool_call");
    assert_eq!(rows[0].detail["tool"], serde_json::json!("echo"));
    assert_eq!(rows[0].detail["verdict"], serde_json::json!("ok"));
}

#[test]
fn timeout_kills_the_group_leaving_no_writer() {
    let (guard, dir) = guarded(300);
    let marker = dir.path().join("orphan.txt");
    // Grandchild sleeps, then writes: a group kill means no marker.
    let script = format!("sleep 5; echo x > {}", marker.display());
    let result = common_core::runtime::block_on(guard.run("sleep", &argv(&["sh", "-c", &script])));
    let error = result.expect_err("budget must fire");
    assert_eq!(
        error,
        crate::tool::ToolError::Timeout("sleep".to_string(), 300)
    );
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        !marker.exists(),
        "grandchild must die with the group, leaving no marker"
    );
    assert_eq!(
        guard.audit_rows()[0].detail["verdict"],
        serde_json::json!("timeout")
    );
}

#[test]
fn env_scrubs_and_path_pins() {
    let (guard, _dir) = guarded(5000);
    std::env::set_var("ORATOR_REDTEAM_SECRET", "hunter2");
    let result =
        common_core::runtime::block_on(guard.run("env", &argv(&["env"]))).expect("env runs");
    assert!(
        !result.output.contains("hunter2"),
        "parent secrets must not leak"
    );
    let path =
        common_core::runtime::block_on(guard.run("path", &argv(&["sh", "-c", "echo $PATH"])))
            .expect("path check runs");
    assert_eq!(path.output.trim(), "/usr/bin:/bin");
}

#[test]
fn within_refuses_escapes_and_loops() {
    let (guard, dir) = guarded(5000);
    let inside = dir.path().join("ok.txt");
    std::fs::write(&inside, "data").expect("fixture");
    assert!(guard.within(std::path::Path::new("ok.txt")).is_ok());
    assert_eq!(
        guard
            .within(std::path::Path::new("../outside.txt"))
            .unwrap_err(),
        WithinError::Escape("../outside.txt".to_string())
    );
    assert_eq!(
        guard
            .within(std::path::Path::new("/etc/passwd"))
            .unwrap_err(),
        WithinError::Escape("/etc/passwd".to_string())
    );
    // Symlink loop: a ↔ b never resolves.
    std::os::unix::fs::symlink("b", dir.path().join("a")).expect("link a");
    std::os::unix::fs::symlink("a", dir.path().join("b")).expect("link b");
    assert!(matches!(
        guard.within(std::path::Path::new("a")).unwrap_err(),
        WithinError::HopCap(_) | WithinError::Escape(_)
    ));
    let _ = Arc::new(guard);
}

#[test]
fn shell_quoting_escapes_embedded_quotes() {
    let quoted = shell_escape(&["echo".to_string(), "it's 'quoted'".to_string()]);
    assert_eq!(quoted, "'echo' 'it'\\''s '\\''quoted'\\'''");
    let single = shell_escape(&["a'b".to_string()]);
    assert_eq!(single, "'a'\\''b'");
    let spaces = shell_escape(&["x y".to_string()]);
    assert_eq!(spaces, "'x y'");
}

#[test]
fn shell_quoting_round_trips_through_sh() {
    let (guard, _dir) = guarded(5000);
    let tricky = "it's 'quoted' $HOME `id`".to_string();
    let argv = vec!["echo".to_string(), tricky.clone()];
    let inner = shell_escape(&argv);
    let wrapped = vec!["sh".to_string(), "-c".to_string(), format!("exec {inner}")];
    let result =
        common_core::runtime::block_on(guard.run("echo", &wrapped)).expect("quoted echo runs");
    assert!(
        result.output.contains(&tricky),
        "quoted arg survives shell: {}",
        result.output
    );
    assert!(
        !result.output.contains("uid="),
        "no command substitution ran"
    );
}

#[test]
fn kill_program_prefers_absolute_path() {
    let program = kill_program();
    if program == "kill" {
        return;
    }
    assert!(program.starts_with('/'), "kill path is absolute: {program}");
    assert!(
        std::path::Path::new(&program).is_file(),
        "kill binary exists: {program}"
    );
}

#[test]
fn open_within_refuses_symlink_swap() {
    use std::fs::OpenOptions;
    let (guard, dir) = guarded(5000);
    let inside = dir.path().join("inside.txt");
    std::fs::write(&inside, "inside").expect("fixture");
    let outside_dir = tempfile::tempdir().expect("outside");
    let outside = outside_dir.path().join("secret.txt");
    std::fs::write(&outside, "secret").expect("outside fixture");
    std::os::unix::fs::symlink(&inside, dir.path().join("link")).expect("link inside");
    assert!(guard.within(std::path::Path::new("link")).is_ok());
    std::fs::remove_file(dir.path().join("link")).expect("unlink");
    std::os::unix::fs::symlink(&outside, dir.path().join("link")).expect("link outside");
    let opened = guard.open_within(std::path::Path::new("link"), OpenOptions::new().read(true));
    assert!(
        matches!(opened, Err(WithinError::Escape(_))),
        "swapped link must not open: {opened:?}"
    );
}

#[test]
fn open_within_reads_validated_handle() {
    use std::fs::OpenOptions;
    use std::io::Read as _;
    let (guard, dir) = guarded(5000);
    std::fs::write(dir.path().join("note.txt"), "hello").expect("fixture");
    let mut file = guard
        .open_within(
            std::path::Path::new("note.txt"),
            OpenOptions::new().read(true),
        )
        .expect("valid open");
    let mut text = String::new();
    file.read_to_string(&mut text).expect("read");
    assert_eq!(text, "hello");
}

#[test]
fn real_dir_replacement_invalidates() {
    let (guard, dir) = guarded(5000);
    let target = dir.path().join("real");
    std::fs::create_dir_all(&target).expect("mkdir");
    let first = guard.real_dir(&target).expect("resolves");
    assert!(first.starts_with(dir.path()));
    std::fs::remove_dir(&target).expect("rmdir");
    let outside = tempfile::tempdir().expect("outside");
    std::os::unix::fs::symlink(outside.path(), &target).expect("swap to outside");
    let second = guard.real_dir(&target);
    assert!(
        second.is_none() || second.as_ref() != Some(&first),
        "replaced dir must not return stale jail path: {second:?}"
    );
}

#[test]
fn audit_vector_rotates_under_cap() {
    use crate::sandbox::MAX_AUDIT_ROWS;
    let (guard, _dir) = guarded(5000);
    let argv = vec!["true".to_string()];
    for _ in 0..(MAX_AUDIT_ROWS + 50) {
        let _ = common_core::runtime::block_on(guard.run("true", &argv));
    }
    let rows = guard.audit_rows();
    assert!(
        rows.len() <= MAX_AUDIT_ROWS,
        "audit rotates: {} <= {MAX_AUDIT_ROWS}",
        rows.len()
    );
}

#[test]
fn default_root_is_private_and_never_tmp() {
    let root = crate::sandbox::GuardedConfig::default().root;
    assert!(
        root.is_absolute(),
        "jail root is absolute: {}",
        root.display()
    );
    assert!(
        !root.starts_with("/tmp"),
        "jail root never lives in /tmp: {}",
        root.display()
    );
    // Private home-rooted jail when a home resolves; fail-closed
    // sentinel otherwise (never /tmp, never relative-cwd).
    if let Some(home) = orator_core::paths::home_dir() {
        assert!(
            root.starts_with(&home),
            "default jail lives under the home base: {} vs {}",
            root.display(),
            home.display()
        );
        assert!(
            root.ends_with("orator/sandbox"),
            "default jail is the private sandbox dir: {}",
            root.display()
        );
    } else {
        assert!(
            !root.starts_with("/tmp"),
            "no-home fallback still never /tmp: {}",
            root.display()
        );
    }
}

#[test]
fn within_rejects_dotdot_and_chained_escapes() {
    let (guard, dir) = guarded(5000);
    // Ancestor escape via `..`.
    for evil in [
        "a/../../outside.txt",
        "../outside.txt",
        "sub/../../outside.txt",
    ] {
        assert!(
            matches!(
                guard.within(std::path::Path::new(evil)),
                Err(crate::sandbox::WithinError::Escape(_))
            ),
            "{evil} must escape, not resolve"
        );
    }
    // Absolute path with `..` that lexically lands outside.
    let outside_abs = dir
        .path()
        .join("a")
        .join("..")
        .join("..")
        .join("outside.txt");
    assert!(
        matches!(
            guard.within(&outside_abs),
            Err(crate::sandbox::WithinError::Escape(_))
        ),
        "absolute-plus-dotdot outside must escape"
    );
    // Chained symlinks: b -> c -> outside.
    let outside_dir = tempfile::tempdir().expect("outside");
    let target = outside_dir.path().join("secret.txt");
    std::fs::write(&target, "secret").expect("outside fixture");
    std::os::unix::fs::symlink(&target, dir.path().join("c")).expect("c links outside");
    std::os::unix::fs::symlink("c", dir.path().join("b")).expect("b links c");
    assert!(
        matches!(
            guard.within(std::path::Path::new("b")),
            Err(crate::sandbox::WithinError::Escape(_))
        ),
        "chained b->c->outside must escape"
    );
    // Tail symlink escape: dir/sub is real, sub/link points outside.
    std::fs::create_dir_all(dir.path().join("sub")).expect("sub");
    std::os::unix::fs::symlink(&target, dir.path().join("sub").join("link"))
        .expect("tail link outside");
    assert!(
        matches!(
            guard.within(std::path::Path::new("sub/link")),
            Err(crate::sandbox::WithinError::Escape(_))
        ),
        "tail symlink to outside must escape"
    );
    // Symlink loop is HopCap (or Escape), never Ok outside.
    std::os::unix::fs::symlink("loop-b", dir.path().join("loop-a")).expect("loop a");
    std::os::unix::fs::symlink("loop-a", dir.path().join("loop-b")).expect("loop b");
    assert!(
        matches!(
            guard.within(std::path::Path::new("loop-a")),
            Err(crate::sandbox::WithinError::HopCap(_) | crate::sandbox::WithinError::Escape(_))
        ),
        "symlink loop must not resolve"
    );
}

#[test]
fn open_within_is_fail_closed_without_fd_revalidation() {
    // The fd-backed check must refuse when the descriptor path cannot
    // be re-resolved: an error never falls back to the pre-open
    // candidate. The helper models the decision so the masked-/proc
    // case is pinned without masking /proc itself.
    let candidate = std::path::PathBuf::from("/jail/candidate.txt");
    let root = std::path::PathBuf::from("/jail");
    let refused: Result<std::path::PathBuf, crate::sandbox::WithinError> =
        crate::sandbox::check_fd_containment(Err("masked".to_string()), &candidate, &root);
    assert!(
        matches!(
            refused,
            Err(crate::sandbox::WithinError::Escape(_) | crate::sandbox::WithinError::Missing(_))
        ),
        "unresolvable fd must refuse, never open the candidate: {refused:?}"
    );
    let inside = std::path::PathBuf::from("/jail/note.txt");
    let ok = crate::sandbox::check_fd_containment(Ok(inside.clone()), &candidate, &root)
        .expect("inside fd opens");
    assert_eq!(ok, inside);
    let outside = std::path::PathBuf::from("/etc/passwd");
    assert!(
        crate::sandbox::check_fd_containment(Ok(outside), &candidate, &root).is_err(),
        "outside fd must refuse even when the candidate is inside"
    );
}
