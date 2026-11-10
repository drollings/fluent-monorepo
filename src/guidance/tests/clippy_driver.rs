//! Clippy-driver pins: JSON parsing, file attribution, shaping matrix,
//! and argv construction (no live cargo in unit tests).

use std::path::Path;

use crate::clippy::{attribute_diagnostics, parse_clippy_json, shaping_for, ClippyCtx};

const FIXTURE: &str = r#"{"reason": "compiler-message", "message": {"message": "unused import: `foo`", "code": {"code": "unused_imports"}, "level": "warning", "spans": [{"file_name": "src/a.rs", "byte_start": 10, "byte_end": 20, "line_start": 2, "suggested_replacement": "", "suggestion_applicability": "MachineApplicable"}]}}
{"reason": "build-finished", "success": true}
{"reason": "compiler-message", "message": {"message": "cannot find value", "code": {"code": "E0425"}, "level": "error", "spans": [{"file_name": "src/b.rs", "byte_start": 0, "byte_end": 5, "suggested_replacement": null, "suggestion_applicability": null}]}}
not json at all"#;

#[test]
fn parses_messages_and_skips_noise() {
    let report = parse_clippy_json(FIXTURE);
    assert_eq!(report.diagnostics.len(), 2);
    assert_eq!(report.diagnostics[0].code.as_deref(), Some("unused_imports"));
    assert_eq!(report.diagnostics[0].file.as_deref(), Some("src/a.rs"));
    assert_eq!(report.diagnostics[1].level, "error");
    assert!(report.diagnostics[1].suggestions.is_empty());
    assert_eq!(report.diagnostics[0].suggestions.len(), 1);
}

#[test]
fn attributes_diagnostics_to_files() {
    let report = parse_clippy_json(FIXTURE);
    let by_file = attribute_diagnostics(&report);
    assert_eq!(by_file.get("src/a.rs").map(Vec::len), Some(1));
    assert_eq!(by_file.get("src/b.rs").map(Vec::len), Some(1));
    assert_eq!(by_file.get("src/c.rs"), None);
}

#[test]
fn shaping_lowers_priority_under_interactive_load() {
    let idle = shaping_for(false);
    assert_eq!(idle.jobs, None);
    assert!(!idle.nice);
    let busy = shaping_for(true);
    assert_eq!(busy.jobs, Some(2));
    assert!(busy.nice);
}

#[test]
fn argv_carries_target_dir_jobs_and_nice() {
    let ctx = ClippyCtx {
        target_dir: Some(Path::new("/tmp/t").to_path_buf()),
        shaping: shaping_for(true),
        extra_args: vec!["--all-features".to_string()],
    };
    let (argv, env): (Vec<String>, Vec<(String, String)>) = crate::clippy::clippy_command(
        Path::new("/pkg"),
        &ctx,
    );
    assert_eq!(&argv[..3], &["nice", "-n", "10"]);
    assert!(argv.contains(&"cargo".to_string()));
    assert!(argv.contains(&"--target-dir".to_string()));
    assert!(argv.contains(&"--all-features".to_string()));
    assert!(env.iter().any(|(k, v)| k == "CARGO_BUILD_JOBS" && v == "2"));
    let ctx = ClippyCtx {
        target_dir: None,
        shaping: shaping_for(false),
        extra_args: Vec::new(),
    };
    let (argv, env) = crate::clippy::clippy_command(Path::new("/pkg"), &ctx);
    assert_eq!(argv[0], "cargo");
    assert!(!argv.contains(&"--target-dir".to_string()));
    assert!(env.is_empty());
}
