//! The one hierarchy-closure index for the workspace.
//!
//! Subclass/ancestor closure was historically derived independently in the
//! ontology, concept-store, and type-inference layers — against a
//! `DependencyGraph`, a parent map, TTL edges, and CSR arrays. Those layers now
//! compose the single, concrete [`HierarchyIndex`] below rather than
//! re-implementing transitive closure. It is a concrete type, not a trait:
//! there is no second backend that would justify the indirection.
//!
//! Edges are `(child, parent)` pairs — `child` is a subclass of `parent`.
//! Queries are:
//!
//! - **memoized** — the first walk over a node caches its closure;
//! - **cycle-resilient** — a back-edge is visited once and the walk
//!   terminates, so a dependency cycle yields a finite (possibly partial)
//!   result instead of hanging;
//! - **deterministic** — breadth-first, nearest-first, with sibling order
//!   fixed by edge/insertion order.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, RwLock};

use common_core::sync::{lock_read, lock_write};

use crate::dep_graph::DependencyGraph;

/// A precomputed, lazily-memoized transitive-closure index over a class
/// hierarchy keyed by `K`.
///
/// Build one from `(child, parent)` edges ([`from_edges`](Self::from_edges)),
/// a child → parents map ([`from_parent_map`](Self::from_parent_map)), or a
/// `DependencyGraph` whose `deps` encode a node's children
/// ([`from_dependency_graph`](Self::from_dependency_graph)). Query with
/// [`ancestors`](Self::ancestors), [`descendants`](Self::descendants), and
/// [`is_subclass_of`](Self::is_subclass_of).
///
/// Every node registered during construction (including a node that appears
/// only as a parent, or an isolated node added with
/// [`add_node`](Self::add_node)) is queryable. Queries on unregistered nodes
/// return empty/`false` — except `is_subclass_of(x, x)`, which is identity
/// and therefore always `true` (matching the stores' `ConceptStore`
/// semantics).
#[derive(Debug)]
pub struct HierarchyIndex<K: Eq + Hash + Clone> {
    /// child → direct parents.
    parents: HashMap<K, Vec<K>>,
    /// parent → direct children.
    children: HashMap<K, Vec<K>>,
    /// Memoized ancestor closures (excludes the queried node itself).
    ancestor_memo: RwLock<HashMap<K, Arc<[K]>>>,
    /// Memoized descendant closures (excludes the queried node itself).
    descendant_memo: RwLock<HashMap<K, Arc<[K]>>>,
}

impl<K: Eq + Hash + Clone> Default for HierarchyIndex<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Eq + Hash + Clone> HierarchyIndex<K> {
    /// An empty index.
    #[must_use]
    pub fn new() -> Self {
        Self {
            parents: HashMap::new(),
            children: HashMap::new(),
            ancestor_memo: RwLock::new(HashMap::new()),
            descendant_memo: RwLock::new(HashMap::new()),
        }
    }

    /// Build from `(child, parent)` edges. Both endpoints are registered, so a
    /// parent-only node is still queryable.
    pub fn from_edges(edges: impl IntoIterator<Item = (K, K)>) -> Self {
        let mut index = Self::new();
        for (child, parent) in edges {
            index.add_edge(&child, &parent);
        }
        index
    }

    /// Build from a child → direct-parents map. Every map key is registered
    /// even when its parent list is empty (an isolated class).
    pub fn from_parent_map(parents: HashMap<K, Vec<K>>) -> Self {
        let mut index = Self::new();
        for (child, direct_parents) in parents {
            index.add_node(child.clone());
            for parent in direct_parents {
                index.add_edge(&child, &parent);
            }
        }
        index
    }

    /// Build from a [`DependencyGraph`] whose convention is *a node's `deps`
    /// are its children* (the encoding the concept-store hierarchy uses: each
    /// parent depends on its children and self-provides). For each node `n`
    /// and each `c` in `deps_of(n)`, the edge `(c, n)` is added.
    pub fn from_dependency_graph(graph: &DependencyGraph<K>) -> Self {
        let mut index = Self::new();
        for node in graph.nodes() {
            index.add_node(node.clone());
            if let Some(node_children) = graph.deps_of(node) {
                for child in node_children {
                    index.add_edge(child, node);
                }
            }
        }
        index
    }

    /// Register `node` as queryable even when it has no edges.
    pub fn add_node(&mut self, node: K) {
        self.parents.entry(node.clone()).or_default();
        self.children.entry(node).or_default();
    }

