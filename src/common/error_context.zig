/// error_context.zig — Structured error context for non-builder code paths.
///
/// Provides a lighter-weight alternative to BuilderError for cases where:
/// - Arena allocation is not justified (single error, not a chain)
/// - Stack allocation is preferred
/// - The error needs context but not the full builder lifecycle
///
/// Usage:
///   const ctx = ErrorContext.init("parse_config", "file", path, error.FileNotFound);
///   std.log.err("error: {}", .{ctx});
///   // Output: error: [parse_config] file=/etc/config.json: FileNotFound
///
/// For richer context with arena allocation, use BuilderError instead.
const std = @import("std");

/// Maximum bytes copied from a user-supplied value into an ErrorContext.
const max_value_len: usize = 128;

/// Manages error context structures, owns state, ensures invariants; not thread-safe.
pub const ErrorContext = struct {
    /// Operation that failed (static string literal).
    operation: []const u8,
    /// Field being processed (static string literal).
    field: ?[]const u8,
    /// Value being processed (copied into internal buffer).
    value: ValueBuffer,
    /// Underlying Zig error.
    cause: anyerror,

    /// Internal buffer for value copy.
    const ValueBuffer = struct {
        data: [max_value_len]u8,
        len: usize,

        pub fn slice(self: *const ValueBuffer) []const u8 {
            return self.data[0..self.len];
        }
    };

    /// Initialize an ErrorContext with all fields.
    /// `operation` and `field` should be static string literals.
    /// `value` is copied (truncated to max_value_len bytes).
    pub fn init(
        operation: []const u8,
        field: ?[]const u8,
        value: ?[]const u8,
        cause: anyerror,
    ) ErrorContext {
        var ctx: ErrorContext = .{
            .operation = operation,
            .field = field,
            .value = .{ .data = undefined, .len = 0 },
            .cause = cause,
        };
        if (value) |v| {
            const len = @min(v.len, max_value_len);
            @memcpy(ctx.value.data[0..len], v[0..len]);
            ctx.value.len = len;
        }
        return ctx;
    }

    /// Create an ErrorContext with just operation and cause.
    pub fn simple(operation: []const u8, cause: anyerror) ErrorContext {
        return .{
            .operation = operation,
            .field = null,
            .value = .{ .data = undefined, .len = 0 },
            .cause = cause,
        };
    }

    /// Format error for logging.
    /// Format: "[{operation}] {field}={value}: {cause}"
    /// Null fields are omitted.
    pub fn format(
        self: *const ErrorContext,
        comptime _: []const u8,
        _: std.fmt.FormatOptions,
        writer: anytype,
    ) !void {
        try writer.writeAll("[");
        try writer.writeAll(self.operation);
        try writer.writeAll("]");
        if (self.field) |f| {
            try writer.writeAll(" ");
            try writer.writeAll(f);
            if (self.value.len > 0) {
                try writer.writeAll("=");
                try writer.writeAll(self.value.slice());
            }
        }
        try writer.writeAll(": ");
        try writer.print("{s}", .{@errorName(self.cause)});
    }

    /// Convert to a heap-allocated message via arena.
    /// Use when you need error message persistence beyond the stack frame.
    pub fn toMessage(self: *const ErrorContext, arena: std.mem.Allocator) ![]const u8 {
        return std.fmt.allocPrint(arena, "{}", .{self});
    }
};

/// Manages error context structures for arena allocations, ensuring ownership and invariants during initialization.
pub const ArenaErrorContext = struct {
    operation: []const u8,
    field: ?[]const u8,
    value: ?[]const u8,
    cause: anyerror,
    message: []const u8,

    /// Allocate from arena and format message.
    pub fn init(
        arena: std.mem.Allocator,
        operation: []const u8,
        field: ?[]const u8,
        value: ?[]const u8,
        cause: anyerror,
    ) !ArenaErrorContext {
        const value_copy = if (value) |v|
            try arena.dupe(u8, v[0..@min(v.len, max_value_len)])
        else
            null;

        const message = try std.fmt.allocPrint(
            arena,
            "[{s}] {s}={s}: {s}",
            .{
                operation,
                field orelse "",
                value_copy orelse "",
                @errorName(cause),
            },
        );

        return .{
            .operation = operation,
            .field = field,
            .value = value_copy,
            .cause = cause,
            .message = message,
        };
    }

    pub fn format(
        self: *const ArenaErrorContext,
        comptime _: []const u8,
        _: std.fmt.FormatOptions,
        writer: anytype,
    ) !void {
        try writer.writeAll(self.message);
    }
};

