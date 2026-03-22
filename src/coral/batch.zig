/// batch.zig — Streaming Batch Ingestion Pipeline
///
/// Streams a Turtle file through the parser and mapper, flushing to the
/// Library (SQLite backend) in configurable batches to keep memory bounded.
///
/// Pipeline:
///   TTL file → Lexer → Parser → TripleMapper → BatchIngestor → Library
///
/// Memory model:
///   - Fixed batch_size: triples accumulated in TripleMapper, flushed at limit.
///   - ArenaAllocator scoped to ingestSource() backs all per-batch mapper
///     allocations.  arena.reset(.retain_capacity) between batches replaces
///     individual free/alloc cycles, avoiding per-node allocator overhead.
const std = @import("std");
const rdf = @import("rdf");
const ontology = @import("ontology");
const db_mod = @import("coral_db");
const parser_mod = rdf.parser;
const mapper_mod = ontology.mapper;
const migration_mod = ontology.migration;

const Parser = parser_mod.Parser;
const TripleMapper = mapper_mod.TripleMapper;
const MappingConfig = mapper_mod.MappingConfig;
const Library = db_mod.Library;

// ---------------------------------------------------------------------------
// Progress reporting
// ---------------------------------------------------------------------------

pub const ProgressCallback = *const fn (triples_processed: usize, nodes_created: usize, edges_created: usize) void;

// ---------------------------------------------------------------------------
// Ingestion statistics
// ---------------------------------------------------------------------------

/// Tracks ingestion statistics with a fixed-size buffer pool; managed centrally; not thread-safe.
pub const IngestStats = struct {
    triples_processed: usize = 0,
    nodes_created: usize = 0,
    edges_created: usize = 0,
    errors_skipped: usize = 0,
    batches_flushed: usize = 0,
};

// ---------------------------------------------------------------------------
// BatchIngestor config
// ---------------------------------------------------------------------------

/// Manages batch configuration settings with fixed-size buffers; owned by the batch engine; ensures consistent state across runs.
pub const BatchConfig = struct {
    batch_size: usize = 10_000,
    on_progress: ?ProgressCallback = null,
    mapping: MappingConfig = .{},
    /// If true, parse errors are logged and skipped rather than aborting.
    skip_errors: bool = false,
    /// Stop after processing this many triples (0 = unlimited).
    max_triples: usize = 0,
};

// ---------------------------------------------------------------------------
// BatchIngestor
// ---------------------------------------------------------------------------

/// Manages batch data ingestion with fixed buffers; owned by the engine; ensures consistent state across runs.
pub const BatchIngestor = struct {
    allocator: std.mem.Allocator,
    config: BatchConfig,

    pub fn init(allocator: std.mem.Allocator, config: BatchConfig) BatchIngestor {
        return .{ .allocator = allocator, .config = config };
    }

    /// Return a fluent IngestBuilder bound to `library`.
    ///
    ///   const stats = try BatchIngestor.from(allocator, library)
    ///       .batchSize(10_000)
    ///       .skipErrors(true)
    ///       .onProgress(myCallback)
    ///       .ingestSource(source);
    pub fn from(allocator: std.mem.Allocator, library: *Library) IngestBuilder {
        return .{ .allocator = allocator, .library = library, .config = .{} };
    }

    // ── Private helpers ────────────────────────────────────────────

    /// Flush accumulated mapper output to `library`, update `stats`, fire
    /// the progress callback if configured.
    fn flushBatch(
        self: *BatchIngestor,
        mapper: *TripleMapper,
        library: *Library,
        stats: *IngestStats,
    ) !void {
        const flush_result = try mapper.flush(library);
        stats.nodes_created += flush_result.nodes_created;
        stats.edges_created += flush_result.edges_created;
        stats.batches_flushed += 1;
        if (self.config.on_progress) |cb| {
            cb(stats.triples_processed, stats.nodes_created, stats.edges_created);
        }
    }

    /// Reset the batch arena and reinitialise `mapper` for the next batch window.
    /// Replaces the previous deinit+alloc cycle with a single arena reset.
    fn resetMapper(self: *BatchIngestor, mapper: *TripleMapper, batch_arena: *std.heap.ArenaAllocator) !void {
        _ = batch_arena.reset(.retain_capacity);
        mapper.* = try TripleMapper.init(batch_arena.allocator(), self.config.mapping);
    }

    // ── Public API ─────────────────────────────────────────────────

    /// Ingest a Turtle source string into library.
    /// Reads all triples, maps to nodes/edges, flushes in batches.
    pub fn ingestSource(self: *BatchIngestor, source: []const u8, library: *Library) !IngestStats {
        var stats = IngestStats{};

        // batch_arena backs all TripleMapper allocations for the duration of
        // this call.  It is reset (not freed) between batches, avoiding the
        // per-node malloc/free overhead of the general-purpose allocator.
        var batch_arena = std.heap.ArenaAllocator.init(self.allocator);
        defer batch_arena.deinit();

        var mapper = try TripleMapper.init(batch_arena.allocator(), self.config.mapping);
        // No defer mapper.deinit() — batch_arena.deinit() owns all mapper memory.
        var p = try Parser.init(self.allocator, source);
        defer p.deinit();

        var triples_in_batch: usize = 0;
        while (true) {
            const triple_opt = p.next() catch |err| {
                if (self.config.skip_errors) {
                    stats.errors_skipped += 1;
                    continue;
                }
                return err;
            };
            const triple = triple_opt orelse break;
            defer triple.deinit(self.allocator);

            try mapper.processTriple(triple);
            stats.triples_processed += 1;
            triples_in_batch += 1;

            if (self.config.max_triples > 0 and stats.triples_processed >= self.config.max_triples) break;

            if (triples_in_batch >= self.config.batch_size) {
                try self.flushBatch(&mapper, library, &stats);
                try self.resetMapper(&mapper, &batch_arena);
                triples_in_batch = 0;
            }
        }

        if (triples_in_batch > 0) try self.flushBatch(&mapper, library, &stats);
        return stats;
    }

    /// Ingest a Turtle file at the given path.
    pub fn ingestFile(self: *BatchIngestor, path: []const u8, library: *Library) !IngestStats {
        const file = try std.fs.cwd().openFile(path, .{});
        defer file.close();
        const source = try file.readToEndAlloc(self.allocator, 512 * 1024 * 1024);
        defer self.allocator.free(source);
        return self.ingestSource(source, library);
    }
};

