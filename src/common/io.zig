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
/// Callers declare storage on the stack and pass a pointer to `WriterState`.
pub const WriterState = struct {
    buf: [BUFFER_SIZE]u8,
    fw: std.fs.File.Writer,

    pub fn stdout() WriterState {
        var s: WriterState = undefined;
        s.fw = std.fs.File.stdout().writer(&s.buf);
        return s;
    }

    pub fn writer(self: *WriterState) *std.Io.Writer {
        return &self.fw.interface;
    }
};

/// A buffered reader wrapping a `std.fs.File`.
pub const ReaderState = struct {
    buf: [BUFFER_SIZE]u8,
    fr: std.fs.File.Reader,

    pub fn stdin() ReaderState {
        var s: ReaderState = undefined;
        s.fr = std.fs.File.stdin().reader(&s.buf);
        return s;
    }

    pub fn reader(self: *ReaderState) *std.Io.Reader {
        return &self.fr.interface;
    }
};
