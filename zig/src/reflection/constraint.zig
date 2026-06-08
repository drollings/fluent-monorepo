/// constraint.zig — ConstraintVTable, constraintSet, constraintGet, Constraint(T).
const std = @import("std");
const schema_version_mod = @import("schema_version.zig");
pub const SchemaVersion = schema_version_mod.SchemaVersion;
pub const SCHEMA_CURRENT = schema_version_mod.SCHEMA_CURRENT;

// ============================================================
// § 2  Constraint VTable
// ============================================================

/// Defines a constraint table structure for validation; managed centrally, immutable after creation.
pub const ConstraintVTable = struct {
    /// Parse `input` and write the result to the field at `ptr`.
    /// An allocator is required for string/slice fields to avoid dangling pointers.
    setFn: *const fn (allocator: std.mem.Allocator, ptr: *anyopaque, input: []const u8) anyerror!void,

    /// Serialise the value at `ptr` to a caller-owned string.
    /// The caller must free the returned slice with the same allocator.
    getFn: *const fn (allocator: std.mem.Allocator, ptr: *const anyopaque) anyerror![]const u8,

    // ── Extended / optional vtable entries ───────────────────────────────

    /// Opaque context pointer for parameterised vtables (e.g. BitSetConstraint
    /// stores a `*StringInterner` here).  Null for stateless constraints.
    context: ?*const anyopaque = null,

    /// Release any heap memory owned by the field at `ptr`.
    /// Called by DynamicEditable.releaseAll() and before overwriting an owned
    /// field via set().  Null means the field owns no heap memory.
    releaseFn: ?*const fn (allocator: std.mem.Allocator, ptr: *anyopaque) void = null,

    /// Convert the bitset at `src_ptr` (encoded relative to `src_context`) into
    /// the bitset at `dst_ptr` (encoded relative to `self.context`), by matching
    /// capability names across two different StringInterners.
    /// Used for DAG duck-typing across module boundaries.
    /// Null for non-bitset constraints.
    convertFn: ?*const fn (
        self: *const ConstraintVTable,
        allocator: std.mem.Allocator,
        dst_ptr: *anyopaque,
        src_ptr: *const anyopaque,
        src_context: *const anyopaque,
    ) anyerror!void = null,

    /// Context-aware string setter — receives `self` so runtime-parameterised
    /// constraints (e.g. BitSetConstraint) can reach their stored context pointer.
    /// When non-null, DynamicEditable and Editable prefer this over setFn.
    setCtxFn: ?*const fn (
        vtable: *const ConstraintVTable,
        allocator: std.mem.Allocator,
        ptr: *anyopaque,
        input: []const u8,
    ) anyerror!void = null,

    /// Context-aware string getter — symmetric with setCtxFn.
    getCtxFn: ?*const fn (
        vtable: *const ConstraintVTable,
        allocator: std.mem.Allocator,
        ptr: *const anyopaque,
    ) anyerror![]const u8 = null,

    /// Write the binary representation of the field at `ptr` into `out_buf`.
    /// Returns the number of bytes written.  The binary layout is constraint-
    /// specific (e.g. bitset → length-prefixed usize word array; string → length
    /// prefix + UTF-8 bytes).  Null for types without a defined binary path.
    setBinaryFn: ?*const fn (
        vtable: *const ConstraintVTable,
        ptr: *const anyopaque,
        out_buf: []u8,
    ) anyerror!usize = null,

    /// Read a binary representation from `in_buf` and write it to the field at
    /// `ptr`.  The format must match what setBinaryFn produces.
    /// Null for types without a defined binary path.
    getBinaryFn: ?*const fn (
        vtable: *const ConstraintVTable,
        allocator: std.mem.Allocator,
        ptr: *anyopaque,
        in_buf: []const u8,
    ) anyerror!void = null,

    // ── Schema versioning (M4) ─────────────────────────────────────────────

    /// Schema version this constraint was compiled for.
    /// Default: SCHEMA_CURRENT (v1.0 for all pre-versioning constraints).
    version: SchemaVersion = SCHEMA_CURRENT,

    /// Optional migration function.  Called when the stored schema version
    /// differs from this constraint's version and an in-place transform is
    /// needed before set/get can proceed:
    ///
    ///   if (vtable.migrateFn) |migrate| {
    ///       try migrate(from_version, to_version, allocator, field_ptr);
    ///   }
    ///
    /// The function should transform the raw bytes at `ptr` from the layout
    /// expected by `from_version` to the layout expected by `to_version`.
    /// Null means no migration is required (forward-compatible change).
    migrateFn: ?*const fn (
        from_version: SchemaVersion,
        to_version: SchemaVersion,
        allocator: std.mem.Allocator,
        ptr: *anyopaque,
    ) anyerror!void = null,
};

