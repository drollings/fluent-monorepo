//! Tests for io.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const io_mod = @import("io.zig");

test "WriterState writes to a pipe without emitting garbage" {
    // Create a pipe so we can capture what WriterState actually writes.
    const pipe = try std.posix.pipe();
    const read_fd = std.fs.File{ .handle = pipe[0] };
    const write_fd = std.fs.File{ .handle = pipe[1] };
    defer read_fd.close();

    // Build a WriterState backed by the write end of the pipe.
    var ws: io_mod.WriterState = .{};
    ws.fw = write_fd.writer(&ws.buf);
    const w = ws.writer();

    const msg = "hello io";
    try w.writeAll(msg);
    try ws.fw.interface.flush();
    write_fd.close();

    var rbuf: [64]u8 = undefined;
    const n = try read_fd.read(&rbuf);
    // Must receive exactly the message — no leading garbage bytes.
    try std.testing.expectEqualStrings(msg, rbuf[0..n]);
}
test "WriterState buf pointer stays valid after initStdout (no dangling)" {
    // Ensure the internal buf pointer inside fw refers to *this* struct's buf,
    // not a stale copy.  We verify by checking that fw.interface.buffer points
    // into ws.buf.
    var ws: io_mod.WriterState = .{};
    ws.initStdout();
    const w = ws.writer();
    // The writer interface must point into ws, not a dangling stack frame.
    const buf_start = @intFromPtr(&ws.buf[0]);
    const buf_end = buf_start + io_mod.BUFFER_SIZE;
    const fw_buf_ptr = @intFromPtr(ws.fw.interface.buffer.ptr);
    try std.testing.expect(fw_buf_ptr >= buf_start and fw_buf_ptr < buf_end);
    _ = w; // writer() must compile and return without crash
}
test "makePathAbsolute creates nested directories" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const nested = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "a", "b", "c" });
    defer std.testing.allocator.free(nested);

    try io_mod.makePathAbsolute(nested);

    // Verify all levels exist
    const level_a = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "a" });
    defer std.testing.allocator.free(level_a);
    var dir_a = try std.Io.Dir.openDirAbsolute(std.Io.Threaded.global_single_threaded.io(), level_a, .{});
    dir_a.close();

    const level_b = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "a", "b" });
    defer std.testing.allocator.free(level_b);
    var dir_b = try std.Io.Dir.openDirAbsolute(std.Io.Threaded.global_single_threaded.io(), level_b, .{});
    dir_b.close();
}
test "makePathAbsolute is idempotent for existing paths" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const nested = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "x", "y" });
    defer std.testing.allocator.free(nested);

    try io_mod.makePathAbsolute(nested);
    // Calling again should succeed (PathAlreadyExists is handled)
    try io_mod.makePathAbsolute(nested);
}
test "readFileAlloc returns null for non-existent file" {
    const result = io_mod.readFileAlloc(std.testing.allocator, "/nonexistent/path/file.txt", 1024);
    try std.testing.expect(result == null);
}
test "readFileAlloc reads file content" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const file_path = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "test.txt" });
    defer std.testing.allocator.free(file_path);

    const f = try std.Io.Dir.createFileAbsolute(std.Io.Threaded.global_single_threaded.io(), file_path, .{});
    try f.writeAll("hello world");
    f.close();

    const content = io_mod.readFileAlloc(std.testing.allocator, file_path, 1024).?;
    defer std.testing.allocator.free(content);
    try std.testing.expectEqualStrings("hello world", content);
}
test "resolvePath returns absolute path unchanged" {
    const result = try io_mod.resolvePath(std.testing.allocator, "/base", "/absolute/path");
    defer std.testing.allocator.free(result);
    try std.testing.expectEqualStrings("/absolute/path", result);
}
test "resolvePath joins relative path with base" {
    const result = try io_mod.resolvePath(std.testing.allocator, "/base", "relative/path");
    defer std.testing.allocator.free(result);
    try std.testing.expectEqualStrings("/base/relative/path", result);
}
test "resolvePath handles dot by returning base" {
    const result = try io_mod.resolvePath(std.testing.allocator, "/base", ".");
    defer std.testing.allocator.free(result);
    try std.testing.expectEqualStrings("/base", result);
}
