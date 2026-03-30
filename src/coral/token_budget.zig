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
