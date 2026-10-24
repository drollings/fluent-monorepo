//! Audit-ignore hygiene: every `#[ignore]` in the audit suites carries a
//! reason matching `AUDIT-H<n>: <non-empty>`. A bare `#[ignore]` — or a
//! reason that does not name its audit — fails this gate, mirroring the
//! `lint-live-ai` spirit for audit discipline.

use std::path::PathBuf;

fn audit_files() -> Vec<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<PathBuf> = std::fs::read_dir(manifest.join("tests"))
        .expect("tests dir must be readable")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("audit_") && name.ends_with(".rs"))
        })
        .collect();
    files.push(manifest.join("../fluent-concurrency/src/audit_tests.rs"));
    files.sort();
    files
}

fn check_file(path: &PathBuf) -> Vec<String> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("must read {}: {error}", path.display()));
    let mut violations = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("#[ignore") {
            continue;
        }
        let reason = trimmed
            .strip_prefix("#[ignore")
            .unwrap_or("")
            .trim_start_matches([' ', '='])
            .trim();
        let body = reason.strip_prefix('=').unwrap_or(reason).trim();
        let quoted = body
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix("\"]"));
        let Some(quoted) = quoted else {
            violations.push(format!(
                "{}:{}: bare #[ignore] has no reason",
                path.display(),
                index + 1
            ));
            continue;
        };
        let mut parts = quoted.splitn(2, ':');
        let head = parts.next().unwrap_or("").trim();
        let tail = parts.next().unwrap_or("").trim();
        let head_ok = head.starts_with("AUDIT-H")
            && head["AUDIT-H".len()..].chars().all(|c| c.is_ascii_digit())
            && head.len() > "AUDIT-H".len();
        if !head_ok || tail.is_empty() {
            violations.push(format!(
                "{}:{}: ignore reason must match `AUDIT-H<n>: <non-empty>`, got {quoted:?}",
                path.display(),
                index + 1
            ));
        }
    }
    violations
}

#[test]
fn audit_ignores_carry_audit_reasons() {
    let files = audit_files();
    assert!(
        !files.is_empty(),
        "expected at least one audit suite to scan"
    );
    let mut violations = Vec::new();
    for path in &files {
        violations.extend(check_file(path));
    }
    assert!(
        violations.is_empty(),
        "audit-ignore hygiene violations:\n{}",
        violations.join("\n")
    );
}
