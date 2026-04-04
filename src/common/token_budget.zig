/// token_budget.zig — Token Estimation (shared between guidance and coral).
///
/// Provides a lightweight `TokenEstimator` struct that approximates the
/// token count of text. Exact tokenisation requires model-specific libraries;
/// this uses the widely-accepted approximation of 1 token ≈ 4 UTF-8 bytes.
///
/// This module is coral_db-independent. For ContextNode estimation see
/// `src/coral/token_budget.zig` which extends this with coral-specific helpers.
const std = @import("std");

// ---------------------------------------------------------------------------
// Token approximation constants
// ---------------------------------------------------------------------------

/// Bytes-per-token approximation used across all models when exact tokenisation
/// is unavailable. Industry standard rough estimate: 1 token ≈ 4 chars.
pub const BYTES_PER_TOKEN: usize = 4;

/// Per-node schema overhead: JSON structural characters, field names, etc.
pub const NODE_SCHEMA_OVERHEAD_TOKENS: usize = 16;

// ---------------------------------------------------------------------------
// Standalone helper (drop-in replacement for local estimateTokens() calls)
// ---------------------------------------------------------------------------

/// Converts a byte slice into a Zig usize value representing its length.
pub fn estimate(text: []const u8) usize {
    if (text.len == 0) return 0;
    return @max(1, (text.len + BYTES_PER_TOKEN - 1) / BYTES_PER_TOKEN);
}

// ---------------------------------------------------------------------------
// TokenEstimator
// ---------------------------------------------------------------------------

/// Manages token estimation logic, owns estimation model; ensures consistent state across runs.
pub const TokenEstimator = struct {
    const Self = @This();

    // ── Core estimation ───────────────────────────────────────────────────────

    /// Estimate the number of tokens in `text` using the bytes-per-token
    /// approximation. Returns at least 1 for any non-empty input.
    pub fn estimateTokens(_: Self, text: []const u8) usize {
        return estimate(text);
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

/// Manages dynamic token budget allocation; owns runtime state; ensures proportional distribution.
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
// Tests
// =============================================================================

const testing = std.testing;

test "estimate: empty string → 0 tokens" {
    try testing.expectEqual(@as(usize, 0), estimate(""));
}

test "estimate: single char → 1 token" {
    try testing.expectEqual(@as(usize, 1), estimate("A"));
}

test "estimate: 4-char string → 1 token" {
    try testing.expectEqual(@as(usize, 1), estimate("abcd"));
}

test "estimate: 5-char string → 2 tokens" {
    try testing.expectEqual(@as(usize, 2), estimate("abcde"));
}

test "estimate: 100-char string → 25 tokens" {
    const text = "a" ** 100;
    try testing.expectEqual(@as(usize, 25), estimate(text));
}

test "TokenEstimator: fits within budget" {
    const est = TokenEstimator{};
    try testing.expect(est.fits("hello", 10)); // 2 tokens needed
    try testing.expect(!est.fits("a" ** 100, 10)); // 25 tokens needed
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
    const b = ProportionalBudget{
        .total = 1000,
        .community_reports = 0.5,
        .text_units = 0.5,
        .entities_relations = 0.5,
    };
    try testing.expectError(error.InvalidBudget, b.validate());
}

test "ProportionalBudget: token allocation sums approximately to total" {
    const b = ProportionalBudget{ .total = 1000 };
    try b.validate();
    const allocated = b.communityTokens() + b.textTokens() + b.entityTokens();
    try testing.expect(allocated >= 997);
    try testing.expect(allocated <= 1000);
}

test "ProportionalBudget: community 15%, text 50%, entity 35% of 8000" {
    const b = ProportionalBudget{ .total = 8000 };
    try testing.expectEqual(@as(usize, 1200), b.communityTokens());
    try testing.expectEqual(@as(usize, 4000), b.textTokens());
    try testing.expectEqual(@as(usize, 2800), b.entityTokens());
}
