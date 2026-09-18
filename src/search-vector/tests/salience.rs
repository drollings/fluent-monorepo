use super::*;

// Moved verbatim with `pagerank` from `fluent-router`'s `tests/ranking.rs`
// (M7.2) — the centrality kernel's characterization lives with the kernel.

#[test]
#[allow(clippy::float_cmp)]
fn pagerank_converges_on_a_star() {
    // 4-node star: node 0 is the hub.
    let mut graph: NodeGraph = BTreeMap::new();
    graph.insert(0, vec![1, 2, 3]);
    graph.insert(1, vec![0]);
    graph.insert(2, vec![0]);
    graph.insert(3, vec![0]);
    let ranks = pagerank(&graph, PAGERANK_ITERATIONS, PAGERANK_ALPHA);
    assert_eq!(ranks.len(), 4);
    assert!(ranks[&0] > ranks[&1], "hub outranks a leaf");
    assert!(
        ranks[&0] >= 0.999,
        "hub normalizes near 1.0, got {}",
        ranks[&0]
    );
    assert!(ranks[&1] < 1.0);
    // Deterministic: a second run is bit-identical.
    let again = pagerank(&graph, PAGERANK_ITERATIONS, PAGERANK_ALPHA);
    assert_eq!(ranks, again);
}

#[test]
#[allow(clippy::float_cmp)]
fn pagerank_is_well_defined_on_empty_and_single_nodes() {
    assert_eq!(pagerank(&NodeGraph::new(), 3, PAGERANK_ALPHA).len(), 0);
    let mut graph = NodeGraph::new();
    graph.insert(7, vec![]);
    let ranks = pagerank(&graph, 3, PAGERANK_ALPHA);
    assert_eq!(ranks[&7], 1.0, "single node normalizes to 1.0");
}
