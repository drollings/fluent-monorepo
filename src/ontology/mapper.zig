/// mapper.zig — Triple → ContextNode Mapper
///
/// Transforms RDF triples into ContextNodes and edges for SQLite insertion.
/// Uses the YAGO ontology schema to route properties to appropriate LOD levels.
///
/// Design:
///   - TripleMapper accumulates nodes and edges in memory.
///   - flush() batch-inserts to SQLite via Library.
///   - processTriple() is called once per triple from the parser.
///
/// Property routing:
///   rdfs:label / skos:prefLabel → lod4_name
///   rdfs:comment               → lod0_full
///   schema:description         → lod1_summary
///   rdf:type                   → entity_types relation + ContextNode creation
///   Other properties           → edge in neighbor_of with predicate IRI as label
const std = @import("std");
const rdf = @import("rdf");
const parser_mod = rdf.parser;
const normalize_mod = rdf.normalize;
const yago = @import("yago.zig");
const db_mod = @import("coral_db");
const schema_mod = db_mod.schema;

const Triple = parser_mod.Triple;
const Term = parser_mod.Term;
const Literal = parser_mod.Literal;
const TermType = parser_mod.TermType;
const ContextNode = db_mod.ContextNode;
const Library = db_mod.Library;

// ---------------------------------------------------------------------------
// Pending node — accumulates property values before flush
// ---------------------------------------------------------------------------

/// Represents a pending node in the Zig mapping system, managing ownership and invariants for state transitions.
pub const PendingNode = struct {
    allocator: std.mem.Allocator,
    id: i64,
    lod: [schema_mod.LOD_COUNT]std.ArrayList(u8),
    types: std.ArrayList(i64), // type node IDs

    pub fn init(allocator: std.mem.Allocator, id: i64) PendingNode {
        var node = PendingNode{
            .allocator = allocator,
            .id = id,
            .lod = undefined,
            .types = .{},
        };
        for (&node.lod) |*arr| {
            arr.* = .{};
        }
        return node;
    }

    pub fn deinit(self: *PendingNode) void {
        for (&self.lod) |*arr| {
            arr.deinit(self.allocator);
        }
        self.types.deinit(self.allocator);
    }

    /// Convert to ContextNode (caller owns returned strings).
    pub fn toContextNode(self: *const PendingNode, allocator: std.mem.Allocator) !ContextNode {
        var node = ContextNode{
            .id = self.id,
            .lod = [_][]const u8{ "", "", "", "", "", "" },
            .embedding = &[_]f32{},
            .valid_from = @floatFromInt(std.time.timestamp()),
            .valid_to = null,
            .confidence = 0,
            .provenance_id = 0,
        };
        for (self.lod, 0..) |arr, i| {
            if (arr.items.len > 0) {
                node.lod[i] = try allocator.dupe(u8, arr.items);
            } else {
                node.lod[i] = try allocator.dupe(u8, "");
            }
        }
        return node;
    }
};

// ---------------------------------------------------------------------------
// Pending edge — directed edge between two node IDs
// ---------------------------------------------------------------------------

/// Represents a pending edge in the graph; managed by the owner; key invariant is its pending state.
pub const PendingEdge = struct {
    from_id: i64,
    to_id: i64,
    predicate: []const u8, // owned copy of predicate IRI
};

// ---------------------------------------------------------------------------
// Pending contradiction — conflicting literal values for same subject+predicate
// ---------------------------------------------------------------------------

/// Represents a pending contradiction in the Zig ontology, tracking unresolved conflicts with ownership and invariants.
pub const PendingContradiction = struct {
    subject_id: i64,
    predicate: []const u8, // owned copy of predicate IRI
    value_a: []const u8, // first (accepted) value — owned copy
    value_b: []const u8, // conflicting value — owned copy
};

// ---------------------------------------------------------------------------
// TripleMapper
// ---------------------------------------------------------------------------

