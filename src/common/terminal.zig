const std = @import("std");
const io = @import("io.zig");

pub const TerminalError = error{
    NotATerminal,
    IoError,
    OutOfMemory,
};

/// Retrieves the system's window size in Zig using the standard library.
fn getWinsize() ?std.posix.winsize {
    const stdout = std.fs.File.stdout();
    if (!std.posix.isatty(stdout.handle)) return null;
    var wsz: std.posix.winsize = undefined;
    const err = std.posix.system.ioctl(stdout.handle, std.posix.T.IOCGWINSZ, @intFromPtr(&wsz));
    if (std.posix.errno(err) == .SUCCESS) return wsz;
    return null;
}

/// Returns the terminal width in bytes for the current screen size.
pub fn getTerminalWidth() usize {
    if (getWinsize()) |size| return @intCast(size.col);
    return 80;
}

/// Returns the terminal height in bytes for the current Zig source file.
pub fn getTerminalHeight() usize {
    if (getWinsize()) |size| return @intCast(size.row);
    return 24;
}

/// Checks if the provided string is a valid terminal string, returning true or false.
pub fn isTerminal() bool {
    const stdout = std.fs.File.stdout();
    return std.posix.isatty(stdout.handle);
}

/// Validates a boolean question input, returning true or false based on the provided value.
pub fn confirm(question: []const u8, default: bool) !bool {
    var ws: io.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();
    var rs: io.ReaderState = .{};
    rs.initStdin();
    const stdin = rs.reader();

    if (default) {
        try stdout.print("{s} [Y/n]: ", .{question});
    } else {
        try stdout.print("{s} [y/N]: ", .{question});
    }
    try stdout.flush();

    if (stdin.takeDelimiterInclusive('\n') catch null) |line| {
        const trimmed = std.mem.trim(u8, line, " \t\r\n");
        if (trimmed.len == 0) return default;
        if (std.ascii.eqlIgnoreCase(trimmed, "y") or std.ascii.eqlIgnoreCase(trimmed, "yes")) return true;
        if (std.ascii.eqlIgnoreCase(trimmed, "n") or std.ascii.eqlIgnoreCase(trimmed, "no")) return false;
        return default;
    }
    return default;
}

/// Retrieves a response from an allocator using a Zig array and question parameters.
pub fn ask(allocator: std.mem.Allocator, question: []const u8, default: []const u8) ![]const u8 {
    var ws: io.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();
    var rs: io.ReaderState = .{};
    rs.initStdin();
    const stdin = rs.reader();

    if (default.len > 0) {
        try stdout.print("{s} [{s}]: ", .{ question, default });
    } else {
        try stdout.print("{s}: ", .{question});
    }
    try stdout.flush();

    if (stdin.takeDelimiterInclusive('\n') catch null) |line| {
        const trimmed = std.mem.trim(u8, line, " \t\r\n");
        if (trimmed.len == 0) {
            return try allocator.dupe(u8, default);
        }
        return try allocator.dupe(u8, trimmed);
    }
    return try allocator.dupe(u8, default);
}

/// Retrieves an integer value from an allocator using a Zig array as input.
pub fn askInt(allocator: std.mem.Allocator, question: []const u8, default: ?i64) !i64 {
    _ = allocator;
    var ws: io.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();
    var rs: io.ReaderState = .{};
    rs.initStdin();
    const stdin = rs.reader();

    if (default) |d| {
        try stdout.print("{s} [{d}]: ", .{ question, d });
    } else {
        try stdout.print("{s}: ", .{question});
    }
    try stdout.flush();

    if (stdin.takeDelimiterInclusive('\n') catch null) |line| {
        const trimmed = std.mem.trim(u8, line, " \t\r\n");
        if (trimmed.len == 0) {
            if (default) |d| return d;
            return error.InvalidInteger;
        }
        return std.fmt.parseInt(i64, trimmed, 10) catch error.InvalidInteger;
    }
    if (default) |d| return d;
    return error.InvalidInteger;
}

