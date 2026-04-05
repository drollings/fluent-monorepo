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

/// Manages inference logic with a structured keyword structure, owns inference pipelines, and enforces invariants on ownership and state.
pub const InferenceEngine = struct {
    allocator: std.mem.Allocator,
    rules: std.ArrayList(InferenceRule),

    pub fn init(allocator: std.mem.Allocator) InferenceEngine {
        return .{
            .allocator = allocator,
            .rules = .{},
        };
    }

    pub fn deinit(self: *InferenceEngine) void {
        self.rules.deinit(self.allocator);
    }

    pub fn addRule(self: *InferenceEngine, rule: InferenceRule) !void {
        try self.rules.append(self.allocator, rule);
    }

    /// Materialise derived triples from `input` for all registered rules.
    ///
    /// Returns a caller-owned slice.  Each Triple in the slice owns its
    /// subject/predicate/object strings (allocated from `self.allocator`).
    /// Free with: for (result) |t| t.deinit(allocator); allocator.free(result);
    pub fn infer(self: *InferenceEngine, triples: []const Triple) ![]Triple {
        var derived: std.ArrayList(Triple) = .{};
        errdefer {
            for (derived.items) |t| t.deinit(self.allocator);
            derived.deinit(self.allocator);
        }

        for (self.rules.items) |rule| {
            switch (rule.rule_type) {
                .subclass_transitivity => {
                    try inferSubclassTransitivity(self.allocator, triples, &derived, rule.trigger_predicate);
                },
                else => {}, // stubbed
            }
        }

        return derived.toOwnedSlice(self.allocator);
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

/// Checks transitivity between subclasses using an allocator and predicate, returning a boolean result.
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
        if (!isSubclassOfTriple(t, predicate_iri)) continue;
        const s = tripleSubjectIri(t) orelse continue;
        const o = tripleObjectIri(t) orelse continue;
        try known.put(.{ .subject = s, .object = o }, {});
    }
    // Seed from already-derived triples.
    for (derived.items) |t| {
        if (!isSubclassOfTriple(t, predicate_iri)) continue;
        const s = tripleSubjectIri(t) orelse continue;
        const o = tripleObjectIri(t) orelse continue;
        try known.put(.{ .subject = s, .object = o }, {});
    }

    // Iterative fixpoint.
    var changed = true;
    while (changed) {
        changed = false;

        // Collect current edge list (stable snapshot).
        var edges: std.ArrayList(Edge) = .{};
        defer edges.deinit(allocator);
        var kit = known.keyIterator();
        while (kit.next()) |k| try edges.append(allocator, k.*);

        for (edges.items) |ab| { // A → B
            for (edges.items) |bc| { // B → C
                if (!std.mem.eql(u8, ab.object, bc.subject)) continue;
                const ac = Edge{ .subject = ab.subject, .object = bc.object };
                if (known.contains(ac)) continue;
                // New transitive edge: emit triple.
                const new_triple = try buildSubclassTriple(allocator, ac.subject, predicate_iri, ac.object);
                try derived.append(allocator, new_triple);
                try known.put(ac, {});
                changed = true;
            }
        }
    }
}

/// Checks if a triple is a subclass of a given predicate IRI, returning true or false.
fn isSubclassOfTriple(t: Triple, predicate_iri: []const u8) bool {
    return t.predicate == .iri and std.mem.eql(u8, t.predicate.iri, predicate_iri);
}

/// Transforms a Triple into a list of triples using IRI pattern matching.
fn tripleSubjectIri(t: Triple) ?[]const u8 {
    return switch (t.subject) {
        .iri => |s| s,
        else => null,
    };
}

/// Transforms a triple into a list of triples using IRI mapping.
fn tripleObjectIri(t: Triple) ?[]const u8 {
    return switch (t.object) {
        .iri => |s| s,
        else => null,
    };
}

/// Constructs a triple from IRI components, returning a structured representation.
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
// M3.1 — CapabilityInference: duck-typing via rdfs:subClassOf hierarchy
// =============================================================================
//
// Connects the ontology subclass hierarchy to capability matching so that a
// tool registered for "Person" also matches queries about "Scientist" once the
// is-a chain is materialised from YAGO triples.
//
// Usage:
//   var ci = CapabilityInference.init(allocator);
//   defer ci.deinit();
//   try ci.loadHierarchy(triples, RDFS_SUBCLASS_OF);  // from YAGO ingest
//   const has_cap = try ci.duckType("Scientist", "has_birth_date");