pub const MappingConfig = struct {
    /// Preferred language for label/description selection (e.g. "en").
    preferred_lang: []const u8 = "en",
    /// Scope string for blank node hashing.
    scope: []const u8 = "default",
};

/// Manages mapping transformations between Zig types; owns state, ensures consistent key structures; key invariants preserve reference integrity.
pub const TripleMapper = struct {
    allocator: std.mem.Allocator,
    config: MappingConfig,
    /// Map from node IRI → PendingNode (key is owned)
    nodes: std.StringHashMap(PendingNode),
    /// Pending edges (predicate string is owned)
    edges: std.ArrayList(PendingEdge),
    /// Contradictions detected during mapping (all strings owned)
    contradictions: std.ArrayList(PendingContradiction),
    blank_scope: normalize_mod.BlankNodeScope,

    pub fn init(allocator: std.mem.Allocator, config: MappingConfig) !TripleMapper {
        return TripleMapper{
            .allocator = allocator,
            .config = config,
            .nodes = std.StringHashMap(PendingNode).init(allocator),
            .edges = .{},
            .contradictions = .{},
            .blank_scope = try normalize_mod.BlankNodeScope.init(allocator, config.scope),
        };
    }

    pub fn deinit(self: *TripleMapper) void {
        var it = self.nodes.iterator();
        while (it.next()) |entry| {
            self.allocator.free(entry.key_ptr.*);
            entry.value_ptr.deinit();
        }
        self.nodes.deinit();
        for (self.edges.items) |e| self.allocator.free(e.predicate);
        self.edges.deinit(self.allocator);
        for (self.contradictions.items) |c| {
            self.allocator.free(c.predicate);
            self.allocator.free(c.value_a);
            self.allocator.free(c.value_b);
        }
        self.contradictions.deinit(self.allocator);
        self.blank_scope.deinit();
    }

    /// Process one RDF triple. Accumulates nodes/edges in memory.
    pub fn processTriple(self: *TripleMapper, triple: Triple) !void {
        const subj_id = try self.termToId(triple.subject);
        const pred_iri = try self.termToIRI(triple.predicate);

        // Ensure subject node exists
        _ = try self.getOrCreateNode(triple.subject, subj_id);

        // Route by predicate
        if (std.mem.eql(u8, pred_iri, yago.NS_RDF ++ "type")) {
            // rdf:type → record type for subject
            const type_id = try self.termToId(triple.object);
            const node = self.nodes.getPtr(try self.termKey(triple.subject)).?;
            try node.types.append(node.allocator, type_id);
        } else if (std.mem.eql(u8, pred_iri, yago.NS_RDFS ++ "label") or
            std.mem.eql(u8, pred_iri, yago.NS_SKOS ++ "prefLabel"))
        {
            if (triple.object == .literal) {
                const lit = triple.object.literal;
                if (shouldUseLang(lit.lang, self.config.preferred_lang)) {
                    const node = self.nodes.getPtr(try self.termKey(triple.subject)).?;
                    try self.detectAndRecordContradiction(node, subj_id, pred_iri, 4, lit.value);
                    node.lod[4].clearRetainingCapacity();
                    try node.lod[4].appendSlice(node.allocator, lit.value);
                }
            }
        } else if (std.mem.eql(u8, pred_iri, yago.NS_RDFS ++ "comment")) {
            if (triple.object == .literal) {
                const lit = triple.object.literal;
                if (shouldUseLang(lit.lang, self.config.preferred_lang)) {
                    const node = self.nodes.getPtr(try self.termKey(triple.subject)).?;
                    try self.detectAndRecordContradiction(node, subj_id, pred_iri, 0, lit.value);
                    node.lod[0].clearRetainingCapacity();
                    try node.lod[0].appendSlice(node.allocator, lit.value);
                }
            }
        } else if (std.mem.eql(u8, pred_iri, yago.NS_SCHEMA ++ "description")) {
            if (triple.object == .literal) {
                const lit = triple.object.literal;
                if (shouldUseLang(lit.lang, self.config.preferred_lang)) {
                    const node = self.nodes.getPtr(try self.termKey(triple.subject)).?;
                    try self.detectAndRecordContradiction(node, subj_id, pred_iri, 1, lit.value);
                    node.lod[1].clearRetainingCapacity();
                    try node.lod[1].appendSlice(node.allocator, lit.value);
                }
            }
        } else if (triple.object == .iri or triple.object == .blank_node) {
            // Object property → create edge
            const obj_id = try self.termToId(triple.object);
            _ = try self.getOrCreateNode(triple.object, obj_id);
            const pred_copy = try self.allocator.dupe(u8, pred_iri);
            try self.edges.append(self.allocator, PendingEdge{
                .from_id = subj_id,
                .to_id = obj_id,
                .predicate = pred_copy,
            });
        }
        // Literal data properties that don't map to a LOD field are currently ignored.
        // Future: store in a separate attributes relation.
    }

    /// Flush all accumulated nodes and edges to SQLite via Library.
    pub fn flush(self: *TripleMapper, library: *Library) !FlushResult {
        var nodes_created: usize = 0;
        var edges_created: usize = 0;

        var it = self.nodes.iterator();
        while (it.next()) |entry| {
            const cn = try entry.value_ptr.toContextNode(self.allocator);
            defer {
                for (cn.lod) |l| {
                    self.allocator.free(l);
                }
            }
            try library.insertNode(cn);
            nodes_created += 1;

            // Insert entity_types rows
            for (entry.value_ptr.types.items) |type_id| {
                try library.insertEntityType(entry.value_ptr.id, type_id);
            }
        }

        for (self.edges.items) |edge| {
            try library.insertRdfEdge(edge.from_id, edge.to_id, edge.predicate);
            edges_created += 1;
        }

        // Persist detected contradictions
        for (self.contradictions.items) |c| {
            // Use subject_id as both node_a and node_b since the contradiction is
            // within the same entity (same subject, different incoming values).
            library.insertContradiction(
                c.subject_id,
                c.subject_id,
                c.predicate,
                c.value_a,
                c.value_b,
            ) catch |err| {
                // Log but don't abort ingestion for contradiction persistence failures.
                std.log.warn("Failed to persist contradiction for node {d}: {}", .{ c.subject_id, err });
            };
        }

        return FlushResult{
            .nodes_created = nodes_created,
            .edges_created = edges_created,
            .contradictions_detected = self.contradictions.items.len,
        };
    }

    // -------------------------------------------------------------------------
    // Contradiction detection helper
    // -------------------------------------------------------------------------

    /// If the node already has a non-empty value at lod_index that differs from
    /// new_value, record a PendingContradiction.
    fn detectAndRecordContradiction(
        self: *TripleMapper,
        node: *const PendingNode,
        subject_id: i64,
        predicate: []const u8,
        lod_index: usize,
        new_value: []const u8,
    ) !void {
        const existing = node.lod[lod_index].items;
        if (existing.len == 0) return; // no existing value — nothing to conflict with
        if (std.mem.eql(u8, existing, new_value)) return; // same value — no contradiction

        const pred_copy = try self.allocator.dupe(u8, predicate);
        errdefer self.allocator.free(pred_copy);
        const val_a = try self.allocator.dupe(u8, existing);
        errdefer self.allocator.free(val_a);
        const val_b = try self.allocator.dupe(u8, new_value);
        errdefer self.allocator.free(val_b);

        try self.contradictions.append(self.allocator, PendingContradiction{
            .subject_id = subject_id,
            .predicate = pred_copy,
            .value_a = val_a,
            .value_b = val_b,
        });
    }

    // -------------------------------------------------------------------------
    // Internal helpers
    // -------------------------------------------------------------------------

    fn termKey(self: *TripleMapper, term: Term) ![]const u8 {
        return switch (term) {
            .iri => |s| s,
            .blank_node => |s| blk: {
                // Synthesize a key "bnode:<id>" — note this is only used as hashmap key
                _ = try self.blank_scope.resolve(s);
                break :blk s; // the actual hashmap key is the raw blank node id
            },
            .literal => return error.OutOfMemory, // literals can't be subjects
        };
    }

    fn termToId(self: *TripleMapper, term: Term) !i64 {
        return switch (term) {
            .iri => |s| normalize_mod.hashIRI(s),
            .blank_node => |s| try self.blank_scope.resolve(s),
            .literal => normalize_mod.hashIRI("_literal_"),
        };
    }

    fn termToIRI(self: *TripleMapper, term: Term) ![]const u8 {
        _ = self;
        return switch (term) {
            .iri => |s| s,
            .literal => unreachable, // predicates are always IRIs after expansion
            else => return error.OutOfMemory,
        };
    }

    fn getOrCreateNode(self: *TripleMapper, term: Term, id: i64) !*PendingNode {
        const key = switch (term) {
            .iri => |s| s,
            .blank_node => |s| s,
            else => return error.OutOfMemory,
        };
        if (self.nodes.getPtr(key)) |existing| return existing;
        const owned_key = try self.allocator.dupe(u8, key);
        errdefer self.allocator.free(owned_key);
        const node = PendingNode.init(self.allocator, id);
        try self.nodes.put(owned_key, node);
        return self.nodes.getPtr(key).?;
    }
};

