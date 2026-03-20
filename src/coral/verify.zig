/// verify.zig — Ingestion Verification and Integrity Checking
///
/// Checks that the ingested data is complete and consistent.
/// Produces an IngestionReport with counts, errors, and warnings.
const std = @import("std");
const mapper_mod = @import("../ontology/mapper.zig");
const TripleMapper = mapper_mod.TripleMapper;

// ---------------------------------------------------------------------------
// Report types
// ---------------------------------------------------------------------------

pub const IngestionErrorKind = enum {
    count_mismatch,
    orphan_node,
    invalid_iri,
    missing_label,
    dangling_blank_node,
};

pub const IngestionWarningKind = enum {
    no_type,
    no_description,
    blank_node_heavy,
};

pub const IngestionError = struct {
    kind: IngestionErrorKind,
    message: []const u8, // owned
};

pub const IngestionWarning = struct {
    kind: IngestionWarningKind,
    message: []const u8, // owned
};

pub const IngestionReport = struct {
    allocator: std.mem.Allocator,
    triples_total: usize,
    nodes_created: usize,
    edges_created: usize,
    errors: std.ArrayList(IngestionError),
    warnings: std.ArrayList(IngestionWarning),

    pub fn init(allocator: std.mem.Allocator) IngestionReport {
        return .{
            .allocator = allocator,
            .triples_total = 0,
            .nodes_created = 0,
            .edges_created = 0,
            .errors = .{},
            .warnings = .{},
        };
    }

    pub fn deinit(self: *IngestionReport) void {
        for (self.errors.items) |e| self.allocator.free(e.message);
        self.errors.deinit(self.allocator);
        for (self.warnings.items) |w| self.allocator.free(w.message);
        self.warnings.deinit(self.allocator);
    }

    pub fn hasErrors(self: *const IngestionReport) bool {
        return self.errors.items.len > 0;
    }
};

// ---------------------------------------------------------------------------
// Verifier
// ---------------------------------------------------------------------------

pub const VerifierConfig = struct {
    /// Expected triple count (0 = unchecked)
    expected_triples: usize = 0,
    /// Expected node count (0 = unchecked)
    expected_nodes: usize = 0,
    warn_missing_labels: bool = true,
    warn_missing_types: bool = true,
};

pub const Verifier = struct {
    allocator: std.mem.Allocator,
    config: VerifierConfig,

    pub fn init(allocator: std.mem.Allocator, config: VerifierConfig) Verifier {
        return .{ .allocator = allocator, .config = config };
    }

    /// Verify mapper state (before flush) for basic integrity.
    pub fn verifyMapper(self: *Verifier, mapper: *const TripleMapper, report: *IngestionReport) !void {
        report.nodes_created = mapper.nodes.count();
        report.edges_created = mapper.edges.items.len;

        // Check expected node count
        if (self.config.expected_nodes > 0 and
            report.nodes_created != self.config.expected_nodes)
        {
            const msg = try std.fmt.allocPrint(
                self.allocator,
                "Node count mismatch: expected {d}, got {d}",
                .{ self.config.expected_nodes, report.nodes_created },
            );
            try report.errors.append(report.allocator, .{ .kind = .count_mismatch, .message = msg });
        }

        // Check for nodes with no label
        if (self.config.warn_missing_labels) {
            var it = mapper.nodes.valueIterator();
            while (it.next()) |node| {
                if (node.lod[4].items.len == 0) {
                    const msg = try std.fmt.allocPrint(
                        self.allocator,
                        "Node id={d} has no label",
                        .{node.id},
                    );
                    try report.warnings.append(report.allocator, .{ .kind = .no_type, .message = msg });
                }
            }
        }

        // Orphan detection is deferred to post-flush CozoDB queries.
        // TODO: implement after flush() is integrated with CozoDB.
    }

    /// Verify statistics from completed ingestion.
    pub fn verifyStats(self: *Verifier, triples: usize, nodes: usize, edges: usize, report: *IngestionReport) !void {
        report.triples_total = triples;
        report.nodes_created = nodes;
        report.edges_created = edges;

        if (self.config.expected_triples > 0 and triples != self.config.expected_triples) {
            const msg = try std.fmt.allocPrint(
                self.allocator,
                "Triple count mismatch: expected {d}, got {d}",
                .{ self.config.expected_triples, triples },
            );
            try report.errors.append(report.allocator, .{ .kind = .count_mismatch, .message = msg });
        }
    }
};

// =============================================================================
// Tests — Milestone 3.4
// =============================================================================

const testing = std.testing;
const parser_mod2 = @import("../rdf/parser.zig");

test "report initialized empty" {
    var report = IngestionReport.init(testing.allocator);
    defer report.deinit();
    try testing.expect(!report.hasErrors());
    try testing.expectEqual(@as(usize, 0), report.errors.items.len);
}

test "verify stats count match" {
    var v = Verifier.init(testing.allocator, .{ .expected_triples = 10 });
    var report = IngestionReport.init(testing.allocator);
    defer report.deinit();
    try v.verifyStats(10, 5, 3, &report);
    try testing.expect(!report.hasErrors());
}

test "verify stats count mismatch" {
    var v = Verifier.init(testing.allocator, .{ .expected_triples = 10 });
    var report = IngestionReport.init(testing.allocator);
    defer report.deinit();
    try v.verifyStats(9, 5, 3, &report);
    try testing.expect(report.hasErrors());
    try testing.expectEqual(IngestionErrorKind.count_mismatch, report.errors.items[0].kind);
}

test "verify mapper with labelled nodes - no warnings" {
    var mapper = try TripleMapper.init(testing.allocator, .{});
    defer mapper.deinit();

    const src =
        \\<http://example.org/alice> <http://www.w3.org/2000/01/rdf-schema#label> "Alice" .
    ;
    var p = try parser_mod2.Parser.init(testing.allocator, src);
    defer p.deinit();
    while (try p.next()) |t| {
        defer t.deinit(testing.allocator);
        try mapper.processTriple(t);
    }

    var v = Verifier.init(testing.allocator, .{ .warn_missing_labels = true });
    var report = IngestionReport.init(testing.allocator);
    defer report.deinit();
    try v.verifyMapper(&mapper, &report);
    // Alice has a label, so no warnings
    try testing.expectEqual(@as(usize, 0), report.warnings.items.len);
}

test "verify mapper missing label generates warning" {
    var mapper = try TripleMapper.init(testing.allocator, .{});
    defer mapper.deinit();

    // No rdfs:label — node has empty name
    const src =
        \\<http://example.org/unknown> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://schema.org/Person> .
    ;
    var p = try parser_mod2.Parser.init(testing.allocator, src);
    defer p.deinit();
    while (try p.next()) |t| {
        defer t.deinit(testing.allocator);
        try mapper.processTriple(t);
    }

    var v = Verifier.init(testing.allocator, .{ .warn_missing_labels = true });
    var report = IngestionReport.init(testing.allocator);
    defer report.deinit();
    try v.verifyMapper(&mapper, &report);
    // At least one node (unknown) should generate a warning
    try testing.expect(report.warnings.items.len >= 1);
}

test "report serializes error count" {
    var report = IngestionReport.init(testing.allocator);
    defer report.deinit();
    const msg = try testing.allocator.dupe(u8, "test error");
    try report.errors.append(report.allocator, .{ .kind = .count_mismatch, .message = msg });
    try testing.expectEqual(@as(usize, 1), report.errors.items.len);
    try testing.expect(report.hasErrors());
}
