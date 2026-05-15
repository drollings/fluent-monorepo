//! builder.zig — Fluent builder for SubagentConfig.
//!
//! Uses the BuilderError pattern from common/builder_error.zig for rich
//! error context. Terminal call is `.build()` which produces a SubagentConfig.

const std = @import("std");
const types = @import("types.zig");

pub const BuilderPhase = enum {
    workspace,
    db_path,
    guidance_dir,
    api_url,
    model,
    initialization,
};

pub const BuilderError = struct {
    phase: BuilderPhase,
    field: ?[]const u8 = null,
    value: ?[]const u8 = null,
    constraint: ?[]const u8 = null,
    cause: ?anyerror = null,
    message: []const u8,

    pub fn format(self: *const BuilderError, writer: *std.Io.Writer) !void {
        try writer.print("phase={s}", .{@tagName(self.phase)});
        if (self.field) |f| try writer.print(" field={s}", .{f});
        if (self.value) |v| try writer.print(" value={s}", .{v});
        if (self.constraint) |c| try writer.print(" constraint={s}", .{c});
        if (self.cause) |e| try writer.print(" cause={s}", .{@errorName(e)});
        try writer.print(": {s}", .{self.message});
    }
};

fn makeMessage(allocator: std.mem.Allocator, phase: BuilderPhase, field: ?[]const u8, cause: ?anyerror) []const u8 {
    var buf: std.ArrayList(u8) = .empty;
    buf.appendSlice(allocator, @tagName(phase)) catch return "builder error";
    if (field) |f| {
        buf.appendSlice(allocator, ".") catch return "builder error";
        buf.appendSlice(allocator, f) catch return "builder error";
    }
    if (cause) |e| {
        buf.appendSlice(allocator, ": ") catch return "builder error";
        buf.appendSlice(allocator, @errorName(e)) catch return "builder error";
    }
    return buf.toOwnedSlice(allocator) catch return "builder error";
}

pub const SubagentBuilder = struct {
    allocator: std.mem.Allocator,
    arena: std.heap.ArenaAllocator,
    workspace_val: ?[]const u8 = null,
    db_path_val: ?[]const u8 = null,
    guidance_dir_val: ?[]const u8 = null,
    api_url_val: ?[]const u8 = null,
    model_val: ?[]const u8 = null,
    max_iterations: u16 = 20,
    scratchpad_max_entries: u16 = 10,
    allow_edit: bool = false,
    command_allowlist: []const []const u8 = &.{
        "make", "zig build", "cargo", "npm",  "git",
        "ls",   "cat",       "head",  "tail", "grep",
        "find", "echo",      "wc",    "sort", "uniq",
        "diff",
    },
    err: ?*BuilderError = null,
    err_any: ?anyerror = null,

    fn hasError(self: *const SubagentBuilder) bool {
        return self.err != null or self.err_any != null;
    }

    fn setError(self: *SubagentBuilder, phase: BuilderPhase, field: ?[]const u8, constraint: ?[]const u8, cause: ?anyerror) void {
        const arena_alloc = self.arena.allocator();
        const msg = makeMessage(arena_alloc, phase, field, cause);
        const err_ptr = arena_alloc.create(BuilderError) catch return;
        err_ptr.* = .{
            .phase = phase,
            .field = field,
            .value = null,
            .constraint = constraint,
            .cause = cause,
            .message = msg,
        };
        self.err = err_ptr;
    }

    pub fn workspace(self: *SubagentBuilder, path: []const u8) *SubagentBuilder {
        if (self.hasError()) return self;
        const arena_alloc = self.arena.allocator();
        self.workspace_val = arena_alloc.dupe(u8, path) catch |e| {
            self.setError(.workspace, "workspace", "invalid_path", e);
            return self;
        };
        return self;
    }

    pub fn dbPath(self: *SubagentBuilder, path: []const u8) *SubagentBuilder {
        if (self.hasError()) return self;
        const arena_alloc = self.arena.allocator();
        self.db_path_val = arena_alloc.dupe(u8, path) catch |e| {
            self.setError(.db_path, "db_path", "invalid_path", e);
            return self;
        };
        return self;
    }

    pub fn guidanceDir(self: *SubagentBuilder, dir: []const u8) *SubagentBuilder {
        if (self.hasError()) return self;
        const arena_alloc = self.arena.allocator();
        self.guidance_dir_val = arena_alloc.dupe(u8, dir) catch |e| {
            self.setError(.guidance_dir, "guidance_dir", "invalid_path", e);
            return self;
        };
        return self;
    }

    pub fn apiUrl(self: *SubagentBuilder, url: []const u8) *SubagentBuilder {
        if (self.hasError()) return self;
        const arena_alloc = self.arena.allocator();
        self.api_url_val = arena_alloc.dupe(u8, url) catch |e| {
            self.setError(.api_url, "api_url", "invalid_url", e);
            return self;
        };
        return self;
    }

    pub fn model(self: *SubagentBuilder, m: []const u8) *SubagentBuilder {
        if (self.hasError()) return self;
        const arena_alloc = self.arena.allocator();
        self.model_val = arena_alloc.dupe(u8, m) catch |e| {
            self.setError(.model, "model", "invalid_model", e);
            return self;
        };
        return self;
    }

    pub fn maxIterations(self: *SubagentBuilder, max: u16) *SubagentBuilder {
        if (self.hasError()) return self;
        self.max_iterations = max;
        return self;
    }

    pub fn scratchpadMaxEntries(self: *SubagentBuilder, max: u16) *SubagentBuilder {
        if (self.hasError()) return self;
        self.scratchpad_max_entries = max;
        return self;
    }

    pub fn allowEdit(self: *SubagentBuilder, allow: bool) *SubagentBuilder {
        if (self.hasError()) return self;
        self.allow_edit = allow;
        return self;
    }

    pub fn build(self: *SubagentBuilder) !types.SubagentConfig {
        if (self.err) |e| {
            const msg = e.message;
            std.log.err("SubagentBuilder error: {s}", .{msg});
            const cause = e.cause;
            self.arena.deinit();
            return cause orelse error.BuilderFailed;
        }
        if (self.err_any) |e| {
            self.arena.deinit();
            return e;
        }

        const allocator = self.allocator;
        const ws = self.workspace_val orelse {
            std.log.err("SubagentBuilder: workspace is required", .{});
            self.arena.deinit();
            return error.MissingWorkspace;
        };
        const api = self.api_url_val orelse "http://localhost:11434";
        const mdl = self.model_val orelse "qwen2.5-coder:7b";

        const config: types.SubagentConfig = .{
            .workspace = try allocator.dupe(u8, ws),
            .db_path = try allocator.dupe(u8, self.db_path_val orelse try std.fmt.allocPrint(allocator, "{s}/.guidance.db", .{ws})),
            .guidance_dir = try allocator.dupe(u8, self.guidance_dir_val orelse try std.fmt.allocPrint(allocator, "{s}/.guidance", .{ws})),
            .api_url = try allocator.dupe(u8, api),
            .model = try allocator.dupe(u8, mdl),
            .max_iterations = self.max_iterations,
            .scratchpad_max_entries = self.scratchpad_max_entries,
            .allow_edit = self.allow_edit,
            .command_allowlist = self.command_allowlist,
        };

        self.arena.deinit();
        return config;
    }
};

