/// interner.zig — String interning with optional bitset support.
///
/// `StringInterner` assigns a stable integer index to each unique string.
/// Strings are arena-allocated; the interner owns all copies.
///
/// `bitSetConstraint()` returns a `reflection.ConstraintVTable` that bridges
/// this interner to the reflection field-access layer (SQLite row hydration,
/// TUI editors, RPC handlers).  String path: comma-separated capability names.
/// Binary path: u32 word-count + u64 words LE.  Convert path: cross-interner
/// duck-typing by matching capability names.
const std = @import("std");
const reflection = @import("reflection");

pub const StringInterner = @This();

arena: std.heap.ArenaAllocator,
string_to_index: std.StringHashMapUnmanaged(usize),
index_to_string: std.ArrayListUnmanaged([]const u8),
next_index: usize = 0,
lock: std.Thread.RwLock = .{},

/// Initializes a memory allocator with a string interner for Zig code.
pub fn init(allocator: std.mem.Allocator) StringInterner {
    return .{
        .arena = std.heap.ArenaAllocator.init(allocator),
        .string_to_index = .{},
        .index_to_string = .{},
    };
}

/// Releases resources associated with the StringInterner instance.
pub fn deinit(self: *StringInterner) void {
    self.string_to_index.deinit(self.arena.allocator());
    self.index_to_string.deinit(self.arena.allocator());
    self.arena.deinit();
}

