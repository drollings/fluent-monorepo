/// vector_db.zig — Hybrid keyword + vector search for guidance generation.
///
/// Provides a combined search mode that fuses keyword (BM25-style rank)
/// and vector (cosine similarity) scores via Reciprocal Rank Fusion (RRF).
/// Falls back gracefully to keyword-only when no embedding is provided.
const std = @import("std");

/// Manages search result data structures; owned by the module; ensures consistent data invariants.
pub const SearchResult = struct {
    id: i64,
    score: f32,
    name: []const u8,
};

/// Configuration for hybrid search score fusion.
pub const SearchConfig = struct {
    /// Weight for vector (semantic) component.
    vector_weight: f32 = 0.65,
    /// Weight for keyword (lexical) component.
    keyword_weight: f32 = 0.35,
};

/// Manages guidance search modes with fixed buffers; owned by the system; ensures consistent state across operations.
pub const GuidanceSearchMode = enum {
    keyword_only,
    hybrid,
    embedding_only,
};

/// Executes a hybrid search across allocator, keyword and vector results with configurable limits.
pub fn hybridSearch(
    allocator: std.mem.Allocator,
    keyword_results: []const SearchResult,
    vector_results: ?[]const SearchResult,
    limit: usize,
    config: SearchConfig,
) ![]SearchResult {
    var scores = std.AutoHashMapUnmanaged(i64, struct { score: f32, name: []const u8 }){};
    defer scores.deinit(allocator);

    // Keyword component: rank-based score 1.0, 0.95, 0.90, ...
    for (keyword_results, 0..) |result, rank| {
        const kw_score = @max(0.1, 1.0 - @as(f32, @floatFromInt(rank)) * 0.05);
        const entry = try scores.getOrPut(allocator, result.id);
        if (entry.found_existing) {
            entry.value_ptr.score += config.keyword_weight * kw_score;
        } else {
            entry.value_ptr.* = .{ .score = config.keyword_weight * kw_score, .name = result.name };
        }
    }

    // Vector component (optional)
    if (vector_results) |vec_res| {
        for (vec_res) |result| {
            const vec_score = @max(0.0, 1.0 - @min(1.0, result.score)); // result.score is distance
            const entry = try scores.getOrPut(allocator, result.id);
            if (entry.found_existing) {
                entry.value_ptr.score += config.vector_weight * vec_score;
            } else {
                entry.value_ptr.* = .{ .score = config.vector_weight * vec_score, .name = result.name };
            }
        }
    }

    // Collect and sort
    var merged = try allocator.alloc(SearchResult, scores.count());
    var idx: usize = 0;
    var iter = scores.iterator();
    while (iter.next()) |entry| {
        merged[idx] = .{
            .id = entry.key_ptr.*,
            .score = entry.value_ptr.score,
            .name = entry.value_ptr.name,
        };
        idx += 1;
    }

    std.sort.block(SearchResult, merged, {}, struct {
        fn lessThan(_: void, a: SearchResult, b: SearchResult) bool {
            return a.score > b.score; // descending
        }
    }.lessThan);

    const n = @min(limit, merged.len);
    const result = try allocator.alloc(SearchResult, n);
    @memcpy(result, merged[0..n]);
    allocator.free(merged);
    return result;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
const testing = std.testing;

test "hybridSearch: keyword-only returns sorted results" {
    const allocator = testing.allocator;

    const kw = [_]SearchResult{
        .{ .id = 1, .score = 0, .name = "alpha" },
        .{ .id = 2, .score = 0, .name = "beta" },
        .{ .id = 3, .score = 0, .name = "gamma" },
    };

    const results = try hybridSearch(allocator, &kw, null, 2, .{});
    defer allocator.free(results);

    try testing.expectEqual(@as(usize, 2), results.len);
    try testing.expectEqual(@as(i64, 1), results[0].id); // highest rank = best score
}

test "hybridSearch: vector results boost existing keyword matches" {
    const allocator = testing.allocator;

    const kw = [_]SearchResult{
        .{ .id = 1, .score = 0, .name = "alpha" },
        .{ .id = 2, .score = 0, .name = "beta" },
    };
    const vec = [_]SearchResult{
        .{ .id = 2, .score = 0.1, .name = "beta" }, // close match
        .{ .id = 3, .score = 0.2, .name = "gamma" },
    };

    const results = try hybridSearch(allocator, &kw, &vec, 3, .{});
    defer allocator.free(results);

    // id=2 should rank highest: keyword rank 2 + vector boost
    try testing.expectEqual(@as(usize, 3), results.len);
    try testing.expectEqual(@as(i64, 2), results[0].id);
}

test "hybridSearch: empty inputs return empty slice" {
    const allocator = testing.allocator;
    const results = try hybridSearch(allocator, &[_]SearchResult{}, null, 10, .{});
    defer allocator.free(results);
    try testing.expectEqual(@as(usize, 0), results.len);
}



