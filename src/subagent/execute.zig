//! execute.zig — Tool dispatch via VTable + WorkUnit per-tool execution.
//!
//! Each tool is a Handler struct satisfying the `execute(arena) !void` contract
//! from the concurrency module. The SubagentTool VTable provides runtime
//! polymorphism (Pattern 4), while WorkUnit provides per-unit arena cleanup.
//!
//! Tools: BashTool, ReadTool, ExplainTool, EditTool, DiaryTool, ChecklistTool.

const std = @import("std");
const types = @import("types.zig");
const common_io = @import("common").io;
const string_mod = @import("common").string;
const concurrency = @import("concurrency");

pub const ToolVTable = struct {
    name: *const fn () []const u8,
    execute: *const fn (ptr: *anyopaque, allocator: std.mem.Allocator, params: *const types.ToolParams, io: *std.Io) anyerror!types.ToolResult,
    deinit: *const fn (ptr: *anyopaque) void,
};

pub const SubagentTool = struct {
    ptr: *anyopaque,
    vtable: *const ToolVTable,

    pub fn execute(self: SubagentTool, allocator: std.mem.Allocator, params: *const types.ToolParams, io: *std.Io) anyerror!types.ToolResult {
        return self.vtable.execute(self.ptr, allocator, params, io);
    }

    pub fn deinit(self: SubagentTool) void {
        self.vtable.deinit(self.ptr);
    }
};

pub fn toolName(action: types.ActionType) []const u8 {
    return switch (action) {
        .bash => "bash",
        .read => "read",
        .explain => "explain",
        .edit => "edit",
        .diary => "diary",
        .checklist => "checklist",
        .unknown => "unknown",
    };
}

// ── BashTool ──────────────────────────────────────────────────────────────────

pub const BashTool = struct {
    command_allowlist: []const []const u8,

    const vtable: ToolVTable = .{
        .name = name,
        .execute = executeFn,
        .deinit = deinitFn,
    };

    fn name(ptr: *anyopaque) []const u8 {
        _ = ptr;
        return "bash";
    }

    fn executeFn(ptr: *anyopaque, allocator: std.mem.Allocator, params: *const types.ToolParams, io: *std.Io) anyerror!types.ToolResult {
        const self: *BashTool = @ptrCast(@alignCast(ptr));
        return self.executeImpl(allocator, params, io);
    }

    fn deinitFn(ptr: *anyopaque) void {
        const self: *BashTool = @ptrCast(@alignCast(ptr));
        self.allocator().free(self);
    }

    fn executeImpl(self: *BashTool, allocator: std.mem.Allocator, params: *const types.ToolParams, io: *std.Io) anyerror!types.ToolResult {
        const command = params.command orelse return error.MissingCommand;

        var arena = std.heap.ArenaAllocator.init(allocator);
        defer arena.deinit();

        const parsed = @import("common").shell_parser.parseCommand(arena.allocator(), command) catch {
            return .{ .action = .bash, .success = false, .raw = try allocator.dupe(u8, "failed to parse command") };
        };
        defer {
            for (parsed) |arg| arena.allocator().free(arg);
            arena.allocator().free(parsed);
        }

        var allowed = false;
        for (self.command_allowlist) |allowed_cmd| {
            if (parsed.len > 0 and std.mem.eql(u8, parsed[0], allowed_cmd)) {
                allowed = true;
                break;
            }
        }
        if (!allowed) {
            return .{ .action = .bash, .success = false, .raw = try allocator.dupe(u8, "command not in allowlist") };
        }

        const result = std.process.run(allocator, io, .{
            .argv = parsed.ptr[0..parsed.len],
            .stdout_limit = .limited(1024 * 1024),
            .stderr_limit = .limited(256 * 1024),
        }) catch {
            return .{ .action = .bash, .success = false, .raw = try allocator.dupe(u8, "command execution failed") };
        };
        defer {
            allocator.free(result.stdout);
            allocator.free(result.stderr);
        }

        const success = switch (result.term) {
            .exited => |code| code == 0,
            else => false,
        };

        return .{
            .action = .bash,
            .success = success,
            .raw = try allocator.dupe(u8, result.stdout),
            .token_estimate = @intCast((result.stdout.len + 3) / 4),
        };
    }

    pub fn provider(self: *BashTool) SubagentTool {
        return .{ .ptr = @ptrCast(self), .vtable = &vtable };
    }
};

// ── ReadTool ──────────────────────────────────────────────────────────────────

