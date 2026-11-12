//! One topological sort over a registry's declared dependency edges.
//!
//! The kernels crate has two registries — an owned one
//! ([`IndicatorRegistry`](crate::indicator::IndicatorRegistry)) and a shared one
//! ([`ComponentRegistry`](crate::component::ComponentRegistry)) — whose storage
//! and concurrency contracts differ and must stay separate. What they share is
//! *ordering*: both build the same `fluent-dag` graph from the same declared
//! edges and ask it for a topological order. That shared part lives here, once,
//! so a registry cannot get its own subtly different sorter.
//!
//! [`Edged`] is the input the sort needs: the registered ids with their declared
//! `depends`/`provides` edges, and a registration check. The edges are copied
//! out (a handful of ids per kernel, once per plan build) because the shared
//! registry hands out `Arc` handles rather than borrowed specs — copying three
//! ids is cheaper than teaching the sorter two storage shapes.

use fluent_dag::dep_graph::{DependencyGraph, GraphError};

use crate::indicator::IndicatorId;

/// One registered kernel's id and its declared edges.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One registered kernel's id and its declared edges.
pub struct KernelEdges {
    /// Registry key.
    pub id: IndicatorId,
    /// Ids that must execute before this kernel.
    pub depends: Vec<IndicatorId>,
    /// Ids this kernel provides (its own, mirroring self-provision).
    pub provides: Vec<IndicatorId>,
}

/// A registry that can describe its kernels' edges to the sorter.
pub trait Edged {
    /// Every registered kernel's edges.
    fn edges(&self) -> Vec<KernelEdges>;

    /// Whether `id` is registered.
    fn holds(&self, id: &IndicatorId) -> bool;
}

/// Order `requested` so every declared dependency executes first.
///
/// Unknown requested ids and dependencies on unregistered ids are
/// [`GraphError::NodeNotFound`]; loops surface as
/// [`GraphError::CircularDependency`] naming the nodes involved.
///
/// # Errors
///
/// The graph refusals above.
pub fn execution_order_from<R: Edged + ?Sized>(
    registry: &R,
    requested: &[IndicatorId],
) -> Result<Vec<IndicatorId>, GraphError> {
    for id in requested {
        if !registry.holds(id) {
            return Err(GraphError::NodeNotFound(id.clone()));
        }
    }
    let mut graph = DependencyGraph::new();
    for edges in registry.edges() {
        for dependency in &edges.depends {
            if !registry.holds(dependency) {
                return Err(GraphError::NodeNotFound(dependency.clone()));
            }
        }
        graph.register(&edges.id, &edges.depends, &edges.provides)?;
    }
    graph.topo_sort_from(requested)
}

#[cfg(test)]
#[path = "../tests/order.rs"]
mod tests;