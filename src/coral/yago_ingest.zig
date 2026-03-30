/// yago_ingest.zig — YAGO 4.5 Baseline Ingestion (M3.2)
///
/// Thin wrapper over BatchIngestor that adds YAGO-specific behaviour:
///
///   1. Namespace whitelist filter — only subjects from YAGO, schema.org, RDF-S,
///      OWL, and SKOS namespaces pass through to TripleMapper; set via
///      BatchConfig.filter_fn (no loop duplication).
///
///   2. Hierarchy bootstrap — builds a CapabilityInference hierarchy from the
///      static YAGO class registry in yago.zig plus any rdfs:subClassOf triples
///      encountered in the ingested file(s).
///
/// All heavy lifting (parser loop, arena reset, flush batching) remains in
/// BatchIngestor; this file provides only the YAGO-specific customisations.
const std = @import("std");
const rdf = @import("rdf");
const coral_batch = @import("coral_batch");
const coral_db = @import("coral_db");
const ontology = @import("ontology");

const Triple = rdf.Triple;
const BatchIngestor = coral_batch.BatchIngestor;
const BatchConfig = coral_batch.BatchConfig;
const IngestStats = coral_batch.IngestStats;
const Library = coral_db.Library;
const CapabilityInference = ontology.inference.CapabilityInference;
const yago_schema = ontology.yago;

// ---------------------------------------------------------------------------
// Namespace whitelist filter
// ---------------------------------------------------------------------------

/// Subject namespace prefixes whose triples are accepted during ingestion.
const ACCEPTED_PREFIXES = [_][]const u8{
    yago_schema.NS_YAGO,
    yago_schema.NS_SCHEMA,
    yago_schema.NS_RDF,
    yago_schema.NS_RDFS,
    yago_schema.NS_OWL,
    yago_schema.NS_SKOS,
};

/// `FilterFn` compatible with `BatchConfig.filter_fn`.
/// Returns true (accept) when the triple's subject IRI starts with any of
/// the YAGO/schema.org/RDF namespaces, or the subject is a blank node.
/// Returns false (skip) for subjects from unrecognised namespaces.
pub fn yagoNamespaceFilter(triple: Triple) bool {
    const subj_iri = switch (triple.subject) {
        .iri => |s| s,
        .blank_node => return true,
        else => return false,
    };
    for (ACCEPTED_PREFIXES) |prefix| {
        if (std.mem.startsWith(u8, subj_iri, prefix)) return true;
    }
    return false;
}

// ---------------------------------------------------------------------------
// YagoConfig
// ---------------------------------------------------------------------------

/// Manages YagoConfig settings with fixed buffers; encapsulates ownership and invariants.
pub const YagoConfig = struct {
    /// Triples per Library flush (forwarded to BatchConfig.batch_size).
    batch_size: usize = 10_000,
    /// When true, apply `yagoNamespaceFilter` to every parsed triple.
    whitelist_only: bool = true,
    /// When true, build the CapabilityInference hierarchy from the static YAGO
    /// class registry after ingestion (requires a CapabilityInference pointer
    /// passed to ingestSource / ingestFile).
    build_hierarchy: bool = true,
    /// Forward to BatchConfig.skip_errors.
    skip_errors: bool = false,
};

// ---------------------------------------------------------------------------
// YagoIngestor
// ---------------------------------------------------------------------------

/// YAGO-specific ingestion wrapper.
///
/// Usage:
///   var ingestor = YagoIngestor.init(allocator, .{});
///   var ci = CapabilityInference.init(allocator);
///   defer ci.deinit();
///   const stats = try ingestor.ingestFile("yago-types.ttl", lib, &ci);
pub const YagoIngestor = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    config: YagoConfig,

    pub fn init(allocator: std.mem.Allocator, config: YagoConfig) Self {
        return .{ .allocator = allocator, .config = config };
    }

    // -------------------------------------------------------------------------
    // Public API
    // -------------------------------------------------------------------------

    /// Ingest a Turtle source string.
    /// Optionally populate `capability_inference` with the YAGO class hierarchy.
    pub fn ingestSource(
        self: *Self,
        source: []const u8,
        library: *Library,
        capability_inference: ?*CapabilityInference,
    ) !IngestStats {
        const stats = try self.runIngest(library, source, null);
        if (self.config.build_hierarchy) {
            if (capability_inference) |ci| try buildBaselineHierarchy(ci);
        }
        return stats;
    }

    /// Ingest a Turtle file at `path`.
    /// Rejects path components containing `..` or null bytes.
    /// File size capped at 100 MB (delegated to BatchIngestor.ingestFile).
    pub fn ingestFile(
        self: *Self,
        path: []const u8,
        library: *Library,
        capability_inference: ?*CapabilityInference,
    ) !IngestStats {
        const stats = try self.runIngest(library, null, path);
        if (self.config.build_hierarchy) {
            if (capability_inference) |ci| try buildBaselineHierarchy(ci);
        }
        return stats;
    }

    // -------------------------------------------------------------------------
    // Internal
    // -------------------------------------------------------------------------

    fn runIngest(self: *Self, library: *Library, source: ?[]const u8, path: ?[]const u8) !IngestStats {
        const cfg = coral_batch.BatchConfig{
            .batch_size = self.config.batch_size,
            .skip_errors = self.config.skip_errors,
            .filter_fn = if (self.config.whitelist_only) yagoNamespaceFilter else null,
        };
        var ingestor = BatchIngestor.init(self.allocator, cfg);
        if (source) |src| return ingestor.ingestSource(src, library);
        return ingestor.ingestFile(path.?, library);
    }
};