pub const ReadTool = struct {
    workspace: []const u8,

    const vtable: ToolVTable = .{
        .name = name,
        .execute = executeFn,
        .deinit = deinitFn,
    };

    fn name(ptr: *anyopaque) []const u8 {
        _ = ptr;
        return "read";
    }

    fn executeFn(ptr: *anyopaque, allocator: std.mem.Allocator, params: *const types.ToolParams, io: *std.Io) anyerror!types.ToolResult {
        const self: *ReadTool = @ptrCast(@alignCast(ptr));
        return self.executeImpl(allocator, params, io);
    }

    fn deinitFn(ptr: *anyopaque) void {
        _ = ptr;
    }

    fn executeImpl(self: *ReadTool, allocator: std.mem.Allocator, params: *const types.ToolParams, io: *std.Io) anyerror!types.ToolResult {
        _ = io;
        const path = params.path orelse return error.MissingPath;

        const resolved = common_io.resolvePath(allocator, self.workspace, path) catch path;
        defer if (resolved.ptr != path.ptr) allocator.free(resolved);

        const content = common_io.readFileAlloc(allocator, resolved, 1024 * 1024) orelse
            return .{ .action = .read, .success = false, .raw = try std.fmt.allocPrint(allocator, "file not found: {s}", .{path}) };

        const line_start = params.line_start orelse 1;
        const line_end = params.line_end orelse line_start + 50;

        const excerpt = common_io.extractExcerpt(allocator, content, line_start, line_end) catch content;

        return .{
            .action = .read,
            .success = true,
            .raw = try allocator.dupe(u8, excerpt),
            .token_estimate = @intCast((excerpt.len + 3) / 4),
        };
    }

    pub fn provider(self: *ReadTool) SubagentTool {
        return .{ .ptr = @ptrCast(self), .vtable = &vtable };
    }
};

// ── ExplainTool ──────────────────────────────────────────────────────────────

pub const ExplainTool = struct {
    explain_fn: ?*const types.ExplainFn = null,

    const vtable: ToolVTable = .{
        .name = name,
        .execute = executeFn,
        .deinit = deinitFn,
    };

    fn name(ptr: *anyopaque) []const u8 {
        _ = ptr;
        return "explain";
    }

    fn executeFn(ptr: *anyopaque, allocator: std.mem.Allocator, params: *const types.ToolParams, io: *std.Io) anyerror!types.ToolResult {
        _ = io;
        const self: *ExplainTool = @ptrCast(@alignCast(ptr));
        return self.executeImpl(allocator, params);
    }

    fn deinitFn(ptr: *anyopaque) void {
        _ = ptr;
    }

    fn executeImpl(self: *ExplainTool, allocator: std.mem.Allocator, params: *const types.ToolParams) anyerror!types.ToolResult {
        const query = params.query orelse return error.MissingQuery;

        if (self.explain_fn) |fn_ptr| {
            if (fn_ptr(allocator, query)) |result| {
                var stages: std.ArrayList(types.ExplainStage) = .empty;
                if (result.content) |content| {
                    stages.append(allocator, .{
                        .kind = .code,
                        .content = try allocator.dupe(u8, content),
                        .source = try allocator.dupe(u8, result.path orelse "unknown"),
                        .line = result.line,
                        .relevance = 1.0,
                    }) catch {};
                }
                if (result.path) |path| {
                    stages.append(allocator, .{
                        .kind = .prose,
                        .content = try allocator.dupe(u8, query),
                        .source = try allocator.dupe(u8, path),
                        .line = result.line,
                        .relevance = 0.9,
                    }) catch {};
                }
                return .{
                    .action = .explain,
                    .success = true,
                    .stages = stages.items,
                    .token_estimate = @intCast((query.len + 3) / 4 + stages.items.len * 100),
                };
            }
        }

        return .{
            .action = .explain,
            .success = true,
            .structured = try allocator.dupe(u8, query),
            .token_estimate = @intCast((query.len + 3) / 4),
        };
    }

    pub fn provider(self: *ExplainTool) SubagentTool {
        return .{ .ptr = @ptrCast(self), .vtable = &vtable };
    }
};

// ── EditTool ──────────────────────────────────────────────────────────────────

