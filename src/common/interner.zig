/// interner.zig — String interning with optional bitset support.
///
/// `StringInterner` assigns a stable integer index to each unique string.
/// Strings are arena-allocated; the interner owns all copies.
///
/// Ported from coral/src/common/interner.zig.
/// The coral-specific `bitSetConstraint()` vtable method (which depended on
/// coral's `reflection.zig`) has been omitted; only the portable core is kept.
const std = @import("std");

pub const StringInterner = @This();

arena: std.heap.ArenaAllocator,
string_to_index: std.StringHashMapUnmanaged(usize),
index_to_string: std.ArrayListUnmanaged([]const u8),
next_index: usize = 0,

pub fn init(allocator: std.mem.Allocator) StringInterner {
    return .{
        .arena = std.heap.ArenaAllocator.init(allocator),
        .string_to_index = .{},
        .index_to_string = .{},
    };
}

pub fn deinit(self: *StringInterner) void {
    self.string_to_index.deinit(self.arena.allocator());
    self.index_to_string.deinit(self.arena.allocator());
    self.arena.deinit();
}

/// Intern `str`, returning its stable integer index.
/// If `str` was already interned the existing index is returned without
/// allocating.  The interner owns a copy of every new string.
pub fn intern(self: *StringInterner, str: []const u8) !usize {
    const gop = try self.string_to_index.getOrPut(self.arena.allocator(), str);
    if (gop.found_existing) {
        return gop.value_ptr.*;
    }

    const owned = try self.arena.allocator().dupe(u8, str);
    const idx = self.next_index;
    self.next_index += 1;

    gop.value_ptr.* = idx;
    gop.key_ptr.* = owned; // point the map key at the arena copy
    try self.index_to_string.append(self.arena.allocator(), owned);

    return idx;
}

/// Look up the index for `str` without inserting it.
/// Returns null when `str` has not been interned yet.
pub fn getIndex(self: *const StringInterner, str: []const u8) ?usize {
    return self.string_to_index.get(str);
}

/// Return the string at `idx`, or null when `idx` is out of range.
pub fn getString(self: *const StringInterner, idx: usize) ?[]const u8 {
    if (idx >= self.index_to_string.items.len) return null;
    return self.index_to_string.items[idx];
}

/// Return the total number of interned strings.
pub fn count(self: *const StringInterner) usize {
    return self.next_index;
}

/// Intern every string in `strings` (results discarded; useful for pre-loading).
pub fn internList(self: *StringInterner, strings: []const []const u8) !void {
    for (strings) |s| {
        _ = try self.intern(s);
    }
}

/// Intern all strings in `strings` and return a `DynamicBitSetUnmanaged`
/// with each corresponding bit set.  The bitset is allocated with
/// `allocator` (not the interner's arena) and must be freed by the caller
/// via `bitset.deinit(allocator)`.
pub fn internAndGetBitSet(
    self: *StringInterner,
    allocator: std.mem.Allocator,
    strings: []const []const u8,
) !std.bit_set.DynamicBitSetUnmanaged {
    var bitset = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(allocator, self.next_index);
    errdefer bitset.deinit(allocator);

    for (strings) |s| {
        const idx = try self.intern(s);
        if (idx >= bitset.bit_length) {
            try bitset.resize(allocator, self.next_index, false);
        }
        bitset.set(idx);
    }

    return bitset;
}

/// Parse a comma-separated capability string into a `DynamicBitSetUnmanaged`.
/// Interns any new names into `interner`.
/// The bitset is allocated with `allocator` and must be freed by the caller.
pub fn bitSetFromString(
    interner: *StringInterner,
    allocator: std.mem.Allocator,
    bs: *std.bit_set.DynamicBitSetUnmanaged,
    input: []const u8,
) !void {
    bs.deinit(allocator);
    bs.* = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(allocator, interner.count());
    var iter = std.mem.splitScalar(u8, input, ',');
    while (iter.next()) |raw| {
        const name = std.mem.trim(u8, raw, " \t\n\r");
        if (name.len == 0) continue;
        const idx = try interner.intern(name);
        if (idx >= bs.bit_length) {
            try bs.resize(allocator, interner.count(), false);
        }
        bs.set(idx);
    }
}

/// Serialise a `DynamicBitSetUnmanaged` to a comma-separated capability string.
/// Returns an allocator-owned string; caller must free.
pub fn bitSetToString(
    interner: *const StringInterner,
    allocator: std.mem.Allocator,
    bs: *const std.bit_set.DynamicBitSetUnmanaged,
) ![]const u8 {
    var buf: std.ArrayListUnmanaged(u8) = .{};
    errdefer buf.deinit(allocator);
    var first = true;
    var iter = bs.iterator(.{});
    while (iter.next()) |idx| {
        const name = interner.getString(idx) orelse continue;
        if (!first) try buf.append(allocator, ',');
        try buf.appendSlice(allocator, name);
        first = false;
    }
    return buf.toOwnedSlice(allocator);
}

// =============================================================================
// Tests
// =============================================================================

const testing = std.testing;

test "StringInterner basic operations" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    const idx1 = try interner.intern("hello");
    const idx2 = try interner.intern("world");
    const idx3 = try interner.intern("hello");

    try testing.expectEqual(@as(usize, 0), idx1);
    try testing.expectEqual(@as(usize, 1), idx2);
    try testing.expectEqual(@as(usize, 0), idx3); // dedup

    try testing.expectEqual(@as(usize, 2), interner.count());

    try testing.expectEqualStrings("hello", interner.getString(0).?);
    try testing.expectEqualStrings("world", interner.getString(1).?);
}

test "StringInterner getIndex returns null for unknown" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    _ = try interner.intern("known");
    try testing.expect(interner.getIndex("known") != null);
    try testing.expect(interner.getIndex("unknown") == null);
}

test "StringInterner internAndGetBitSet" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    _ = try interner.intern("a");
    _ = try interner.intern("b");
    _ = try interner.intern("c");

    var bitset = try interner.internAndGetBitSet(testing.allocator, &[_][]const u8{ "a", "c" });
    defer bitset.deinit(testing.allocator);

    try testing.expect(bitset.isSet(0));
    try testing.expect(!bitset.isSet(1));
    try testing.expect(bitset.isSet(2));
}

test "StringInterner GPA no leaks" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    const allocator = gpa.allocator();

    {
        var interner = StringInterner.init(allocator);
        defer interner.deinit();

        _ = try interner.intern("foo");
        _ = try interner.intern("bar");
        _ = try interner.intern("foo"); // dedup

        try testing.expectEqual(@as(usize, 2), interner.count());

        var bitset = try interner.internAndGetBitSet(allocator, &[_][]const u8{"foo"});
        defer bitset.deinit(allocator);

        try testing.expect(bitset.isSet(0));
        try testing.expect(!bitset.isSet(1));
    }

    try testing.expectEqual(.ok, gpa.deinit());
}

test "bitSetFromString and bitSetToString roundtrip" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var bs: std.bit_set.DynamicBitSetUnmanaged = .{};
    defer bs.deinit(testing.allocator);

    try bitSetFromString(&interner, testing.allocator, &bs, "compile,link,test");
    const out = try bitSetToString(&interner, testing.allocator, &bs);
    defer testing.allocator.free(out);

    // All three names should round-trip (order is insertion order).
    try testing.expect(std.mem.indexOf(u8, out, "compile") != null);
    try testing.expect(std.mem.indexOf(u8, out, "link") != null);
    try testing.expect(std.mem.indexOf(u8, out, "test") != null);
}
