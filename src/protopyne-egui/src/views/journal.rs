//! Proposal-provenance timeline: journal rows as a dependency graph.
//!
//! Each row depends on the previous row for the same client, so the graph is
//! the journal's own causal chains (signal → risk → grant → submission →
//! fill per client) with no second event model: ordering composes the shared
//! [`DependencyGraph`](fluent_dag::dep_graph::DependencyGraph)'s `topo_sort`,
//! follow-ups compose its `dependents_of`. Rows sort by sequence number at
//! build, so the timeline is seq-causal, never arrival-causal.

use std::collections::HashMap;

use fluent_dag::dep_graph::{DependencyGraph, GraphError};
use fluent_egui::EguiError;
use protopyne_view::JournalRowView;

fn describe_graph(error: &GraphError) -> EguiError {
    EguiError::Dependency { reason: error.to_string() }
}

/// Read-only provenance over journal rows, keyed by sequence number (the
/// graph holds indices, so — like the server gateway over its locks — this
/// prints as its rows and timeline, never its internals: no `Debug`).
pub struct Provenance {
    rows: Vec<JournalRowView>,
    graph: DependencyGraph<i64>,
}

impl Provenance {
    /// Chain each client's rows in sequence order; refuses a journal with
    /// duplicate sequence numbers instead of substituting an order.
    pub fn of(rows: &[JournalRowView]) -> Result<Self, EguiError> {
        let mut ordered: Vec<JournalRowView> = rows.to_vec();
        ordered.sort_by_key(|row| row.seq);
        let mut graph = DependencyGraph::new();
        let mut previous: HashMap<&str, i64> = HashMap::new();
        for row in &ordered {
            let deps: Vec<i64> =
                previous.get(row.client_id.as_str()).copied().into_iter().collect();
            graph.register(&row.seq, &deps, &[row.seq]).map_err(|error| describe_graph(&error))?;
            previous.insert(row.client_id.as_str(), row.seq);
        }
        Ok(Self { rows: ordered, graph })
    }

    /// Sequence numbers in causal order (matches journal order for a
    /// well-formed journal).
    pub fn timeline(&self) -> Result<Vec<i64>, EguiError> {
        self.graph.topo_sort().map_err(|error| describe_graph(&error))
    }

    /// Sequence numbers that (transitively) follow `seq`; empty for unknown
    /// rows, following the graph's own unregistered-node convention.
    #[must_use]
    pub fn followed_by(&self, seq: i64) -> Vec<i64> {
        self.graph.dependents_of(&seq)
    }

    /// The row behind a sequence number, if present.
    #[must_use]
    pub fn row(&self, seq: i64) -> Option<&JournalRowView> {
        self.rows.iter().find(|row| row.seq == seq)
    }
}
