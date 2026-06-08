//! validate.zig — Schema + path existence + command allowlist validation.
//!
//! Validates ToolParams before execution. Uses common/shell_parser.parseCommand
//! for proper command tokenization, and common/error_context.ErrorContext for
//! structured validation error messages.

const std = @import("std");
const types = @import("types.zig");
const shell_parser = @import("common").shell_parser;
const err_ctx = @import("common").error_context;
const io_mod = @import("common").io;

pub const ValidationResult = struct {
    valid: bool,
    errors: []const ValidationError,
};

pub const ValidationError = struct {
    field: []const u8,
    message: []const u8,
    constraint: ?[]const u8 = null,
};

pub fn validateParams(
    allocator: std.mem.Allocator,
    params: *const types.ToolParams,
    workspace: []const u8,
    command_allowlist: []const []const u8,
) ValidationResult {
    var errors: std.ArrayList(ValidationError) = .empty;
    defer errors.deinit(allocator);

    switch (params.action) {
        .bash => validateBash(allocator, params, command_allowlist, &errors),
        .read => validateRead(allocator, params, workspace, &errors),
        .explain => validateExplain(allocator, params, &errors),
        .edit => validateEdit(allocator, params, workspace, &errors),
        .diary => {},
        .checklist => {},
        .unknown => {
            errors.append(allocator, .{
                .field = "action",
                .message = "unknown action type",
                .constraint = "must be one of: bash, read, explain, edit, diary, checklist",
            }) catch {};
        },
    }

    const owned = errors.toOwnedSlice(allocator) catch &.{};
    return .{
        .valid = owned.len == 0,
        .errors = owned,
    };
}

fn validateBash(
    allocator: std.mem.Allocator,
    params: *const types.ToolParams,
    command_allowlist: []const []const u8,
    errors: *std.ArrayList(ValidationError),
) void {
    const command = params.command orelse {
        errors.append(allocator, .{
            .field = "command",
            .message = "bash action requires a command",
            .constraint = "must not be null",
        }) catch {};
        return;
    };

    if (command.len == 0) {
        errors.append(allocator, .{
            .field = "command",
            .message = "command must not be empty",
            .constraint = "non-empty string",
        }) catch {};
        return;
    }

    const parsed = shell_parser.parseCommand(allocator, command) catch {
        errors.append(allocator, .{
            .field = "command",
            .message = "command contains shell metacharacters",
            .constraint = "no pipes, redirects, or shell operators",
        }) catch {};
        return;
    };
    defer {
        for (parsed) |arg| allocator.free(arg);
        allocator.free(parsed);
    }

    if (parsed.len == 0) {
        errors.append(allocator, .{
            .field = "command",
            .message = "parsed command is empty",
            .constraint = "must produce at least one argv element",
        }) catch {};
        return;
    }

    const base_cmd = parsed[0];
    var allowed = false;
    for (command_allowlist) |allowed_cmd| {
        if (std.mem.eql(u8, base_cmd, allowed_cmd)) {
            allowed = true;
            break;
        }
    }
    if (!allowed) {
        errors.append(allocator, .{
            .field = "command",
            .message = "command not in allowlist",
            .constraint = base_cmd,
        }) catch {};
    }
}

fn validateRead(
    allocator: std.mem.Allocator,
    params: *const types.ToolParams,
    workspace: []const u8,
    errors: *std.ArrayList(ValidationError),
) void {
    const path = params.path orelse {
        if (params.query == null) {
            errors.append(allocator, .{
                .field = "path",
                .message = "read action requires a path or query",
                .constraint = "at least one must be set",
            }) catch {};
        }
        return;
    };

    if (path.len == 0) {
        errors.append(allocator, .{
            .field = "path",
            .message = "path must not be empty",
            .constraint = "non-empty string",
        }) catch {};
        return;
    }

    const resolved = io_mod.resolvePath(allocator, workspace, path) catch path;
    defer if (resolved.ptr != path.ptr) allocator.free(resolved);

    const io = std.Io.Threaded.global_single_threaded.io();
    std.Io.Dir.accessAbsolute(io, resolved, .{}) catch {
        errors.append(allocator, .{
            .field = "path",
            .message = "file does not exist",
            .constraint = path,
        }) catch {};
    };

    if (params.line_start) |start| {
        if (start == 0) {
            errors.append(allocator, .{
                .field = "line_start",
                .message = "line_start must be >= 1",
                .constraint = "1-based line numbers",
            }) catch {};
        }
        if (params.line_end) |end| {
            if (end < start) {
                errors.append(allocator, .{
                    .field = "line_end",
                    .message = "line_end must be >= line_start",
                    .constraint = "valid line range",
                }) catch {};
            }
        }
    }
}

