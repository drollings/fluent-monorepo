//! Sig-gated invalidation: whose dependents re-harvest after an interface
//! change, decided from stored rows plus persisted edges — never a miss.
//!
//! Syntactic edges over-approximate by design (extra work, never missed
//! work); macro definitions and glob imports widen conservatively.

use std::collections::{BTreeMap, BTreeSet};

/// Symbol interfaces that changed between stored rows and a fresh
/// harvest: new, deleted, or rehashed symbol ids.
#[must_use]
pub fn interface_changes(
    stored: &[(String, String)],
    fresh: &[(String, String)],
) -> Vec<String> {
    let old: BTreeMap<&str, &str> = stored
        .iter()
        .map(|(id, hash)| (id.as_str(), hash.as_str()))
        .collect();
    let new: BTreeMap<&str, &str> = fresh
        .iter()
        .map(|(id, hash)| (id.as_str(), hash.as_str()))
        .collect();
    let mut changed = BTreeSet::new();
    for (id, hash) in &new {
        if old.get(id) != Some(hash) {
            changed.insert((*id).to_string());
        }
    }
    for id in old.keys() {
        if !new.contains_key(id) {
            changed.insert((*id).to_string());
        }
    }
    changed.into_iter().collect()
}

/// Whether one file's interface set moved (any new, deleted, or
/// rehashed symbol). Body-only edits keep every interface hash stable,
/// so dependents are never invalidated by them.
#[must_use]
pub fn file_sig_changed(stored: &[(String, String)], fresh: &[(String, String)]) -> bool {
    !interface_changes(stored, fresh).is_empty()
}

/// File stem for edge matching (`/a/b.rs` to `b`, `mod.rs` stays `mod`).
fn stem_of(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .rsplit('\\')
        .next()
        .unwrap_or(path)
        .split('.')
        .next()
        .unwrap_or(path)
}

/// Importers that may observe a changed file: any edge whose specifier
/// names the file's stem as a path segment, an exact resolved target,
/// or a glob (globs defeat name analysis, so they widen). Segments, not
/// substrings: stem `a` must not match specifier `unrelated`. A raw
/// substring fallback covers odd specifier formats for stems over two
/// chars, where segmentation alone would miss. Over-approximate
/// otherwise: extra work, never missed work.
#[must_use]
pub fn dependents_of(
    edges: &[(String, String, Option<String>)],
    changed_paths: &[String],
) -> Vec<String> {
    let mut out = BTreeSet::new();
    for (importer, specifier, resolved) in edges {
        for changed in changed_paths {
            if importer == changed {
                continue;
            }
            let stem = stem_of(changed);
            let segments: Vec<&str> = specifier
                .split(['/', ':', '.', '\\'])
                .filter(|segment| !segment.is_empty())
                .collect();
            let reaches = resolved.as_deref() == Some(changed.as_str())
                || segments.iter().any(|segment| *segment == stem || *segment == "*")
                || specifier.contains(stem) && stem.len() > 2;
            if reaches {
                out.insert(importer.clone());
            }
        }
    }
    out.into_iter().collect()
}

/// Whether file text defines macros: a `macro_rules!` definition makes
/// syntactic edges unreliable, so the whole package re-harvests.
#[must_use]
pub fn defines_macros(text: &str) -> bool {
    text.contains("macro_rules!")
}

/// Conservative expansion for macro-defining changes: every scanned file
/// under the same root re-harvests, since macro uses hide from edges.
#[must_use]
pub fn macro_expansion(scanned_roots: &[(String, String)], changed: &[String]) -> Vec<String> {
    let mut out = BTreeSet::new();
    for changed_path in changed {
        let Some(root) = scanned_roots
            .iter()
            .find(|(path, _)| path == changed_path)
            .map(|(_, root)| root.clone())
        else {
            continue;
        };
        for (path, file_root) in scanned_roots {
            if file_root == &root && path != changed_path {
                out.insert(path.clone());
            }
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
#[path = "../tests/invalidation.rs"]
mod invalidation_tests;
