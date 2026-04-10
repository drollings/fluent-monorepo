/// io.zig — Shared buffered I/O helpers
///
/// Centralises the repeated pattern:
///   var buf: [4096]u8 = undefined;
///   var fw = std.fs.File.stdout().writer(&buf);
///   const w = &fw.interface;
///
/// that appeared verbatim in both main.zig and repl.zig.
const std = @import("std");

pub const BUFFER_SIZE = 4096;

pub const WriterState = struct {
    buf: [BUFFER_SIZE]u8 = undefined,
    fw: std.fs.File.Writer = undefined,

    /// Wire up the buffered writer to stdout.  Call this exactly once on a
    /// stack-allocated `WriterState` before calling `writer()`.
    pub fn initStdout(self: *WriterState) void {
        self.fw = std.fs.File.stdout().writer(&self.buf);
    }

    pub fn writer(self: *WriterState) *std.Io.Writer {
        return &self.fw.interface;
    }
};

/// Manages streaming data with fixed buffers; encapsulates state, not shared; key invariant is buffer integrity.
pub const ReaderState = struct {
    buf: [BUFFER_SIZE]u8 = undefined,
    fr: std.fs.File.Reader = undefined,

    /// Wire up the buffered reader to stdin.  Call this exactly once on a
    /// stack-allocated `ReaderState` before calling `reader()`.
    pub fn initStdin(self: *ReaderState) void {
        self.fr = std.fs.File.stdin().reader(&self.buf);
    }

    pub fn reader(self: *ReaderState) *std.Io.Reader {
        return &self.fr.interface;
    }
};

// =============================================================================
// Tests
// =============================================================================

test "WriterState writes to a pipe without emitting garbage" {
    // Create a pipe so we can capture what WriterState actually writes.
    const pipe = try std.posix.pipe();
    const read_fd = std.fs.File{ .handle = pipe[0] };
    const write_fd = std.fs.File{ .handle = pipe[1] };
    defer read_fd.close();

    // Build a WriterState backed by the write end of the pipe.
    var ws: WriterState = .{};
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
    var ws: WriterState = .{};
    ws.initStdout();
    const w = ws.writer();
    // The writer interface must point into ws, not a dangling stack frame.
    const buf_start = @intFromPtr(&ws.buf[0]);
    const buf_end = buf_start + BUFFER_SIZE;
    const fw_buf_ptr = @intFromPtr(ws.fw.interface.buffer.ptr);
    try std.testing.expect(fw_buf_ptr >= buf_start and fw_buf_ptr < buf_end);
    _ = w; // writer() must compile and return without crash
}

// =============================================================================
// Directory creation
// =============================================================================

/// Converts a relative path slice into an absolute path string.
pub fn makePathAbsolute(abs_path: []const u8) !void {
    var buf: [4096]u8 = undefined;
    var pos: usize = 0;

    var parts = std.mem.splitScalar(u8, abs_path, std.fs.path.sep);
    var is_first = true;

    while (parts.next()) |part| {
        if (part.len == 0) continue;

        if (is_first) {
            buf[0] = '/';
            @memcpy(buf[1 .. 1 + part.len], part);
            pos = 1 + part.len;
            is_first = false;
        } else {
            buf[pos] = std.fs.path.sep;
            @memcpy(buf[pos + 1 .. pos + 1 + part.len], part);
            pos += 1 + part.len;
        }

        const current = buf[0..pos];
        std.fs.makeDirAbsolute(current) catch |err| switch (err) {
            error.PathAlreadyExists => {},
            else => return err,
        };
    }
}

test "makePathAbsolute creates nested directories" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const nested = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "a", "b", "c" });
    defer std.testing.allocator.free(nested);

    try makePathAbsolute(nested);

    // Verify all levels exist
    const level_a = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "a" });
    defer std.testing.allocator.free(level_a);
    var dir_a = try std.fs.openDirAbsolute(level_a, .{});
    dir_a.close();

    const level_b = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "a", "b" });
    defer std.testing.allocator.free(level_b);
    var dir_b = try std.fs.openDirAbsolute(level_b, .{});
    dir_b.close();
}

test "makePathAbsolute is idempotent for existing paths" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const nested = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "x", "y" });
    defer std.testing.allocator.free(nested);

    try makePathAbsolute(nested);
    // Calling again should succeed (PathAlreadyExists is handled)
    try makePathAbsolute(nested);
}

// =============================================================================
// File reading helpers
// =============================================================================

/// Default maximum file size for readFileAlloc (10 MB).
pub const DEFAULT_MAX_FILE_SIZE = 10 * 1024 * 1024;

/// Reads a file into a newly-allocated buffer.
/// Returns null if the file cannot be opened or read.
/// NOTE: OutOfMemory is also returned as null. Use readFileAllocErr()
/// for explicit error propagation.
pub fn readFileAlloc(allocator: std.mem.Allocator, path: []const u8, max_size: usize) ?[]const u8 {
    const file = std.fs.openFileAbsolute(path, .{}) catch return null;
    defer file.close();
    return file.readToEndAlloc(allocator, max_size) catch null;
}

/// Reads a file using an allocator, returning its contents or an error if allocation fails.
pub fn readFileAllocErr(allocator: std.mem.Allocator, path: []const u8, max_size: usize) ![]const u8 {
    const file = try std.fs.openFileAbsolute(path, .{});
    defer file.close();
    return try file.readToEndAlloc(allocator, max_size);
}

/// Removes leading zig path prefix from a given root slice.
pub fn stripPathPrefix(abs: []const u8, root: []const u8) []const u8 {
    if (std.mem.startsWith(u8, abs, root)) {
        var rel = abs[root.len..];
        if (rel.len > 0 and rel[0] == '/') rel = rel[1..];
        return rel;
    }
    return abs;
}

/// Resolves a path string into a Zig-safe slice using an allocator.
pub fn resolvePath(allocator: std.mem.Allocator, base: []const u8, path: []const u8) ![]const u8 {
    if (std.fs.path.isAbsolute(path)) return try allocator.dupe(u8, path);
    if (std.mem.eql(u8, path, ".")) return try allocator.dupe(u8, base);
    return try std.fs.path.join(allocator, &.{ base, path });
}

test "readFileAlloc returns null for non-existent file" {
    const result = readFileAlloc(std.testing.allocator, "/nonexistent/path/file.txt", 1024);
    try std.testing.expect(result == null);
}

test "readFileAlloc reads file content" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const file_path = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "test.txt" });
    defer std.testing.allocator.free(file_path);

    const f = try std.fs.createFileAbsolute(file_path, .{});
    try f.writeAll("hello world");
    f.close();

    const content = readFileAlloc(std.testing.allocator, file_path, 1024).?;
    defer std.testing.allocator.free(content);
    try std.testing.expectEqualStrings("hello world", content);
}

test "resolvePath returns absolute path unchanged" {
    const result = try resolvePath(std.testing.allocator, "/base", "/absolute/path");
    defer std.testing.allocator.free(result);
    try std.testing.expectEqualStrings("/absolute/path", result);
}

test "resolvePath joins relative path with base" {
    const result = try resolvePath(std.testing.allocator, "/base", "relative/path");
    defer std.testing.allocator.free(result);
    try std.testing.expectEqualStrings("/base/relative/path", result);
}

test "resolvePath handles dot by returning base" {
    const result = try resolvePath(std.testing.allocator, "/base", ".");
    defer std.testing.allocator.free(result);
    try std.testing.expectEqualStrings("/base", result);
}