/// Intern `str`, returning its stable integer index.
/// If `str` was already interned the existing index is returned without
/// allocating.  The interner owns a copy of every new string.
/// Thread-safe: uses double-checked locking (read lock fast path, write lock slow path).
pub fn intern(self: *StringInterner, str: []const u8) !usize {
    // fast path: read lock
    self.lock.lockShared();
    if (self.string_to_index.get(str)) |idx| {
        self.lock.unlockShared();
        return idx;
    }
    self.lock.unlockShared();

    // slow path: write lock
    self.lock.lock();
    defer self.lock.unlock();
    // double-check after acquiring write lock
    if (self.string_to_index.get(str)) |idx| {
        return idx;
    }

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
/// Thread-safe: uses shared read lock.
pub fn getIndex(self: *StringInterner, str: []const u8) ?usize {
    self.lock.lockShared();
    defer self.lock.unlockShared();
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

/// Validates and returns a ConstraintVTable for bit-setting operations.
pub fn bitSetConstraint(self: *StringInterner) reflection.ConstraintVTable {
    return .{
        .context = @ptrCast(self),

        // setFn / getFn are stubs; callers always go through setCtxFn / getCtxFn.
        // They exist only to satisfy the non-optional vtable fields.
        .setFn = struct {
            fn set(_: std.mem.Allocator, _: *anyopaque, _: []const u8) anyerror!void {
                return error.BitSetRequiresContext;
            }
        }.set,
        .getFn = struct {
            fn get(_: std.mem.Allocator, _: *const anyopaque) anyerror![]const u8 {
                return error.BitSetRequiresContext;
            }
        }.get,

        // setCtxFn: comma-separated capability names → bitset
        .setCtxFn = struct {
            fn setCtx(
                vtable: *const reflection.ConstraintVTable,
                allocator: std.mem.Allocator,
                ptr: *anyopaque,
                input: []const u8,
            ) anyerror!void {
                const interner: *StringInterner = @ptrCast(@alignCast(@constCast(vtable.context.?)));
                const bs: *std.bit_set.DynamicBitSetUnmanaged = @ptrCast(@alignCast(ptr));
                try bitSetFromString(interner, allocator, bs, input);
            }
        }.setCtx,

        // getCtxFn: bitset → comma-separated capability names
        .getCtxFn = struct {
            fn getCtx(
                vtable: *const reflection.ConstraintVTable,
                allocator: std.mem.Allocator,
                ptr: *const anyopaque,
            ) anyerror![]const u8 {
                const interner: *const StringInterner = @ptrCast(@alignCast(vtable.context.?));
                const bs: *const std.bit_set.DynamicBitSetUnmanaged = @ptrCast(@alignCast(ptr));
                return bitSetToString(interner, allocator, bs);
            }
        }.getCtx,

        // setBinaryFn: bitset → [word_count: u32 LE][word0..N: u64 LE]
        .setBinaryFn = struct {
            fn setBin(
                _: *const reflection.ConstraintVTable,
                ptr: *const anyopaque,
                out_buf: []u8,
            ) anyerror!usize {
                const bs: *const std.bit_set.DynamicBitSetUnmanaged = @ptrCast(@alignCast(ptr));
                const bits_per_word = @bitSizeOf(usize);
                const word_count: u32 = @intCast(
                    (bs.bit_length + bits_per_word - 1) / bits_per_word,
                );
                const needed = @sizeOf(u32) + @as(usize, word_count) * @sizeOf(u64);
                if (out_buf.len < needed) return error.BufferTooSmall;
                std.mem.writeInt(u32, out_buf[0..4], word_count, .little);
                var off: usize = 4;
                for (0..word_count) |i| {
                    const w: u64 = bs.masks[i];
                    std.mem.writeInt(u64, out_buf[off..][0..8], w, .little);
                    off += 8;
                }
                return needed;
            }
        }.setBin,

        // getBinaryFn: [word_count: u32 LE][word0..N: u64 LE] → bitset
        .getBinaryFn = struct {
            fn getBin(
                _: *const reflection.ConstraintVTable,
                allocator: std.mem.Allocator,
                ptr: *anyopaque,
                in_buf: []const u8,
            ) anyerror!void {
                if (in_buf.len < 4) return error.BufferTooSmall;
                const word_count = std.mem.readInt(u32, in_buf[0..4], .little);
                const needed = @sizeOf(u32) + @as(usize, word_count) * @sizeOf(u64);
                if (in_buf.len < needed) return error.BufferTooSmall;
                const bs: *std.bit_set.DynamicBitSetUnmanaged = @ptrCast(@alignCast(ptr));
                const bit_length = @as(usize, word_count) * @bitSizeOf(usize);
                bs.deinit(allocator);
                bs.* = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(allocator, bit_length);
                for (0..word_count) |i| {
                    const w = std.mem.readInt(u64, in_buf[4 + i * 8 ..][0..8], .little);
                    bs.masks[i] = @intCast(w);
                }
            }
        }.getBin,

        // convertFn: translate src bitset (foreign interner) → dst bitset (self interner)
        // by matching capability names across the two interners.
        .convertFn = struct {
            fn convert(
                vtable: *const reflection.ConstraintVTable,
                allocator: std.mem.Allocator,
                dst_ptr: *anyopaque,
                src_ptr: *const anyopaque,
                src_context: *const anyopaque,
            ) anyerror!void {
                const dst_interner: *StringInterner = @ptrCast(@alignCast(@constCast(vtable.context.?)));
                const src_interner: *const StringInterner = @ptrCast(@alignCast(src_context));
                const src_bs: *const std.bit_set.DynamicBitSetUnmanaged = @ptrCast(@alignCast(src_ptr));
                const dst_bs: *std.bit_set.DynamicBitSetUnmanaged = @ptrCast(@alignCast(dst_ptr));
                var iter = src_bs.iterator(.{});
                while (iter.next()) |bit_idx| {
                    const name = src_interner.getString(bit_idx) orelse continue;
                    const dst_idx = try dst_interner.intern(name);
                    if (dst_idx >= dst_bs.bit_length) {
                        try dst_bs.resize(allocator, dst_interner.count(), false);
                    }
                    dst_bs.set(dst_idx);
                }
            }
        }.convert,

        // releaseFn: deinit the bitset and zero the field so double-free is safe.
        .releaseFn = struct {
            fn rel(allocator: std.mem.Allocator, ptr: *anyopaque) void {
                const bs: *std.bit_set.DynamicBitSetUnmanaged = @ptrCast(@alignCast(ptr));
                bs.deinit(allocator);
                bs.* = .{};
            }
        }.rel,
    };
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

test "StringInterner: concurrent intern, 8 threads" {
    // spawn 8 threads, all trying to intern the same string "hello"
    // all must return the same index, 0 leaks
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var interner = StringInterner.init(allocator);
    defer interner.deinit();

    const Ctx = struct {
        si: *StringInterner,
        result: usize = 0,
    };

    const threadFn = struct {
        fn run(ctx: *Ctx) void {
            ctx.result = ctx.si.intern("hello") catch 0xFFFF_FFFF;
        }
    }.run;

    var ctxs: [8]Ctx = undefined;
    for (&ctxs) |*ctx| ctx.* = .{ .si = &interner };

    var threads: [8]std.Thread = undefined;
    for (&threads, &ctxs) |*t, *ctx| {
        t.* = try std.Thread.spawn(.{}, threadFn, .{ctx});
    }
    for (&threads) |*t| t.join();

    // All threads must have received index 0
    for (&ctxs) |*ctx| {
        try testing.expectEqual(@as(usize, 0), ctx.result);
    }
    try testing.expectEqual(@as(usize, 1), interner.count());
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