/// Manages flush operations with fixed-size buffers; ensures single ownership and deterministic cleanup.
pub const FlushResult = struct {
    nodes_created: usize,
    edges_created: usize,
    contradictions_detected: usize = 0,
};

/// Checks if the actual and preferred language slices match, returning true if they align.
fn shouldUseLang(actual: ?[]const u8, preferred: []const u8) bool {
    // Accept if preferred matches or if no lang tag (plain literal)
    if (actual == null) return true;
    return std.mem.eql(u8, actual.?, preferred);
}

// =============================================================================
// Tests — Milestone 2.2 (pure memory — no SQLite required)
// =============================================================================

const testing = std.testing;

test "mapper: entity from rdf:type triple" {
    var mapper = try TripleMapper.init(testing.allocator, .{});
    defer mapper.deinit();

    const src = "<http://example.org/alice> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://schema.org/Person> .";
    var p = try parser_mod.Parser.init(testing.allocator, src);
    defer p.deinit();
    const t = (try p.next()).?;
    defer t.deinit(testing.allocator);

    try mapper.processTriple(t);
    try testing.expect(mapper.nodes.count() >= 1);
}

test "mapper: label routes to lod[4]" {
    var mapper = try TripleMapper.init(testing.allocator, .{});
    defer mapper.deinit();

    const src =
        \\<http://example.org/alice> <http://www.w3.org/2000/01/rdf-schema#label> "Alice" .
    ;
    var p = try parser_mod.Parser.init(testing.allocator, src);
    defer p.deinit();
    const t = (try p.next()).?;
    defer t.deinit(testing.allocator);

    try mapper.processTriple(t);
    const node = mapper.nodes.get("http://example.org/alice").?;
    try testing.expectEqualStrings("Alice", node.lod[4].items);
}