/// Map from class IRI to all its direct superclass IRIs.
/// Key is arena-owned; values list is arena-owned.
const HierarchyMap = std.StringHashMapUnmanaged([][]const u8);

/// Map from class IRI to the accumulated capability set (string names).
/// Represents: class C can fulfil capability K if K ∈ capability_cache[C].
const CapabilitySet = std.StringHashMapUnmanaged(void);
const CapabilityCache = std.StringHashMapUnmanaged(CapabilitySet);

/// Manages inference capabilities with a fixed-size structure, ensuring ownership and invariants on initialization/deinit.
pub const CapabilityInference = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    /// Arena used for all internal string copies and list allocations.
    arena: std.heap.ArenaAllocator,
    /// Class IRI → direct superclass IRIs.
    hierarchy: HierarchyMap,
    /// Class IRI → set of capability names provided by that class (direct, not inherited).
    direct_capabilities: CapabilityCache,
    /// Class IRI → set of inherited capability names (computed lazily, cached).
    inferred_cache: CapabilityCache,

    pub fn init(allocator: std.mem.Allocator) Self {
        return .{
            .allocator = allocator,
            .arena = std.heap.ArenaAllocator.init(allocator),
            .hierarchy = .{},
            .direct_capabilities = .{},
            .inferred_cache = .{},
        };
    }

    pub fn deinit(self: *Self) void {
        self.hierarchy.deinit(self.allocator);
        // CapabilitySets inside caches share arena memory; just clear the maps.
        var dit = self.direct_capabilities.iterator();
        while (dit.next()) |entry| entry.value_ptr.deinit(self.allocator);
        self.direct_capabilities.deinit(self.allocator);
        var iit = self.inferred_cache.iterator();
        while (iit.next()) |entry| entry.value_ptr.deinit(self.allocator);
        self.inferred_cache.deinit(self.allocator);
        self.arena.deinit();
    }

    /// Populate the class hierarchy from a slice of RDF triples.
    /// Only triples with `predicate_iri` (typically rdfs:subClassOf) are processed.
    /// Strings are copied into the arena so the input triples may be freed afterwards.
    pub fn loadHierarchy(
        self: *Self,
        triples: []const Triple,
        predicate_iri: []const u8,
    ) !void {
        const a = self.arena.allocator();
        for (triples) |t| {
            if (!isSubclassOfTriple(t, predicate_iri)) continue;
            const child_iri = tripleSubjectIri(t) orelse continue;
            const parent_iri = tripleObjectIri(t) orelse continue;

            const child_owned = try a.dupe(u8, child_iri);
            const parent_owned = try a.dupe(u8, parent_iri);

            // Invalidate any inferred cache entry for this class.
            self.invalidate(child_iri);

            // Append parent to child's superclass list.
            const gop = try self.hierarchy.getOrPut(self.allocator, child_owned);
            if (!gop.found_existing) {
                gop.value_ptr.* = &[_][]const u8{};
            }
            const old = gop.value_ptr.*;
            const new_list = try a.alloc([]const u8, old.len + 1);
            @memcpy(new_list[0..old.len], old);
            new_list[old.len] = parent_owned;
            gop.value_ptr.* = new_list;
        }
    }

    /// Add a single rdfs:subClassOf edge without requiring a Triple slice.
    /// Useful for building the hierarchy from static registries or DB query results.
    /// Strings are copied into the arena; `child_iri` and `parent_iri` may be freed afterward.
    pub fn addSubclassEdge(self: *Self, child_iri: []const u8, parent_iri: []const u8) !void {
        const a = self.arena.allocator();
        const child_owned = try a.dupe(u8, child_iri);
        const parent_owned = try a.dupe(u8, parent_iri);
        self.invalidate(child_iri);
        const gop = try self.hierarchy.getOrPut(self.allocator, child_owned);
        if (!gop.found_existing) {
            gop.value_ptr.* = &[_][]const u8{};
        }
        const old = gop.value_ptr.*;
        const new_list = try a.alloc([]const u8, old.len + 1);
        @memcpy(new_list[0..old.len], old);
        new_list[old.len] = parent_owned;
        gop.value_ptr.* = new_list;
    }

    /// Register a direct capability for `class_iri`.
    /// Invalidates any cached inferred capabilities for `class_iri`.
    pub fn registerCapability(
        self: *Self,
        class_iri: []const u8,
        capability_name: []const u8,
    ) !void {
        self.invalidate(class_iri);
        const a = self.arena.allocator();
        const key = try a.dupe(u8, class_iri);
        const cap = try a.dupe(u8, capability_name);

        const gop = try self.direct_capabilities.getOrPut(self.allocator, key);
        if (!gop.found_existing) gop.value_ptr.* = .{};
        const cap_key = try a.dupe(u8, cap);
        try gop.value_ptr.put(self.allocator, cap_key, {});
    }

    /// Invalidate cached inferred capabilities for `class_iri`.
    /// Call after adding new hierarchy edges or capabilities.
    pub fn invalidate(self: *Self, class_iri: []const u8) void {
        if (self.inferred_cache.getPtr(class_iri)) |set| {
            set.deinit(self.allocator);
            _ = self.inferred_cache.remove(class_iri);
        }
    }

    /// Return all capability names transitively available to `class_iri`
    /// (its own capabilities + all ancestor capabilities via rdfs:subClassOf).
    /// Result is cached; the returned pointer is valid until the next
    /// registerCapability() or loadHierarchy() call for this class.
    pub fn inferCapabilities(self: *Self, class_iri: []const u8) !*const CapabilitySet {
        if (self.inferred_cache.getPtr(class_iri)) |cached| return cached;

        const a = self.arena.allocator();
        var merged: CapabilitySet = .{};
        errdefer merged.deinit(self.allocator);

        // Collect direct capabilities.
        if (self.direct_capabilities.get(class_iri)) |direct| {
            var kit = direct.keyIterator();
            while (kit.next()) |k| try merged.put(self.allocator, try a.dupe(u8, k.*), {});
        }

        // Walk ancestors recursively (cycle-safe via a visited set).
        var visited: std.StringHashMapUnmanaged(void) = .{};
        defer visited.deinit(self.allocator);
        try self.collectAncestorCaps(class_iri, &merged, &visited);

        const owned_key = try a.dupe(u8, class_iri);
        try self.inferred_cache.put(self.allocator, owned_key, merged);
        return self.inferred_cache.getPtr(class_iri).?;
    }

    fn collectAncestorCaps(
        self: *Self,
        class_iri: []const u8,
        out: *CapabilitySet,
        visited: *std.StringHashMapUnmanaged(void),
    ) !void {
        if (visited.contains(class_iri)) return;
        try visited.put(self.allocator, class_iri, {});

        const parents = self.hierarchy.get(class_iri) orelse return;
        const a = self.arena.allocator();
        for (parents) |parent| {
            // Include parent's direct capabilities.
            if (self.direct_capabilities.get(parent)) |direct| {
                var kit = direct.keyIterator();
                while (kit.next()) |k| {
                    try out.put(self.allocator, try a.dupe(u8, k.*), {});
                }
            }
            // Recurse.
            try self.collectAncestorCaps(parent, out, visited);
        }
    }

    /// Return true if an instance of `class_iri` (or any of its ancestors)
    /// can satisfy `capability_name`.
    ///
    /// Example:
    ///   duckType("Scientist", "has_birth_date")
    ///   → true if "Person" has_birth_date and "Scientist" subClassOf "Person"
    pub fn duckType(self: *Self, class_iri: []const u8, capability_name: []const u8) !bool {
        const caps = try self.inferCapabilities(class_iri);
        return caps.contains(capability_name);
    }
};

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

