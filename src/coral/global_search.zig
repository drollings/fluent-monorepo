/// global_search.zig — GlobalSearch Map-Reduce over Communities (P3.4)
///
/// Implements GraphRAG-style global search: instead of querying the full graph,
/// GlobalSearch clusters nodes by community, generates a per-community summary
/// with a local model (map phase), and then combines the top summaries into a
/// final answer (reduce phase).
///
/// §Local model vs. frontier model:
///   Map phase: local model (fast, cheap, edge-compatible).
///   Reduce phase: local model by default; escalate to frontier model if the
///   combined confidence is below the configured threshold.
///
/// §This implementation omits live LLM calls to keep the module self-contained
/// and testable without network access.  The `summarizeCommunity` and
/// `frontierSynthesize` hooks are `anytype` callables for easy injection.
const std = @import("std");
const Allocator = std.mem.Allocator;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Tracks community summaries with fixed-size buffers; managed by owner; key invariant is consistent state.
pub const CommunitySummary = struct {
    community_id: u32,
    /// Summarised text produced by the local model.
    summary: []const u8,
    /// Node count in this community (used for ranking).
    node_count: u32,
    /// Average pagerank of nodes in this community (used for ranking).
    avg_pagerank: f32,
    /// Confidence score in [0.0, 1.0].
    confidence: f32,
};

/// Holds search results with ownership model; ensures data integrity via invariants.
pub const GlobalSearchResult = struct {
    answer: []const u8,
    /// Summaries that contributed to the answer, sorted by relevance.
    sources: []CommunitySummary,
    /// Whether the frontier model was used.
    used_frontier: bool,
};

pub const GlobalSearchConfig = struct {
    /// Maximum communities to include in the map phase.
    max_communities: usize = 10,
    /// Minimum confidence to skip frontier escalation.
    frontier_threshold: f32 = 0.7,
};

// ---------------------------------------------------------------------------
// GlobalSearch
// ---------------------------------------------------------------------------

/// Manages global search keywords with a fixed-size structure; owned by the application; ensures consistent state across sessions.
pub const GlobalSearch = struct {
    config: GlobalSearchConfig,

    const Self = @This();

    pub fn init(config: GlobalSearchConfig) Self {
        return .{ .config = config };
    }

    /// Run global search over the provided community summaries.
    ///
    /// `summaries` is an arena-owned slice of CommunitySummary values, already
    /// generated (e.g. by iterating `context_nodes` grouped by `community_id`).
    ///
    /// `reduce_fn` is a callable with signature:
    ///   fn (arena: Allocator, summaries: []CommunitySummary, query: []const u8) !ReduceResult
    /// where ReduceResult = struct { answer: []const u8, confidence: f32 }.
    ///
    /// Returns an arena-owned `GlobalSearchResult`.
    pub fn search(
        self: Self,
        arena: Allocator,
        query: []const u8,
        summaries: []const CommunitySummary,
        reduce_fn: anytype,
    ) !GlobalSearchResult {
        // Sort by avg_pagerank descending to prioritise structurally important
        // communities.
        var sorted = try arena.dupe(CommunitySummary, summaries);
        std.mem.sort(CommunitySummary, sorted, {}, struct {
            pub fn lessThan(_: void, a: CommunitySummary, b: CommunitySummary) bool {
                return b.avg_pagerank < a.avg_pagerank;
            }
        }.lessThan);

        // Cap to max_communities.
        const cap = @min(sorted.len, self.config.max_communities);
        const top = sorted[0..cap];

        // Reduce phase.
        const reduced = try reduce_fn.call(arena, top, query);

        // Decide whether to escalate.
        const use_frontier = reduced.confidence < self.config.frontier_threshold;

        return GlobalSearchResult{
            .answer = reduced.answer,
            .sources = top,
            .used_frontier = use_frontier,
        };
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

const TestReducer = struct {
    pub fn call(_: @This(), arena: Allocator, summaries: []const CommunitySummary, query: []const u8) !struct { answer: []const u8, confidence: f32 } {
        _ = query;
        if (summaries.len == 0) {
            return .{ .answer = try arena.dupe(u8, "no results"), .confidence = 0.0 };
        }
        return .{ .answer = try arena.dupe(u8, summaries[0].summary), .confidence = summaries[0].confidence };
    }
};

test "GlobalSearch: empty summaries returns no-results answer" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const gs = GlobalSearch.init(.{});
    const result = try gs.search(arena.allocator(), "test query", &[_]CommunitySummary{}, TestReducer{});
    try testing.expectEqualStrings("no results", result.answer);
    // confidence=0.0 < 0.7 → used_frontier = true
    try testing.expect(result.used_frontier);
}

test "GlobalSearch: sorts by avg_pagerank descending" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    const summaries = [_]CommunitySummary{
        .{ .community_id = 0, .summary = "low rank", .node_count = 5, .avg_pagerank = 0.1, .confidence = 0.9 },
        .{ .community_id = 1, .summary = "high rank", .node_count = 5, .avg_pagerank = 0.9, .confidence = 0.9 },
        .{ .community_id = 2, .summary = "mid rank", .node_count = 5, .avg_pagerank = 0.5, .confidence = 0.9 },
    };

    const gs = GlobalSearch.init(.{ .max_communities = 10 });
    const result = try gs.search(a, "query", &summaries, TestReducer{});
    // Top summary should be the highest-pagerank one.
    try testing.expectEqualStrings("high rank", result.answer);
}

test "GlobalSearch: max_communities caps number of sources" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    var summaries: [5]CommunitySummary = undefined;
    for (&summaries, 0..) |*s, i| {
        s.* = .{
            .community_id = @intCast(i),
            .summary = "s",
            .node_count = 1,
            .avg_pagerank = @as(f32, @floatFromInt(i)) * 0.1,
            .confidence = 0.9,
        };
    }

    const gs = GlobalSearch.init(.{ .max_communities = 2 });
    const result = try gs.search(a, "query", &summaries, TestReducer{});
    try testing.expectEqual(@as(usize, 2), result.sources.len);
}

test "GlobalSearch: low confidence triggers frontier flag" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    const summaries = [_]CommunitySummary{
        .{ .community_id = 0, .summary = "uncertain", .node_count = 1, .avg_pagerank = 0.5, .confidence = 0.3 },
    };
    const gs = GlobalSearch.init(.{ .frontier_threshold = 0.7 });
    const result = try gs.search(a, "q", &summaries, TestReducer{});
    try testing.expect(result.used_frontier); // 0.3 < 0.7
}

test "GlobalSearch: high confidence skips frontier" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    const summaries = [_]CommunitySummary{
        .{ .community_id = 0, .summary = "confident", .node_count = 1, .avg_pagerank = 0.5, .confidence = 0.95 },
    };
    const gs = GlobalSearch.init(.{ .frontier_threshold = 0.7 });
    const result = try gs.search(a, "q", &summaries, TestReducer{});
    try testing.expect(!result.used_frontier); // 0.95 >= 0.7
}



