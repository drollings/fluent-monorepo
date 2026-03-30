//! builder_error.zig — Structured error type for fluent builder chains.
//!
//! Replaces bare `?anyerror` in builder structs with rich context:
//!   - Which phase the error occurred in (depends, provides, command, …)
//!   - Which field was being set
//!   - What value was supplied (truncated to 128 bytes)
//!   - Which constraint was violated
//!   - The underlying anyerror cause
//!
//! All strings except field/constraint names are allocated from the builder's
//! arena; the arena is deinited by the terminal method (register / build / sync).

const std = @import("std");
const builtin = @import("builtin");

// ── Phase ─────────────────────────────────────────────────────────────────────

/// The builder phase in which an error occurred.
pub const Phase = enum {
    depends,
    provides,
    command,
    registration,
    validation,
    initialization,

    pub fn label(self: Phase) []const u8 {
        return @tagName(self);
    }
};

// ── BuilderError ──────────────────────────────────────────────────────────────

/// Structured error captured by a fluent builder.
/// Allocated from the builder's arena — lifetime matches the builder.
pub const BuilderError = struct {
    phase: Phase,
    /// Field name (static string literal; not arena-allocated).
    field: ?[]const u8 = null,
    /// User-supplied value, truncated to 128 bytes (arena-allocated).
    value: ?[]const u8 = null,
    /// Constraint that failed (static string literal; not arena-allocated).
    constraint: ?[]const u8 = null,
    /// Underlying Zig error.
    cause: anyerror,
    /// Formatted message (arena-allocated).
    message: []const u8,

    /// Allocate a BuilderError from `arena` and format its message.
    ///
    /// `field` and `constraint` are expected to be static string literals
    /// (compile-time or lifetime-of-builder).  `value` is copied and
    /// truncated to `max_value_len` bytes.
    pub fn init(
        arena: std.mem.Allocator,
        phase: Phase,
        field: ?[]const u8,
        value: ?[]const u8,
        constraint: ?[]const u8,
        cause: anyerror,
    ) !*BuilderError {
        const self = try arena.create(BuilderError);
        const value_copy: ?[]const u8 = if (value) |v| blk: {
            const len = @min(v.len, max_value_len);
            break :blk try arena.dupe(u8, v[0..len]);
        } else null;

        self.* = .{
            .phase = phase,
            .field = field,
            .value = value_copy,
            .constraint = constraint,
            .cause = cause,
            .message = "", // filled below
        };
        self.message = try std.fmt.allocPrint(
            arena,
            "phase={s} field={s} value={s} constraint={s} cause={s}",
            .{
                @tagName(phase),
                field orelse "(none)",
                value_copy orelse "(none)",
                constraint orelse "(none)",
                @errorName(cause),
            },
        );
        return self;
    }

    /// `std.fmt` format support: `{}` logs `self.message`.
    pub fn format(
        self: *const BuilderError,
        comptime _: []const u8,
        _: std.fmt.FormatOptions,
        writer: anytype,
    ) !void {
        try writer.writeAll(self.message);
    }

    /// Chain a parent BuilderError as the cause of a child.
    /// The child's cause is set to the parent's cause; the parent's
    /// message is appended to the child's message.
    pub fn chain(
        arena: std.mem.Allocator,
        child: *BuilderError,
        parent: *const BuilderError,
    ) !void {
        child.message = try std.fmt.allocPrint(
            arena,
            "{s} (caused by: {s})",
            .{ child.message, parent.message },
        );
    }
};

/// Maximum bytes copied from a user-supplied value into a BuilderError.
pub const max_value_len: usize = 128;