// CapabilityInference tests

test "CapabilityInference: direct capability match" {
    var ci = CapabilityInference.init(testing.allocator);
    defer ci.deinit();

    try ci.registerCapability("Person", "has_birth_date");
    try testing.expect(try ci.duckType("Person", "has_birth_date"));
    try testing.expect(!try ci.duckType("Person", "has_altitude"));
}

test "CapabilityInference: inherited capability via subClassOf" {
    var ci = CapabilityInference.init(testing.allocator);
    defer ci.deinit();

    // Scientist subClassOf Person; Person has capability has_birth_date
    const triple = try buildSubclassTriple(testing.allocator, "Scientist", RDFS_SUBCLASS_OF, "Person");
    defer triple.deinit(testing.allocator);
    try ci.loadHierarchy(&[_]Triple{triple}, RDFS_SUBCLASS_OF);
    try ci.registerCapability("Person", "has_birth_date");

    // Scientist should inherit has_birth_date from Person
    try testing.expect(try ci.duckType("Scientist", "has_birth_date"));
}

test "CapabilityInference: transitive inheritance A→B→C" {
    var ci = CapabilityInference.init(testing.allocator);
    defer ci.deinit();

    const ab = try buildSubclassTriple(testing.allocator, "Developer", RDFS_SUBCLASS_OF, "Person");
    defer ab.deinit(testing.allocator);
    const bc = try buildSubclassTriple(testing.allocator, "Person", RDFS_SUBCLASS_OF, "Agent");
    defer bc.deinit(testing.allocator);
    try ci.loadHierarchy(&[_]Triple{ ab, bc }, RDFS_SUBCLASS_OF);
    try ci.registerCapability("Agent", "has_id");

    // Developer should inherit has_id from Agent through Person
    try testing.expect(try ci.duckType("Developer", "has_id"));
}

