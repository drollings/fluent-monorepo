//! YaGO TTL → JSON taxonomy pipeline — Rust owner of the conversion the
//! deprecated `src/ontology/tools/*.py` shims used to perform.
//!
//! Converts a pruned YaGO taxonomy TTL into a JSON mapping of each `yago:`
//! class to its direct parents + transitive ancestors (multiple inheritance
//! preserved — ancestors are the full transitive closure of
//! `rdfs:subClassOf`, deduplicated and sorted).
//!
//! # Behavior change vs the Python shims
//!
//! Name normalization unifies on the Rust semantics
//! ([`crate::normalize`]): the local name additionally decodes `_UXXXX`
//! escapes (upper- and lowercase) where the Python `normalize_curie` merely
//! turned underscores into spaces. E.g.
//! `yago:Remix__U0028_Work_U0029__Q113171270` → Python
//! `yago:remix  u0028 work u0029`, Rust `yago:remix ( work )`. The CURIE
//! prefix is preserved lowercased in both. Hyphens are word separators only
//! in [`crate::normalize::matches_lexicon`], never rewritten here.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use guidance_rdf::lexer::{Lexer, TokenKind};
use serde::{Deserialize, Serialize};

use crate::normalize::{normalize_extracted_name, normalize_yago_name};

/// One taxonomy entry: direct parents + all transitive ancestors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassEntry {
    /// Direct `rdfs:subClassOf` parents (normalized, sorted).
    pub parents: Vec<String>,
    /// Full transitive ancestor closure (normalized, sorted, deduplicated).
    pub ancestors: Vec<String>,
}

/// Expand a CURIE to a full IRI; `<...>` IRIs are returned stripped.
#[must_use]
pub fn expand_curie(curie: &str, prefixes: &BTreeMap<String, String>) -> String {
    let curie = curie.strip_suffix([',', '.', ';']).unwrap_or(curie);
    let curie = curie.trim();
    if let Some(inner) = curie.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
        return inner.to_string();
    }
    if let Some((pfx, local)) = curie.split_once(':') {
        let local = local.trim_end_matches([',', '.', ';']);
        if let Some(ns) = prefixes.get(pfx) {
            return format!("{ns}{local}");
        }
    }
    curie.to_string()
}

/// Compress a full IRI back to a CURIE via longest-prefix match.
#[must_use]
pub fn curie_for_iri(iri: &str, prefixes: &BTreeMap<String, String>) -> String {
    let mut best: Option<(&String, &String)> = None;
    for (pfx, ns) in prefixes {
        if iri.starts_with(ns.as_str())
            && best.is_none_or(|(_, best_ns): (&String, &String)| ns.len() > best_ns.len())
        {
            best = Some((pfx, ns));
        }
    }
    if let Some((pfx, ns)) = best {
        return format!("{pfx}:{}", &iri[ns.len()..]);
    }
    format!("<{iri}>")
}

/// Normalize a CURIE with the unified Rust semantics: the prefix is
/// preserved lowercased; the local name goes through
/// [`normalize_yago_name`] (Wikidata `_Q\d+` strip, `_UXXXX` decode,
/// `_`→space, lowercase, WS collapse). Full `<...>` IRIs normalize the
/// inner IRI the same way.
#[must_use]
pub fn normalize_curie(curie: &str) -> String {
    let curie = curie.trim();
    if let Some(inner) = curie
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
    {
        // Full IRI: normalize the whole inner string (lowercase,
        // `_UXXXX` decode, `_`→space) like the Python shim — the local-name
        // extraction of `normalize_yago_name` does NOT apply here.
        return format!("<{}>", normalize_extracted_name(inner));
    }
    if curie.contains("://") {
        // Bare full IRI (no angle brackets): same whole-string pipeline,
        // no `<>` wrapping — mirrors the Python bare branch.
        return normalize_extracted_name(curie);
    }
    if let Some((pfx, _)) = curie.split_once(':') {
        return format!("{}:{}", pfx.to_lowercase(), normalize_yago_name(curie));
    }
    normalize_yago_name(curie)
}

/// Collect `(subject_iri, object_iri)` `rdfs:subClassOf` edges from TTL text
/// plus the declared prefix map. Delegates to the crate's single Turtle
/// reader ([`crate::turtle::subclass_edges`]); the former line regex is gone.
#[must_use]
pub fn collect_edges(ttl: &str) -> (BTreeMap<String, String>, Vec<(String, String)>) {
    crate::turtle::subclass_edges(ttl)
}

