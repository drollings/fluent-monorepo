//! Error-envelope tests: every builder line carries the prefix, the
//! action, and the remedy where a kind is known.

use std::io;

use crate::error::{file_error, parse_error};

fn missing() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "No such file or directory (os error 2)",
    )
}

#[test]
fn missing_file_names_path_and_remedy() {
    let line = file_error(
        "read stat file",
        std::path::Path::new("/nonexistent/stat.jsonl"),
        &missing(),
    );
    assert!(line.starts_with("orator: "), "{line}");
    assert!(line.contains("/nonexistent/stat.jsonl"), "{line}");
    assert!(line.contains("check the path"), "{line}");
}

#[test]
fn permission_denied_names_its_remedy() {
    let error = io::Error::new(
        io::ErrorKind::PermissionDenied,
        "permission denied (os error 13)",
    );
    let line = file_error(
        "read stat file",
        std::path::Path::new("/root/stat.jsonl"),
        &error,
    );
    assert!(line.starts_with("orator: "), "{line}");
    assert!(line.contains("check permissions"), "{line}");
}

#[test]
fn unknown_kinds_carry_no_hint_but_keep_the_prefix() {
    let error = io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe (os error 32)");
    let line = file_error(
        "read stat file",
        std::path::Path::new("/tmp/s.jsonl"),
        &error,
    );
    assert!(line.starts_with("orator: "), "{line}");
    assert!(line.contains("/tmp/s.jsonl"), "{line}");
    assert!(!line.contains("check the"), "no remedy hint: {line}");
}

#[test]
fn parse_errors_name_file_and_line() {
    let line = parse_error(
        "parse stat file",
        std::path::Path::new("/tmp/s.jsonl"),
        3,
        "expected value at line 1 column 1",
    );
    assert!(line.starts_with("orator: "), "{line}");
    assert!(line.contains("/tmp/s.jsonl"), "{line}");
    assert!(line.contains("line 3"), "{line}");
}