test "mapper: comment routes to lod[0]" {
    var mapper = try TripleMapper.init(testing.allocator, .{});
    defer mapper.deinit();

    const src =
        \\<http://example.org/alice> <http://www.w3.org/2000/01/rdf-schema#comment> "A person named Alice" .
    ;
    var p = try parser_mod.Parser.init(testing.allocator, src);
    defer p.deinit();
    const t = (try p.next()).?;
    defer t.deinit(testing.allocator);

    try mapper.processTriple(t);
    const node = mapper.nodes.get("http://example.org/alice").?;
    try testing.expectEqualStrings("A person named Alice", node.lod[0].items);
}

test "mapper: object property creates edge" {
    var mapper = try TripleMapper.init(testing.allocator, .{});
    defer mapper.deinit();

    const src =
        \\<http://example.org/alice> <http://yago-knowledge.org/resource/bornIn> <http://example.org/Paris> .
    ;
    var p = try parser_mod.Parser.init(testing.allocator, src);
    defer p.deinit();
    const t = (try p.next()).?;
    defer t.deinit(testing.allocator);

    try mapper.processTriple(t);
    try testing.expectEqual(@as(usize, 1), mapper.edges.items.len);
    try testing.expectEqualStrings(
        "http://yago-knowledge.org/resource/bornIn",
        mapper.edges.items[0].predicate,
    );
}