pub const EditTool = struct {
    workspace: []const u8,
    allow_edit: bool = false,

    const vtable: ToolVTable = .{
        .name = name,
        .execute = executeFn,
        .deinit = deinitFn,
    };

    fn name(ptr: *anyopaque) []const u8 {
        _ = ptr;
        return "edit";
    }

    fn executeFn(ptr: *anyopaque, allocator: std.mem.Allocator, params: *const types.ToolParams, io: *std.Io) anyerror!types.ToolResult {
        const self: *EditTool = @ptrCast(@alignCast(ptr));
        return self.executeImpl(allocator, params, io);
    }

    fn deinitFn(ptr: *anyopaque) void {
        _ = ptr;
    }

    fn executeImpl(self: *EditTool, allocator: std.mem.Allocator, params: *const types.ToolParams, io: *std.Io) anyerror!types.ToolResult {
        if (!self.allow_edit) {
            return .{
                .action = .explain,
                .success = true,
                .raw = try std.fmt.allocPrint(allocator, "edit not allowed; would edit {s}", .{params.path orelse "unknown"}),
                .token_estimate = 20,
            };
        }

        const path = params.path orelse return error.MissingPath;
        const content = params.content orelse return error.MissingContent;

        const resolved = common_io.resolvePath(allocator, self.workspace, path) catch path;
        defer if (resolved.ptr != path.ptr) allocator.free(resolved);

        const existing = common_io.readFileAlloc(allocator, resolved, 10 * 1024 * 1024) orelse
            return .{ .action = .edit, .success = false, .raw = try std.fmt.allocPrint(allocator, "file not found: {s}", .{path}) };

        var lines: std.ArrayList([]const u8) = .empty;
        defer lines.deinit(allocator);
        var line_it = std.mem.splitScalar(u8, existing, '\n');
        while (line_it.next()) |line| {
            try lines.append(allocator, line);
        }

        const line_start = params.line_start orelse 1;
        const line_end = params.line_end orelse @min(lines.items.len, line_start + 20);

        var result_buf: std.ArrayList(u8) = .empty;
        errdefer result_buf.deinit(allocator);

        for (lines.items, 0..) |line, i| {
            const line_num: u32 = @intCast(i + 1);
            if (line_num >= line_start and line_num <= line_end) {
                try result_buf.appendSlice(allocator, content);
                try result_buf.append(allocator, '\n');
                if (line_num == line_start) continue;
            } else {
                try result_buf.appendSlice(allocator, line);
                try result_buf.append(allocator, '\n');
            }
        }

        const tmp_path = try std.fmt.allocPrint(allocator, "{s}.tmp", .{resolved});
        defer allocator.free(tmp_path);

        const new_file = std.Io.Dir.createFileAbsolute(io, tmp_path, .{}) catch {
            return .{ .action = .edit, .success = false, .raw = try allocator.dupe(u8, "failed to create temp file") };
        };
        defer new_file.close(io);

        var wbuf: [4096]u8 = undefined;
        var writer = new_file.writer(io, &wbuf);
        try writer.interface.writeAll(result_buf.items);
        try writer.interface.flush();

        std.Io.Dir.renameAbsolute(io, tmp_path, resolved) catch {
            std.Io.Dir.deleteFileAbsolute(io, tmp_path) catch {};
            return .{ .action = .edit, .success = false, .raw = try allocator.dupe(u8, "failed to rename temp file") };
        };

        allocator.free(existing);

        return .{
            .action = .edit,
            .success = true,
            .raw = try std.fmt.allocPrint(allocator, "edited {s} lines {d}-{d}", .{ path, line_start, line_end }),
            .token_estimate = 30,
        };
    }

    pub fn provider(self: *EditTool) SubagentTool {
        return .{ .ptr = @ptrCast(self), .vtable = &vtable };
    }
};

// ── DiaryTool ─────────────────────────────────────────────────────────────────

