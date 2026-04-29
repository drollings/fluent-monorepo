//! Tests for json.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const json_mod = @import("json.zig");

test "jsonStringifyAlloc basic struct" {
    const v = .{ .x = 1, .y = 2 };
    const out = try json_mod.jsonStringifyAlloc(std.testing.allocator, v);
    defer std.testing.allocator.free(out);
    try std.testing.expect(std.mem.indexOf(u8, out, "\"x\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "\"y\"") != null);
}
test "writeEscaped handles special chars" {
    var aw: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer aw.deinit();
    try json_mod.writeEscaped(&aw.writer, "a\"b\\c\nd");
    try std.testing.expectEqualStrings("a\\\"b\\\\c\\nd", aw.written());
}
test "parseJsonFile returns null for missing file" {
    const result = json_mod.parseJsonFile(std.testing.allocator, "/nonexistent/file.json", 1024);
    try std.testing.expect(result == null);
}
test "parseJsonFile parses a valid object" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const json_path = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "test.json" });
    defer std.testing.allocator.free(json_path);

    const io = std.Io.Threaded.global_single_threaded.io();
    const f = try std.Io.Dir.createFileAbsolute(io, json_path, .{});
    {
        var wbuf: [4096]u8 = undefined;
        var writer = f.writer(io, &wbuf);
        try writer.interface.writeAll("{\"key\":\"value\"}");
        try writer.interface.flush();
    }
    f.close(io);

    var parsed = json_mod.parseJsonFile(std.testing.allocator, json_path, 1024).?;
    defer parsed.deinit();
    try std.testing.expect(parsed.value == .object);
    try std.testing.expect(parsed.value.object.get("key") != null);
}
test "appendEscaped handles control chars" {
    var buf: std.ArrayList(u8) = .empty;
    defer buf.deinit(std.testing.allocator);
    try json_mod.appendEscaped(&buf, std.testing.allocator, "\x01\x1f normal");
    try std.testing.expect(std.mem.indexOf(u8, buf.items, "\\u0001") != null);
    try std.testing.expect(std.mem.indexOf(u8, buf.items, "normal") != null);
}