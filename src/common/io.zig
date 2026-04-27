/// io.zig — Shared buffered I/O helpers
///
/// Centralises the repeated pattern:
///   var buf: [4096]u8 = undefined;
///   var fw = std.Io.File.stdout().writer(io, &buf);
///   const w = &fw.interface;
///
/// that appeared verbatim in both main.zig and repl.zig.
const std = @import("std");

pub const BUFFER_SIZE = 4096;

/// Returns the global single-threaded Io context.
/// Use this for synchronous file I/O in functions that don't receive an Io from their caller.
pub fn singleIo() std.Io {
    return std.Io.Threaded.global_single_threaded.io();
}

pub const WriterState = struct {
    buf: [BUFFER_SIZE]u8 = undefined,
    fw: std.Io.File.Writer = undefined,

    /// Wire up the buffered writer to stdout.  Call this exactly once on a
    /// stack-allocated `WriterState` before calling `writer()`.
    pub fn initStdout(self: *WriterState) void {
        self.fw = std.Io.File.stdout().writer(
            std.Io.Threaded.global_single_threaded.io(),
            &self.buf,
        );
    }

    pub fn writer(self: *WriterState) *std.Io.Writer {
        return &self.fw.interface;
    }
};

/// Manages streaming data with fixed buffers; encapsulates state, not shared; key invariant is buffer integrity.
pub const ReaderState = struct {
    buf: [BUFFER_SIZE]u8 = undefined,
    fr: std.Io.File.Reader = undefined,

    /// Wire up the buffered reader to stdin.  Call this exactly once on a
    /// stack-allocated `ReaderState` before calling `reader()`.
    pub fn initStdin(self: *ReaderState) void {
        self.fr = std.Io.File.stdin().reader(
            std.Io.Threaded.global_single_threaded.io(),
            &self.buf,
        );
    }

    pub fn reader(self: *ReaderState) *std.Io.Reader {
        return &self.fr.interface;
    }
};

// =============================================================================
// Tests
// =============================================================================

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
        std.Io.Dir.createDirAbsolute(singleIo(), current, .default_dir) catch |err| switch (err) {
            error.PathAlreadyExists => {},
            else => return err,
        };
    }
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
    const io = singleIo();
    return std.Io.Dir.cwd().readFileAlloc(io, path, allocator, .limited(max_size)) catch null;
}

/// Reads a file using an allocator, returning its contents or an error if allocation fails.
pub fn readFileAllocErr(allocator: std.mem.Allocator, path: []const u8, max_size: usize) ![]const u8 {
    const io = singleIo();
    return std.Io.Dir.cwd().readFileAlloc(io, path, allocator, .limited(max_size));
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