pub const DiaryTool = struct {
    workspace: []const u8,

    const vtable: ToolVTable = .{
        .name = name,
        .execute = executeFn,
        .deinit = deinitFn,
    };

    fn name(ptr: *anyopaque) []const u8 {
        _ = ptr;
        return "diary";
    }

    fn executeFn(ptr: *anyopaque, allocator: std.mem.Allocator, params: *const types.ToolParams, io: *std.Io) anyerror!types.ToolResult {
        const self: *DiaryTool = @ptrCast(@alignCast(ptr));
        return self.executeImpl(allocator, params, io);
    }

    fn deinitFn(ptr: *anyopaque) void {
        _ = ptr;
    }

    fn executeImpl(self: *DiaryTool, allocator: std.mem.Allocator, params: *const types.ToolParams, io: *std.Io) anyerror!types.ToolResult {
        const content = params.content orelse return error.MissingContent;

        const diary_path = try std.fmt.allocPrint(allocator, "{s}/DIARY.md", .{self.workspace});
        defer allocator.free(diary_path);

        const timestamp_ns = std.Io.Timestamp.now(io, .real).nanoseconds;
        const timestamp_s: i64 = @divTrunc(timestamp_ns, std.time.ns_per_s);

        var entry_buf: std.ArrayList(u8) = .empty;
        errdefer entry_buf.deinit(allocator);

        const existing = common_io.readFileAlloc(allocator, diary_path, 10 * 1024 * 1024);
        if (existing) |content_inner| {
            try entry_buf.appendSlice(allocator, content_inner);
            allocator.free(content_inner);
        }

        var aw: std.Io.Writer.Allocating = .init(allocator);
        errdefer aw.deinit();
        try aw.writer.print("\n## Entry {d}\n\n{s}\n", .{ timestamp_s, content });
        try entry_buf.appendSlice(allocator, aw.written());
        aw.deinit();

        const tmp_path = try std.fmt.allocPrint(allocator, "{s}.tmp", .{diary_path});
        defer allocator.free(tmp_path);

        const new_file = std.Io.Dir.createFileAbsolute(io, tmp_path, .{}) catch {
            return .{ .action = .diary, .success = false, .raw = try allocator.dupe(u8, "failed to create DIARY.md temp") };
        };
        defer new_file.close(io);

        var wbuf: [4096]u8 = undefined;
        var writer = new_file.writer(io, &wbuf);
        try writer.interface.writeAll(entry_buf.items);
        try writer.interface.flush();

        std.Io.Dir.renameAbsolute(io, tmp_path, diary_path) catch {
            std.Io.Dir.deleteFileAbsolute(io, tmp_path) catch {};
            return .{ .action = .diary, .success = false, .raw = try allocator.dupe(u8, "failed to rename DIARY.md") };
        };

        entry_buf.deinit(allocator);

        return .{
            .action = .diary,
            .success = true,
            .raw = try allocator.dupe(u8, "diary entry appended"),
            .token_estimate = 5,
        };
    }

    pub fn provider(self: *DiaryTool) SubagentTool {
        return .{ .ptr = @ptrCast(self), .vtable = &vtable };
    }
};

// ── ChecklistTool ─────────────────────────────────────────────────────────────

pub const ChecklistTool = struct {
    workspace: []const u8,

    const vtable: ToolVTable = .{
        .name = name,
        .execute = executeFn,
        .deinit = deinitFn,
    };

    fn name(ptr: *anyopaque) []const u8 {
        _ = ptr;
        return "checklist";
    }

    fn executeFn(ptr: *anyopaque, allocator: std.mem.Allocator, params: *const types.ToolParams, io: *std.Io) anyerror!types.ToolResult {
        const self: *ChecklistTool = @ptrCast(@alignCast(ptr));
        return self.executeImpl(allocator, params, io);
    }

    fn deinitFn(ptr: *anyopaque) void {
        _ = ptr;
    }

    fn executeImpl(self: *ChecklistTool, allocator: std.mem.Allocator, params: *const types.ToolParams, io: *std.Io) anyerror!types.ToolResult {
        _ = io;
        const item_index = params.item_index orelse return error.MissingItemIndex;

        const checklist_path = try std.fmt.allocPrint(allocator, "{s}/CHECKLIST.md", .{self.workspace});
        defer allocator.free(checklist_path);

        const content = common_io.readFileAlloc(allocator, checklist_path, 10 * 1024 * 1024) orelse
            return .{ .action = .checklist, .success = false, .raw = try std.fmt.allocPrint(allocator, "CHECKLIST.md not found: {s}", .{checklist_path}) };

        const toggled = fsm_internal.toggleChecklistItem(allocator, content, item_index, true) catch
            return .{ .action = .checklist, .success = false, .raw = try allocator.dupe(u8, content) };
        defer allocator.free(toggled);
        allocator.free(content);

        return .{
            .action = .checklist,
            .success = true,
            .raw = try std.fmt.allocPrint(allocator, "marked item {d} complete", .{item_index}),
            .token_estimate = 5,
        };
    }

    pub fn provider(self: *ChecklistTool) SubagentTool {
        return .{ .ptr = @ptrCast(self), .vtable = &vtable };
    }
};

const fsm_internal = @import("fsm.zig");

const testing = std.testing;

test "toolName: returns correct names" {
    try testing.expectEqualStrings("bash", toolName(.bash));
    try testing.expectEqualStrings("read", toolName(.read));
    try testing.expectEqualStrings("explain", toolName(.explain));
    try testing.expectEqualStrings("edit", toolName(.edit));
    try testing.expectEqualStrings("diary", toolName(.diary));
    try testing.expectEqualStrings("checklist", toolName(.checklist));
    try testing.expectEqualStrings("unknown", toolName(.unknown));
}
