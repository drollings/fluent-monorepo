use common_core::shell::*;

#[test]
fn run_command_true() {
    assert!(run_command(&["true"]));
}

#[test]
fn run_command_false() {
    assert!(!run_command(&["false"]));
}

#[test]
fn add_unique_path_deduplicates() {
    let mut list = Vec::new();
    assert!(add_unique_path(&mut list, "path1", None));
    assert!(!add_unique_path(&mut list, "path1", None));
    assert!(add_unique_path(&mut list, "path2", None));
    assert_eq!(list.len(), 2);
}

#[test]
fn run_command_empty_argv_returns_false() {
    assert!(!run_command(&[]));
}

#[test]
fn add_unique_path_with_project_root_existing() {
    let mut list = Vec::new();
    let root = std::env::current_dir().unwrap();
    let root_str = root.to_str().unwrap();
    assert!(add_unique_path(&mut list, "src", Some(root_str)));
    assert_eq!(list.len(), 1);
}

#[test]
fn add_unique_path_with_project_root_missing() {
    let mut list = Vec::new();
    assert!(!add_unique_path(
        &mut list,
        "nonexistent_path_xyz",
        Some("/tmp")
    ));
    assert_eq!(list.len(), 0);
}

#[test]
fn add_unique_path_with_project_root_trailing_slash() {
    let mut list = Vec::new();
    let root = std::env::current_dir().unwrap();
    let root_str = format!("{}/", root.to_str().unwrap());
    assert!(add_unique_path(&mut list, "src", Some(&root_str)));
    assert_eq!(list.len(), 1);
}

#[test]
fn add_unique_path_with_empty_project_root() {
    let mut list = Vec::new();
    assert!(add_unique_path(&mut list, "some_path", Some("")));
    assert_eq!(list.len(), 1);
}

#[test]
fn shell_cmd_returns_valid_pair() {
    let (prog, arg) = shell_cmd();
    assert!(!prog.is_empty());
    assert!(!arg.is_empty());
    if cfg!(target_os = "windows") {
        assert_eq!(prog, "cmd");
        assert_eq!(arg, "/C");
    } else {
        assert_eq!(prog, "sh");
        assert_eq!(arg, "-c");
    }
}

#[test]
fn run_capture_true() {
    let result = run_capture(&["true"]).unwrap();
    assert!(result.success);
    assert!(result.stderr.is_empty());
}

#[test]
fn run_capture_false() {
    let result = run_capture(&["false"]).unwrap();
    assert!(!result.success);
}

#[test]
fn run_capture_empty_argv() {
    let result = run_capture(&[]).unwrap();
    assert!(!result.success);
}

#[test]
fn run_capture_stdout() {
    let result = run_capture(&["echo", "hello"]).unwrap();
    assert!(result.success);
    assert_eq!(result.stdout.trim(), "hello");
}

#[test]
fn run_shell_capture_echo() {
    let result = run_shell_capture("echo world").unwrap();
    assert!(result.success);
    assert_eq!(result.stdout.trim(), "world");
}

#[test]
fn run_shell_capture_false() {
    let result = run_shell_capture("false").unwrap();
    assert!(!result.success);
}

#[test]
fn run_capture_with_env_honors_env() {
    let out = run_capture_with_env(
        &["sh", "-c", "echo $SPLIT_M11_PROBE"],
        &[("SPLIT_M11_PROBE".to_string(), "found".to_string())],
    )
    .expect("spawns");
    assert!(out.success);
    assert_eq!(out.stdout.trim(), "found");
}

#[test]
fn run_capture_with_env_empty_matches_plain_capture() {
    let plain = run_capture(&["echo", "hello"]).expect("spawns");
    let with_empty: Vec<(String, String)> = Vec::new();
    let via_variant = run_capture_with_env(&["echo", "hello"], &with_empty).expect("spawns");
    assert_eq!(plain.stdout, via_variant.stdout);
    assert_eq!(plain.success, via_variant.success);
}

#[test]
fn run_capture_with_env_empty_argv() {
    let out =
        run_capture_with_env(&[], &[("A".to_string(), "b".to_string())]).expect("never spawns");
    assert!(!out.success);
    assert!(out.stdout.is_empty());
}

#[test]
fn run_capture_with_env_decodes_lossy() {
    let out = run_capture_with_env(&["sh", "-c", "printf '\\xff'"], &[]).expect("spawns");
    assert!(out.stdout.contains('\u{fffd}'), "lossy: {:?}", out.stdout);
}
