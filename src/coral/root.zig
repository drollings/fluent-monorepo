//! coral/root.zig — Public API re-exports for the coral module.
//!
//! This root.zig provides a unified import point for the coral module,
//! re-exporting all public submodules for convenient access.
pub const db = @import("db.zig");
pub const batch = @import("batch.zig");
pub const cache = @import("cache.zig");
pub const config = @import("config.zig");
pub const session = @import("session.zig");
pub const targets = @import("targets.zig");
pub const executor = @import("executor.zig");
pub const tool_registry = @import("tool_registry.zig");
pub const mcp = @import("mcp.zig");
pub const cli = @import("cli.zig");
pub const benchmark = @import("benchmark.zig");
pub const verify = @import("verify.zig");
pub const yago_ingest = @import("yago_ingest.zig");
pub const token_budget = @import("token_budget.zig");
pub const context_node_schema = @import("context_node_schema.zig");
pub const global_search = @import("global_search.zig");
pub const metrics = @import("metrics.zig");
pub const algorithm_runner = @import("algorithm_runner.zig");
pub const agent_loop = @import("agent_loop.zig");
pub const frontier = @import("frontier.zig");
pub const frontier_tool_compiler = @import("frontier_tool_compiler.zig");
pub const type_inference = @import("type_inference.zig");
pub const frozen_snapshot = @import("frozen_snapshot.zig");
pub const csr_graph = @import("csr_graph.zig");
pub const schema = @import("schema.zig");
pub const delegation = @import("delegation.zig");
