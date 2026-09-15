//! Equivalence + golden coverage for the scan/sort/inference hot paths.
//!
//! Each test compares the production implementation against a local
//! reference copy of the pre-change algorithm (full scan, sorted-`Vec`
//! Kahn core, `contains`-based dependents, whole-graph fixpoint). After the
//! indexed rewrites land, these suites prove byte-identical outputs on
//! randomized inputs plus pinned goldens for diamond/cycle/disconnected
//! shapes.

use bitvec::prelude::*;
use super::dep_graph::DependencyGraph;
use super::target::{Target, TargetRegistry};
use super::type_inference::TypeInference;
use fluent_types::{ExecutorKind, TargetType};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

// ── deterministic RNG (xorshift64*, fixed seed ⇒ reproducible suites) ──

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

// ── fixtures ──

fn mask(rng: &mut Rng, width: usize, density: u64) -> BitVec {
    let mut bv = BitVec::repeat(false, width);
    for i in 0..width {
        if rng.next() % 100 < density {
            bv.set(i, true);
        }
    }
    bv
}

fn make_target(id: i64, provides: BitVec, depends: BitVec) -> Target {
    Target::new()
        .id(id)
        .name(format!("t{id}").into())
        .target_type(TargetType::File)
        .executor(ExecutorKind::Native)
        .depends(depends)
        .provides(provides)
        .build()
}

fn build_registry(rng: &mut Rng, n: usize, width: usize) -> TargetRegistry {
    let mut reg = TargetRegistry::new();
    for i in 0..n {
        reg.register(make_target(
            i as i64,
            mask(rng, width, 35),
            mask(rng, width, 25),
        ))
        .unwrap();
    }
    reg
}

fn names<'a>(targets: Vec<&'a Target>) -> Vec<String> {
    let mut out: Vec<String> = targets.into_iter().map(|t| t.name.to_string()).collect();
    out.sort();
    out
}

// ── reference: pre-change full-scan find_providers ──

fn ref_find_providers(reg: &TargetRegistry, required: &BitVec) -> Vec<String> {
    // Mirrors the old implementation: per-target clone + bit-op superset test.
    // Uniform width in these fixtures, so the bit-op cannot panic here.
    // `list_names` is insertion-ordered, matching the old scan order.
    let mut out = Vec::new();
    for name in reg.list_names() {
        let t = reg.get(&name).unwrap();
        let missing: BitVec = required.clone() & !t.provides.clone();
        if missing.not_any() {
            out.push(t.name.to_string());
        }
    }
    out.sort();
    out
}

// ── reference: pre-change sorted-Vec Kahn core ──

fn ref_kahn<K: Ord + Clone + Hash>(
    in_degree: &mut HashMap<K, usize>,
    adj: &HashMap<K, Vec<K>>,
    total: usize,
) -> Result<Vec<K>, Vec<K>> {
    let mut queue: Vec<K> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(k, _)| k.clone())
        .collect();
    queue.sort_unstable();
    let mut order = Vec::with_capacity(total);
    let mut head = 0;
    while head < queue.len() {
        let current = queue[head].clone();
        head += 1;
        order.push(current.clone());
        if let Some(dependents) = adj.get(&current) {
            for dep in dependents {
                if let Some(deg) = in_degree.get_mut(dep) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push(dep.clone());
                        queue[head..].sort_unstable();
                    }
                }
            }
        }
    }
    if order.len() != total {
        return Err(order);
    }
    Ok(order)
}

/// Rebuild `topo_sort`'s edge derivation from the public accessors, then run
/// the reference Kahn core. Returns the same shape as `topo_sort`: `Ok`
/// order or `Err` leftover set.
fn ref_topo_sort(nodes: &[String], deps: &HashMap<String, Vec<String>>) -> Result<Vec<String>, HashSet<String>> {
    let mut in_degree: HashMap<String, usize> =
        nodes.iter().map(|n| (n.clone(), 0)).collect();
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    // Edge P → N when N depends on an asset P provides. Fixtures below use
    // one asset per provider named `provides:<node>`, depended on by name.
    for node in nodes {
        if let Some(ds) = deps.get(node) {
            for d in ds {
                if d == node {
                    continue;
                }
                if in_degree.contains_key(d) {
                    adj.entry(d.clone()).or_default().push(node.clone());
                    *in_degree.get_mut(node).unwrap() += 1;
                }
            }
        }
    }
    match ref_kahn(&mut in_degree, &adj, nodes.len()) {
        Ok(order) => Ok(order),
        Err(partial) => {
            let in_partial: HashSet<&String> = partial.iter().collect();
            Err(nodes
                .iter()
                .filter(|n| !in_partial.contains(n))
                .cloned()
                .collect())
        }
    }
}

