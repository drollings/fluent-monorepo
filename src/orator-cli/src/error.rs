//! One error envelope: every CLI failure line starts with `orator:`,
//! names what was attempted, and carries a remedy hint for known
//! failure kinds. Exit codes never change here — this only builds the
//! text; the binary prints it and returns the same code as before.

use std::io;

/// Remedy hint for a known failure kind; empty when nothing specific
/// applies. Hints name the next operator action, never internals.
fn io_remedy(kind: io::ErrorKind) -> &'static str {
    match kind {
        io::ErrorKind::NotFound => " (check the path)",
        io::ErrorKind::PermissionDenied => " (check permissions)",
        _ => "",
    }
}

/// One line for a file-backed failure: `orator:` prefix, the action,
/// the path, the OS detail, and a remedy hint for known kinds.
#[must_use]
pub fn file_error(action: &str, path: &std::path::Path, error: &io::Error) -> String {
    format!(
        "orator: {action} {}: {error}{}",
        path.display(),
        io_remedy(error.kind())
    )
}

/// One line for a malformed file: prefix, action, path, line number,
/// and the parse detail.
#[must_use]
pub fn parse_error(
    action: &str,
    path: &std::path::Path,
    line_no: usize,
    detail: impl std::fmt::Display,
) -> String {
    format!(
        "orator: {action} {} (line {line_no}): {detail}",
        path.display()
    )
}

#[cfg(test)]
#[path = "../tests/error.rs"]
mod tests;
