//! drift.zig — BitSet DRIFT: deterministic follow-up query generation.
//!
//! Shared between guidance and coral. No coral_db dependency.
//!
//! Implements the DRIFT algorithm using bitwise set operations instead of
//! LLM-based decomposition. Given a set of *needed* capabilities and the set
//! of *available* capabilities resolved so far, DRIFT computes the missing
//! set as `needed & ~available` and maps each missing bit back to its interned
//! string name to produce exact follow-up queries.
//!
//! Follow-ups are TIER 1 (exact identifier) by construction — each is a single
//! capability name, which routes directly to the vector-search path without LLM
//! synthesis.
//!
//! §Design decisions:
//! - Pure bitwise ops: O(bits/word), deterministic, zero LLM calls.
//! - If needed.count() == 0, no follow-ups are generated.
//! - If follow-up count exceeds `max_followups`, callers should fall back to
//!   L4.5 local model decomposition.

const std = @import("std");
const Allocator = std.mem.Allocator;
const DynamicBitSet = std.bit_set.DynamicBitSetUnmanaged;
const StringInterner = @import("interner.zig").StringInterner;

const Self = BitSetDrift;

/// Manages a bit-set drift structure for efficient memory updates; owned by the module; ensures consistent state across operations.
pub const BitSetDrift = struct {
    interner: *StringInterner,

    /// Generate follow-up queries for bits that are in `needed` but not in `available`.
    ///
    /// Returns an arena-owned slice of strings. Each string is the interned
    /// capability name for a missing bit, formatted as `"Provide <name>"`.
    /// Returns an empty slice when all needed capabilities are satisfied.
    ///
    /// Caller owns `needed` and `available`; they are not modified.
    pub fn generateFollowUps(
        self: *const Self,
        arena: Allocator,
        needed: *const DynamicBitSet,
        available: *const DynamicBitSet,
    ) ![]const []const u8 {
        var followups: std.ArrayList([]const u8) = .empty;

        // Clone needed so we can compute the difference in-place.
        var missing = try needed.clone(arena);
        defer missing.deinit(arena);

        // missing = needed & ~available  (set-difference)
        // Zig's DynamicBitSetUnmanaged uses: clone available, toggleAll, then setIntersection.
        var complement = try available.clone(arena);
        defer complement.deinit(arena);
        complement.toggleAll();
        missing.setIntersection(complement);

        var iter = missing.iterator(.{});
        while (iter.next()) |bit| {
            const name = self.interner.getString(bit) orelse continue;
            const q = try std.fmt.allocPrint(arena, "Provide {s}", .{name});
            try followups.append(arena, q);
        }

        return followups.toOwnedSlice(arena);
    }

    /// Return true when all bits in `needed` are covered by `available`.
    ///
    /// `needed.count() == 0` is considered fully resolved (vacuous truth).
    pub fn isResolved(
        needed: *const DynamicBitSet,
        available: *const DynamicBitSet,
    ) bool {
        if (needed.count() == 0) return true;

        // Iterate missing bits; if any exist, not resolved.
        // We clone into a stack arena to avoid touching callers' data.
        // The clone is small (bits/64 words), so a fixed arena is safe.
        var buf: [512]u8 = undefined;
        var fba = std.heap.FixedBufferAllocator.init(&buf);
        const fba_alloc = fba.allocator();

        var missing = needed.clone(fba_alloc) catch {
            // If we can't clone (very large bitset), conservatively say not resolved.
            return false;
        };
        defer missing.deinit(fba_alloc);

        // missing = needed & ~available
        var complement = available.clone(fba_alloc) catch return false;
        defer complement.deinit(fba_alloc);
        complement.toggleAll();
        missing.setIntersection(complement);
        return missing.count() == 0;
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "BitSetDrift.isResolved: empty needed is always resolved" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    var needed = try DynamicBitSet.initEmpty(a, 8);
    var available = try DynamicBitSet.initEmpty(a, 8);
    try testing.expect(BitSetDrift.isResolved(&needed, &available));

    available.set(3);
    try testing.expect(BitSetDrift.isResolved(&needed, &available));
}

test "BitSetDrift.isResolved: fully covered" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    var needed = try DynamicBitSet.initEmpty(a, 8);
    needed.set(1);
    needed.set(3);

    var available = try DynamicBitSet.initEmpty(a, 8);
    available.set(1);
    available.set(3);
    available.set(5);

    try testing.expect(BitSetDrift.isResolved(&needed, &available));
}

test "BitSetDrift.isResolved: partially covered" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    var needed = try DynamicBitSet.initEmpty(a, 8);
    needed.set(1);
    needed.set(3);

    var available = try DynamicBitSet.initEmpty(a, 8);
    available.set(1);

    try testing.expect(!BitSetDrift.isResolved(&needed, &available));
}

test "BitSetDrift.generateFollowUps: produces follow-ups for missing bits" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    // Set up interner with known strings
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    const idx_search = try interner.intern("vector-search");
    const idx_cache = try interner.intern("coral-cache");
    const idx_route = try interner.intern("query-routing");

    const drift = BitSetDrift{ .interner = &interner };

    // needed = {vector-search, coral-cache, query-routing}
    // available = {vector-search}
    // missing = {coral-cache, query-routing}
    const cap_count = idx_route + 1;
    var needed = try DynamicBitSet.initEmpty(a, cap_count);
    needed.set(idx_search);
    needed.set(idx_cache);
    needed.set(idx_route);

    var available = try DynamicBitSet.initEmpty(a, cap_count);
    available.set(idx_search);

    const followups = try drift.generateFollowUps(a, &needed, &available);

    try testing.expect(followups.len == 2);
    for (followups) |fq| {
        try testing.expect(std.mem.startsWith(u8, fq, "Provide "));
    }
}

test "BitSetDrift.generateFollowUps: no follow-ups when all resolved" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    const idx = try interner.intern("vector-search");

    const drift = BitSetDrift{ .interner = &interner };

    var needed = try DynamicBitSet.initEmpty(a, idx + 1);
    needed.set(idx);
    var available = try DynamicBitSet.initEmpty(a, idx + 1);
    available.set(idx);

    const followups = try drift.generateFollowUps(a, &needed, &available);
    try testing.expect(followups.len == 0);
}
