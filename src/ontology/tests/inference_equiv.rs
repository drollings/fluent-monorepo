//! Equivalence coverage for the subclass-transitivity rewrite: the
//! `TypeInference`-backed implementation must derive the same triple SET as
//! the pre-change fixpoint on chains, diamonds, cycles, duplicates, and
//! randomized DAGs. Comparisons are order-normalized (sorted subject/object
//! pairs) — the rewrite emits deterministic sorted order.

use super::*;

fn triple(s: &str, p: &str, o: &str) -> Triple {
    Triple {
        subject: Term::Iri(s.to_string()),
        predicate: Term::Iri(p.to_string()),
        object: Term::Iri(o.to_string()),
    }
}

const SUB: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

fn engine() -> InferenceEngine {
    let mut e = InferenceEngine::new();
    e.add_rule(InferenceRule {
        rule_type: RuleType::SubclassTransitivity,
        trigger_predicate: SUB.to_string(),
    });
    e
}

fn derived_set(derived: &[Triple]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = derived
        .iter()
        .map(|t| {
            (
                triple_subject_iri(t).unwrap_or_default(),
                triple_object_iri(t).unwrap_or_default(),
            )
        })
        .collect();
    pairs.sort();
    pairs.dedup();
    pairs
}

/// Local naive reference: the pre-change whole-graph fixpoint over string
/// edge pairs (kept as the oracle for the property test).
fn naive_closure(base: &[(String, String)]) -> Vec<(String, String)> {
    let mut known: std::collections::HashSet<(String, String)> =
        base.iter().cloned().collect();
    loop {
        let edges: Vec<(String, String)> = known.iter().cloned().collect();
        let mut grown = false;
        for (a, b) in &edges {
            for (c, d) in &edges {
                if b == c && !known.contains(&(a.clone(), d.clone())) {
                    known.insert((a.clone(), d.clone()));
                    grown = true;
                }
            }
        }
        if !grown {
            break;
        }
    }
    let base_set: std::collections::HashSet<(String, String)> =
        base.iter().cloned().collect();
    let mut derived: Vec<(String, String)> =
        known.difference(&base_set).cloned().collect();
    derived.sort();
    derived
}

#[test]
fn equiv_diamond() {
    let base = vec![
        triple("B", SUB, "A"),
        triple("C", SUB, "A"),
        triple("D", SUB, "B"),
        triple("D", SUB, "C"),
    ];
    let got = derived_set(&engine().infer(&base).unwrap());
    assert_eq!(
        got,
        vec![
            ("D".to_string(), "A".to_string()),
        ]
    );
}

#[test]
fn equiv_cycle_reaches_fixpoint() {
    let base = vec![
        triple("X", SUB, "Y"),
        triple("Y", SUB, "X"),
        triple("Z", SUB, "X"),
    ];
    let got = derived_set(&engine().infer(&base).unwrap());
    // X↔Y mutual + reflexive pairs from the cycle, Z→Y via Z→X→Y, Z→Z.
    assert!(got.contains(&("X".to_string(), "X".to_string())));
    assert!(got.contains(&("Z".to_string(), "Y".to_string())));
}

#[test]
fn equiv_non_subclass_predicates_ignored() {
    let other = "http://example.org/relatedTo";
    let base = vec![
        triple("A", SUB, "B"),
        triple("B", other, "C"),
        triple("A", other, "C"),
    ];
    let got = derived_set(&engine().infer(&base).unwrap());
    assert!(got.is_empty());
}

#[test]
fn equiv_duplicate_edges_derive_once() {
    let base = vec![
        triple("A", SUB, "B"),
        triple("A", SUB, "B"),
        triple("B", SUB, "C"),
        triple("B", SUB, "C"),
    ];
    let got = derived_set(&engine().infer(&base).unwrap());
    assert_eq!(got, vec![("A".to_string(), "C".to_string())]);
}

#[test]
fn equiv_matches_naive_fixpoint_on_random_graphs() {
    // Deterministic xorshift64* (fixed seed ⇒ reproducible suite).
    let mut state: u64 = 0x1234_5678_9ABC_DEF0;
    let mut next = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        state
    };
    for _ in 0..30 {
        let n = 2 + (next() % 7) as usize;
        let names: Vec<String> = (0..n).map(|i| format!("C{i}")).collect();
        let mut base: Vec<Triple> = Vec::new();
        let mut pairs: Vec<(String, String)> = Vec::new();
        for _ in 0..(n * 2) {
            let a = names[(next() % n as u64) as usize].clone();
            let b = names[(next() % n as u64) as usize].clone();
            base.push(triple(&a, SUB, &b));
            pairs.push((a, b));
        }
        let want = naive_closure(&pairs);
        let got = derived_set(&engine().infer(&base).unwrap());
        assert_eq!(got, want);
    }
}