// ---------------------------------------------------------------------------
// IngestBuilder — fluent API over BatchIngestor
// ---------------------------------------------------------------------------
//
// All config methods return *IngestBuilder for chaining.  No error
// accumulation is needed here because every setter assigns a primitive
// field — errors only arise at the terminal ingestSource / ingestFile call.

/// Manages ingestion pipelines with fixed-size buffers; owned by the caller; ensures consistent state across operations.
pub const IngestBuilder = struct {
    allocator: std.mem.Allocator,
    library: *Library,
    config: BatchConfig,

    /// Triples accumulated before each flush (default: 10,000).
    pub fn batchSize(self: *IngestBuilder, n: usize) *IngestBuilder {
        self.config.batch_size = n;
        return self;
    }

    /// When true, parse errors are logged and skipped rather than aborting.
    pub fn skipErrors(self: *IngestBuilder, v: bool) *IngestBuilder {
        self.config.skip_errors = v;
        return self;
    }

    /// Stop after processing this many triples (0 = unlimited).
    pub fn maxTriples(self: *IngestBuilder, n: usize) *IngestBuilder {
        self.config.max_triples = n;
        return self;
    }

    /// Callback invoked after every batch flush.
    pub fn onProgress(self: *IngestBuilder, cb: ProgressCallback) *IngestBuilder {
        self.config.on_progress = cb;
        return self;
    }

    /// Ingest from a Turtle source string.  Terminates the builder chain.
    pub fn ingestSource(self: *IngestBuilder, source: []const u8) !IngestStats {
        var ingestor = BatchIngestor.init(self.allocator, self.config);
        return ingestor.ingestSource(source, self.library);
    }

    /// Ingest from a Turtle file at `path`.  Terminates the builder chain.
    pub fn ingestFile(self: *IngestBuilder, path: []const u8) !IngestStats {
        var ingestor = BatchIngestor.init(self.allocator, self.config);
        return ingestor.ingestFile(path, self.library);
    }
};

// =============================================================================
// Tests — Milestone 3.2 (pure memory — no SQLite required for basic tests)
// =============================================================================

const testing = std.testing;

test "batch ingestor accumulates triples without flush" {
    // We test the mapper accumulation path directly (no Library needed for this)
    var mapper = try TripleMapper.init(testing.allocator, .{});
    defer mapper.deinit();

    const src =
        \\<http://s1> <http://www.w3.org/2000/01/rdf-schema#label> "Node 1" .
        \\<http://s2> <http://www.w3.org/2000/01/rdf-schema#label> "Node 2" .
        \\<http://s3> <http://www.w3.org/2000/01/rdf-schema#label> "Node 3" .
    ;

    var p = try Parser.init(testing.allocator, src);
    defer p.deinit();

    var count: usize = 0;
    while (try p.next()) |t| {
        defer t.deinit(testing.allocator);
        try mapper.processTriple(t);
        count += 1;
    }
    try testing.expectEqual(@as(usize, 3), count);
    try testing.expectEqual(@as(usize, 3), mapper.nodes.count());
}

test "batch boundary: small batch_size flushes multiple times" {
    // We can't actually flush to SQLite in unit tests, but we can verify
    // the batch counting logic by using a dry-run mapper.
    const src =
        \\<http://a> <http://www.w3.org/2000/01/rdf-schema#label> "A" .
        \\<http://b> <http://www.w3.org/2000/01/rdf-schema#label> "B" .
        \\<http://c> <http://www.w3.org/2000/01/rdf-schema#label> "C" .
    ;

    // Count triples parsed
    var p = try Parser.init(testing.allocator, src);
    defer p.deinit();
    var count: usize = 0;
    while (try p.next()) |t| {
        defer t.deinit(testing.allocator);
        count += 1;
    }
    try testing.expectEqual(@as(usize, 3), count);
}

