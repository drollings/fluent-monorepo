//! Normalized item hashes: BLAKE3 over token streams, split into
//! signature, body, and doc so cosmetic edits never cascade.
//!
//! Rust is comment-aware (`//` and `/* */` never enter `sig` or
//! `body`); every other language gets whitespace collapsing only.
//! Hashes are content fingerprints, never persisted verdicts.

use std::collections::BTreeMap;

/// Per-item hashes over normalized text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemHashes {
    /// Normalized signature (generics, bounds, visibility, attributes).
    pub sig: String,
    /// Normalized body tokens.
    pub body: String,
    /// Normalized doc attributes.
    pub doc: String,
    /// Combined item identity over the three hashes.
    pub item: String,
}

/// Collapse whitespace runs to single spaces and trim ends.
fn collapse_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Strip Rust line and block comments, keeping string and char literal
/// contents byte-identical (a `//` inside `"..."` is content, not prose).
/// Lifetimes (`'a`) scan like char literals: an approximation that costs
/// extra work on comment-adjacent lifetimes, never missed work.
fn strip_rust_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
            out.push(c);
        } else if c == '/' && chars.peek() == Some(&'/') {
            for d in chars.by_ref() {
                if d == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut prev_star = false;
            for d in chars.by_ref() {
                if prev_star && d == '/' {
                    break;
                }
                prev_star = d == '*';
            }
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// Split normalized text into leaf tokens: identifier runs, string and
/// char literals (escapes respected), and single punctuation chars.
/// Whitespace never survives; joining with NUL keeps `ab` apart from
/// `a b` while staying stable across cosmetic-only edits. Offsets ride
/// char boundaries, so non-ASCII identifiers never panic slicing.
fn leaf_tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = text;
    let mut offset = 0;
    while !rest.is_empty() {
        let c = rest.chars().next().unwrap_or('\0');
        let len = c.len_utf8();
        if c.is_whitespace() {
            rest = &rest[len..];
            offset += len;
        } else if c.is_alphanumeric() || c == '_' {
            let start = offset;
            while !rest.is_empty() {
                let d = rest.chars().next().unwrap_or('\0');
                if d.is_alphanumeric() || d == '_' {
                    rest = &rest[d.len_utf8()..];
                    offset += d.len_utf8();
                } else {
                    break;
                }
            }
            tokens.push(&text[start..offset]);
        } else if c == '"' || c == '\'' {
            let start = offset;
            rest = &rest[len..];
            offset += len;
            let mut escaped = false;
            let mut closed = false;
            while !rest.is_empty() {
                let d = rest.chars().next().unwrap_or('\0');
                let dlen = d.len_utf8();
                rest = &rest[dlen..];
                offset += dlen;
                if escaped {
                    escaped = false;
                } else if d == '\\' {
                    escaped = true;
                } else if d == c {
                    closed = true;
                    break;
                }
            }
            let _ = closed;
            tokens.push(&text[start..offset]);
        } else {
            tokens.push(&text[offset..offset + len]);
            rest = &rest[len..];
            offset += len;
        }
    }
    tokens
}

/// Normalize one text for hashing: comment-aware token stream for Rust,
/// whitespace collapsing for every other language.
#[must_use]
pub fn normalize_code_text(language: &str, text: &str) -> String {
    if language == "rust" {
        leaf_tokens(&strip_rust_comments(text)).join("\0")
    } else {
        collapse_ws(text)
    }
}

/// Hash one normalized text with the shared hasher (never a second one).
fn hash_text(text: &str) -> String {
    common_core::hash::blake3_hex(text.as_bytes())
}

/// Split hashes for one item: signature, body, and doc hashed apart so
/// a body-only edit keeps `sig` stable and a doc-only edit keeps both
/// `sig` and `body` stable. `item` binds the three together.
#[must_use]
pub fn item_hashes_for(
    language: &str,
    signature: &str,
    body: &str,
    doc: Option<&str>,
) -> ItemHashes {
    let sig = hash_text(&normalize_code_text(language, signature));
    let body = hash_text(&normalize_code_text(language, body));
    // Doc attributes are prose, not code: collapsing keeps reflows
    // stable without letting comment-stripping erase the text itself.
    let doc = hash_text(&collapse_ws(doc.unwrap_or("")));
    let item = common_core::hash::blake3_hex(format!("{sig}\0{body}\0{doc}").as_bytes());
    ItemHashes {
        sig,
        body,
        doc,
        item,
    }
}

/// Language-neutral signature normalization: whitespace collapsing
/// only. Comment awareness is per-language (`normalize_code_text`);
/// built signature strings are single-line, so this is the stable
/// comparator for cross-language merge decisions.
#[must_use]
pub fn normalize_signature_text(signature: &str) -> String {
    signature.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether two signatures name the same interface under the
/// language-neutral normalizer (merge fallback; see `normalize_code_text`
/// for the comment-aware Rust comparator).
pub fn interface_stable(language: &str, old_sig: &str, new_sig: &str) -> bool {
    normalize_code_text(language, old_sig) == normalize_code_text(language, new_sig)
}

/// Per-item work key: the item plus its dependencies' sig hashes plus
/// the consumed input components. Unchanged keys reuse stored receipts.
#[must_use]
pub fn work_key(
    item_hash: &str,
    dep_sig_hashes: &[&str],
    components: &BTreeMap<String, String>,
) -> String {
    let mut sorted_deps: Vec<&str> = dep_sig_hashes.to_vec();
    sorted_deps.sort_unstable();
    let mut framed =
        format!("item\0{item_hash}\0deps\0{}\0comp\0", sorted_deps.join(","));
    for (name, hash) in components {
        framed.push_str(name);
        framed.push('\0');
        framed.push_str(hash);
        framed.push('\0');
    }
    common_core::hash::blake3_hex(framed.as_bytes())
}

/// Whether a stored work key still covers this item: exact match on
/// the recomputed key, fail closed on missing evidence. Task value,
/// never producer confidence.
#[must_use]
pub fn work_key_matches(
    item_hash: &str,
    dep_sig_hashes: &[&str],
    components: &BTreeMap<String, String>,
    stored: Option<&str>,
) -> bool {
    stored.is_some_and(|want| work_key(item_hash, dep_sig_hashes, components) == want)
}

#[cfg(test)]
#[path = "../tests/m3_items.rs"]
mod m3_items_tests;