/// Converts a Zig type to a ConstraintVTable representation.
pub fn Constraint(comptime T: type) ConstraintVTable {
    // releaseFn is only meaningful for owned heap types.  For []const u8 we
    // free the slice and zero out the field so double-free is safe.
    const release: ?*const fn (std.mem.Allocator, *anyopaque) void = blk: {
        switch (@typeInfo(T)) {
            .pointer => |p| {
                if (p.size == .slice and p.child == u8) {
                    break :blk struct {
                        fn rel(allocator: std.mem.Allocator, ptr: *anyopaque) void {
                            const s: *[]const u8 = @ptrCast(@alignCast(ptr));
                            if (s.len > 0) {
                                allocator.free(s.*);
                                s.* = "";
                            }
                        }
                    }.rel;
                }
            },
            else => {},
        }
        break :blk null;
    };

    return .{
        .setFn = struct {
            fn set(allocator: std.mem.Allocator, ptr: *anyopaque, input: []const u8) anyerror!void {
                const typed_ptr: *align(@alignOf(T)) T = @ptrCast(@alignCast(ptr));
                try constraintSet(T, allocator, typed_ptr, input);
            }
        }.set,
        .getFn = struct {
            fn get(allocator: std.mem.Allocator, ptr: *const anyopaque) anyerror![]const u8 {
                const typed_ptr: *align(@alignOf(T)) const T = @ptrCast(@alignCast(ptr));
                return constraintGet(T, allocator, typed_ptr);
            }
        }.get,
        .releaseFn = release,
    };
}

// ─── internal helpers ────────────────────────────────────────────────────────
// Extracted so that:
//   (a) the VTable closures above are thin wrappers (no duplicated switch),
//   (b) the optional/array/vector cases can recurse without constructing a
//       new ConstraintVTable just to call through a function pointer, and
//   (c) setFast/getFast can call these directly, bypassing type erasure.

/// Validates and sets constraints for a Zig type using an allocator and input data.
pub fn constraintSet(comptime T: type, allocator: std.mem.Allocator, typed_ptr: *T, input: []const u8) anyerror!void {
    switch (@typeInfo(T)) {
        .int => typed_ptr.* = try std.fmt.parseInt(T, input, 10),
        .float => typed_ptr.* = try std.fmt.parseFloat(T, input),
        .bool => {
            if (std.ascii.eqlIgnoreCase(input, "true") or std.mem.eql(u8, input, "1")) {
                typed_ptr.* = true;
            } else if (std.ascii.eqlIgnoreCase(input, "false") or std.mem.eql(u8, input, "0")) {
                typed_ptr.* = false;
            } else {
                return error.InvalidBool;
            }
        },
        .pointer => |ptr_info| {
            // Only []const u8 / []u8 string slices are supported.
            if (ptr_info.size == .slice and ptr_info.child == u8) {
                typed_ptr.* = try allocator.dupe(u8, input);
            } else {
                return error.UnsupportedPointerType;
            }
        },
        .@"enum" => {
            typed_ptr.* = std.meta.stringToEnum(T, input) orelse return error.InvalidEnum;
        },
        .optional => |opt_info| {
            if (std.mem.eql(u8, input, "null") or input.len == 0) {
                typed_ptr.* = null;
            } else {
                const Child = opt_info.child;
                var temp: Child = undefined;
                try constraintSet(Child, allocator, &temp, input);
                typed_ptr.* = temp;
            }
        },
        .array => |arr_info| {
            const Child = arr_info.child;
            var iter = std.mem.splitScalar(u8, input, ',');
            var i: usize = 0;
            while (iter.next()) |elem| : (i += 1) {
                if (i >= arr_info.len) return error.ArrayTooLong;
                const trimmed = std.mem.trim(u8, elem, " \t\n\r");
                try constraintSet(Child, allocator, &typed_ptr[i], trimmed);
            }
            if (i != arr_info.len) return error.ArrayTooShort;
        },
        .vector => |vec_info| {
            const Child = vec_info.child;
            var iter = std.mem.splitScalar(u8, input, ',');
            var i: usize = 0;
            const arr_ptr: *[vec_info.len]Child = @ptrCast(typed_ptr);
            while (iter.next()) |elem| : (i += 1) {
                if (i >= vec_info.len) return error.VectorTooLong;
                const trimmed = std.mem.trim(u8, elem, " \t\n\r");
                var val: Child = undefined;
                try constraintSet(Child, allocator, &val, trimmed);
                arr_ptr[i] = val;
            }
            if (i != vec_info.len) return error.VectorTooShort;
        },
        else => return error.UnsupportedType,
    }
}