test "mapper: multiple triples for same entity" {
    var mapper = try TripleMapper.init(testing.allocator, .{});
    defer mapper.deinit();

    const src =
        \\@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        \\@prefix schema: <http://schema.org/> .
        \\<http://example.org/alice> rdfs:label "Alice" ; rdfs:comment "A person" .
    ;
    var p = try parser_mod.Parser.init(testing.allocator, src);
    defer p.deinit();
    while (try p.next()) |t| {
        defer t.deinit(testing.allocator);
        try mapper.processTriple(t);
    }
    const node = mapper.nodes.get("http://example.org/alice").?;
    try testing.expectEqualStrings("Alice", node.lod[4].items);
    try testing.expectEqualStrings("A person", node.lod[0].items);
}

test "mapper: contradiction detection for duplicate label" {
    var mapper = try TripleMapper.init(testing.allocator, .{});
    defer mapper.deinit();

    // First label — sets lod[4]
    const src1 = "<http://example.org/alice> <http://www.w3.org/2000/01/rdf-schema#label> \"Alice\" .";
    var p1 = try parser_mod.Parser.init(testing.allocator, src1);
    defer p1.deinit();
    const t1 = (try p1.next()).?;
    defer t1.deinit(testing.allocator);
    try mapper.processTriple(t1);
    try testing.expectEqual(@as(usize, 0), mapper.contradictions.items.len);

    // Second, different label for the same entity — should be flagged
    const src2 = "<http://example.org/alice> <http://www.w3.org/2000/01/rdf-schema#label> \"Alicia\" .";
    var p2 = try parser_mod.Parser.init(testing.allocator, src2);
    defer p2.deinit();
    const t2 = (try p2.next()).?;
    defer t2.deinit(testing.allocator);
    try mapper.processTriple(t2);
    try testing.expectEqual(@as(usize, 1), mapper.contradictions.items.len);
    try testing.expectEqualStrings("Alice", mapper.contradictions.items[0].value_a);
    try testing.expectEqualStrings("Alicia", mapper.contradictions.items[0].value_b);
}

test "mapper: no contradiction for identical label" {
    var mapper = try TripleMapper.init(testing.allocator, .{});
    defer mapper.deinit();

    const src =
        \\<http://example.org/bob> <http://www.w3.org/2000/01/rdf-schema#label> "Bob" .
        \\<http://example.org/bob> <http://www.w3.org/2000/01/rdf-schema#label> "Bob" .
    ;
    var p = try parser_mod.Parser.init(testing.allocator, src);
    defer p.deinit();
    while (try p.next()) |t| {
        defer t.deinit(testing.allocator);
        try mapper.processTriple(t);
    }
    try testing.expectEqual(@as(usize, 0), mapper.contradictions.items.len);
}

test "mapper: pending node to ContextNode" {
    var mapper = try TripleMapper.init(testing.allocator, .{});
    defer mapper.deinit();

    const src =
        \\<http://example.org/bob> <http://www.w3.org/2000/01/rdf-schema#label> "Bob" .
    ;
    var p = try parser_mod.Parser.init(testing.allocator, src);
    defer p.deinit();
    const t = (try p.next()).?;
    defer t.deinit(testing.allocator);
    try mapper.processTriple(t);

    const node_pending = mapper.nodes.getPtr("http://example.org/bob").?;
    const cn = try node_pending.toContextNode(testing.allocator);
    defer {
        for (cn.lod) |l| {
            testing.allocator.free(l);
        }
    }
    try testing.expectEqualStrings("Bob", cn.lod[4]);
}
