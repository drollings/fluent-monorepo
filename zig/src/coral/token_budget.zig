/// token_budget.zig — Token Estimation for Context Packing (M7.1)
///
/// Re-exports the coral_db-independent parts from `src/llm/token_budget.zig`
/// and adds `estimateEmbeddingTokens()` which requires `coral_db.ContextNode`.
///
/// Callers that only need basic text estimation should prefer
/// `@import("llm").token_budget` to avoid pulling in coral_db.
const std = @import("std");
const coral_db = @import("coral_db");
const llm_budget = @import("llm").token_budget;

const ContextNode = coral_db.ContextNode;

// ---------------------------------------------------------------------------
// Re-exports from common
// ---------------------------------------------------------------------------

pub const BYTES_PER_TOKEN = llm_budget.BYTES_PER_TOKEN;
pub const NODE_SCHEMA_OVERHEAD_TOKENS = llm_budget.NODE_SCHEMA_OVERHEAD_TOKENS;
pub const estimate = llm_budget.estimate;
pub const TokenEstimator = llm_budget.TokenEstimator;
pub const ProportionalBudget = llm_budget.ProportionalBudget;

// ---------------------------------------------------------------------------
// Coral-specific extension: ContextNode token estimation
// ---------------------------------------------------------------------------

/// Calculates the estimated embedding token count for a given context node.
pub fn estimateEmbeddingTokens(node: *const ContextNode) usize {
    var total: usize = NODE_SCHEMA_OVERHEAD_TOKENS;
    for (node.content.lod) |level_text| {
        total += estimate(level_text);
    }
    return total;
}

// =============================================================================
// Tests — M7.1 (coral-specific)
// =============================================================================

const testing = std.testing;

test "estimateEmbeddingTokens includes overhead" {
    var node = ContextNode{
        .id = 1,
        .content = .{ .lod = [_][]const u8{ "hello", "", "", "", "", "" } },
        .embedding = &[_]f32{},
        .valid_from = 0,
        .valid_to = null,
        .confidence = 0,
        .provenance_id = 0,
    };
    const tokens = estimateEmbeddingTokens(&node);
    // "hello" = 2 tokens + overhead
    try testing.expect(tokens >= NODE_SCHEMA_OVERHEAD_TOKENS + 1);
    try testing.expect(tokens <= NODE_SCHEMA_OVERHEAD_TOKENS + 2);
}