// ── reference: pre-change contains-based direct_dependents ──

fn ref_direct_dependents(
    nodes: &[String],
    provides: &HashMap<String, Vec<String>>,
    deps: &HashMap<String, Vec<String>>,
    node: &str,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(pv) = provides.get(node) {
        for (other, other_deps) in deps {
            if other != node
                && other_deps.iter().any(|d| pv.contains(d))
                && !out.contains(other)
            {
                out.push(other.clone());
            }
        }
    }
    let _ = nodes;
    out.sort();
    out
}

// ── reference: pre-change whole-graph fixpoint closure ──

fn ref_closure(class_ids: &[i64], edges: &[[i64; 2]]) -> HashMap<i64, BitVec> {
    let class_count = class_ids.len();
    let mut id_to_bit: HashMap<i64, usize> = HashMap::new();
    for (i, &id) in class_ids.iter().enumerate() {
        id_to_bit.insert(id, i);
    }
    let mut ancestors: HashMap<i64, BitVec> = HashMap::new();
    for &id in class_ids {
        let mut bs = BitVec::repeat(false, class_count);
        if let Some(&bit) = id_to_bit.get(&id) {
            bs.set(bit, true);
        }
        ancestors.insert(id, bs);
    }
    let mut changed = true;
    while changed {
        changed = false;
        let mut updates: Vec<(i64, BitVec)> = Vec::new();
        for &[child, parent] in edges {
            if let (Some(&pb), Some(child_anc)) =
                (id_to_bit.get(&parent), ancestors.get(&child))
            {
                let parent_anc = ancestors.get(&parent).cloned().unwrap_or_else(BitVec::new);
                if !child_anc[pb]
                    || parent_anc
                        .iter()
                        .enumerate()
                        .any(|(i, b)| *b && !child_anc[i])
                {
                    let mut new_bits = child_anc.clone();
                    new_bits.set(pb, true);
                    for (i, bit) in parent_anc.iter().enumerate() {
                        if *bit && !new_bits[i] {
                            new_bits.set(i, true);
                            changed = true;
                        }
                    }
                    if new_bits != *child_anc {
                        changed = true;
                    }
                    updates.push((child, new_bits));
                }
            }
        }
        for (id, bits) in updates {
            ancestors.insert(id, bits);
        }
    }
    ancestors
}

fn closure_matrix(closure: &HashMap<i64, BitVec>, ids: &[i64], id_to_bit: &HashMap<i64, usize>) -> Vec<(i64, i64)> {
    let mut pairs = Vec::new();
    for &child in ids {
        for &parent in ids {
            if let (Some(anc), Some(&pb)) = (closure.get(&child), id_to_bit.get(&parent)) {
                if anc[pb] {
                    pairs.push((child, parent));
                }
            }
        }
    }
    pairs.sort();
    pairs
}

// ── M6.1: find_providers equivalence ──

#[test]
fn find_providers_matches_reference_on_random_registries() {
    for seed in 1..=40u64 {
        let mut rng = Rng(seed);
        let n = 3 + rng.below(14);
        let reg = build_registry(&mut rng, n, 6);
        for _ in 0..6 {
            let required = mask(&mut rng, 6, 40);
            assert_eq!(
                names(reg.find_providers(&required)),
                ref_find_providers(&reg, &required),
                "seed {seed}"
            );
        }
        // Vacuous case: empty requirement matches every target.
        assert_eq!(reg.find_providers(&BitVec::repeat(false, 6)).len(), n);
    }
}