/// Transitive closure of `subClassOf`: subject → sorted ancestor list.
/// Cycle-safe via memoization + an in-progress stack.
#[must_use]
pub fn transitive_ancestors(direct: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    fn dfs<'a>(
        node: &'a str,
        direct: &'a BTreeMap<String, Vec<String>>,
        memo: &mut BTreeMap<String, BTreeSet<String>>,
        stack: &mut BTreeSet<String>,
    ) -> BTreeSet<String> {
        if let Some(hit) = memo.get(node) {
            return hit.clone();
        }
        if !stack.insert(node.to_string()) {
            return BTreeSet::new();
        }
        let mut out = BTreeSet::new();
        if let Some(parents) = direct.get(node) {
            for parent in parents {
                out.insert(parent.clone());
                out.extend(dfs(parent, direct, memo, stack));
            }
        }
        stack.remove(node);
        memo.insert(node.to_string(), out.clone());
        out
    }

    let mut all: BTreeSet<String> = direct.keys().cloned().collect();
    for parents in direct.values() {
        all.extend(parents.iter().cloned());
    }
    let mut memo = BTreeMap::new();
    let mut out = BTreeMap::new();
    for node in all {
        let mut anc = dfs(&node, direct, &mut memo, &mut BTreeSet::new());
        anc.remove(&node);
        out.insert(node, anc.into_iter().collect());
    }
    out
}

/// YaGO namespace used for the `yago_only` filter.
pub const YAGO_NS: &str = "http://yago-knowledge.org/resource/";

/// Build the `{class -> {parents, ancestors}}` map. Normalized keys merge
/// (Q-suffix collapse); self-references after normalization are dropped.
#[must_use]
pub fn build_json(
    edges: &[(String, String)],
    prefixes: &BTreeMap<String, String>,
    yago_only: bool,
    use_curie: bool,
) -> BTreeMap<String, ClassEntry> {
    let mut direct: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (s, o) in edges {
        direct.entry(s.clone()).or_default().insert(o.clone());
    }
    let direct: BTreeMap<String, Vec<String>> = direct
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect();
    let trans = transitive_ancestors(&direct);
    let yago_ns = prefixes
        .get("yago")
        .map_or(YAGO_NS.to_string(), Clone::clone);

    let name_of = |iri: &str| {
        if use_curie {
            normalize_curie(&curie_for_iri(iri, prefixes))
        } else {
            normalize_curie(iri)
        }
    };

    let mut merged: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
    for (subj, ancestors) in &trans {
        let is_yago = subj.starts_with(&yago_ns) || subj.starts_with(YAGO_NS);
        if yago_only && !is_yago {
            continue;
        }
        let key = name_of(subj);
        let entry = merged.entry(key.clone()).or_default();
        for a in ancestors {
            entry.1.insert(name_of(a));
        }
        if let Some(parents) = direct.get(subj) {
            for p in parents {
                entry.0.insert(name_of(p));
            }
        }
        entry.0.remove(&key);
        entry.1.remove(&key);
    }
    merged
        .into_iter()
        .map(|(k, (parents, ancestors))| {
            (
                k,
                ClassEntry {
                    parents: parents.into_iter().collect(),
                    ancestors: ancestors.into_iter().collect(),
                },
            )
        })
        .collect()
}

/// Convert TTL text to the taxonomy map in one call.
#[must_use]
pub fn taxonomy_from_ttl(
    ttl: &str,
    yago_only: bool,
    use_curie: bool,
) -> BTreeMap<String, ClassEntry> {
    let (prefixes, edges) = collect_edges(ttl);
    build_json(&edges, &prefixes, yago_only, use_curie)
}

/// Serialize the map exactly as the operator JSON: pretty, 2-space indent,
/// sorted keys, trailing newline.
#[must_use]
pub fn to_json_string(map: &BTreeMap<String, ClassEntry>) -> String {
    format!("{}\n", serde_json::to_string_pretty(map).unwrap_or_default())
}

/// Flat mapping: class → ancestors list.
#[must_use]
pub fn to_flat(map: &BTreeMap<String, ClassEntry>) -> BTreeMap<String, Vec<String>> {
    map.iter()
        .map(|(k, v)| (k.clone(), v.ancestors.clone()))
        .collect()
}

// ── Tier prune (Rust owner of `prune_yago_taxonomy.py`) ────────────────────

/// Meta-vocabulary IRIs never survive the prune (mirrors the shim).
fn is_meta_iri(iri: &str) -> bool {
    ["rdf-schema", "rdf-syntax", "owl#", "shacl", "/sh#", "skos", "xsd#"]
        .iter()
        .any(|k| iri.contains(k))
}

