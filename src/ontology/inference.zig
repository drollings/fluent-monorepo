/// inference.zig — Ontology Inference Engine (R5)
///
/// Implements rdfs:subClassOf forward-chaining transitivity as the minimum
/// viable inference required for duck-typing support: a tool built for
/// "Person" will match queries about "Scientist" once the is-a chain is
/// materialised.
///
/// Supported rules (R5 scope):
///   subclass_transitivity: if A subClassOf B and B subClassOf C → A subClassOf C
///
/// Stubbed rules (future):
///   subproperty_transitivity, domain_range, inverse_of
const std = @import("std");
const rdf = @import("rdf");
const Triple = rdf.Triple;

// We re-import rdf parser types for Triple construction.
const parser_mod = rdf.parser;
const Term = parser_mod.Term;
const TermType = parser_mod.TermType;

/// Defines inference rules with plain English semantics, manages ownership, ensures invariants; key model is rule-based inference.
pub const RuleType = enum {
    subclass_transitivity,
    subproperty_transitivity,
    domain_range,
    inverse_of,
};

pub const InferenceRule = struct {
    rule_type: RuleType,
    /// Source predicate IRI (e.g. rdfs:subClassOf)
    trigger_predicate: []const u8,
};

/// Ontology inference engine.
///
/// Call `addRule` to register rules, then `infer(triples)` to materialise
/// derived triples.  The engine is stateless between `infer` calls.
pub const InferenceEngine = struct {
    allocator: std.mem.Allocator,
    rules: std.ArrayList(InferenceRule),

    pub fn init(allocator: std.mem.Allocator) InferenceEngine {
        return .{
            .allocator = allocator,
            .rules = std.ArrayList(InferenceRule).init(allocator),
        };
    }

    pub fn deinit(self: *InferenceEngine) void {
        self.rules.deinit();
    }

    pub fn addRule(self: *InferenceEngine, rule: InferenceRule) !void {
        try self.rules.append(rule);
    }

    /// Materialise derived triples from `input` for all registered rules.
    ///
    /// Returns a caller-owned slice.  Each Triple in the slice owns its
    /// subject/predicate/object strings (allocated from `self.allocator`).
    /// Free with: for (result) |t| t.deinit(allocator); allocator.free(result);
    pub fn infer(self: *InferenceEngine, triples: []const Triple) ![]Triple {
        var derived = std.ArrayList(Triple).init(self.allocator);
        errdefer {
            for (derived.items) |t| t.deinit(self.allocator);
            derived.deinit();
        }

        for (self.rules.items) |rule| {
            switch (rule.rule_type) {
                .subclass_transitivity => {
                    try inferSubclassTransitivity(self.allocator, triples, &derived, rule.trigger_predicate);
                },
                else => {}, // stubbed
            }
        }

        return derived.toOwnedSlice();
    }

    /// Persist inferred edges to SQLite via Library.
    /// Currently a no-op stub (see materializeInto for the Library variant).
    pub fn materialize(self: *InferenceEngine) !void {
        _ = self;
    }
};

// ---------------------------------------------------------------------------
// subClassOf transitivity: A subClassOf B, B subClassOf C → A subClassOf C
// ---------------------------------------------------------------------------

/// Build the transitive closure of `rdfs:subClassOf` edges using iterative
/// forward chaining.  New triples are appended to `derived`.
///
/// Algorithm: repeat until no new triples are produced.
///   For each triple (A, subClassOf, B) and (B, subClassOf, C):
///     if (A, subClassOf, C) not already in known set → emit it.
fn inferSubclassTransitivity(
    allocator: std.mem.Allocator,
    base: []const Triple,
    derived: *std.ArrayList(Triple),
    predicate_iri: []const u8,
) !void {
    // Combine base + previously derived into a working set (pointers only, no copies).
    // We use a StringHashMap<StringHashMap<void>> for fast (subject, object) lookup.
    const Edge = struct { subject: []const u8, object: []const u8 };

    var known = std.HashMap(Edge, void, struct {
        pub fn hash(_: @This(), k: Edge) u64 {
            var h = std.hash.Wyhash.init(0);
            h.update(k.subject);
            h.update("|");
            h.update(k.object);
            return h.final();
        }
        pub fn eql(_: @This(), a: Edge, b: Edge) bool {
            return std.mem.eql(u8, a.subject, b.subject) and
                std.mem.eql(u8, a.object, b.object);
        }
    }, 80).init(allocator);
    defer known.deinit();

    // Seed known set from base triples.
    for (base) |t| {
        if (!isSubclassOf(t, predicate_iri)) continue;
        const s = tripleSubjectIri(t) orelse continue;
        const o = tripleObjectIri(t) orelse continue;
        try known.put(.{ .subject = s, .object = o }, {});
    }
    // Seed from already-derived triples.
    for (derived.items) |t| {
        if (!isSubclassOf(t, predicate_iri)) continue;
        const s = tripleSubjectIri(t) orelse continue;
        const o = tripleObjectIri(t) orelse continue;
        try known.put(.{ .subject = s, .object = o }, {});
    }

    // Iterative fixpoint.
    var changed = true;
    while (changed) {
        changed = false;

        // Collect current edge list (stable snapshot).
        var edges = std.ArrayList(Edge).init(allocator);
        defer edges.deinit();
        var kit = known.keyIterator();
        while (kit.next()) |k| try edges.append(k.*);

        for (edges.items) |ab| { // A → B
            for (edges.items) |bc| { // B → C
                if (!std.mem.eql(u8, ab.object, bc.subject)) continue;
                const ac = Edge{ .subject = ab.subject, .object = bc.object };
                if (known.contains(ac)) continue;
                // New transitive edge: emit triple.
                const new_triple = try buildSubclassTriple(allocator, ac.subject, predicate_iri, ac.object);
                try derived.append(new_triple);
                try known.put(ac, {});
                changed = true;
            }
        }
    }
}

