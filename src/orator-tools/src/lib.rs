#![forbid(unsafe_code)]

//! orator-tools: Tool trait and static registry, policy toggles, sandbox, approvals.

pub mod approve;
pub mod builtins;
pub mod loop_;
pub mod parse;
pub mod policy;
pub mod sandbox;
pub mod tool;
pub mod untrusted;

pub use approve::{ApprovalHub, ApprovalId};
pub use builtins::{default_registry, FETCH_CHARS};
pub use loop_::{
    bound_prompt, last_prompt_for_tests, run_turn, TurnOutcome, MAX_PROMPT_TOKENS, MAX_TOOL_ROUNDS,
};
pub use parse::{gbnf_for_tools, parse_model_output, parse_xml, ParseError};
pub use policy::{CallOrigin, Policy, PolicyCtx, Verdict};
pub use sandbox::{
    check_fd_containment, default_sandbox_root, kill_program, shell_escape, Guarded, GuardedConfig,
    ToolAudit, WithinError, MAX_AUDIT_ROWS, MAX_DIR_CACHE_ENTRIES, MAX_SYMLINK_HOPS,
};
pub use tool::{
    truncate_tool_output, Tool, ToolCall, ToolError, ToolRegistry, ToolTier, TOOL_OUTPUT_CHARS,
};
pub use untrusted::{wrap_tool_result, wrap_untrusted, TRANSCRIPT_AS_DATA};
