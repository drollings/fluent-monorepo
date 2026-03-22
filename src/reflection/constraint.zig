/// constraint.zig — ConstraintVTable, constraintSet, constraintGet, Constraint(T).
const std = @import("std");

// ============================================================
// § 2  Constraint VTable
// ============================================================

/// Type-erased vtable for parsing a string into a field value and serialising
/// a field value back to a string.  One static instance exists per type T —
/// the Accessor only holds a pointer to it, so per-field cost is one pointer.
///
/// Extended fields (context, releaseFn, convertFn, setBinaryFn, getBinaryFn)
/// are optional — null means "not applicable for this type".  The original
/// setFn / getFn always operate on UTF-8 string representations.
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
};

/// Build a static ConstraintVTable for type T.
/// The returned value is typically stored as a comptime constant and referenced
/// by pointer from Accessor.  Supported types: int, float, bool, []const u8,
/// enum, optional, array, vector.
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
            while (iter.next()) |elem| : (i += 1) {
                if (i >= vec_info.len) return error.VectorTooLong;
                const trimmed = std.mem.trim(u8, elem, " \t\n\r");
                var val: Child = undefined;
                try constraintSet(Child, allocator, &val, trimmed);
                typed_ptr[i] = val;
            }
            if (i != vec_info.len) return error.VectorTooShort;
        },
        else => return error.UnsupportedType,
    }
}

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
            var buf: std.ArrayListUnmanaged(u8) = .{};
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
            var buf: std.ArrayListUnmanaged(u8) = .{};
            errdefer buf.deinit(allocator);
            try buf.append(allocator, '[');
            for (0..vec_info.len) |i| {
                if (i > 0) try buf.append(allocator, ',');
                const elem = typed_ptr[i];
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
