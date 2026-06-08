//! core/ranking.zig — Unified result ranking and scoring.
//!
//! Consolidates:
//!   - query_engine.zig:isExactNameMatch() sort
//!   - staged.zig capability boosting via cap_confidence HashMap
//!   - vector_db.zig RRF merge weighting

const std = @import("std");
const vector_db_mod = @import("vector");

pub const GuidanceDb = vector_db_mod.GuidanceDb;
pub const SearchResult = GuidanceDb.SearchResult;

/// Parameters for result ranking.
pub const RankParams = struct {
    /// Lowercased query tokens for exact-name matching.
    query_tokens: []const []const u8,
    /// Optional capability boost map: source_path → confidence (0.0–1.0).
    /// Applied as: score *= (1.0 + confidence * 0.3)
    cap_boost: ?*const std.StringHashMapUnmanaged(f32) = null,
    /// Whether to demote test_decl results.
    demote_tests: bool = true,
};

/// Rank results in-place using composite key:
///   1. Exact name match → highest priority
///   2. Non-test results → second priority
///   3. Within each tier, sort by score descending
///   4. Apply capability boost if provided (before sort)
pub fn rankResults(results: []SearchResult, params: RankParams) void {
    // Phase 1: apply capability boosts
    if (params.cap_boost) |boost_map| {
        for (results, 0..) |r, i| {
            if (boost_map.get(r.source)) |conf| {
                results[i].score *= (1.0 + @as(f64, conf) * 0.3);
            }
        }
    }

    // Phase 2: stable sort by composite key
    std.sort.insertion(SearchResult, results, params, struct {
        fn lessThan(p: RankParams, a: SearchResult, b: SearchResult) bool {
            const a_exact = isExactNameMatch(a.name, p.query_tokens);
            const b_exact = isExactNameMatch(b.name, p.query_tokens);
            if (a_exact != b_exact) return a_exact;

            if (p.demote_tests) {
                const a_test = std.mem.eql(u8, a.node_type, "test_decl");
                const b_test = std.mem.eql(u8, b.node_type, "test_decl");
                if (a_test != b_test) return !a_test;
            }

            return a.score > b.score;
        }
    }.lessThan);
}

/// Check if a result name exactly matches any query token (case-insensitive).
/// Stack buffer for names ≤ 128 bytes; returns false for longer names.
pub fn isExactNameMatch(name: []const u8, terms: []const []const u8) bool {
    var buf: [128]u8 = undefined;
    if (name.len > buf.len) return false;
    const lower = std.ascii.lowerString(buf[0..name.len], name);
    for (terms) |term| {
        if (std.mem.eql(u8, lower, term)) return true;
    }
    return false;
}

// =============================================================================
// Tests
// =============================================================================

const testing = std.testing;

test "isExactNameMatch: case-insensitive match" {
    const terms = [_][]const u8{"cmdexplain"};
    try testing.expect(isExactNameMatch("cmdExplain", &terms));
    try testing.expect(isExactNameMatch("CMDEXPLAIN", &terms));
    try testing.expect(!isExactNameMatch("other", &terms));
}

test "isExactNameMatch: empty terms never match" {
    const terms: []const []const u8 = &.{};
    try testing.expect(!isExactNameMatch("anything", terms));
}

test "isExactNameMatch: long name returns false" {
    const long_name = "a" ** 200;
    const terms = [_][]const u8{"aaaa"};
    try testing.expect(!isExactNameMatch(long_name, &terms));
}