/// Log `err.message` at the `err` level if `maybe_err` is non-null.
/// Call this at terminal builder methods before returning the error.
///
///   if (self.err) |e| {
///       logIfError(e);
///       return e.cause;
///   }
pub fn logIfError(maybe_err: ?*const BuilderError) void {
    const e = maybe_err orelse return;
    // Skip logging during test builds to avoid test output pollution.
    if (builtin.is_test) return;
    std.log.err("builder error: {s}", .{e.message});
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Join `names` into a comma-separated string, allocated from `arena`.
/// Returns empty slice for an empty input.
pub fn joinStringSlice(
    arena: std.mem.Allocator,
    names: []const []const u8,
) ![]const u8 {
    if (names.len == 0) return arena.dupe(u8, "");

    // Calculate total length.
    var total: usize = 0;
    for (names) |n| total += n.len;
    total += names.len - 1; // separating commas

    const buf = try arena.alloc(u8, total);
    var pos: usize = 0;
    for (names, 0..) |n, i| {
        @memcpy(buf[pos .. pos + n.len], n);
        pos += n.len;
        if (i + 1 < names.len) {
            buf[pos] = ',';
            pos += 1;
        }
    }
    return buf;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

const testing = std.testing;

test "BuilderError: init captures field, value, constraint, cause" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const alloc = arena.allocator();

    const err = try BuilderError.init(alloc, .depends, "depends", "compile,link", "invalid_reference", error.OutOfMemory);

    try testing.expectEqual(Phase.depends, err.phase);
    try testing.expectEqualStrings("depends", err.field.?);
    try testing.expectEqualStrings("compile,link", err.value.?);
    try testing.expectEqualStrings("invalid_reference", err.constraint.?);
    try testing.expectEqual(error.OutOfMemory, err.cause);
    try testing.expect(std.mem.indexOf(u8, err.message, "depends") != null);
    try testing.expect(std.mem.indexOf(u8, err.message, "compile,link") != null);
    try testing.expect(std.mem.indexOf(u8, err.message, "OutOfMemory") != null);
}

test "BuilderError: value is truncated to max_value_len" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const alloc = arena.allocator();

    var long_value: [200]u8 = undefined;
    @memset(&long_value, 'x');

    const err = try BuilderError.init(alloc, .command, "command", &long_value, null, error.InvalidCharacter);
    try testing.expectEqual(max_value_len, err.value.?.len);
}

test "BuilderError: null field and constraint format correctly" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const alloc = arena.allocator();

    const err = try BuilderError.init(alloc, .registration, null, null, null, error.FileNotFound);
    try testing.expect(std.mem.indexOf(u8, err.message, "FileNotFound") != null);
    try testing.expect(std.mem.indexOf(u8, err.message, "(none)") != null);
}

test "BuilderError: format writes message" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const alloc = arena.allocator();

    const err = try BuilderError.init(alloc, .provides, "provides", "artifact", "required", error.NameTooLong);

    var buf: [512]u8 = undefined;
    var fbs = std.io.fixedBufferStream(&buf);
    try fbs.writer().writeAll(err.message);
    const written = fbs.getWritten();
    try testing.expect(std.mem.indexOf(u8, written, "provides") != null);
    try testing.expect(std.mem.indexOf(u8, written, "artifact") != null);
}

test "BuilderError: chain appends parent message" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const alloc = arena.allocator();

    const parent = try BuilderError.init(alloc, .registration, "id", "42", "unique", error.AlreadyExists);
    const child = try BuilderError.init(alloc, .depends, "deps", "x", "missing", error.FileNotFound);
    try BuilderError.chain(alloc, child, parent);

    try testing.expect(std.mem.indexOf(u8, child.message, "caused by") != null);
    try testing.expect(std.mem.indexOf(u8, child.message, "AlreadyExists") != null);
}

test "joinStringSlice: empty returns empty" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const result = try joinStringSlice(arena.allocator(), &[_][]const u8{});
    try testing.expectEqualStrings("", result);
}

test "joinStringSlice: single name" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const result = try joinStringSlice(arena.allocator(), &[_][]const u8{"compile"});
    try testing.expectEqualStrings("compile", result);
}

test "joinStringSlice: multiple names comma-separated" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const result = try joinStringSlice(arena.allocator(), &[_][]const u8{ "compile", "link", "test" });
    try testing.expectEqualStrings("compile,link,test", result);
}

test "BuilderError: GPA no leaks" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("memory leak");
    var arena = std.heap.ArenaAllocator.init(gpa.allocator());
    defer arena.deinit();

    _ = try BuilderError.init(arena.allocator(), .validation, "port", "99999", "max=65535", error.Overflow);
}