/// Retrieves a value from a typed pointer using reflection, returning an error if invalid.
pub fn constraintGet(comptime T: type, allocator: std.mem.Allocator, typed_ptr: *const T) anyerror![]const u8 {
    switch (@typeInfo(T)) {
        .int, .float => return std.fmt.allocPrint(allocator, "{d}", .{typed_ptr.*}),
        .bool => return std.fmt.allocPrint(allocator, "{}", .{typed_ptr.*}),
        .pointer => |ptr_info| {
            if (ptr_info.size == .slice and ptr_info.child == u8) {
                return allocator.dupe(u8, typed_ptr.*);
            }
            return error.UnsupportedPointerType;
        },
        .@"enum" => return allocator.dupe(u8, @tagName(typed_ptr.*)),
        .optional => |opt_info| {
            if (typed_ptr.*) |val| {
                return constraintGet(opt_info.child, allocator, &val);
            } else {
                return allocator.dupe(u8, "null");
            }
        },
        .array => |arr_info| {
            var buf: std.ArrayListUnmanaged(u8) = .empty;
            errdefer buf.deinit(allocator);
            try buf.append(allocator, '[');
            for (typed_ptr.*, 0..) |elem, i| {
                if (i > 0) try buf.append(allocator, ',');
                const s = try constraintGet(arr_info.child, allocator, &elem);
                defer allocator.free(s);
                try buf.appendSlice(allocator, s);
            }
            try buf.append(allocator, ']');
            return buf.toOwnedSlice(allocator);
        },
        .vector => |vec_info| {
            var buf: std.ArrayList(u8) = .empty;
            errdefer buf.deinit(allocator);
            try buf.append(allocator, '[');
            const arr_ptr: *const [vec_info.len]vec_info.child = @ptrCast(typed_ptr);
            for (0..vec_info.len) |i| {
                if (i > 0) try buf.append(allocator, ',');
                const elem = arr_ptr[i];
                const s = try constraintGet(vec_info.child, allocator, &elem);
                defer allocator.free(s);
                try buf.appendSlice(allocator, s);
            }
            try buf.append(allocator, ']');
            return buf.toOwnedSlice(allocator);
        },
        else => return error.UnsupportedType,
    }
}

const testing = std.testing;

test "constraintSet: integer types" {
    var val: i32 = 0;
    try constraintSet(i32, testing.allocator, &val, "42");
    try testing.expectEqual(@as(i32, 42), val);

    var uval: u8 = 0;
    try constraintSet(u8, testing.allocator, &uval, "255");
    try testing.expectEqual(@as(u8, 255), uval);
}

test "constraintSet: float types" {
    var val: f64 = 0;
    try constraintSet(f64, testing.allocator, &val, "3.14159");
    try testing.expectApproxEqAbs(@as(f64, 3.14159), val, 0.00001);

    var fval: f32 = 0;
    try constraintSet(f32, testing.allocator, &fval, "-2.5");
    try testing.expectApproxEqAbs(@as(f32, -2.5), fval, 0.001);
}

test "constraintSet: bool types" {
    var val: bool = false;
    try constraintSet(bool, testing.allocator, &val, "true");
    try testing.expect(val);
    try constraintSet(bool, testing.allocator, &val, "false");
    try testing.expect(!val);
    try constraintSet(bool, testing.allocator, &val, "1");
    try testing.expect(val);
    try constraintSet(bool, testing.allocator, &val, "0");
    try testing.expect(!val);
}

test "constraintSet: bool invalid value" {
    var val: bool = false;
    try testing.expectError(error.InvalidBool, constraintSet(bool, testing.allocator, &val, "yes"));
}

test "constraintSet: string slice" {
    var val: []const u8 = "";
    try constraintSet([]const u8, testing.allocator, &val, "hello world");
    defer testing.allocator.free(val);
    try testing.expectEqualSlices(u8, "hello world", val);
}

test "constraintSet: enum types" {
    const Color = enum { red, green, blue };
    var val: Color = .red;
    try constraintSet(Color, testing.allocator, &val, "green");
    try testing.expectEqual(Color.green, val);
}

test "constraintSet: enum invalid value" {
    const Color = enum { red, green, blue };
    var val: Color = .red;
    try testing.expectError(error.InvalidEnum, constraintSet(Color, testing.allocator, &val, "yellow"));
}

test "constraintSet: optional types" {
    var val: ?i32 = null;
    try constraintSet(?i32, testing.allocator, &val, "42");
    try testing.expectEqual(@as(?i32, 42), val);
    try constraintSet(?i32, testing.allocator, &val, "null");
    try testing.expect(val == null);
    try constraintSet(?i32, testing.allocator, &val, "");
    try testing.expect(val == null);
}

