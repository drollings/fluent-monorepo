//! Pure lexical path helpers (no I/O): separator normalization, `.`/`..`
//! cleaning, parent/basename splits, and absolute-path detection.
//!
//! These own the single canonical copy of the lexical cleaners previously
//! duplicated across `guidance` (`change_set`, `graph_index`, `query::glob`,
//! `grounding`). Deleted paths stay representable — nothing here touches the
//! filesystem, resolves symlinks, or requires existence.
//!
//! Pinned semantics (byte-for-byte with the pre-extraction call sites):
//! - `\` becomes `/`; runs of `/` collapse to one.
//! - `.` segments drop; `..` pops the previous segment and never climbs
//!   above root `/` (a relative `..` that would climb above the working
//!   directory collapses to empty/remainder).
//! - A leading `/` is preserved; trailing `/` strips except for root `/`.
//! - Relative inputs stay relative; drive prefixes (`C:/`, `C:\`) pass
//!   through as ordinary first segments.
//!
//! # Examples
//!
//! ```
//! use common_core::path::{file_name_lexical, is_absolute_lexical, normalize_lexical, parent_lexical};
//!
//! assert_eq!(normalize_lexical("a//b/./c/../d"), "a/b/d");
//! assert_eq!(normalize_lexical("C:\\a\\b"), "C:/a/b");
//! assert_eq!(normalize_lexical("/../a"), "/a");
//! assert_eq!(normalize_lexical("/a/b/"), "/a/b");
//! assert!(is_absolute_lexical("C:/x"));
//! assert_eq!(parent_lexical("/a/b"), Some("/a".to_string()));
//! assert_eq!(file_name_lexical("/a/b/"), Some("b"));
//! ```

/// Replace `\` with `/` and collapse runs of `/` to one.
///
/// Pure separator step shared by [`normalize_lexical`] and the glob-pattern
/// cleaners (which must NOT resolve `.`/`..` — pattern syntax, not paths).
#[must_use]
pub fn collapse_separators(path: &str) -> String {
    let slashed = path.replace('\\', "/");
    let mut collapsed = String::with_capacity(slashed.len());
    let mut previous_slash = false;
    for ch in slashed.chars() {
        if ch == '/' {
            if previous_slash {
                continue;
            }
            previous_slash = true;
        } else {
            previous_slash = false;
        }
        collapsed.push(ch);
    }
    collapsed
}

/// Normalize separators to `/` and clean `.`/`..` lexically (no I/O, so
/// deleted paths stay representable).
#[must_use]
pub fn normalize_lexical(path: &str) -> String {
    let slashed = collapse_separators(path);
    let mut parts: Vec<&str> = Vec::new();
    let absolute = slashed.starts_with('/');
    for part in slashed.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Parent directory of a lexically-cleaned path (`None` when the path has no
/// `/` — including root `/`, whose trimmed form carries no separator).
#[must_use]
pub fn parent_lexical(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    let index = trimmed.rfind('/')?;
    if index == 0 {
        return Some("/".to_string());
    }
    Some(trimmed[..index].to_string())
}

/// Final segment of a lexically-cleaned path (trailing `/` ignored).
#[must_use]
pub fn file_name_lexical(path: &str) -> Option<&str> {
    path.trim_end_matches('/').rsplit('/').next()
}

/// Absolute-path detection: leading `/`, or a drive prefix (`X:/`, `X:\`).
#[must_use]
pub fn is_absolute_lexical(path: &str) -> bool {
    path.starts_with('/')
        || (path.len() >= 3
            && path.as_bytes()[1] == b':'
            && (path.as_bytes()[2] == b'/' || path.as_bytes()[2] == b'\\'))
}
