//! llm — General-purpose LLM inference client.
//!
//! Provides:
//!   LlmError    — error set for all LLM operations
//!   LlmConfig   — endpoint, model, timeout, think-mode settings
//!   LlmClient   — HTTP chat-completion client (OpenAI + Ollama)
//!   LocalDecomposer, DecomposerConfig — query decomposition
//!   stripThinkBlock, isMalformedResponse, extractCommentTag,
//!   stripPreamble, isBlankOrPlausible — response post-processing
//!
//! All definitions live in llm.zig; this file re-exports them for
//! convenient `@import("llm")` access.

const llm_mod = @import("llm.zig");

pub const token_budget = @import("token_budget.zig");
pub const anonymize = @import("anonymize.zig");
pub const context_packer = @import("context_packer.zig");
pub const context_compressor = @import("context_compressor.zig");

// ── Re-exports from llm.zig ──────────────────────────────────────────────────

pub const LlmError = llm_mod.LlmError;
pub const LlmConfig = llm_mod.LlmConfig;
pub const LlmClient = llm_mod.LlmClient;

pub const LocalDecomposer = llm_mod.LocalDecomposer;
pub const DecomposerConfig = llm_mod.DecomposerConfig;

pub const stripThinkBlock = llm_mod.stripThinkBlock;
pub const isMalformedResponse = llm_mod.isMalformedResponse;
pub const extractCommentTag = llm_mod.extractCommentTag;
pub const stripPreamble = llm_mod.stripPreamble;
pub const isBlankOrPlausible = llm_mod.isBlankOrPlausible;

// ── Tests ────────────────────────────────────────────────────────────────────