/// Width-mismatch requirements apply strict superset semantics: a required
/// bit outside every provides vector disqualifies (matches nothing).
///
/// This pins an intentional fix: the old bit-op (`required.clone() &
/// !prov.clone()`) truncated to the shorter width, silently dropping
/// out-of-range required bits and matching targets that do not provide
/// them. The indexed implementation tests membership per required bit, so
/// unsatisfiable bits correctly yield an empty set — and it can never
/// panic on short vectors.
#[test]
fn find_providers_width_mismatch_is_strict() {
    let mut rng = Rng(7);
    let reg = build_registry(&mut rng, 8, 6);
    let mut wide = BitVec::repeat(false, 7);
    wide.set(6, true);
    assert!(reg.find_providers(&wide).is_empty());
    // A wide requirement whose in-range bits are satisfiable still matches
    // (only the out-of-range bit disqualifies when set).
    let wide_ok = BitVec::repeat(false, 7);
    assert_eq!(
        reg.find_providers(&wide_ok).len(),
        reg.find_providers(&BitVec::repeat(false, 6)).len()
    );
}

#[test]
fn find_providers_insertion_order_and_subset_semantics() {
    let mut reg = TargetRegistry::new();
    reg.register(make_target(
        10,
        bitvec::bitvec![1, 1, 0],
        BitVec::repeat(false, 3),
    ))
    .unwrap();
    reg.register(make_target(
        11,
        bitvec::bitvec![1, 0, 0],
        BitVec::repeat(false, 3),
    ))
    .unwrap();
    reg.register(make_target(
        12,
        bitvec::bitvec![1, 1, 1],
        BitVec::repeat(false, 3),
    ))
    .unwrap();
    // Require bits {0,1}: t10 and t12 match, in registration order.
    let got: Vec<String> = reg
        .find_providers(&bitvec::bitvec![1, 1, 0])
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    assert_eq!(got, vec!["t10".to_string(), "t12".to_string()]);
}

// ── M6.2: Kahn heap equivalence + goldens ──

fn dep_graph_fixture(rng: &mut Rng, n: usize) -> (DependencyGraph<String>, Vec<String>, HashMap<String, Vec<String>>) {
    // Acyclic-by-construction with probability ~1: node i depends only on
    // assets provided by lower-indexed nodes; plus occasional cross edges
    // that may introduce cycles (both impls must agree on those too).
    let mut g: DependencyGraph<String> = DependencyGraph::new();
    let mut nodes = Vec::new();
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    for i in 0..n {
        let name = format!("n{i:02}");
        let mut d = Vec::new();
        for j in 0..i {
            if rng.next() % 100 < 22 {
                d.push(format!("n{j:02}"));
            }
        }
        if n > 1 && rng.next() % 100 < 6 {
            d.push(format!("n{:02}", rng.below(n)));
        }
        // Each node provides an asset named after itself; depends on the
        // provider names directly (single-provider assets keep edge
        // derivation unambiguous between reference and implementation).
        let provides = vec![name.clone()];
        g.register(&name, &d, &provides).unwrap();
        deps.insert(name.clone(), d);
        nodes.push(name);
    }
    (g, nodes, deps)
}

#[test]
fn kahn_matches_reference_on_random_graphs() {
    for seed in 1..=40u64 {
        let mut rng = Rng(seed);
        let n = 2 + rng.below(10);
        let (g, nodes, deps) = dep_graph_fixture(&mut rng, n);
        let reference = ref_topo_sort(&nodes, &deps);
        match (g.topo_sort(), reference) {
            (Ok(got), Ok(want)) => assert_eq!(got, want, "seed {seed}"),
            (Err(got), Err(want)) => {
                // Leftover sets must agree exactly. The display form is
                // `dependency cycle detected involving: "n00", "n02"`.
                let msg = got.to_string();
                let body = msg
                    .strip_prefix("dependency cycle detected involving: ")
                    .unwrap_or(&msg);
                let got_set: HashSet<String> = body
                    .split(", ")
                    .map(|s| s.trim_matches('"').to_string())
                    .collect();
                assert_eq!(got_set, want, "seed {seed}");
            }
            (got, want) => panic!("seed {seed}: impl {got:?} vs ref {want:?}"),
        }
    }
}

