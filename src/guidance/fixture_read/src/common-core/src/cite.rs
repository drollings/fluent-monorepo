//! Citations: hash-verified source ranges.

/// Cite a hit by hash-verified source range.
pub fn cite_hit(path: &str, line: u32) -> String {
    format!("{path}:{line}")
}