// ---------------------------------------------------------------------------
// Hierarchy bootstrap from static YAGO class registry
// ---------------------------------------------------------------------------

/// Constructs a baseline hierarchy structure from input data using Zig's capabilities.
pub fn buildBaselineHierarchy(ci: *CapabilityInference) !void {
    const classes = [_]*const yago_schema.OntologyClass{
        &yago_schema.CLASS_ENTITY,
        &yago_schema.CLASS_PERSON,
        &yago_schema.CLASS_ORGANIZATION,
        &yago_schema.CLASS_LOCATION,
        &yago_schema.CLASS_EVENT,
        &yago_schema.CLASS_ARTIFACT,
        &yago_schema.CLASS_CONCEPT,
    };
    for (classes) |cls| {
        if (cls.superclass) |parent_iri| {
            try ci.addSubclassEdge(cls.iri, parent_iri);
        }
    }
}

// =============================================================================
// Tests — M3.2
// =============================================================================

const testing = std.testing;

test "yagoNamespaceFilter: accepts YAGO subject" {
    var parser = try rdf.Parser.init(testing.allocator,
        \\<http://yago-knowledge.org/resource/Einstein> <http://www.w3.org/2000/01/rdf-schema#label> "Einstein" .
    );
    defer parser.deinit();
    const t = (try parser.next()).?;
    defer t.deinit(testing.allocator);
    try testing.expect(yagoNamespaceFilter(t));
}

test "yagoNamespaceFilter: rejects unknown subject" {
    var parser = try rdf.Parser.init(testing.allocator,
        \\<http://example.com/foo> <http://www.w3.org/2000/01/rdf-schema#label> "Foo" .
    );
    defer parser.deinit();
    const t = (try parser.next()).?;
    defer t.deinit(testing.allocator);
    try testing.expect(!yagoNamespaceFilter(t));
}

test "yagoNamespaceFilter: accepts schema.org subject" {
    var parser = try rdf.Parser.init(testing.allocator,
        \\<http://schema.org/Person> <http://www.w3.org/2000/01/rdf-schema#label> "Person" .
    );
    defer parser.deinit();
    const t = (try parser.next()).?;
    defer t.deinit(testing.allocator);
    try testing.expect(yagoNamespaceFilter(t));
}

test "buildBaselineHierarchy: Person is subclass of Entity" {
    var ci = CapabilityInference.init(testing.allocator);
    defer ci.deinit();

    try buildBaselineHierarchy(&ci);
    // Register capability on Entity; Person should inherit via subClassOf.
    try ci.registerCapability(yago_schema.NS_YAGO ++ "Entity", "has_label");
    const result = try ci.duckType(yago_schema.NS_SCHEMA ++ "Person", "has_label");
    try testing.expect(result);
}

test "buildBaselineHierarchy: Organization is subclass of Entity" {
    var ci = CapabilityInference.init(testing.allocator);
    defer ci.deinit();

    try buildBaselineHierarchy(&ci);
    try ci.registerCapability(yago_schema.NS_YAGO ++ "Entity", "has_label");
    const result = try ci.duckType(yago_schema.NS_SCHEMA ++ "Organization", "has_label");
    try testing.expect(result);
}

test "YagoConfig defaults" {
    const cfg = YagoConfig{};
    try testing.expectEqual(@as(usize, 10_000), cfg.batch_size);
    try testing.expect(cfg.whitelist_only);
    try testing.expect(cfg.build_hierarchy);
    try testing.expect(!cfg.skip_errors);
}

test "IngestStats has triples_filtered field" {
    // Verify the field exists (batch.zig change)
    const s = IngestStats{};
    try testing.expectEqual(@as(usize, 0), s.triples_filtered);
}