#[test]
fn topo_sort_goldens_diamond_disconnected_cycle() {
    // Diamond: a → {b, c} → d.
    let mut g: DependencyGraph<&str> = DependencyGraph::new();
    g.register(&"a", &[], &["a"]).unwrap();
    g.register(&"b", &["a"], &["b"]).unwrap();
    g.register(&"c", &["a"], &["c"]).unwrap();
    g.register(&"d", &["b", "c"], &["d"]).unwrap();
    assert_eq!(g.topo_sort().unwrap(), vec!["a", "b", "c", "d"]);

    // Disconnected: ties break by Ord.
    let mut h: DependencyGraph<&str> = DependencyGraph::new();
    h.register(&"z", &[], &["z"]).unwrap();
    h.register(&"m", &[], &["m"]).unwrap();
    assert_eq!(h.topo_sort().unwrap(), vec!["m", "z"]);

    // Cycle: both members reported.
    let mut c: DependencyGraph<&str> = DependencyGraph::new();
    c.register(&"x", &["y"], &["x"]).unwrap();
    c.register(&"y", &["x"], &["y"]).unwrap();
    let err = c.topo_sort().unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains('x') && msg.contains('y'), "got: {msg}");
}

// ── M6.3: direct_dependents equivalence ──

#[test]
fn direct_dependents_matches_reference_on_random_graphs() {
    for seed in 1..=30u64 {
        let mut rng = Rng(seed);
        let n = 2 + rng.below(10);
        let (g, nodes, deps) = dep_graph_fixture(&mut rng, n);
        let provides: HashMap<String, Vec<String>> = nodes
            .iter()
            .map(|nd| (nd.clone(), vec![nd.clone()]))
            .collect();
        for node in &nodes {
            assert_eq!(
                g.direct_dependents(node),
                ref_direct_dependents(&nodes, &provides, &deps, node),
                "seed {seed} node {node}"
            );
        }
    }
}

// ── M6.4: type-inference equivalence (DAGs, cycles, self-loops, dup ids) ──

fn subclass_matrix(ti: &TypeInference, ids: &[i64]) -> Vec<(i64, i64)> {
    let mut pairs = Vec::new();
    for &c in ids {
        for &p in ids {
            if ti.is_subclass_of(c, p) {
                pairs.push((c, p));
            }
        }
    }
    pairs.sort();
    pairs
}

#[test]
fn type_inference_matches_fixpoint_on_random_graphs() {
    for seed in 1..=40u64 {
        let mut rng = Rng(seed);
        let n = 2 + rng.below(8);
        let ids: Vec<i64> = (0..n as i64).collect();
        let mut edges: Vec<[i64; 2]> = Vec::new();
        for _ in 0..(n * 2) {
            // Arbitrary edges (DAG-biased but cycles/self-loops possible —
            // the scoped-leftover path must agree with the full fixpoint).
            edges.push([rng.below(n) as i64, rng.below(n) as i64]);
        }
        let ti = TypeInference::build(&ids, &edges);
        let reference = ref_closure(&ids, &edges);
        let id_to_bit: HashMap<i64, usize> =
            ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        assert_eq!(
            subclass_matrix(&ti, &ids),
            closure_matrix(&reference, &ids, &id_to_bit),
            "seed {seed}"
        );
    }
}

#[test]
fn type_inference_goldens_chain_diamond_cycle() {
    // Chain 3 → 2 → 1.
    let ti = TypeInference::build(&[1, 2, 3], &[[2, 1], [3, 2]]);
    assert!(ti.is_subclass_of(3, 1));
    assert!(ti.is_subclass_of(3, 3));
    assert!(!ti.is_subclass_of(1, 3));
    // Diamond 4 → {2, 3} → 1.
    let diamond = TypeInference::build(&[1, 2, 3, 4], &[[2, 1], [3, 1], [4, 2], [4, 3]]);
    assert!(diamond.is_subclass_of(4, 1));
    assert!(!diamond.is_subclass_of(2, 3));
    // Two-cycle 5 ↔ 6: mutual reachability, no hang.
    let cyc = TypeInference::build(&[5, 6], &[[5, 6], [6, 5]]);
    assert!(cyc.is_subclass_of(5, 6) && cyc.is_subclass_of(6, 5));
    // Unknown ids stay false.
    assert!(!cyc.is_subclass_of(5, 99) && !cyc.is_subclass_of(99, 5));
}
