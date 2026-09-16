use super::*;

// ── Shape goldens (the five migrated closure sites share these shapes) ──

#[test]
fn chain_is_nearest_first() {
    // Animal ← Mammal ← Dog ← Poodle, as `(child, parent)` edges.
    let index = HierarchyIndex::from_edges([(2, 1), (3, 2), (4, 3)]);
    assert_eq!(index.ancestors(&4), vec![3, 2, 1]);
    assert!(index.is_subclass_of(&4, &1));
    assert!(index.is_subclass_of(&4, &4));
    assert!(!index.is_subclass_of(&1, &4));
}

#[test]
fn diamond_is_nearest_first() {
    // 4 → {2, 3} → 1.
    let index = HierarchyIndex::from_edges([(2, 1), (3, 1), (4, 2), (4, 3)]);
    // Nearest layer first (2, 3 in edge order), then the shared root.
    assert_eq!(index.ancestors(&4), vec![2, 3, 1]);
    assert!(index.is_subclass_of(&4, &1));
    assert!(!index.is_subclass_of(&2, &3));
}

#[test]
fn cycle_terminates_and_is_mutual() {
    let index = HierarchyIndex::from_edges([(5, 6), (6, 5)]);
    assert!(index.is_subclass_of(&5, &6) && index.is_subclass_of(&6, &5));
    assert!(index.ancestors(&5).contains(&6));
}

#[test]
fn descendants_are_nearest_first() {
    let index = HierarchyIndex::from_edges([(2, 1), (3, 1), (4, 2)]);
    assert_eq!(index.descendants(&1), vec![2, 3, 4]);
    assert_eq!(index.descendants(&2), vec![4]);
    assert!(index.descendants(&4).is_empty());
}

// ── Constructors ──

#[test]
fn empty_index() {
    let index: HierarchyIndex<i64> = HierarchyIndex::new();
    assert!(index.is_empty());
    assert_eq!(index.len(), 0);
    assert!(index.ancestors(&1).is_empty());
}

#[test]
fn parent_only_node_is_queryable() {
    let index = HierarchyIndex::from_edges([(10, 11)]);
    assert!(index.contains(&10) && index.contains(&11));
    assert!(index.is_subclass_of(&10, &11));
    assert!(index.ancestors(&11).is_empty());
    assert!(index.parents_of(&11).is_empty());
}

#[test]
fn unknown_node_has_empty_closure() {
    let index = HierarchyIndex::from_edges([(1, 2)]);
    assert!(!index.contains(&99));
    assert!(index.ancestors(&99).is_empty());
    assert!(index.descendants(&99).is_empty());
    assert!(!index.is_subclass_of(&99, &1));
    // Identity is still true for the unregistered node (store semantics).
    assert!(index.is_subclass_of(&99, &99));
}

#[test]
fn from_parent_map_registers_isolated_key() {
    let mut parents: HashMap<i64, Vec<i64>> = HashMap::new();
    parents.insert(3, vec![2]);
    parents.insert(2, vec![1]);
    parents.insert(7, vec![]);
    let index = HierarchyIndex::from_parent_map(parents);
    assert_eq!(index.ancestors(&3), vec![2, 1]);
    assert!(index.contains(&7));
    assert!(index.ancestors(&7).is_empty());
}

#[test]
fn from_dependency_graph_reads_deps_as_children() {
    // parent depends on its children, self-provides (concept-store encoding).
    let mut graph: DependencyGraph<i64> = DependencyGraph::new();
    graph.register(&1, &[2, 3], &[1]).unwrap();
    graph.register(&2, &[4], &[2]).unwrap();
    graph.register(&4, &[], &[4]).unwrap();
    graph.register(&3, &[], &[3]).unwrap();
    let index = HierarchyIndex::from_dependency_graph(&graph);
    assert_eq!(index.ancestors(&4), vec![2, 1]);
    assert!(index.is_subclass_of(&3, &1));
}

#[test]
fn add_edge_extends_the_memoized_closure() {
    let mut index = HierarchyIndex::from_edges([(2, 1)]);
    assert!(!index.is_subclass_of(&3, &1));
    index.add_edge(&3, &2);
    assert_eq!(index.ancestors(&3), vec![2, 1]);
    index.add_node(50);
    assert!(index.contains(&50));
}

// ── Cycle-handling parity with the whole-graph fixpoint ──

/// Deterministic xorshift64* (fixed seed ⇒ reproducible suite).
struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: u32) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0 = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (self.0 % u64::from(bound)) as u32
    }
}

fn reference_closure(registered: &HashSet<u32>, edges: &[(u32, u32)]) -> HashSet<(u32, u32)> {
    let mut known: HashSet<(u32, u32)> = edges
        .iter()
        .copied()
        .filter(|(c, p)| registered.contains(c) && registered.contains(p))
        .collect();
    loop {
        let snapshot: Vec<(u32, u32)> = known.iter().copied().collect();
        let mut grew = false;
        for &(a, b) in &snapshot {
            for &(c, d) in &snapshot {
                if b == c && known.insert((a, d)) {
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    known
}

#[test]
fn is_subclass_of_matches_fixpoint_on_random_graphs() {
    for seed in 1..=40u64 {
        let mut rng = Rng(seed);
        let n = 2 + rng.below(8);
        let registered: HashSet<u32> = (0..n).collect();
        let mut edges: Vec<(u32, u32)> = Vec::new();
        for _ in 0..(n * 2) {
            edges.push((rng.below(n), rng.below(n)));
        }
        let index = HierarchyIndex::from_edges(edges.iter().copied());
        let reference = reference_closure(&registered, &edges);
        for c in 0..n {
            for p in 0..n {
                let expected = c == p || reference.contains(&(c, p));
                assert_eq!(index.is_subclass_of(&c, &p), expected, "seed {seed} ({c},{p})");
            }
        }
    }
}

// ── Per-site parity goldens ──

#[test]
fn concept_store_chain_golden() {
    // TaxonomyHierarchy/YagoView shallow taxonomy: nearest-first ancestors.
    let index = HierarchyIndex::from_edges([(2, 1), (3, 2), (4, 3)]);
    assert_eq!(index.ancestors(&4), vec![3, 2, 1]);
    // YagoView n2: Person → Thing.
    let yago = HierarchyIndex::from_edges([("schema:Person", "schema:Thing")]);
    assert!(yago.is_subclass_of(&"schema:Person", &"schema:Thing"));
}

#[test]
fn static_superclass_chain_golden() {
    // yago.rs `superclass_chain`: self-first, then ancestors.
    let index = HierarchyIndex::from_edges([
        ("http://schema.org/Person", "http://yago-knowledge.org/resource/Entity"),
        ("http://schema.org/Organization", "http://yago-knowledge.org/resource/Entity"),
    ]);
    let mut chain = vec!["http://schema.org/Person"];
    chain.extend(index.ancestors(&"http://schema.org/Person").iter().copied());
    assert_eq!(chain[1], "http://yago-knowledge.org/resource/Entity");
}
