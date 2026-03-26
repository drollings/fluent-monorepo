const std = @import("std");
const args = @import("args.zig");

pub const CommonArgs = args.CommonArgs;
pub const parseCommonArgs = args.parseCommonArgs;

pub const CliError = error{
    UnknownCommand,
    MissingArgument,
    InvalidArgument,
    OutOfMemory,
};

/// Manages command execution logic, owns CLI workflow; ensures consistent initialization and cleanup.
pub const Command = struct {
    name: []const u8,
    description: []const u8,
    handler: *const fn (*std.process.ArgIterator, std.mem.Allocator) CliError!?i32,
    usage: []const u8 = "",
    examples: []const u8 = "",
};

/// Maps command names to their handlers; used by App to dispatch argv[1] at startup.
pub const CommandRegistry = struct {
    commands: std.StringHashMapUnmanaged(Command),
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator) CommandRegistry {
        return .{
            .commands = .{},
            .allocator = allocator,
        };
    }

    pub fn deinit(self: *CommandRegistry) void {
        self.commands.deinit(self.allocator);
    }

    pub fn register(self: *CommandRegistry, cmd: Command) !void {
        try self.commands.put(self.allocator, cmd.name, cmd);
    }

    pub fn get(self: *const CommandRegistry, name: []const u8) ?Command {
        return self.commands.get(name);
    }

    pub fn names(self: *const CommandRegistry, allocator: std.mem.Allocator) ![][]const u8 {
        var list: std.ArrayListUnmanaged([]const u8) = .{};
        errdefer list.deinit(allocator);
        var iter = self.commands.iterator();
        while (iter.next()) |entry| {
            try list.append(allocator, entry.key_ptr.*);
        }
        std.mem.sort([]const u8, list.items, {}, struct {
            fn lessThan(_: void, a: []const u8, b: []const u8) bool {
                return std.mem.lessThan(u8, a, b);
            }
        }.lessThan);
        return list.toOwnedSlice(allocator);
    }
};

/// Top-level CLI entry point: name, description, version string, and a CommandRegistry for subcommand dispatch.
pub const App = struct {
    name: []const u8,
    description: []const u8,
    version: []const u8,
    registry: CommandRegistry,
    global_args: CommonArgs,

    pub fn init(
        allocator: std.mem.Allocator,
        name: []const u8,
        description: []const u8,
        version: []const u8,
    ) App {
        return .{
            .name = name,
            .description = description,
            .version = version,
            .registry = CommandRegistry.init(allocator),
            .global_args = .{},
        };
    }

    pub fn deinit(self: *App) void {
        self.registry.deinit();
    }

    pub fn registerCommand(self: *App, cmd: Command) !void {
        try self.registry.register(cmd);
    }

    pub fn printHelp(self: *const App, writer: *std.Io.Writer) !void {
        try writer.print("{s} - {s}\n\n", .{ self.name, self.description });
        try writer.print("Usage: {s} [options] <command> [args]\n\n", .{self.name});
        try writer.writeAll("Global options:\n");
        try writer.writeAll("  -h, --help           Show this help message\n");
        try writer.writeAll("  -v, --version        Show version\n");
        try writer.writeAll("  --verbose            Enable verbose output\n");
        try writer.writeAll("  --debug              Enable debug output\n");
        try writer.writeAll("  --dry-run            Show what would be done\n");
        try writer.writeAll("  --force              Force execution\n");
        try writer.writeAll("\nCommands:\n");

        const cmd_names = try self.registry.names(std.heap.page_allocator);
        defer std.heap.page_allocator.free(cmd_names);

        for (cmd_names) |cmd_name| {
            if (self.registry.get(cmd_name)) |cmd| {
                try writer.print("  {s:<20} {s}\n", .{ cmd.name, cmd.description });
            }
        }
    }

    pub fn run(self: *App, argv: []const []const u8) !i32 {
        var positional: std.ArrayListUnmanaged([]const u8) = .{};
        defer positional.deinit(std.heap.page_allocator);

        self.global_args = parseCommonArgs(argv, &positional, std.heap.page_allocator) catch |err| {
            switch (err) {
                error.MissingValue => {
                    std.log.err("Flag requires a value argument", .{});
                    return 1;
                },
                else => return err,
            }
        };

        var ws: @import("io.zig").WriterState = .{};
        ws.initStdout();
        const stdout = ws.writer();

        if (self.global_args.show_help) {
            try self.printHelp(stdout);
            try stdout.flush();
            return 0;
        }

        if (self.global_args.show_version) {
            try stdout.print("{s} {s}\n", .{ self.name, self.version });
            try stdout.flush();
            return 0;
        }

        if (positional.items.len == 0) {
            try self.printHelp(stdout);
            try stdout.flush();
            return 0;
        }

        const cmd_name = positional.items[0];
        if (self.registry.get(cmd_name)) |cmd| {
            var iter = std.process.ArgIterator{};
            _ = iter.skip();
            for (positional.items[1..]) |_| {
                _ = iter.skip();
            }
            return (cmd.handler(&iter, std.heap.page_allocator) catch |err| {
                switch (err) {
                    CliError.UnknownCommand => {
                        try stdout.print("Unknown command: {s}\n", .{cmd_name});
                        return 1;
                    },
                    else => {
                        std.log.err("Command failed: {}", .{err});
                        return 1;
                    },
                }
            }) orelse 0;
        } else {
            try stdout.print("Unknown command: {s}\n", .{cmd_name});
            try stdout.flush();
            return 1;
        }
    }
};

const testing = std.testing;

test "CommandRegistry: register and get" {
    var registry = CommandRegistry.init(testing.allocator);
    defer registry.deinit();

    const cmd = Command{
        .name = "test",
        .description = "A test command",
        .handler = undefined,
    };

    try registry.register(cmd);
    const found = registry.get("test");
    try testing.expect(found != null);
    try testing.expectEqualStrings("test", found.?.name);
}

test "CommandRegistry: names returns sorted list" {
    var registry = CommandRegistry.init(testing.allocator);
    defer registry.deinit();

    try registry.register(.{ .name = "zebra", .description = "", .handler = undefined });
    try registry.register(.{ .name = "alpha", .description = "", .handler = undefined });
    try registry.register(.{ .name = "middle", .description = "", .handler = undefined });

    const names = try registry.names(testing.allocator);
    defer testing.allocator.free(names);

    try testing.expectEqual(@as(usize, 3), names.len);
    try testing.expectEqualStrings("alpha", names[0]);
    try testing.expectEqualStrings("middle", names[1]);
    try testing.expectEqualStrings("zebra", names[2]);
}
