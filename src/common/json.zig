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
// JSON file loading
// =============================================================================

/// Converts a JSON string to a Zig JSON value, handling allocator and size limits.
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

test "parseJsonFile returns null for missing file" {
    const result = parseJsonFile(std.testing.allocator, "/nonexistent/file.json", 1024);
    try std.testing.expect(result == null);
}

test "parseJsonFile parses a valid object" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const json_path = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "test.json" });
    defer std.testing.allocator.free(json_path);

    const f = try std.fs.createFileAbsolute(json_path, .{});
    try f.writeAll("{\"key\":\"value\"}");
    f.close();

    var parsed = parseJsonFile(std.testing.allocator, json_path, 1024).?;
    defer parsed.deinit();
    try std.testing.expect(parsed.value == .object);
    try std.testing.expect(parsed.value.object.get("key") != null);
}

test "appendEscaped handles control chars" {
    var buf: std.ArrayListUnmanaged(u8) = .{};
    defer buf.deinit(std.testing.allocator);
    try appendEscaped(&buf, std.testing.allocator, "\x01\x1f normal");
    try std.testing.expect(std.mem.indexOf(u8, buf.items, "\\u0001") != null);
    try std.testing.expect(std.mem.indexOf(u8, buf.items, "normal") != null);
}
