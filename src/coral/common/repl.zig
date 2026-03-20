const std = @import("std");
const BuildContext = @import("context.zig").BuildContext;
const TargetRegistry = @import("registry.zig").TargetRegistry;
const StringInterner = @import("interner.zig").StringInterner;
const json_parser = @import("json_parser.zig");
const io = @import("common");

pub const Repl = @This();

allocator: std.mem.Allocator,
ctx: *BuildContext,

pub fn init(allocator: std.mem.Allocator, ctx: *BuildContext) Repl {
    return .{
        .allocator = allocator,
        .ctx = ctx,
    };
}

pub fn deinit(_: *Repl) void {}

pub fn run(self: *Repl) !void {
    var ws: io.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    var rs: io.ReaderState = .{};
    rs.initStdin();
    const stdin = rs.reader();

    try stdout.writeAll("coral REPL\n");
    try stdout.writeAll("Type .help for commands\n");
    try stdout.flush();

    while (true) {
        try stdout.writeAll("\ncoral> ");
        try stdout.flush();

        const line_with_nl = stdin.takeDelimiterInclusive('\n') catch |err| {
            if (err == error.EndOfStream) break;
            return err;
        };

        const input = std.mem.trim(u8, line_with_nl, " \t\r\n");
        if (input.len == 0) continue;

        if (std.mem.startsWith(u8, input, ".")) {
            const handled = try self.handleDotCommand(input, stdout);
            if (!handled) break;
        } else {
            try self.handleBuildCommand(input, stdout);
        }

        try stdout.flush();
    }

    try stdout.writeAll("Goodbye!\n");
    try stdout.flush();
}

fn handleDotCommand(self: *Repl, input: []const u8, writer: *std.Io.Writer) !bool {
    var tokens = std.mem.tokenizeSequence(u8, input, " ");
    const cmd = tokens.next() orelse return true;

    if (std.mem.eql(u8, cmd, ".quit") or std.mem.eql(u8, cmd, ".exit")) {
        return false;
    }

    if (std.mem.eql(u8, cmd, ".help")) {
        try writer.writeAll(
            \\Commands:
            \\  .help                Show this help
            \\  .list                List available targets
            \\  .graph [targets]     Show dependency graph
            \\  .load <file>         Load targets from JSON file
            \\  .clear               Clear loaded targets
            \\  .quit / .exit        Exit REPL
            \\
        );
        return true;
    }

    if (std.mem.eql(u8, cmd, ".list")) {
        try self.ctx.listTargets(writer);
        return true;
    }

    if (std.mem.eql(u8, cmd, ".graph")) {
        var tgt_list: std.ArrayListUnmanaged([]const u8) = .{};
        defer tgt_list.deinit(self.allocator);

        while (tokens.next()) |tok| {
            try tgt_list.append(self.allocator, tok);
        }

        try self.ctx.showGraph(tgt_list.items, writer);
        return true;
    }

    if (std.mem.eql(u8, cmd, ".load")) {
        const filepath = tokens.next() orelse {
            try writer.writeAll("Usage: .load <file.json>\n");
            return true;
        };

        json_parser.parseFile(self.allocator, filepath, self.ctx.registry, self.ctx.interner) catch |err| {
            try writer.print("Failed to load '{s}': {}\n", .{ filepath, err });
            return true;
        };

        try writer.print("Loaded targets from '{s}'\n", .{filepath});
        return true;
    }

    if (std.mem.eql(u8, cmd, ".clear")) {
        self.ctx.registry.deinit();
        self.ctx.registry.* = TargetRegistry.init(self.allocator, self.ctx.interner);
        try writer.writeAll("Cleared all targets\n");
        return true;
    }

    try writer.print("Unknown command: {s}\n", .{cmd});
    return true;
}

fn handleBuildCommand(self: *Repl, input: []const u8, writer: *std.Io.Writer) !void {
    var tgt_list: std.ArrayListUnmanaged([]const u8) = .{};
    defer tgt_list.deinit(self.allocator);

    var tokens = std.mem.tokenizeSequence(u8, input, " ");
    while (tokens.next()) |tok| {
        try tgt_list.append(self.allocator, tok);
    }

    var result = self.ctx.build(tgt_list.items) catch |err| {
        try writer.print("Build failed: {}\n", .{err});
        return;
    };
    defer result.deinit(self.allocator);

    if (result.success) {
        try writer.print("Build completed: {d} targets built in {d}ms\n", .{
            result.targets_built,
            result.duration_ns / 1_000_000,
        });
    } else {
        try writer.print("Build failed: {d}/{d} targets\n", .{
            result.targets_failed,
            result.targets_built + result.targets_failed,
        });
    }
}
