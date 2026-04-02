/// token_budget.zig — Token Estimation for Context Packing (M7.1)
///
/// Provides a lightweight `TokenEstimator` struct that approximates the
/// token count of text and `ContextNode` payloads.  Exact tokenisation
/// requires model-specific libraries (Tiktoken for GPT-4, SentencePiece for
/// Llama); this implementation uses the widely-accepted approximation of
/// 1 token ≈ 4 UTF-8 bytes with a small fixed overhead per node.
///
/// The estimator is stateless and safe to use from multiple goroutines.
const std = @import("std");
const coral_db = @import("coral_db");
const schema_mod = coral_db.schema;

const ContextNode = coral_db.ContextNode;

// ---------------------------------------------------------------------------
// Token approximation constants
// ---------------------------------------------------------------------------

/// Bytes-per-token approximation used across all models when exact tokenisation
/// is unavailable.  Industry standard rough estimate: 1 token ≈ 4 chars.
pub const BYTES_PER_TOKEN: usize = 4;

/// Per-node schema overhead: JSON structural characters, field names, etc.
pub const NODE_SCHEMA_OVERHEAD_TOKENS: usize = 16;

// ---------------------------------------------------------------------------
// TokenEstimator
// ---------------------------------------------------------------------------

/// Lightweight, stateless token estimator.
///
/// Usage:
///   const est = TokenEstimator{};
///   const tokens = est.estimateTokens("hello world");  // → 3
pub const TokenEstimator = struct {
    const Self = @This();

    // ── Core estimation ───────────────────────────────────────────────────────

    /// Estimate the number of tokens in `text` using the bytes-per-token
    /// approximation.  Returns at least 1 for any non-empty input.
    pub fn estimateTokens(_: Self, text: []const u8) usize {
        if (text.len == 0) return 0;
        return @max(1, (text.len + BYTES_PER_TOKEN - 1) / BYTES_PER_TOKEN);
    }

    /// Estimate the total number of tokens required to represent `node`'s
    /// LOD payload (all six levels) plus the fixed per-node schema overhead.
    ///
    /// This is the budget cost for including one node in an LLM context window.
    pub fn estimateEmbeddingTokens(self: Self, node: *const ContextNode) usize {
        var total: usize = NODE_SCHEMA_OVERHEAD_TOKENS;
        for (node.lod) |level_text| {
            total += self.estimateTokens(level_text);
        }
        return total;
    }

    /// Estimate tokens for a slice of text segments (e.g. multiple LOD fields).
    pub fn estimateSliceTokens(self: Self, texts: []const []const u8) usize {
        var total: usize = 0;
        for (texts) |t| total += self.estimateTokens(t);
        return total;
    }

    /// Return true when `text` fits within `budget_tokens` remaining capacity.
    pub fn fits(_: Self, text: []const u8, budget_tokens: usize) bool {
        if (text.len == 0) return true;
        const needed = (text.len + BYTES_PER_TOKEN - 1) / BYTES_PER_TOKEN;
        return needed <= budget_tokens;
    }
};

// ---------------------------------------------------------------------------
// ProportionalBudget — P3.1
// ---------------------------------------------------------------------------

/// Allocate a total token budget across three context sections.
/// Fractions must sum to 1.0 (validated by `validate()`).
pub const ProportionalBudget = struct {
    total: usize,
    /// Fraction for community-report context (default 15%).
    community_reports: f32 = 0.15,
    /// Fraction for source text units (default 50%).
    text_units: f32 = 0.50,
    /// Fraction for entity + relation context (default 35%).
    entities_relations: f32 = 0.35,

    const Self = @This();

    /// Validate that the three fractions sum to 1.0 (within 0.1% tolerance).
    pub fn validate(self: Self) !void {
        const sum = self.community_reports + self.text_units + self.entities_relations;
        if (@abs(sum - 1.0) > 0.001) return error.InvalidBudget;
    }

    pub fn communityTokens(self: Self) usize {
        return @intFromFloat(@as(f32, @floatFromInt(self.total)) * self.community_reports);
    }

    pub fn textTokens(self: Self) usize {
        return @intFromFloat(@as(f32, @floatFromInt(self.total)) * self.text_units);
    }

    pub fn entityTokens(self: Self) usize {
        return @intFromFloat(@as(f32, @floatFromInt(self.total)) * self.entities_relations);
    }
};