/// Links error contexts across levels in the arena hierarchy.
pub fn chain(
    arena: std.mem.Allocator,
    child: *ArenaErrorContext,
    parent: *const ArenaErrorContext,
) !void {
    child.message = try std.fmt.allocPrint(
        arena,
        "{s} (caused by: {s})",
        .{ child.message, parent.message },
    );
}

// ── Tests ─────────────────────────────────────────────────────────────────────

const testing = std.testing;

test "ErrorContext: init with all fields" {
    const ctx = ErrorContext.init("parse_config", "file", "/etc/config.json", error.FileNotFound);
    try testing.expectEqualStrings("parse_config", ctx.operation);
    try testing.expectEqualStrings("file", ctx.field.?);
    try testing.expectEqualStrings("/etc/config.json", ctx.value.slice());
    try testing.expectEqual(error.FileNotFound, ctx.cause);
}

test "ErrorContext: value truncation" {
    var long_value: [200]u8 = undefined;
    @memset(&long_value, 'x');
    const ctx = ErrorContext.init("test", "field", &long_value, error.Overflow);
    try testing.expectEqual(@as(usize, max_value_len), ctx.value.len);
}

test "ErrorContext: simple operation" {
    const ctx = ErrorContext.simple("connect", error.ConnectionRefused);
    try testing.expectEqualStrings("connect", ctx.operation);
    try testing.expect(ctx.field == null);
    try testing.expectEqual(error.ConnectionRefused, ctx.cause);
}

test "ErrorContext: format with all fields" {
    const ctx = ErrorContext.init("parse", "port", "99999", error.Overflow);
    var buf: [256]u8 = undefined;
    var fbs = std.io.fixedBufferStream(&buf);
    try ctx.format("", .{}, fbs.writer());
    const written = fbs.getWritten();
    try testing.expect(std.mem.indexOf(u8, written, "[parse]") != null);
    try testing.expect(std.mem.indexOf(u8, written, "port=99999") != null);
    try testing.expect(std.mem.indexOf(u8, written, "Overflow") != null);
}

test "ErrorContext: format without field" {
    const ctx = ErrorContext.simple("connect", error.Timeout);
    var buf: [256]u8 = undefined;
    var fbs = std.io.fixedBufferStream(&buf);
    try ctx.format("", .{}, fbs.writer());
    const written = fbs.getWritten();
    try testing.expect(std.mem.indexOf(u8, written, "[connect]") != null);
    try testing.expect(std.mem.indexOf(u8, written, "Timeout") != null);
}

test "ErrorContext: toMessage arena allocation" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const ctx = ErrorContext.init("load", "path", "/data/file.txt", error.FileNotFound);
    const msg = try ctx.toMessage(arena.allocator());
    try testing.expect(std.mem.indexOf(u8, msg, "[load]") != null);
    try testing.expect(std.mem.indexOf(u8, msg, "FileNotFound") != null);
}

test "ArenaErrorContext: init allocates from arena" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const alloc = arena.allocator();

    const ctx = try ArenaErrorContext.init(alloc, "write", "buffer", "output.bin", error.DiskFull);
    try testing.expectEqualStrings("write", ctx.operation);
    try testing.expectEqualStrings("buffer", ctx.field.?);
    try testing.expectEqualStrings("output.bin", ctx.value.?);
    try testing.expectEqual(error.DiskFull, ctx.cause);
}

test "ArenaErrorContext: chain appends parent" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const alloc = arena.allocator();

    const parent = try ArenaErrorContext.init(alloc, "read", "file", "input.txt", error.FileNotFound);
    var child = try ArenaErrorContext.init(alloc, "process", "config", null, error.InvalidData);
    try chain(alloc, &child, &parent);

    try testing.expect(std.mem.indexOf(u8, child.message, "caused by") != null);
    try testing.expect(std.mem.indexOf(u8, child.message, "FileNotFound") != null);
}



