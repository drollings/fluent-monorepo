/// json.zig — Generic JSON serialization helpers
///
/// Provides allocator-friendly JSON utilities that have no dependency on
/// any guidance-domain types.  Suitable for reuse in any Zig tool that
/// needs to produce or manipulate JSON text.
const std = @import("std");

// =============================================================================
// Allocating serialization
// =============================================================================

/// Converts a value to a memory-safe Zig slice using an allocator.
pub fn jsonStringifyAlloc(allocator: std.mem.Allocator, value: anytype) ![]u8 {
    var out: std.io.Writer.Allocating = .init(allocator);
    const writer = &out.writer;
    defer out.deinit();

    try std.json.Stringify.value(value, .{ .whitespace = .indent_2 }, writer);
    return try allocator.dupe(u8, out.written());
}

// =============================================================================
// JSON string escaping
// =============================================================================

/// Writes a null-terminated byte slice to a writer, escaping characters as needed.
pub fn writeEscaped(writer: anytype, s: []const u8) !void {
    for (s) |c| {
        switch (c) {
            '"' => try writer.writeAll("\\\""),
            '\\' => try writer.writeAll("\\\\"),
            '\n' => try writer.writeAll("\\n"),
            '\r' => try writer.writeAll("\\r"),
            '\t' => try writer.writeAll("\\t"),
            else => try writer.writeByte(c),
        }
    }
}

/// Appends escaped characters to a Zig buffer, handling null-terminated strings with an allocator.
pub fn appendEscaped(
    buf: *std.ArrayListUnmanaged(u8),
    allocator: std.mem.Allocator,
    text: []const u8,
) !void {
    for (text) |ch| {
        switch (ch) {
            '"' => try buf.appendSlice(allocator, "\\\""),
            '\\' => try buf.appendSlice(allocator, "\\\\"),
            '\n' => try buf.appendSlice(allocator, "\\n"),
            '\r' => try buf.appendSlice(allocator, "\\r"),
            '\t' => try buf.appendSlice(allocator, "\\t"),
            0x00...0x08, 0x0b, 0x0c, 0x0e...0x1f => {
                var tmp: [6]u8 = undefined;
                const s = std.fmt.bufPrint(&tmp, "\\u{x:0>4}", .{ch}) catch unreachable;
                try buf.appendSlice(allocator, s);
            },
            else => try buf.append(allocator, ch),
        }
    }
}

// =============================================================================
// JSON file loading
// =============================================================================

/// Converts a JSON string to a Zig JSON value, handling allocation and parsing.
pub fn parseJsonFile(
    allocator: std.mem.Allocator,
    path: []const u8,
    max_size: usize,
) ?std.json.Parsed(std.json.Value) {
    const f = std.fs.openFileAbsolute(path, .{}) catch return null;
    defer f.close();
    const content = f.readToEndAlloc(allocator, max_size) catch return null;
    defer allocator.free(content);
    var parsed = std.json.parseFromSlice(
        std.json.Value,
        allocator,
        content,
        .{ .ignore_unknown_fields = true },
    ) catch return null;
    if (parsed.value != .object) {
        parsed.deinit();
        return null;
    }
    return parsed;
}

// =============================================================================
// Tests
// =============================================================================