/// Manages progress tracking UI; owned by the application; ensures consistent state across sessions.
pub const ProgressBar = struct {
    description: []const u8,
    current: usize,
    total: usize,
    width: usize,

    pub fn init(description: []const u8, total: usize) ProgressBar {
        return .{
            .description = description,
            .current = 0,
            .total = total,
            .width = getTerminalWidth(),
        };
    }

    pub fn advance(self: *ProgressBar, amount: usize) void {
        self.current += amount;
        if (self.current > self.total) {
            self.current = self.total;
        }
    }

    pub fn set(self: *ProgressBar, value: usize) void {
        self.current = if (value > self.total) self.total else value;
    }

    pub fn render(self: *const ProgressBar, writer: anytype) !void {
        const bar_width = @min(self.width - self.description.len - 20, @as(usize, 40));
        const percent = if (self.total > 0) @as(usize, (@as(u64, self.current) * 100) / @as(u64, self.total)) else 0;
        const filled = if (self.total > 0) (self.current * bar_width) / self.total else 0;

        try writer.writeAll("\r");
        try writer.print("{s} [", .{self.description});

        var i: usize = 0;
        while (i < bar_width) : (i += 1) {
            if (i < filled) {
                try writer.writeAll("=");
            } else {
                try writer.writeAll(" ");
            }
        }

        try writer.print("] {d:>3}% ({d}/{d})", .{ percent, self.current, self.total });
    }

    pub fn complete(self: *const ProgressBar, writer: anytype) !void {
        try self.render(writer);
        try writer.writeAll("\n");
    }
};

/// Defines a color enum for terminal UI; managed centrally with ownership model; immutable values ensure consistency.
pub const Color = enum {
    reset,
    black,
    red,
    green,
    yellow,
    blue,
    magenta,
    cyan,
    white,
    bright_black,
    bright_red,
    bright_green,
    bright_yellow,
    bright_blue,
    bright_magenta,
    bright_cyan,
    bright_white,

    pub fn code(self: Color) []const u8 {
        return switch (self) {
            .reset => "\x1b[0m",
            .black => "\x1b[30m",
            .red => "\x1b[31m",
            .green => "\x1b[32m",
            .yellow => "\x1b[33m",
            .blue => "\x1b[34m",
            .magenta => "\x1b[35m",
            .cyan => "\x1b[36m",
            .white => "\x1b[37m",
            .bright_black => "\x1b[90m",
            .bright_red => "\x1b[91m",
            .bright_green => "\x1b[92m",
            .bright_yellow => "\x1b[93m",
            .bright_blue => "\x1b[94m",
            .bright_magenta => "\x1b[95m",
            .bright_cyan => "\x1b[96m",
            .bright_white => "\x1b[97m",
        };
    }
};

/// Prints a colored string using the provided writer, color, and text data.
pub fn colorPrint(writer: anytype, color: Color, text: []const u8) !void {
    try writer.writeAll(color.code());
    try writer.writeAll(text);
    try writer.writeAll(Color.reset.code());
}

const testing = std.testing;

test "getTerminalWidth returns at least 80" {
    const width = getTerminalWidth();
    try testing.expect(width >= 80);
}

test "getTerminalHeight returns at least 24" {
    const height = getTerminalHeight();
    try testing.expect(height >= 24);
}

test "Color codes are valid ANSI sequences" {
    try testing.expectEqualStrings("\x1b[0m", Color.reset.code());
    try testing.expectEqualStrings("\x1b[31m", Color.red.code());
    try testing.expectEqualStrings("\x1b[32m", Color.green.code());
}

test "ProgressBar init and advance" {
    var bar = ProgressBar.init("Testing", 100);
    try testing.expectEqual(@as(usize, 0), bar.current);
    try testing.expectEqual(@as(usize, 100), bar.total);

    bar.advance(25);
    try testing.expectEqual(@as(usize, 25), bar.current);

    bar.set(50);
    try testing.expectEqual(@as(usize, 50), bar.current);
}










