/// json.zig — Generic JSON serialization helpers
///
/// Provides allocator-friendly JSON utilities that have no dependency on
/// any guidance-domain types.  Suitable for reuse in any Zig tool that
/// needs to produce or manipulate JSON text.
const std = @import("std");

// =============================================================================
// Allocating serialization
// =============================================================================

/// Serialize `value` to pretty-printed JSON (2-space indent).
/// Returns an owned allocation; caller must free.
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

/// Write `s` to `writer` with all JSON-special characters escaped.
/// Handles `"`, `\`, `\n`, `\r`, `\t`.  Suitable for building JSON by hand.
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

/// Append `text` to `buf`, escaping all JSON-special characters including
/// C0 control characters (encoded as `\uXXXX`).
/// More thorough than `writeEscaped`; use when the input may contain
/// arbitrary binary data.
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
// Tests
// =============================================================================

test "jsonStringifyAlloc basic struct" {
    const v = .{ .x = 1, .y = 2 };
    const out = try jsonStringifyAlloc(std.testing.allocator, v);
    defer std.testing.allocator.free(out);
    try std.testing.expect(std.mem.indexOf(u8, out, "\"x\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "\"y\"") != null);
}

test "writeEscaped handles special chars" {
    var buf: std.ArrayListUnmanaged(u8) = .{};
    defer buf.deinit(std.testing.allocator);
    try writeEscaped(buf.writer(std.testing.allocator), "a\"b\\c\nd");
    try std.testing.expectEqualStrings("a\\\"b\\\\c\\nd", buf.items);
}

test "appendEscaped handles control chars" {
    var buf: std.ArrayListUnmanaged(u8) = .{};
    defer buf.deinit(std.testing.allocator);
    try appendEscaped(&buf, std.testing.allocator, "\x01\x1f normal");
    try std.testing.expect(std.mem.indexOf(u8, buf.items, "\\u0001") != null);
    try std.testing.expect(std.mem.indexOf(u8, buf.items, "normal") != null);
}