    /// Add one `(child, parent)` subclass edge. Duplicate edges are ignored.
    /// Clears the memoized closures — an edge can extend any descendant's or
    /// ancestor's closure.
    pub fn add_edge(&mut self, child: &K, parent: &K) {
        self.add_node(child.clone());
        self.add_node(parent.clone());
        if let Some(direct_parents) = self.parents.get_mut(child) {
            if !direct_parents.contains(parent) {
                direct_parents.push(parent.clone());
            }
        }
        if let Some(direct_children) = self.children.get_mut(parent) {
            if !direct_children.contains(child) {
                direct_children.push(child.clone());
            }
        }
        lock_write(&self.ancestor_memo).clear();
        lock_write(&self.descendant_memo).clear();
    }

    /// Direct parents of `node`, in edge-insertion order. Empty for an
    /// unregistered node.
    #[must_use]
    pub fn parents_of(&self, node: &K) -> &[K] {
        match self.parents.get(node) {
            Some(direct) => direct.as_slice(),
            None => &[],
        }
    }

    /// Direct children of `node`, in edge-insertion order. Empty for an
    /// unregistered node.
    #[must_use]
    pub fn children_of(&self, node: &K) -> &[K] {
        match self.children.get(node) {
            Some(direct) => direct.as_slice(),
            None => &[],
        }
    }

    /// All transitive ancestors of `node`, nearest first, excluding `node`
    /// itself. Empty for an unregistered node. Cycle-resilient.
    #[must_use]
    pub fn ancestors(&self, node: &K) -> Vec<K> {
        if let Some(cached) = lock_read(&self.ancestor_memo).get(node) {
            return cached.to_vec();
        }
        let closure = walk(node, &self.parents);
        lock_write(&self.ancestor_memo).insert(node.clone(), Arc::from(closure.clone()));
        closure
    }

    /// All transitive descendants of `node`, nearest first, excluding `node`
    /// itself. Empty for an unregistered node. Cycle-resilient.
    #[must_use]
    pub fn descendants(&self, node: &K) -> Vec<K> {
        if let Some(cached) = lock_read(&self.descendant_memo).get(node) {
            return cached.to_vec();
        }
        let closure = walk(node, &self.children);
        lock_write(&self.descendant_memo).insert(node.clone(), Arc::from(closure.clone()));
        closure
    }

    /// `true` when `child` is a transitive subclass of `parent` (identity
    /// included). `is_subclass_of(x, x)` is `true` for every `x`, registered
    /// or not.
    #[must_use]
    pub fn is_subclass_of(&self, child: &K, parent: &K) -> bool {
        child == parent || self.ancestors(child).contains(parent)
    }

    /// Whether `node` was registered — as an edge endpoint or via
    /// [`add_node`](Self::add_node).
    #[must_use]
    pub fn contains(&self, node: &K) -> bool {
        self.parents.contains_key(node)
    }

    /// Number of registered nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.parents.len()
    }

    /// Whether no nodes are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parents.is_empty()
    }
}

/// Breadth-first, nearest-first transitive walk over one adjacency map,
/// excluding `start` itself and visiting each node once (cycle-resilient).
fn walk<K: Eq + Hash + Clone>(start: &K, adjacency: &HashMap<K, Vec<K>>) -> Vec<K> {
    let mut out: Vec<K> = Vec::new();
    let mut seen: HashSet<K> = HashSet::new();
    seen.insert(start.clone());
    let mut frontier: VecDeque<K> = VecDeque::new();
    push_neighbours(start, adjacency, &mut out, &mut seen, &mut frontier);
    while let Some(current) = frontier.pop_front() {
        push_neighbours(&current, adjacency, &mut out, &mut seen, &mut frontier);
    }
    out
}

/// Enqueue the unvisited direct neighbours of `node` (in adjacency order).
fn push_neighbours<K: Eq + Hash + Clone>(
    node: &K,
    adjacency: &HashMap<K, Vec<K>>,
    out: &mut Vec<K>,
    seen: &mut HashSet<K>,
    frontier: &mut VecDeque<K>,
) {
    if let Some(next) = adjacency.get(node) {
        for neighbour in next {
            if seen.insert(neighbour.clone()) {
                out.push(neighbour.clone());
                frontier.push_back(neighbour.clone());
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/hierarchy.rs"]
mod tests;