// =============================================================================
// Tests — M7.1
// =============================================================================

const testing = std.testing;

test "TokenEstimator: empty string → 0 tokens" {
    const est = TokenEstimator{};
    try testing.expectEqual(@as(usize, 0), est.estimateTokens(""));
}

test "TokenEstimator: single char → 1 token" {
    const est = TokenEstimator{};
    try testing.expectEqual(@as(usize, 1), est.estimateTokens("A"));
}

test "TokenEstimator: 4-char string → 1 token" {
    const est = TokenEstimator{};
    try testing.expectEqual(@as(usize, 1), est.estimateTokens("abcd"));
}

test "TokenEstimator: 5-char string → 2 tokens" {
    const est = TokenEstimator{};
    try testing.expectEqual(@as(usize, 2), est.estimateTokens("abcde"));
}

test "TokenEstimator: 100-char string → 25 tokens" {
    const text = "a" ** 100;
    const est = TokenEstimator{};
    try testing.expectEqual(@as(usize, 25), est.estimateTokens(text));
}

test "TokenEstimator: fits within budget" {
    const est = TokenEstimator{};
    try testing.expect(est.fits("hello", 10)); // 2 tokens needed
    try testing.expect(!est.fits("a" ** 100, 10)); // 25 tokens needed
}

test "TokenEstimator: estimateEmbeddingTokens includes overhead" {
    const est = TokenEstimator{};
    var node = ContextNode{
        .id = 1,
        .lod = [_][]const u8{ "hello", "", "", "", "", "" },
        .embedding = &[_]f32{},
        .valid_from = 0,
        .valid_to = null,
        .confidence = 0,
        .provenance_id = 0,
    };
    const tokens = est.estimateEmbeddingTokens(&node);
    // "hello" = 2 tokens + overhead
    try testing.expect(tokens >= NODE_SCHEMA_OVERHEAD_TOKENS + 1);
    try testing.expect(tokens <= NODE_SCHEMA_OVERHEAD_TOKENS + 2);
}

test "TokenEstimator: estimateSliceTokens sums correctly" {
    const est = TokenEstimator{};
    const texts = [_][]const u8{ "abcd", "abcd", "abcd" }; // 3 × 1 token
    try testing.expectEqual(@as(usize, 3), est.estimateSliceTokens(&texts));
}

test "ProportionalBudget: validate passes on correct fractions" {
    const b = ProportionalBudget{ .total = 1000 };
    try b.validate();
}

test "ProportionalBudget: validate fails when fractions don't sum to 1.0" {
    const b = ProportionalBudget{ .total = 1000, .community_reports = 0.5, .text_units = 0.5, .entities_relations = 0.5 };
    try testing.expectError(error.InvalidBudget, b.validate());
}

test "ProportionalBudget: token allocation sums approximately to total" {
    const b = ProportionalBudget{ .total = 1000 };
    try b.validate();
    const allocated = b.communityTokens() + b.textTokens() + b.entityTokens();
    // Allow for rounding; should be within 3 tokens of total.
    try testing.expect(allocated >= 997);
    try testing.expect(allocated <= 1000);
}

test "ProportionalBudget: community 15%, text 50%, entity 35% of 8000" {
    const b = ProportionalBudget{ .total = 8000 };
    try testing.expectEqual(@as(usize, 1200), b.communityTokens());
    try testing.expectEqual(@as(usize, 4000), b.textTokens());
    try testing.expectEqual(@as(usize, 2800), b.entityTokens());
}
