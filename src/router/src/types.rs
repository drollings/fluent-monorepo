//! Router view of the OpenAI-compatible wire types.
//!
//! M3.2 migration shim: the canonical definitions live in
//! `fluent_types::openai_wire` — this module re-exports them so existing
//! `crate::types::…` paths keep working while consumers migrate one module
//! per commit to the canonical path. New code imports from `fluent_types`
//! directly. See `types.rs` module docs (now `openai_wire`) for the
//! stay-vs-move rationale.

pub use fluent_types::openai_wire::{
    ContentPart, FunctionCall, FunctionDef, ImageUrl, RouterChoice, RouterMessage,
    RouterMessageContent, RouterRequest, RouterResponse, RouterTool, ToolCall, Usage,
};

#[cfg(test)]
#[path = "../tests/types.rs"]
mod tests;