fn validateExplain(
    allocator: std.mem.Allocator,
    params: *const types.ToolParams,
    errors: *std.ArrayList(ValidationError),
) void {
    if (params.query == null and params.path == null) {
        errors.append(allocator, .{
            .field = "query",
            .message = "explain action requires a query or path",
            .constraint = "at least one must be set",
        }) catch {};
    }
}

fn validateEdit(
    allocator: std.mem.Allocator,
    params: *const types.ToolParams,
    workspace: []const u8,
    errors: *std.ArrayList(ValidationError),
) void {
    if (params.path == null) {
        errors.append(allocator, .{
            .field = "path",
            .message = "edit action requires a path",
            .constraint = "must not be null",
        }) catch {};
    }
    if (params.content == null) {
        errors.append(allocator, .{
            .field = "content",
            .message = "edit action requires content",
            .constraint = "must not be null",
        }) catch {};
    }

    if (params.path) |path| {
        if (path.len > 0) {
            const resolved = io_mod.resolvePath(allocator, workspace, path) catch path;
            defer if (resolved.ptr != path.ptr) allocator.free(resolved);

            const io = std.Io.Threaded.global_single_threaded.io();
            std.Io.Dir.accessAbsolute(io, resolved, .{}) catch {
                errors.append(allocator, .{
                    .field = "path",
                    .message = "file does not exist for edit",
                    .constraint = path,
                }) catch {};
            };
        }
    }
}

pub fn deinit(allocator: std.mem.Allocator, result: *ValidationResult) void {
    for (result.errors) |e| {
        allocator.free(e.field);
        allocator.free(e.message);
        if (e.constraint) |c| allocator.free(c);
    }
    allocator.free(result.errors);
}

const testing = std.testing;

test "validateParams: valid bash command" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const allowlist = &[_][]const u8{ "make", "zig build" };

    const params: types.ToolParams = .{ .action = .bash, .command = "make test" };
    var result = validateParams(allocator, &params, "/tmp", allowlist);
    defer deinit(allocator, &result);
    try testing.expect(result.valid);
}

test "validateParams: missing bash command" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const allowlist = &[_][]const u8{};

    const params: types.ToolParams = .{ .action = .bash };
    var result = validateParams(allocator, &params, "/tmp", allowlist);
    defer deinit(allocator, &result);
    try testing.expect(!result.valid);
}

test "validateParams: command not in allowlist" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const allowlist = &[_][]const u8{ "make", "zig build" };

    const params: types.ToolParams = .{ .action = .bash, .command = "rm -rf /" };
    var result = validateParams(allocator, &params, "/tmp", allowlist);
    defer deinit(allocator, &result);
    try testing.expect(!result.valid);
}

test "validateParams: shell metacharacters blocked" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const allowlist = &[_][]const u8{"make"};

    const params: types.ToolParams = .{ .action = .bash, .command = "make; rm -rf /" };
    var result = validateParams(allocator, &params, "/tmp", allowlist);
    defer deinit(allocator, &result);
    try testing.expect(!result.valid);
}

test "validateParams: diary and checklist always valid" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const diary_params: types.ToolParams = .{ .action = .diary, .content = "entry text" };
    const result1 = validateParams(allocator, &diary_params, "/tmp", &.{});
    try testing.expect(result1.valid);

    const checklist_params: types.ToolParams = .{ .action = .checklist, .item_index = 0 };
    const result2 = validateParams(allocator, &checklist_params, "/tmp", &.{});
    try testing.expect(result2.valid);
}

test "validateParams: explain requires query or path" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const params: types.ToolParams = .{ .action = .explain };
    var result = validateParams(allocator, &params, "/tmp", &.{});
    defer deinit(allocator, &result);
    try testing.expect(!result.valid);

    const valid_params: types.ToolParams = .{ .action = .explain, .query = "test" };
    const result3 = validateParams(allocator, &valid_params, "/tmp", &.{});
    try testing.expect(result3.valid);
}
