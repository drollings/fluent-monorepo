//! Equivalence between [`crate::taxonomy::transitive_ancestors`] and the shared
//! [`fluent_dag::hierarchy::HierarchyIndex`] closure primitive: ancestor
//! *sets* must agree (precision and recall exactly 1.0) on every fixture,
//! and cyclic inputs must terminate.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use fluent_dag::hierarchy::HierarchyIndex;

fn direct(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
    pairs
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                v.iter().map(ToString::to_string).collect(),
            )
        })
        .collect()
}

/// Independent oracle: same `direct` map through the shared
/// `HierarchyIndex` primitive, normalized to sorted vecs.
fn oracle(direct: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    let index = HierarchyIndex::from_parent_map(
        direct
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    let mut all: BTreeSet<String> = direct.keys().cloned().collect();
    for parents in direct.values() {
        all.extend(parents.iter().cloned());
    }
    all.into_iter()
        .map(|node| {
            let mut anc = index.ancestors(&node);
            anc.sort();
            (node, anc)
        })
        .collect()
}

fn assert_equivalent(direct: &BTreeMap<String, Vec<String>>) {
    let got = transitive_ancestors(direct);
    let want = oracle(direct);
    assert_eq!(got.keys().collect::<Vec<_>>(), want.keys().collect::<Vec<_>>());
    for (node, want_anc) in &want {
        let got_anc = &got[node];
        let got_set: BTreeSet<_> = got_anc.iter().collect();
        let want_set: BTreeSet<_> = want_anc.iter().collect();
        let tp = got_set.intersection(&want_set).count() as f64;
        let precision = if got_set.is_empty() { 1.0 } else { tp / got_set.len() as f64 };
        let recall = if want_set.is_empty() { 1.0 } else { tp / want_set.len() as f64 };
        assert_eq!((precision, recall), (1.0, 1.0), "closure mismatch at {node}");
        assert_eq!(got_anc, want_anc, "ancestor order mismatch at {node}");
    }
}

#[test]
fn equivalence_diamond() {
    assert_equivalent(&direct(&[
        ("a", &["b", "c"]),
        ("b", &["d"]),
        ("c", &["d"]),
        ("d", &[]),
    ]));
}

#[test]
fn equivalence_cycle_terminates() {
    assert_equivalent(&direct(&[("a", &["b"]), ("b", &["a"])]));
}

#[test]
fn equivalence_longer_cycle_terminates() {
    // NOTE: this case FAILS against the pre-migration memoized DFS (recall
    // 0.5 at `c`): the old in-progress-stack poisoned the memo with a partial
    // closure (`c -> {a}`, missing `b`). The shared primitive computes the
    // complete cycle-resilient closure, so the migration fixes it. The
    // corrected closure is pinned below; the migration must produce it.
    let d = direct(&[("a", &["b"]), ("b", &["c"]), ("c", &["a"])]);
    assert_equivalent(&d);
}

#[test]
fn pinned_closures_diamond_and_cycles() {
    // Implementation-independent pins: the correct closure for these shapes,
    // regardless of which code computes it.
    let diamond = transitive_ancestors(&direct(&[
        ("a", &["b", "c"]),
        ("b", &["d"]),
        ("c", &["d"]),
        ("d", &[]),
    ]));
    assert_eq!(diamond["a"], vec!["b", "c", "d"]);
    assert_eq!(diamond["b"], vec!["d"]);
    assert_eq!(diamond["c"], vec!["d"]);
    assert_eq!(diamond["d"], Vec::<String>::new());

    let two_cycle = transitive_ancestors(&direct(&[("a", &["b"]), ("b", &["a"])]));
    assert_eq!(two_cycle["a"], vec!["b"]);
    assert_eq!(two_cycle["b"], vec!["a"]);

    let three_cycle = transitive_ancestors(&direct(&[
        ("a", &["b"]),
        ("b", &["c"]),
        ("c", &["a"]),
    ]));
    assert_eq!(three_cycle["a"], vec!["b", "c"]);
    assert_eq!(three_cycle["b"], vec!["a", "c"]);
    assert_eq!(three_cycle["c"], vec!["a", "b"]);
}

#[test]
fn equivalence_multi_parent_with_parent_only_node() {
    // `root` never appears as a key; `lone` has no parents at all.
    assert_equivalent(&direct(&[
        ("c", &["a", "b"]),
        ("a", &["root"]),
        ("b", &["root"]),
        ("lone", &[]),
    ]));
}

#[test]
fn equivalence_empty_and_singleton() {
    assert_equivalent(&BTreeMap::new());
    assert_equivalent(&direct(&[("only", &[])]));
}

#[test]
fn equivalence_fixture_ttl_edges() {
    let ttl = std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("yago_mini.ttl"),
    )
    .expect("fixture TTL readable");
    let (_, edges) = collect_edges(&ttl);
    let mut direct: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (s, o) in &edges {
        direct.entry(s.clone()).or_default().insert(o.clone());
    }
    let direct: BTreeMap<String, Vec<String>> = direct
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect();
    assert_equivalent(&direct);
}
