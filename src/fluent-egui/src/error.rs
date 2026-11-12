//! Typed errors for the paint edge.
//!
//! Every fallible entry refuses with a reason instead of substituting a
//! value; display code never invents coordinates, thresholds, or colors.

/// Fail-closed refusal for paint-edge construction.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
/// Typed errors for the paint edge.
/// 
/// Every fallible entry refuses with a reason instead of substituting a
/// value; display code never invents coordinates, thresholds, or colors.
/// Fail-closed refusal for paint-edge construction.
pub enum EguiError {
    /// A paint parameter (height, spacing, threshold, …) is not usable.
    #[error("invalid parameter for {what}: {reason}")]
    InvalidParam {
        /// Which parameter refused.
        what: String,
        /// Why it refused.
        reason: String,
    },
    /// An ordering or upstream dependency is not satisfiable.
    #[error("dependency refused: {reason}")]
    Dependency {
        /// Why the dependency refused.
        reason: String,
    },
    /// A dependency-graph refusal with its structured cause attached. The
    /// wording matches the plain dependency refusal; match on the source
    /// instead of parsing it.
    #[error("dependency refused: {reason}")]
    GraphDependency {
        /// Why the dependency refused, in words.
        reason: String,
        /// The structured graph failure behind the reason.
        #[source]
        source: fluent_dag::dep_graph::GraphError,
    },
}

#[cfg(test)]
#[path = "../tests/error.rs"]
mod tests;