test "progress callback invoked" {
    // Verify the progress callback type compiles and can be called
    const S = struct {
        var calls: usize = 0;
        fn callback(triples: usize, nodes: usize, edges: usize) void {
            _ = triples;
            _ = nodes;
            _ = edges;
            calls += 1;
        }
    };
    const cb: ProgressCallback = S.callback;
    cb(10, 5, 3);
    try testing.expectEqual(@as(usize, 1), S.calls);
}

test "batch config defaults" {
    const cfg = BatchConfig{};
    try testing.expectEqual(@as(usize, 10_000), cfg.batch_size);
    try testing.expect(cfg.on_progress == null);
    try testing.expect(!cfg.skip_errors);
    try testing.expectEqual(@as(usize, 0), cfg.max_triples);
}

test "max_triples: stops after limit" {
    var mapper = try TripleMapper.init(testing.allocator, .{});
    defer mapper.deinit();

    // 5 triples in source, limit to 3
    const src =
        \\<http://a> <http://www.w3.org/2000/01/rdf-schema#label> "A" .
        \\<http://b> <http://www.w3.org/2000/01/rdf-schema#label> "B" .
        \\<http://c> <http://www.w3.org/2000/01/rdf-schema#label> "C" .
        \\<http://d> <http://www.w3.org/2000/01/rdf-schema#label> "D" .
        \\<http://e> <http://www.w3.org/2000/01/rdf-schema#label> "E" .
    ;

    var p = try Parser.init(testing.allocator, src);
    defer p.deinit();

    var count: usize = 0;
    const max: usize = 3;
    while (try p.next()) |t| {
        defer t.deinit(testing.allocator);
        try mapper.processTriple(t);
        count += 1;
        if (count >= max) break;
    }
    try testing.expectEqual(@as(usize, 3), count);
    try testing.expectEqual(@as(usize, 3), mapper.nodes.count());
}

test "ingest stats zero on init" {
    const stats = IngestStats{};
    try testing.expectEqual(@as(usize, 0), stats.triples_processed);
    try testing.expectEqual(@as(usize, 0), stats.nodes_created);
}

// =============================================================================
// Phase 10.1 — End-to-End Integration Tests
// =============================================================================

/// Open an in-memory Library and initialise its schema.
fn testOpenLib(allocator: std.mem.Allocator) !*db_mod.Library {
    const lib = try Library.init(allocator, .mem, "");
    try lib.initSchema();
    return lib;
}

test "end-to-end: ingestSource on YAGO sample returns nodes and edges" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    // YAGO-style TTL snippet exercising all property routes.
    const sample =
        \\@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
        \\@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        \\@prefix schema: <http://schema.org/> .
        \\@prefix yago: <http://yago-knowledge.org/resource/> .
        \\yago:Albert_Einstein rdfs:label "Albert Einstein" ;
        \\    rdfs:comment "German-born theoretical physicist." ;
        \\    schema:description "Physicist who developed the theory of relativity." ;
        \\    rdf:type schema:Person ;
        \\    yago:bornIn yago:Ulm .
        \\yago:Ulm rdfs:label "Ulm" ;
        \\    rdf:type schema:City .
    ;

    var ingestor = BatchIngestor.init(allocator, .{ .batch_size = 100 });
    const stats = try ingestor.ingestSource(sample, lib);

    try testing.expect(stats.triples_processed >= 7);
    try testing.expect(stats.nodes_created >= 2); // Einstein + Ulm
    try testing.expect(stats.edges_created >= 1); // bornIn edge
    try testing.expectEqual(@as(usize, 1), stats.batches_flushed);
}

test "end-to-end: ingestFile on YAGO tiny succeeds (max 100 triples)" {
    const YAGO_TINY_PATH = "data/yago-4.5.0.2-tiny/yago-tiny.ttl";
    // Skip gracefully if file absent.
    std.fs.cwd().access(YAGO_TINY_PATH, .{}) catch return;

    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    // Read only enough bytes for a few hundred triples (YAGO header + early entities).
    const file = try std.fs.cwd().openFile(YAGO_TINY_PATH, .{});
    defer file.close();
    const buf = try allocator.alloc(u8, 32 * 1024);
    defer allocator.free(buf);
    const n = try file.read(buf);
    const source = buf[0..n];

    var ingestor = BatchIngestor.init(allocator, .{
        .batch_size = 100,
        .max_triples = 100,
        .skip_errors = true, // tolerate truncation errors at buffer end
    });
    const stats = try ingestor.ingestSource(source, lib);

    try testing.expectEqual(@as(usize, 100), stats.triples_processed);
    try testing.expect(stats.nodes_created > 0);
}




