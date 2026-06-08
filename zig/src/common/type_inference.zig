/// type_inference.zig — Type Inference Cache (P3.7)
///
/// Caches the transitive `rdfs:subClassOf` closure for fast type-membership
/// queries.  Instead of traversing the ontology graph on every lookup, the
/// closure is precomputed at ingestion time and stored as a per-class ancestor
/// bitset.
///
/// §Why bitsets:
///   - Membership test: O(1) single bit check
///   - Subsumption: bitset intersection → O(n/64) instead of graph traversal
///   - Memory: K classes × K bits ÷ 8 bytes  (K=1000 → ~125 KB)
///
/// §Thread safety:
///   TypeInference is immutable after `build()`.  Safe to share across threads
///   without locking once constructed.
///
/// §Integration:
///   Build once at session start from the `entity_types` + `neighbor_of`
///   (predicate_iri = "rdfs:subClassOf") tables.  Discard and rebuild when
///   the ontology changes.
const std = @import("std");
const Allocator = std.mem.Allocator;

// ---------------------------------------------------------------------------
// TypeInference
// ---------------------------------------------------------------------------

/// Manages inference context for type checking; owns inference state; ensures type invariants are preserved.
pub const TypeInference = struct {
    /// Maps class node ID → bitset of all ancestor class IDs (including self).
    /// Arena-owned; freed when arena is freed.
    ancestors: std.AutoHashMapUnmanaged(i64, std.DynamicBitSetUnmanaged),
    /// Total number of tracked class IDs (used to size bitsets).
    class_count: usize,
    /// Dense mapping: class_id → bit index.
    id_to_bit: std.AutoHashMapUnmanaged(i64, usize),

    const Self = @This();

    /// Build the transitive closure from a list of (child_id, parent_id) edges.
    ///
    /// All class IDs that appear in `edges` are registered.  Edges must encode
    /// `rdfs:subClassOf` relationships (child IS-A parent).
    ///
    /// The returned `TypeInference` is arena-owned; call `deinit(arena)` when done.
    pub fn build(
        arena: Allocator,
        class_ids: []const i64,
        edges: []const [2]i64, // [child_id, parent_id]
    ) !Self {
        const class_count = class_ids.len;

        // Build dense id → bit-index mapping.
        var id_to_bit: std.AutoHashMapUnmanaged(i64, usize) = .empty;
        for (class_ids, 0..) |cid, i| {
            try id_to_bit.put(arena, cid, i);
        }

        // Initialise each class with its own bit set (self is ancestor of self).
        var ancestors: std.AutoHashMapUnmanaged(i64, std.DynamicBitSetUnmanaged) = .empty;
        for (class_ids) |cid| {
            var bs = try std.DynamicBitSetUnmanaged.initEmpty(arena, class_count);
            const bit = id_to_bit.get(cid) orelse continue;
            bs.set(bit);
            try ancestors.put(arena, cid, bs);
        }

        // Propagate parent bits into child bitsets.
        // Fixed-point: repeat until no bitset grows in an iteration.
        var changed = true;
        while (changed) {
            changed = false;
            for (edges) |edge| {
                const child_id = edge[0];
                const parent_id = edge[1];
                const child_bs = ancestors.getPtr(child_id) orelse continue;
                const parent_bs = ancestors.get(parent_id) orelse continue;
                const before = child_bs.count();
                // Merge all of parent's ancestors (including self) into child.
                child_bs.setUnion(parent_bs);
                if (child_bs.count() != before) changed = true;
            }
        }

        return Self{
            .ancestors = ancestors,
            .class_count = class_count,
            .id_to_bit = id_to_bit,
        };
    }

    /// Free all arena-managed bitsets.
    pub fn deinit(self: *Self, arena: Allocator) void {
        var it = self.ancestors.valueIterator();
        while (it.next()) |bs| bs.deinit(arena);
        self.ancestors.deinit(arena);
        self.id_to_bit.deinit(arena);
    }

    /// Return true if `class_id` is a subclass of (or equal to) `ancestor_id`.
    pub fn isSubclassOf(self: *const Self, class_id: i64, ancestor_id: i64) bool {
        const bit = self.id_to_bit.get(ancestor_id) orelse return false;
        const bs = self.ancestors.get(class_id) orelse return false;
        return bs.isSet(bit);
    }

    /// Return the number of registered classes.
    pub fn classCount(self: *const Self) usize {
        return self.class_count;
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "TypeInference: empty ontology" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var ti = try TypeInference.build(arena.allocator(), &[_]i64{}, &[_][2]i64{});
    defer ti.deinit(arena.allocator());
    try testing.expectEqual(@as(usize, 0), ti.classCount());
}

test "TypeInference: class is subclass of itself" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const classes = [_]i64{ 1, 2, 3 };
    var ti = try TypeInference.build(arena.allocator(), &classes, &[_][2]i64{});
    defer ti.deinit(arena.allocator());
    try testing.expect(ti.isSubclassOf(1, 1));
    try testing.expect(ti.isSubclassOf(2, 2));
}

test "TypeInference: direct subclass" {
    // Animal → Entity: Animal IS-A Entity
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const classes = [_]i64{ 1, 2 }; // 1=Animal, 2=Entity
    const edges = [_][2]i64{.{ 1, 2 }}; // Animal subClassOf Entity
    var ti = try TypeInference.build(arena.allocator(), &classes, &edges);
    defer ti.deinit(arena.allocator());
    try testing.expect(ti.isSubclassOf(1, 2)); // Animal IS-A Entity
    try testing.expect(!ti.isSubclassOf(2, 1)); // Entity is NOT Animal
}

test "TypeInference: transitive subclass" {
    // Dog → Animal → Entity
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const classes = [_]i64{ 1, 2, 3 }; // 1=Dog, 2=Animal, 3=Entity
    const edges = [_][2]i64{
        .{ 1, 2 }, // Dog subClassOf Animal
        .{ 2, 3 }, // Animal subClassOf Entity
    };
    var ti = try TypeInference.build(arena.allocator(), &classes, &edges);
    defer ti.deinit(arena.allocator());
    try testing.expect(ti.isSubclassOf(1, 2)); // Dog IS-A Animal
    try testing.expect(ti.isSubclassOf(1, 3)); // Dog IS-A Entity (transitive)
    try testing.expect(ti.isSubclassOf(2, 3)); // Animal IS-A Entity
    try testing.expect(!ti.isSubclassOf(3, 1)); // Entity is NOT Dog
}

test "TypeInference: unknown class returns false" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const classes = [_]i64{1};
    var ti = try TypeInference.build(arena.allocator(), &classes, &[_][2]i64{});
    defer ti.deinit(arena.allocator());
    try testing.expect(!ti.isSubclassOf(99, 1)); // unknown child
    try testing.expect(!ti.isSubclassOf(1, 99)); // unknown ancestor
}
