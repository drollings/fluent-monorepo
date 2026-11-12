//! Kernel-side typed errors: invalid inputs, over-cap runs, bad configuration.

use fluent_dag::dep_graph::GraphError;
use thiserror::Error;

/// Errors from run-context handling, input validation, and runtime checks.
#[derive(Error, Debug)]
/// Errors from run-context handling, input validation, and runtime checks.
pub enum KernelError {
    /// An input declaration or assignment failed validation.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// A run was offered more bars than the runtime allows.
    #[error("too many bars: got {got}, max {max}")]
    TooManyBars {
        /// Bars offered.
        got: u64,
        /// Configured cap.
        max: u64,
    },
    /// The runtime configuration contradicts itself.
    #[error("invalid runtime configuration: {0}")]
    ConfigConflict(String),
    /// No indicator is registered under the requested id.
    #[error("unknown indicator: {0}")]
    UnknownIndicator(String),
    /// An indicator id was registered twice.
    #[error("duplicate indicator: {0}")]
    DuplicateIndicator(String),
    /// A parameter is missing, mistyped, or out of range.
    #[error("invalid parameter: {0}")]
    InvalidParam(String),
    /// A research-frame conversion failed. Research helpers never run in the
    /// per-bar hot loop; this error only surfaces during analysis.
    #[error("research frame error: {0}")]
    Research(String),
    /// A Pine source file could not be read from its path.
    #[error("unreadable pine source: {0}")]
    UnreadableSource(String),
    /// A Pine program refused evaluation with a reason (unknown function,
    /// type mismatch, loop bound, unsupported node) instead of miscomputing.
    #[error("pine evaluation refused: {0}")]
    Eval(String),
    /// A program could not be linked for the compiled loop in a build
    /// without the tree oracle. The `construct` carries the linker's own
    /// diagnostic, so this never masks a script-level refusal as a
    /// load-time one: script refusals keep their `Eval` wording on both
    /// engines, while this variant fires only when no engine remains.
    #[error("uncompilable program in {module}: {construct}")]
    Uncompilable {
        /// Owning module of the unlowered construct.
        module: String,
        /// The linker's diagnostic for the unlowered construct.
        construct: String,
    },
    /// The tree-walk selection was requested in a build without the tree
    /// oracle. Selecting it is an operator choice against a retired
    /// engine, so it refuses at run start instead of silently running
    /// another engine.
    #[error("tree-walk engine retired in this build (requested {mode})")]
    EngineRetired {
        /// The retired selection that was requested.
        mode: String,
    },
    /// Bisection exhausted its iteration cap without bracketing the price.
    #[error("implied volatility did not converge after {tried} iterations")]
    VolNotConverged {
        /// Iterations attempted (the documented cap).
        tried: u32,
    },
    /// A dependency-graph refusal with its structured cause attached. The
    /// message keeps the historical wording; match on the source instead
    /// of parsing it.
    #[error("invalid input: {message}")]
    Graph {
        /// The historical human-readable refusal, prefix included.
        message: String,
        /// The structured graph failure behind the message.
        #[source]
        source: GraphError,
    },
}

impl From<GraphError> for KernelError {
    /// Dependency-graph registration failures (duplicate nodes on paths
    /// guarded by membership checks — defensive, never the reachable
    /// refusal). Message stays caller-neutral: both the module import graph
    /// and the kernel dep graph convert here. The structured cause rides
    /// along as the source; the wording is unchanged.
    fn from(source: GraphError) -> Self {
        let message = format!("dependency graph failed: {source}");
        KernelError::Graph { message, source }
    }
}