test "CapabilityInference: cache invalidation on new capability" {
    var ci = CapabilityInference.init(testing.allocator);
    defer ci.deinit();

    const triple = try buildSubclassTriple(testing.allocator, "Cat", RDFS_SUBCLASS_OF, "Animal");
    defer triple.deinit(testing.allocator);
    try ci.loadHierarchy(&[_]Triple{triple}, RDFS_SUBCLASS_OF);

    // Initially no capability
    try testing.expect(!try ci.duckType("Cat", "can_purr"));

    // Register capability and re-query (cache should be invalidated for Animal's subclasses)
    try ci.registerCapability("Animal", "can_breathe");
    try testing.expect(try ci.duckType("Cat", "can_breathe"));
}

test "CapabilityInference: cycle detection in subClassOf" {
    var ci = CapabilityInference.init(testing.allocator);
    defer ci.deinit();

    // Create a cycle: A → B → A
    const ab = try buildSubclassTriple(testing.allocator, "CycleA", RDFS_SUBCLASS_OF, "CycleB");
    defer ab.deinit(testing.allocator);
    const ba = try buildSubclassTriple(testing.allocator, "CycleB", RDFS_SUBCLASS_OF, "CycleA");
    defer ba.deinit(testing.allocator);
    try ci.loadHierarchy(&[_]Triple{ ab, ba }, RDFS_SUBCLASS_OF);

    // Register capability on CycleA; CycleB should inherit it (no infinite loop).
    try ci.registerCapability("CycleA", "cycle_cap");
    try testing.expect(try ci.duckType("CycleB", "cycle_cap"));

    // inferCapabilities on CycleA should also terminate.
    const caps = try ci.inferCapabilities("CycleA");
    try testing.expect(caps.contains("cycle_cap"));
}

test "CapabilityInference: inferCapabilities traverses subClassOf chain" {
    var ci = CapabilityInference.init(testing.allocator);
    defer ci.deinit();

    // Chain: Engineer → Person → Agent
    const ep = try buildSubclassTriple(testing.allocator, "Engineer", RDFS_SUBCLASS_OF, "Person");
    defer ep.deinit(testing.allocator);
    const pa = try buildSubclassTriple(testing.allocator, "Person", RDFS_SUBCLASS_OF, "Agent");
    defer pa.deinit(testing.allocator);
    try ci.loadHierarchy(&[_]Triple{ ep, pa }, RDFS_SUBCLASS_OF);

    try ci.registerCapability("Agent", "has_id");
    try ci.registerCapability("Person", "has_name");

    // inferCapabilities("Engineer") should return both has_id and has_name.
    const caps = try ci.inferCapabilities("Engineer");
    try testing.expect(caps.contains("has_id"));
    try testing.expect(caps.contains("has_name"));
    try testing.expect(!caps.contains("has_altitude"));
}
