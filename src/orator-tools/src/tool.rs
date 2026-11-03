//! One `Tool` trait: the single source of truth for name, schema,
//! tier, and executor. The registry owns every definition; a name
//! resolves to exactly one tool or to nothing.

use std::sync::Arc;

use thiserror::Error;

/// Effect tier. Defaults follow the policy table; the policy decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolTier {
    /// Read-only: time, calculator.
    T0,
    /// Reversible local effects: notify, clipboard.
    T1,
    /// Effectful: shell, file write, network.
    T2,
}

/// One parsed invocation plus the spoken lead-in it must never diverge
/// from. Empty preamble means the turn spoke nothing first.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// Registry tool name.
    pub name: String,
    /// Arguments object.
    pub args: serde_json::Value,
    /// Spoken lead-in carried with the calls.
    pub preamble: String,
}

/// Tool failures. Denials name the policy; timeouts name the budget.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// No tool by that name.
    #[error("unknown tool: {0}")]
    Unknown(String),
    /// Arguments failed validation.
    #[error("bad arguments for {0}: {1}")]
    Args(String, String),
    /// The policy refused.
    #[error("denied: {0}")]
    Denied(String),
    /// The tool reported failure.
    #[error("tool {0} failed: {1}")]
    Failed(String, String),
    /// The tool outlived its budget (and was killed).
    #[error("tool {0} timed out after {1} ms")]
    Timeout(String, u64),
}

/// A callable tool. Schemas are plain `serde_json` in OpenAI
/// function-calling shape (`{name, description, parameters}`), shared by
/// the prompt, the grammar emitter, and config editors.
pub trait Tool: Send + Sync {
    /// Registry name. Defined exactly once per tool.
    fn name(&self) -> &'static str;
    /// Effect tier.
    fn tier(&self) -> ToolTier;
    /// One-line description for prompts and menus.
    fn description(&self) -> &'static str;
    /// OpenAI function-calling schema.
    fn schema(&self) -> serde_json::Value;
    /// Run to completion on the caller's thread. Blocking is expected;
    /// callers bound it with timeouts, never with faith.
    fn execute(&self, args: serde_json::Value) -> Result<String, ToolError>;
}

/// The static registry. Registration rejects duplicates: a name is
/// defined exactly once, so prompt, grammar, and executor agree.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one tool. Duplicate names are rejected, never shadowed.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), ToolError> {
        if self.tools.iter().any(|known| known.name() == tool.name()) {
            return Err(ToolError::Failed(
                tool.name().to_string(),
                "duplicate tool definition".to_string(),
            ));
        }
        self.tools.push(tool);
        Ok(())
    }

    /// Look up one tool by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|tool| tool.name() == name).cloned()
    }

    /// Every registered name, in registration order.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|tool| tool.name()).collect()
    }

    /// Every registered schema, in registration order.
    #[must_use]
    pub fn schemas(&self) -> Vec<serde_json::Value> {
        self.tools.iter().map(|tool| tool.schema()).collect()
    }
}

/// Shared tool-output cap and truncator: re-exported from the core
/// vocabulary (single home); tool code composes it from here.
pub use orator_core::truncate::{truncate_tool_output, TOOL_OUTPUT_CHARS};

#[cfg(test)]
#[path = "../tests/tool.rs"]
mod tests;