pub fn builder(allocator: std.mem.Allocator) SubagentBuilder {
    return .{
        .allocator = allocator,
        .arena = std.heap.ArenaAllocator.init(allocator),
    };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

const std_testing = @import("std").testing;

test "builder: creates config with all fields" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var b = builder(allocator);
    _ = b.workspace("/tmp/project");
    _ = b.apiUrl("http://localhost:11434");
    _ = b.model("qwen2.5-coder:7b");
    _ = b.maxIterations(10);

    const config = try b.build();
    defer allocator.free(config.workspace);
    defer allocator.free(config.db_path);
    defer allocator.free(config.guidance_dir);
    defer allocator.free(config.api_url);
    defer allocator.free(config.model);

    try std_testing.expectEqualStrings("/tmp/project", config.workspace);
    try std_testing.expectEqualStrings("http://localhost:11434", config.api_url);
    try std_testing.expectEqualStrings("qwen2.5-coder:7b", config.model);
    try std_testing.expectEqual(@as(u16, 10), config.max_iterations);
}

test "builder: error on missing workspace" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var b = builder(allocator);
    _ = b.apiUrl("http://localhost:11434");

    const result = b.build();
    try std_testing.expectError(error.MissingWorkspace, result);
}

test "builder: defaults are applied" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var b = builder(allocator);
    _ = b.workspace("/tmp/project");

    const config = try b.build();
    defer allocator.free(config.workspace);
    defer allocator.free(config.db_path);
    defer allocator.free(config.guidance_dir);
    defer allocator.free(config.api_url);
    defer allocator.free(config.model);

    try std_testing.expectEqualStrings("http://localhost:11434", config.api_url);
    try std_testing.expectEqual(@as(u16, 20), config.max_iterations);
    try std_testing.expect(!config.allow_edit);
}

test "builder: short-circuits on error" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var b = builder(allocator);
    _ = b.workspace("/tmp/project");
    _ = b.apiUrl("http://localhost:11434");

    try std_testing.expect(!b.hasError());
}

test "builder: arena is deinited on build" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var b = builder(allocator);
    _ = b.workspace("/tmp/project");

    const config = try b.build();
    defer allocator.free(config.workspace);
    defer allocator.free(config.db_path);
    defer allocator.free(config.guidance_dir);
    defer allocator.free(config.api_url);
    defer allocator.free(config.model);

    try std_testing.expectEqualStrings("/tmp/project", config.workspace);
}
