//! Global logger with console + file output.
//!
//! For request-scoped structured logging, see common.logging.LogContext.
//! These two modules are complementary: Logger handles output destinations,
//! LogContext attaches metadata (request ID, timing).

const std = @import("std");
const io = @import("io.zig");

pub const Level = enum {
    debug,
    info,
    warn,
    err,

    pub fn prefix(self: Level) []const u8 {
        return switch (self) {
            .debug => "DEBUG",
            .info => "INFO",
            .warn => "WARN",
            .err => "ERROR",
        };
    }

    pub fn color(self: Level) []const u8 {
        return switch (self) {
            .debug => "\x1b[36m",
            .info => "\x1b[32m",
            .warn => "\x1b[33m",
            .err => "\x1b[31m",
        };
    }
};

pub const LogConfig = struct {
    level: Level = .info,
    show_timestamp: bool = false,
    show_source: bool = false,
    use_colors: bool = true,
    file_path: ?[]const u8 = null,
    name: []const u8 = "app",
};

/// Manages logging structures with ownership and invariants; ensures safe initialization/deinit.
pub const Logger = struct {
    config: LogConfig,
    allocator: std.mem.Allocator,
    file: ?std.fs.File = null,

    pub fn init(allocator: std.mem.Allocator, config: LogConfig) !*Logger {
        const self = try allocator.create(Logger);
        self.* = .{
            .config = config,
            .allocator = allocator,
            .file = null,
        };

        if (config.file_path) |path| {
            const file = try std.fs.cwd().createFile(path, .{ .truncate = false });
            try file.seekFromEnd(0);
            self.file = file;
        }

        return self;
    }

    pub fn deinit(self: *Logger) void {
        if (self.file) |f| f.close();
        self.allocator.destroy(self);
    }

    pub fn log(self: *const Logger, level: Level, comptime format: []const u8, args: anytype) void {
        if (@intFromEnum(level) < @intFromEnum(self.config.level)) return;

        var ws: io.WriterState = .{};
        ws.initStdout();
        const stderr = ws.writer();

        var buf: [8192]u8 = undefined;
        var fba = std.io.fixedBufferStream(&buf);
        const writer = fba.writer();

        if (self.config.use_colors) {
            writer.writeAll(level.color()) catch {};
        }

        if (self.config.show_timestamp) {
            const timestamp = std.time.timestamp();
            const epoch: std.time.epoch.EpochSeconds = .{ .secs = @intCast(timestamp) };
            const day_seconds = epoch.getDaySeconds();
            const year_day = epoch.getEpochDay();
            writer.print("{d:0>4}-{d:0>2}-{d:0>2} {d:0>2}:{d:0>2}:{d:0>2} ", .{
                year_day.calculateYearDay().year,
                year_day.calculateYearDay().calculateMonthDay().month.numeric(),
                year_day.calculateYearDay().calculateMonthDay().day_index + 1,
                day_seconds.getHoursIntoDay(),
                day_seconds.getMinutesIntoHour(),
                day_seconds.getSecondsIntoMinute(),
            }) catch {};
        }

        writer.print("{s:>5}", .{level.prefix()}) catch {};

        if (self.config.show_source) {
            writer.print(" [{s}]", .{self.config.name}) catch {};
        }

        writer.print(" ", .{}) catch {};
        writer.print(format, args) catch {};

        if (self.config.use_colors) {
            writer.writeAll("\x1b[0m") catch {};
        }

        writer.writeAll("\n") catch {};

        const output = fba.getWritten();
        stderr.writeAll(output) catch {};

        if (self.file) |f| {
            const file_writer = f.writer();
            file_writer.writeAll(output) catch {};
        }
    }

    pub fn debug(self: *const Logger, comptime format: []const u8, args: anytype) void {
        self.log(.debug, format, args);
    }

    pub fn info(self: *const Logger, comptime format: []const u8, args: anytype) void {
        self.log(.info, format, args);
    }

    pub fn warn(self: *const Logger, comptime format: []const u8, args: anytype) void {
        self.log(.warn, format, args);
    }

    pub fn err(self: *const Logger, comptime format: []const u8, args: anytype) void {
        self.log(.err, format, args);
    }
};

threadlocal var global_logger: ?*Logger = null;

/// Updates global logging configuration by setting a logger instance.
pub fn setGlobal(logger: *Logger) void {
    global_logger = logger;
}

/// Retrieves a global logger instance from the system.
pub fn getGlobal() ?*Logger {
    return global_logger;
}

/// Logs a message with specified level and format, returning void.
pub fn logGlobal(level: Level, comptime format: []const u8, args: anytype) void {
    if (global_logger) |logger| {
        logger.log(level, format, args);
    }
}

/// Initializes logging configuration with allocator, verbosity, quiet settings, and optional log file paths.
pub fn setupLogging(
    allocator: std.mem.Allocator,
    verbose: bool,
    quiet: bool,
    log_file: ?[]const u8,
    name: []const u8,
) !*Logger {
    const level: Level = if (quiet) .err else if (verbose) .debug else .info;

    const config = LogConfig{
        .level = level,
        .show_timestamp = verbose,
        .show_source = verbose,
        .use_colors = true,
        .file_path = log_file,
        .name = name,
    };

    const logger = try Logger.init(allocator, config);
    setGlobal(logger);
    return logger;
}

const testing = std.testing;

test "Level: prefix and color" {
    try testing.expectEqualStrings("DEBUG", Level.debug.prefix());
    try testing.expectEqualStrings("INFO", Level.info.prefix());
    try testing.expectEqualStrings("WARN", Level.warn.prefix());
    try testing.expectEqualStrings("ERROR", Level.err.prefix());
}

test "Logger: init and deinit" {
    const logger = try Logger.init(testing.allocator, .{ .name = "test" });
    defer logger.deinit();
    try testing.expectEqualStrings("test", logger.config.name);
}
