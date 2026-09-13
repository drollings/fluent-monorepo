//! Token helpers for the search pipeline: lowercase/dedup collectors,
//! separator splitting, and code-symbol name extraction.
//!
//! Canonical home (ROADMAP_20260911_PRIMITIVES M13) for the bespoke
//! tokenizers previously duplicated in `guidance-core` (`graph_index`,
//! `search_types`). Deliberately non-canonical here:
//! - CJK bigrams/predicates live in `common_core::string` (shared by the
//!   index side and the FTS query side; `db.rs` already composes them).
//! - Char-boundary flooring is `str::floor_char_boundary` (std).
//! - Word/predicate helpers (`contains_any`, …) stay in
//!   `common_core::string`.

use std::collections::HashSet;

/// Lowercase `value` and push it unless empty or already present
/// (order-preserving dedup). Source: `graph_index::push_lower`.
pub fn lower_dedup_push(out: &mut Vec<String>, value: &str) {
    let lowered = value.to_lowercase();
    if !lowered.is_empty() && !out.contains(&lowered) {
        out.push(lowered);
    }
}

/// Split on non-alphanumeric separators, lowercase, drop empties.
/// Duplicates are preserved (collect into a `HashSet` for membership, as
/// `role_overlap` does). Underscore splits (it is not alphanumeric), so
/// `Foo.Bar` and `foo_bar` both split. Source: `graph_index::extend_tokens`.
#[must_use]
pub fn split_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|token| !token.is_empty())
        .collect()
}

/// Query tokens that name code symbols, minus the keyword stoplist.
/// Source: `pipeline/search/index.ts` (`extractSymbolNames`), via
/// `guidance-core::search_types::extract_symbol_names` (moved, not copied).
#[must_use]
pub fn symbol_names(query: &str) -> Vec<String> {
    const KEYWORDS: &[&str] = &[
        "class", "struct", "enum", "interface", "function", "method", "type", "const", "let",
        "var", "namespace", "where", "find", "explain",
    ];
    let mut names = HashSet::new();
    let mut ordered = Vec::new();
    let bytes = query.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let is_start = |b: u8| b.is_ascii_alphabetic() || b == b'_' || b == b'~';
        if !is_start(bytes[i]) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len()
            && (bytes[j].is_ascii_alphanumeric() || matches!(bytes[j], b'_' | b'~' | b':'))
        {
            j += 1;
        }
        let token = &query[i..j];
        if !KEYWORDS.contains(&token.to_ascii_lowercase().as_str()) {
            if let Some(name) = symbol_name_from_token(token) {
                if names.insert(name.clone()) {
                    ordered.push(name);
                }
            }
        }
        i = j;
    }
    ordered
}

/// Owner-aware symbol name: keeps `Owner::name` only for uppercase owners.
/// Source: `pipeline/search/index.ts` (`symbolNameFromToken`), via
/// `guidance-core::search_types::symbol_name_from_token` (moved, not copied).
#[must_use]
pub fn symbol_name_from_token(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split("::").filter(|p| !p.is_empty()).collect();
    let name = parts.last().copied().unwrap_or(token);
    let valid = |s: &str| {
        let mut chars = s.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '~')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '~')
    };
    if !valid(name) {
        return None;
    }
    if parts.len() < 2 {
        return Some(name.to_string());
    }
    let owner = parts[parts.len() - 2];
    if owner.starts_with(|c: char| c.is_ascii_uppercase() || c == '_' || c == '~') {
        return Some(format!("{owner}::{name}"));
    }
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_dedup_push_collapses_case_variants_and_empties() {
        let mut out = Vec::new();
        lower_dedup_push(&mut out, "Find");
        lower_dedup_push(&mut out, "find");
        lower_dedup_push(&mut out, "");
        lower_dedup_push(&mut out, "gamma");
        assert_eq!(out, vec!["find".to_string(), "gamma".to_string()]);
    }

    #[test]
    fn split_tokens_edges() {
        assert!(split_tokens("").is_empty());
        assert_eq!(split_tokens("Foo.Bar"), vec!["foo".to_string(), "bar".to_string()]);
        // Underscore is not alphanumeric: it splits too.
        assert_eq!(split_tokens("foo_bar"), vec!["foo".to_string(), "bar".to_string()]);
        assert_eq!(
            split_tokens("a::b c-d"),
            vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string()]
        );
        // Duplicates preserved; empties dropped.
        assert_eq!(
            split_tokens("x..x"),
            vec!["x".to_string(), "x".to_string()]
        );
    }

    #[test]
    fn symbol_names_edges() {
        assert!(symbol_names("").is_empty());
        assert_eq!(
            symbol_names("foo bar foo"),
            vec!["foo".to_string(), "bar".to_string()]
        );
        assert_eq!(symbol_names("~temp"), vec!["~temp".to_string()]);
        assert!(symbol_names("a:b:c").is_empty());
        assert_eq!(symbol_names("café"), vec!["caf".to_string()]);
    }

    #[test]
    fn symbol_name_from_token_edges() {
        assert_eq!(symbol_name_from_token("foo::"), Some("foo".to_string()));
        assert_eq!(symbol_name_from_token("::foo"), Some("foo".to_string()));
        assert_eq!(
            symbol_name_from_token("Foo::Bar::baz"),
            Some("Bar::baz".to_string())
        );
        assert_eq!(symbol_name_from_token(""), None);
        assert_eq!(symbol_name_from_token("::"), None);
    }
}