/// BFS from roots to `num_tiers`: tier 0 = roots (classes that never appear
/// as a subject, plus children of meta-only parents), tier N = nodes at
/// shortest distance ≤ N from any root. Shortest-distance BFS covers
/// multiple inheritance (a class reachable shallowly via any parent is kept).
#[must_use]
pub fn compute_kept_iris(edges: &[(String, String)], num_tiers: usize) -> BTreeSet<String> {
    let subjects: BTreeSet<&str> = edges.iter().map(|(s, _)| s.as_str()).collect();
    let all: BTreeSet<&str> = edges
        .iter()
        .flat_map(|(s, o)| [s.as_str(), o.as_str()])
        .collect();

    let mut roots: BTreeSet<String> = all
        .difference(&subjects)
        .map(ToString::to_string)
        .collect();
    for (s, o) in edges {
        if is_meta_iri(o) && !is_meta_iri(s) {
            let has_non_meta_parent = edges
                .iter()
                .any(|(subj, obj)| subj == s && obj != o && !is_meta_iri(obj));
            if !has_non_meta_parent {
                roots.insert(s.clone());
            }
        }
    }
    roots.retain(|r| !is_meta_iri(r));
    if roots.is_empty() {
        roots = all.into_iter().take(1).map(ToString::to_string).collect();
    }

    let mut children_of: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (s, o) in edges {
        children_of.entry(o.as_str()).or_default().push(s.as_str());
    }
    let mut depth: BTreeMap<String, usize> = BTreeMap::new();
    let mut queue: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    for r in &roots {
        depth.insert(r.clone(), 0);
        queue.push_back(r.clone());
    }
    let mut kept = BTreeSet::new();
    while let Some(node) = queue.pop_front() {
        let d = depth[&node];
        if d > num_tiers {
            continue;
        }
        kept.insert(node.clone());
        if d == num_tiers {
            continue;
        }
        if let Some(children) = children_of.get(node.as_str()) {
            for ch in children {
                let nd = d + 1;
                if depth.get(*ch).is_none_or(|prev| nd < *prev) {
                    depth.insert((*ch).to_string(), nd);
                    queue.push_back((*ch).to_string());
                }
            }
        }
    }
    kept.extend(roots.iter().cloned());
    if num_tiers == 0 {
        kept = roots;
    }
    kept
}

fn should_keep_line(
    line: &str,
    kept: &BTreeSet<String>,
    prefixes: &BTreeMap<String, String>,
    edges_known: &BTreeSet<String>,
) -> bool {
    let stripped = line.trim();
    if stripped.is_empty() || stripped.starts_with("@prefix") || stripped.starts_with("@base") || stripped.starts_with('#') {
        return true;
    }
    if line.contains("subClassOf") {
        for (subject, object) in crate::turtle::subclass_edges_in_line(line, prefixes) {
            if !(kept.contains(&subject) && kept.contains(&object)) {
                return false;
            }
        }
        return true;
    }
    // `rdf:type rdfs:Class` declarations die with their class.
    if line.contains("rdf:type") && line.contains("rdfs:Class") {
        if let Some(subject) = first_subject_iri(line, prefixes) {
            return kept.contains(&subject);
        }
        return true;
    }
    // Other yago:/schema: subjects die with their class; the rest stays.
    if let Some(subject) = first_subject_iri(line, prefixes) {
        if edges_known.contains(&subject) {
            return kept.contains(&subject);
        }
        if subject.starts_with(YAGO_NS) || subject.starts_with("http://schema.org/") {
            return kept.contains(&subject);
        }
    }
    true
}

/// The line's first subject IRI, expanded through `prefixes`, using the
/// canonical `guidance_rdf` lexer (the former `SUBJ_RE` regex is gone).
fn first_subject_iri(line: &str, prefixes: &BTreeMap<String, String>) -> Option<String> {
    let token = Lexer::new(line).next_token().ok()?;
    match token.kind {
        TokenKind::Iri | TokenKind::PrefixedName => Some(expand_curie(token.value, prefixes)),
        _ => None,
    }
}

/// Prune TTL text to the top `num_tiers` tiers, preserving prefix headers.
#[must_use]
pub fn prune_ttl(ttl: &str, num_tiers: usize) -> String {
    let (prefixes, edges) = collect_edges(ttl);
    let kept = compute_kept_iris(&edges, num_tiers);
    let known: BTreeSet<String> = edges
        .iter()
        .flat_map(|(s, o)| [s.clone(), o.clone()])
        .collect();
    let mut out = String::new();
    for line in ttl.split_inclusive('\n') {
        if should_keep_line(line, &kept, &prefixes, &known) {
            out.push_str(line);
        }
    }
    out
}

#[cfg(test)]
#[path = "../tests/taxonomy.rs"]
mod tests;