fn isSubclassOf(t: Triple, predicate_iri: []const u8) bool {
    return t.predicate == .iri and std.mem.eql(u8, t.predicate.iri, predicate_iri);
}

fn tripleSubjectIri(t: Triple) ?[]const u8 {
    return switch (t.subject) {
        .iri => |s| s,
        else => null,
    };
}

fn tripleObjectIri(t: Triple) ?[]const u8 {
    return switch (t.object) {
        .iri => |s| s,
        else => null,
    };
}

/// Allocate a new Triple with owned IRI strings.
fn buildSubclassTriple(
    allocator: std.mem.Allocator,
    subject_iri: []const u8,
    predicate_iri: []const u8,
    object_iri: []const u8,
) !Triple {
    const s = try allocator.dupe(u8, subject_iri);
    errdefer allocator.free(s);
    const p = try allocator.dupe(u8, predicate_iri);
    errdefer allocator.free(p);
    const o = try allocator.dupe(u8, object_iri);
    errdefer allocator.free(o);
    return Triple{
        .subject = .{ .iri = s },
        .predicate = .{ .iri = p },
        .object = .{ .iri = o },
    };
}

// =============================================================================
// Tests
// =============================================================================

const testing = std.testing;
const RDFS_SUBCLASS_OF = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

test "inference stub returns empty triples" {
    var engine = InferenceEngine.init(testing.allocator);
    defer engine.deinit();
    const result = try engine.infer(&[_]Triple{});
    defer testing.allocator.free(result);
    try testing.expectEqual(@as(usize, 0), result.len);
}

test "inference stub materialize is no-op" {
    var engine = InferenceEngine.init(testing.allocator);
    defer engine.deinit();
    try engine.materialize();
}

test "inference rule addition" {
    var engine = InferenceEngine.init(testing.allocator);
    defer engine.deinit();
    try engine.addRule(.{
        .rule_type = .subclass_transitivity,
        .trigger_predicate = RDFS_SUBCLASS_OF,
    });
    try testing.expectEqual(@as(usize, 1), engine.rules.items.len);
    try testing.expectEqual(RuleType.subclass_transitivity, engine.rules.items[0].rule_type);
}

test "subClassOf transitivity: A→B, B→C gives A→C" {
    // Build triples: Scientist subClassOf Person, Person subClassOf Agent
    const a_b = try buildSubclassTriple(testing.allocator, "Scientist", RDFS_SUBCLASS_OF, "Person");
    defer a_b.deinit(testing.allocator);
    const b_c = try buildSubclassTriple(testing.allocator, "Person", RDFS_SUBCLASS_OF, "Agent");
    defer b_c.deinit(testing.allocator);

    var engine = InferenceEngine.init(testing.allocator);
    defer engine.deinit();
    try engine.addRule(.{ .rule_type = .subclass_transitivity, .trigger_predicate = RDFS_SUBCLASS_OF });

    const derived = try engine.infer(&[_]Triple{ a_b, b_c });
    defer {
        for (derived) |t| t.deinit(testing.allocator);
        testing.allocator.free(derived);
    }

    // Expect exactly one derived triple: Scientist subClassOf Agent
    try testing.expectEqual(@as(usize, 1), derived.len);
    try testing.expectEqualStrings("Scientist", derived[0].subject.iri);
    try testing.expectEqualStrings("Agent", derived[0].object.iri);
}

test "subClassOf transitivity: longer chain A→B→C→D" {
    const ab = try buildSubclassTriple(testing.allocator, "Developer", RDFS_SUBCLASS_OF, "Programmer");
    defer ab.deinit(testing.allocator);
    const bc = try buildSubclassTriple(testing.allocator, "Programmer", RDFS_SUBCLASS_OF, "Person");
    defer bc.deinit(testing.allocator);
    const cd = try buildSubclassTriple(testing.allocator, "Person", RDFS_SUBCLASS_OF, "Agent");
    defer cd.deinit(testing.allocator);

    var engine = InferenceEngine.init(testing.allocator);
    defer engine.deinit();
    try engine.addRule(.{ .rule_type = .subclass_transitivity, .trigger_predicate = RDFS_SUBCLASS_OF });

    const derived = try engine.infer(&[_]Triple{ ab, bc, cd });
    defer {
        for (derived) |t| t.deinit(testing.allocator);
        testing.allocator.free(derived);
    }

    // Expected: Developer→Person, Developer→Agent, Programmer→Agent (3 new edges)
    try testing.expectEqual(@as(usize, 3), derived.len);
}

test "subClassOf transitivity: no new edges when already transitive" {
    const ab = try buildSubclassTriple(testing.allocator, "Cat", RDFS_SUBCLASS_OF, "Animal");
    defer ab.deinit(testing.allocator);

    var engine = InferenceEngine.init(testing.allocator);
    defer engine.deinit();
    try engine.addRule(.{ .rule_type = .subclass_transitivity, .trigger_predicate = RDFS_SUBCLASS_OF });

    const derived = try engine.infer(&[_]Triple{ab});
    defer {
        for (derived) |t| t.deinit(testing.allocator);
        testing.allocator.free(derived);
    }

    try testing.expectEqual(@as(usize, 0), derived.len);
}

