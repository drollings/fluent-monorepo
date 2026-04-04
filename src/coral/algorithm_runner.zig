/// algorithm_runner.zig — Algorithm Runner with Strict Ingestion/Query Separation (P3.6)
///
/// Orchestrates all graph algorithm computations and enforces the critical invariant:
///
///   INGESTION-TIME functions mutate the database.
///   QUERY-TIME functions are read-only accessors.
///
/// This separation guarantees that the hot query path never triggers expensive
/// recomputation.  The dirty flags track which algorithms need re-running after
/// new nodes or edges are ingested.
///
/// §Integration with BatchIngestor:
///   After a batch completes, call `markDirty()`.  If `auto_compute_degree`
///   is set in config, `computeDegrees` runs automatically.
///   PageRank and community detection are always CLI-triggered.
const std = @import("std");
const Allocator = std.mem.Allocator;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub const AlgorithmRunnerConfig = struct {
    /// Automatically recompute degrees after each ingestion batch.
    auto_compute_degree: bool = true,
};

/// Manages algorithm execution state, owns run context, ensures invariant correctness.
pub const AlgorithmRunner = struct {
    config: AlgorithmRunnerConfig,
    /// True once degree computation has run on the current graph state.
    degree_computed: bool = false,
    /// True when the graph has changed since the last PageRank run.
    pagerank_dirty: bool = true,
    /// True when the graph has changed since the last community detection run.
    community_dirty: bool = true,
    /// True when the CSR BLOB is stale.
    csr_dirty: bool = true,

    const Self = @This();

    pub fn init(config: AlgorithmRunnerConfig) Self {
        return .{ .config = config };
    }

    // -----------------------------------------------------------------------
    // Ingestion-time operations — NEVER call from the query path.
    // -----------------------------------------------------------------------

    /// Mark all algorithm state dirty (call after adding nodes or edges).
    pub fn markDirty(self: *Self) void {
        self.degree_computed = false;
        self.pagerank_dirty = true;
        self.community_dirty = true;
        self.csr_dirty = true;
    }

    /// Compute degree centrality for all nodes and persist to `context_nodes.degree`.
    ///
    /// `library` must implement:
    ///   `computeAndPersistDegrees(arena: Allocator) !void`
    pub fn computeDegrees(self: *Self, arena: Allocator, library: anytype) !void {
        try library.computeAndPersistDegrees(arena);
        self.degree_computed = true;
    }

    /// Run PageRank power iteration and persist scores to `context_nodes.pagerank`.
    ///
    /// `library` must implement:
    ///   `computeAndPersistPageRank(arena: Allocator) !void`
    pub fn computePageRank(self: *Self, arena: Allocator, library: anytype) !void {
        try library.computeAndPersistPageRank(arena);
        self.pagerank_dirty = false;
    }

    /// Run Louvain community detection and persist `community_id` to nodes.
    ///
    /// `library` must implement:
    ///   `computeAndPersistCommunities(arena: Allocator) !void`
    pub fn computeCommunities(self: *Self, arena: Allocator, library: anytype) !void {
        try library.computeAndPersistCommunities(arena);
        self.community_dirty = false;
    }

    /// Build and serialize the CSR graph to the `csr_cache` table.
    ///
    /// `library` must implement:
    ///   `buildAndPersistCSR(arena: Allocator) !void`
    pub fn serializeCSR(self: *Self, arena: Allocator, library: anytype) !void {
        try library.buildAndPersistCSR(arena);
        self.csr_dirty = false;
    }

    // -----------------------------------------------------------------------
    // Query-time accessors — read-only, never trigger computation.
    // -----------------------------------------------------------------------

    /// Return the pre-computed degree for a node (query-safe).
    pub fn getDegree(_: Self, degree: u32) u32 {
        return degree;
    }

    /// Return the pre-computed PageRank for a node (query-safe).
    pub fn getPageRank(_: Self, pagerank: f32) f32 {
        return pagerank;
    }

    /// Return the pre-computed community ID for a node (query-safe).
    pub fn getCommunity(_: Self, community_id: ?i64) ?i64 {
        return community_id;
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "AlgorithmRunner: initial state is dirty" {
    const runner = AlgorithmRunner.init(.{});
    try testing.expect(!runner.degree_computed);
    try testing.expect(runner.pagerank_dirty);
    try testing.expect(runner.community_dirty);
    try testing.expect(runner.csr_dirty);
}

test "AlgorithmRunner: markDirty resets all flags" {
    var runner = AlgorithmRunner.init(.{});
    runner.degree_computed = true;
    runner.pagerank_dirty = false;
    runner.community_dirty = false;
    runner.csr_dirty = false;
    runner.markDirty();
    try testing.expect(!runner.degree_computed);
    try testing.expect(runner.pagerank_dirty);
    try testing.expect(runner.community_dirty);
    try testing.expect(runner.csr_dirty);
}

test "AlgorithmRunner: computeDegrees sets degree_computed" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var runner = AlgorithmRunner.init(.{});

    const MockLibrary = struct {
        pub fn computeAndPersistDegrees(_: @This(), _: Allocator) !void {}
    };
    try runner.computeDegrees(arena.allocator(), MockLibrary{});
    try testing.expect(runner.degree_computed);
}

test "AlgorithmRunner: query-time accessors return values unchanged" {
    const runner = AlgorithmRunner.init(.{});
    try testing.expectEqual(@as(u32, 42), runner.getDegree(42));
    try testing.expectApproxEqAbs(@as(f32, 0.75), runner.getPageRank(0.75), 0.001);
    try testing.expectEqual(@as(?i64, 7), runner.getCommunity(7));
    try testing.expectEqual(@as(?i64, null), runner.getCommunity(null));
}

test "AlgorithmRunner: computePageRank clears dirty flag" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var runner = AlgorithmRunner.init(.{});

    const MockLibrary = struct {
        pub fn computeAndPersistPageRank(_: @This(), _: Allocator) !void {}
    };
    try runner.computePageRank(arena.allocator(), MockLibrary{});
    try testing.expect(!runner.pagerank_dirty);
}

test "AlgorithmRunner: computeCommunities clears community dirty flag" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var runner = AlgorithmRunner.init(.{});

    const MockLibrary = struct {
        pub fn computeAndPersistCommunities(_: @This(), _: Allocator) !void {}
    };
    try runner.computeCommunities(arena.allocator(), MockLibrary{});
    try testing.expect(!runner.community_dirty);
}