test "constraintSet: array types" {
    var val: [3]i32 = .{ 0, 0, 0 };
    try constraintSet([3]i32, testing.allocator, &val, "1,2,3");
    try testing.expectEqualSlices(i32, &[_]i32{ 1, 2, 3 }, &val);
}

test "constraintSet: array too long" {
    var val: [2]i32 = .{ 0, 0 };
    try testing.expectError(error.ArrayTooLong, constraintSet([2]i32, testing.allocator, &val, "1,2,3"));
}

test "constraintSet: array too short" {
    var val: [3]i32 = .{ 0, 0, 0 };
    try testing.expectError(error.ArrayTooShort, constraintSet([3]i32, testing.allocator, &val, "1,2"));
}

test "constraintSet: vector types" {
    var val: @Vector(3, f32) = .{ 0, 0, 0 };
    try constraintSet(@Vector(3, f32), testing.allocator, &val, "1.0, 2.0, 3.0");
    try testing.expectApproxEqAbs(@as(f32, 1.0), val[0], 0.001);
    try testing.expectApproxEqAbs(@as(f32, 2.0), val[1], 0.001);
    try testing.expectApproxEqAbs(@as(f32, 3.0), val[2], 0.001);
}

test "constraintGet: integer types" {
    var val: i32 = 42;
    const s = try constraintGet(i32, testing.allocator, &val);
    defer testing.allocator.free(s);
    try testing.expectEqualSlices(u8, "42", s);
}

test "constraintGet: float types" {
    var val: f64 = 3.5;
    const s = try constraintGet(f64, testing.allocator, &val);
    defer testing.allocator.free(s);
    try testing.expect(std.mem.startsWith(u8, s, "3.5"));
}

test "constraintGet: bool types" {
    var val: bool = true;
    const s_true = try constraintGet(bool, testing.allocator, &val);
    defer testing.allocator.free(s_true);
    try testing.expectEqualSlices(u8, "true", s_true);
    val = false;
    const s_false = try constraintGet(bool, testing.allocator, &val);
    defer testing.allocator.free(s_false);
    try testing.expectEqualSlices(u8, "false", s_false);
}

test "constraintGet: string slice" {
    var val: []const u8 = "test string";
    const s = try constraintGet([]const u8, testing.allocator, &val);
    defer testing.allocator.free(s);
    try testing.expectEqualSlices(u8, "test string", s);
}

test "constraintGet: enum types" {
    const Color = enum { red, green, blue };
    var val: Color = .green;
    const s = try constraintGet(Color, testing.allocator, &val);
    defer testing.allocator.free(s);
    try testing.expectEqualSlices(u8, "green", s);
}

test "constraintGet: optional types" {
    var val: ?i32 = 42;
    const s_some = try constraintGet(?i32, testing.allocator, &val);
    defer testing.allocator.free(s_some);
    try testing.expectEqualSlices(u8, "42", s_some);
    val = null;
    const s_null = try constraintGet(?i32, testing.allocator, &val);
    defer testing.allocator.free(s_null);
    try testing.expectEqualSlices(u8, "null", s_null);
}

test "constraintGet: array types" {
    var val: [3]i32 = .{ 1, 2, 3 };
    const s = try constraintGet([3]i32, testing.allocator, &val);
    defer testing.allocator.free(s);
    try testing.expectEqualSlices(u8, "[1,2,3]", s);
}

test "Constraint: round-trip integer" {
    const vtable = Constraint(i32);
    var val: i32 = 0;
    try vtable.setFn(testing.allocator, &val, "123");
    const s = try vtable.getFn(testing.allocator, &val);
    defer testing.allocator.free(s);
    try testing.expectEqualSlices(u8, "123", s);
}

test "Constraint: round-trip string" {
    const vtable = Constraint([]const u8);
    var val: []const u8 = "";
    const ptr: *anyopaque = @ptrCast(&val);
    try vtable.setFn(testing.allocator, ptr, "hello");
    defer if (val.len > 0) testing.allocator.free(val);
    const s = try vtable.getFn(testing.allocator, ptr);
    defer testing.allocator.free(s);
    try testing.expectEqualSlices(u8, "hello", s);
    if (vtable.releaseFn) |release| {
        release(testing.allocator, ptr);
    }
}

test "Constraint: releaseFn frees string slice" {
    const vtable = Constraint([]const u8);
    var val: []const u8 = "";
    const ptr: *anyopaque = @ptrCast(&val);
    try vtable.setFn(testing.allocator, ptr, "owned string");
    try testing.expectEqualSlices(u8, "owned string", val);
    if (vtable.releaseFn) |release| {
        release(testing.allocator, ptr);
    }
    try testing.expectEqualSlices(u8, "", val);
}
