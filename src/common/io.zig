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

/// A buffered writer wrapping a `std.fs.File`.
/// Callers declare storage on the stack and call `initStdout()` on the
/// local variable **after** it is at its final address.  Never copy a
/// `WriterState` after initialisation — the internal pointer would dangle.
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

/// A buffered reader wrapping a `std.fs.File`.
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
