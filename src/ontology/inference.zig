/// inference.zig — Ontology-Based Inference Engine Stub
///
/// Defines the interface for RDFS/OWL inference without a full implementation.
/// Current stub: infer() returns empty slice, materialize() is no-op.
///
/// Future implementation should handle:
///   - rdfs:subClassOf transitivity
///   - rdfs:subPropertyOf transitivity
///   - rdfs:domain / rdfs:range type inference
///   - owl:inverseOf bidirectional edges
const std = @import("std");
const rdf = @import("rdf");
const Triple = rdf.Triple;

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

    /// Add a rule for future use.
    pub fn addRule(self: *InferenceEngine, rule: InferenceRule) !void {
        try self.rules.append(self.allocator, rule);
    }

    /// Return inferred triples from the given input.
    /// STUB: always returns empty slice.
    /// TODO: Implement RDFS/OWL forward-chaining inference.
    pub fn infer(self: *InferenceEngine, triples: []const Triple) ![]Triple {
        _ = self;
        _ = triples;
        return &[_]Triple{};
    }

    /// Persist inferred edges to SQLite via Library.
    /// STUB: no-op.
    pub fn materialize(self: *InferenceEngine) !void {
        _ = self;
    }
};

// =============================================================================
// Tests — Milestone 2.3
// =============================================================================

const testing = std.testing;

test "inference stub returns empty triples" {
    var engine = InferenceEngine.init(testing.allocator);
    defer engine.deinit();
    const result = try engine.infer(&[_]Triple{});
    try testing.expectEqual(@as(usize, 0), result.len);
}

test "inference stub materialize is no-op" {
    var engine = InferenceEngine.init(testing.allocator);
    defer engine.deinit();
    try engine.materialize(); // should succeed silently
}

test "inference rule addition" {
    var engine = InferenceEngine.init(testing.allocator);
    defer engine.deinit();
    try engine.addRule(.{
        .rule_type = .subclass_transitivity,
        .trigger_predicate = "http://www.w3.org/2000/01/rdf-schema#subClassOf",
    });
    try testing.expectEqual(@as(usize, 1), engine.rules.items.len);
    try testing.expectEqual(RuleType.subclass_transitivity, engine.rules.items[0].rule_type);
}
